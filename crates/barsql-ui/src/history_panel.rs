use std::time::Duration;

use barsql_core::{ConnectionConfig, HistoryEntry};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt, DropdownMenu, PopupMenu, PopupMenuItem};
use gpui_kit::component::scroll::ScrollbarAxis;
use gpui_kit::component::{ActiveTheme, Icon, IconName, Sizable, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use jiff::{Timestamp, Zoned};

use crate::dialogs::{self, Confirm};
use crate::form;
use crate::i18n::{I18n, t, t_with};
use crate::list_nav::{self, LIST_INSET, ListNav, NavDelete, NavDown, NavFirst, NavLast, NavOpen, NavUp, ROW_INSET};
use crate::relative_time::{format_relative_time, one_line_preview, time_bucket};
use crate::saved_queries;
use crate::scrollbars::HoverScrollbar as _;
use crate::state;
use crate::toast;
use crate::tokens::{RADIUS, TEXT_2XS, TEXT_XS};

const HISTORY_LIMIT: usize = 200;
const FILTER_DEBOUNCE: Duration = Duration::from_millis(150);

pub enum HistoryEvent {
    Open { connection_id: String, sql: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Connection,
    All,
}

// Shown as the Recent tab. Newest runs first, grouped by day.
pub struct HistoryPanel {
    connection_id: Option<String>,
    connections: Vec<ConnectionConfig>,
    scope: Scope,
    entries: Vec<HistoryEntry>,
    filter: Entity<InputState>,
    needle: String,
    filter_task: Task<()>,
    nav: ListNav,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<HistoryEvent> for HistoryPanel {}

impl HistoryPanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let placeholder = t(cx, "sidebar.filterHistory");
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        let subscriptions = vec![
            cx.subscribe(&filter, |this, filter, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    let value = filter.read(cx).value().trim().to_lowercase();
                    this.filter_task = cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(FILTER_DEBOUNCE).await;
                        let _ = this.update(cx, |this, cx| {
                            this.needle = value;
                            cx.notify();
                        });
                    });
                }
            }),
            cx.observe_global_in::<I18n>(window, |this, window, cx| {
                let placeholder = t(cx, "sidebar.filterHistory");
                this.filter.update(cx, |filter, cx| filter.set_placeholder(placeholder, window, cx));
            }),
        ];
        Self {
            connection_id: None,
            connections: Vec::new(),
            scope: Scope::Connection,
            entries: Vec::new(),
            filter,
            needle: String::new(),
            filter_task: Task::ready(()),
            nav: ListNav::new(cx),
            _subscriptions: subscriptions,
        }
    }

    pub fn set_connection(&mut self, id: Option<String>, connections: Vec<ConnectionConfig>, cx: &mut Context<Self>) {
        self.connections = connections;
        if self.connection_id != id {
            self.connection_id = id;
            self.reload(cx);
        }
    }

    // An empty id lists every connection's runs.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let id = match (self.scope, &self.connection_id) {
            (Scope::Connection, Some(id)) => id.clone(),
            _ => String::new(),
        };
        self.entries = state::bar(cx).query_history(&id, HISTORY_LIMIT);
        cx.notify();
    }

    fn set_scope(&mut self, scope: Scope, cx: &mut Context<Self>) {
        self.scope = scope;
        self.reload(cx);
    }

    fn visible(&self) -> Vec<&HistoryEntry> {
        self.entries
            .iter()
            .filter(|e| {
                self.needle.is_empty()
                    || e.sql.to_lowercase().contains(&self.needle)
                    || e.error.to_lowercase().contains(&self.needle)
            })
            .collect()
    }

    fn delete(&mut self, id: &str, cx: &mut Context<Self>) {
        state::bar(cx).delete_query_history_entry(id);
        self.reload(cx);
    }

    fn clear_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let all = self.scope == Scope::All;
        let id = if all { String::new() } else { self.connection_id.clone().unwrap_or_default() };
        if !all && id.is_empty() {
            dialogs::alert(t(cx, "errors.noConnection"), t(cx, "dialog.noDatabaseDescription"), window, cx);
            return;
        }
        let name = self.connections.iter().find(|c| c.id == id).map(|c| c.name.clone());
        let description = match name.filter(|_| !all) {
            Some(name) => t_with(cx, "dialog.clearHistoryDescription", &[("name", &name)]),
            None => t(cx, "dialog.clearHistoryDescriptionGeneric"),
        };
        let confirm = Confirm {
            title: t(cx, "dialog.clearHistoryTitle"),
            description,
            detail: None,
            confirm: t(cx, "dialog.clearHistoryConfirm"),
            danger: true,
        };
        let this = cx.entity().downgrade();
        dialogs::confirm(confirm, window, cx, move |window, cx| {
            if let Err(error) = state::bar(cx).clear_query_history(&id) {
                saved_queries::failed(error.message, "errors.generic", window, cx);
            }
            let _ = this.update(cx, |this, cx| this.reload(cx));
        });
    }

    fn save_as_query(connection_id: String, sql: String, window: &mut Window, cx: &mut App) {
        saved_queries::save_as_new(connection_id, sql, window, cx, |_, _, cx| {
            toast::success(t(cx, "toast.savedQuery"), cx);
        });
    }

    fn options_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let this = cx.entity().downgrade();
        let scope = self.scope;
        let empty = self.entries.is_empty();
        form::filter_button("history-options", Icon::new(Lucide::SlidersHorizontal))
            .debug_selector(|| "history-options".into())
            .tooltip(t(cx, "tooltip.queryOptions"))
            .dropdown_menu(move |menu: PopupMenu, _, cx| {
                let pick = |target: Scope| {
                    let this = this.clone();
                    move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                        let _ = this.update(cx, |this, cx| this.set_scope(target, cx));
                    }
                };
                let clear = this.clone();
                menu.item(
                    PopupMenuItem::new(t(cx, "sidebar.scopeConnection"))
                        .icon(Icon::new(Lucide::Database))
                        .checked(scope == Scope::Connection)
                        .on_click(pick(Scope::Connection)),
                )
                .item(
                    PopupMenuItem::new(t(cx, "sidebar.scopeAll"))
                        .icon(Icon::new(IconName::Globe))
                        .checked(scope == Scope::All)
                        .on_click(pick(Scope::All)),
                )
                .separator()
                .item(
                    PopupMenuItem::new(t(cx, "sidebar.clearAll"))
                        .icon(Icon::new(Lucide::Trash))
                        .disabled(empty)
                        .on_click(move |_, window, cx| {
                            let _ = clear.update(cx, |this, cx| this.clear_all(window, cx));
                        }),
                )
            })
    }

    fn row_menu(
        &self,
        entry: &HistoryEntry,
        cx: &mut Context<Self>,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        let this = cx.entity().downgrade();
        let (id, connection_id, sql) = (entry.id.clone(), entry.connection_id.clone(), entry.sql.clone());
        move |menu, _, cx| {
            let (open, delete) = (this.clone(), this.clone());
            let (open_conn, open_sql) = (connection_id.clone(), sql.clone());
            let (save_conn, save_sql) = (connection_id.clone(), sql.clone());
            let copy_sql = sql.clone();
            let delete_id = id.clone();
            menu.item(PopupMenuItem::new(t(cx, "sidebar.openQuery")).on_click(move |_, _, cx| {
                let event = HistoryEvent::Open { connection_id: open_conn.clone(), sql: open_sql.clone() };
                let _ = open.update(cx, |_, cx| cx.emit(event));
            }))
            .item(
                PopupMenuItem::new(t(cx, "sidebar.saveAsQuery")).on_click(move |_, window, cx| {
                    Self::save_as_query(save_conn.clone(), save_sql.clone(), window, cx)
                }),
            )
            .item(
                PopupMenuItem::new(t(cx, "sidebar.copySql"))
                    .on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(copy_sql.clone()))),
            )
            .separator()
            .item(PopupMenuItem::new(t(cx, "common.delete")).on_click(move |_, _, cx| {
                let _ = delete.update(cx, |this, cx| this.delete(&delete_id, cx));
            }))
        }
    }

    // Entry ids with their child index in the list. Day headers count as children.
    fn nav_rows(&self) -> Vec<(String, usize)> {
        let now = Zoned::now();
        let (mut rows, mut child, mut last) = (Vec::new(), 0, None);
        for entry in self.visible() {
            let bucket = time_bucket(&entry.executed_at, &now);
            if last != Some(bucket) {
                last = Some(bucket);
                child += 1;
            }
            rows.push((entry.id.clone(), child));
            child += 1;
        }
        rows
    }

    #[cfg(test)]
    pub fn focus_list(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.nav.focus_list(window, cx);
    }

    #[cfg(test)]
    pub(crate) fn listed(&self) -> Vec<String> {
        self.visible().into_iter().map(|entry| entry.sql.clone()).collect()
    }

    #[cfg(test)]
    pub fn show_entries(&mut self, entries: Vec<HistoryEntry>, cx: &mut Context<Self>) {
        self.scope = Scope::All;
        self.entries = entries;
        cx.notify();
    }

    fn step(&mut self, step: isize, cx: &mut Context<Self>) {
        let rows = self.nav_rows();
        self.nav.step(&rows, step);
        cx.notify();
    }

    fn nav_entry(&self) -> Option<HistoryEntry> {
        let id = self.nav.cursor()?;
        self.visible().into_iter().find(|entry| entry.id == id).cloned()
    }

    fn render_entry(
        &self,
        ix: usize,
        entry: &HistoryEntry,
        now_ms: i64,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let ring = self.nav.ringed(&entry.id, window);
        let theme = cx.theme().clone();
        let lang = cx.global::<I18n>().lang();
        let mut meta = format!("{} · {}ms", format_relative_time(&entry.executed_at, lang, now_ms), entry.duration_ms);
        if self.scope == Scope::All
            && let Some(connection) = self.connections.iter().find(|c| c.id == entry.connection_id)
        {
            meta.push_str(&format!(" · {}", connection.name));
        }
        let error = (!entry.success && !entry.error.is_empty()).then(|| one_line_preview(&entry.error, 100));
        let (open_conn, open_sql) = (entry.connection_id.clone(), entry.sql.clone());
        let (save_conn, save_sql) = (entry.connection_id.clone(), entry.sql.clone());
        let delete_id = entry.id.clone();
        let menu = self.row_menu(entry, cx);
        h_flex()
            .id(SharedString::from(format!("history-{ix}-{}", entry.id)))
            .debug_selector(move || format!("history-row-{ix}"))
            .group("history-row")
            .relative()
            .w_full()
            .px(ROW_INSET)
            .py_1()
            .rounded(RADIUS)
            .gap_2()
            .items_start()
            .cursor_pointer()
            .hover(|s| s.bg(theme.sidebar_accent))
            .child(div().flex_none().mt(px(6.)).size(px(6.)).rounded_full().bg(if entry.success {
                theme.success
            } else {
                theme.danger
            }))
            .child(
                v_flex()
                    .debug_selector(move || format!("history-text-{ix}"))
                    .flex_1()
                    .min_w_0()
                    // Without the ellipsis, the clamp leaves the rest on the last line, clipped mid-glyph.
                    .child(
                        div()
                            .line_clamp(2)
                            .text_ellipsis()
                            .text_size(TEXT_XS)
                            .line_height(relative(1.45))
                            .font_family(theme.mono_font_family.clone())
                            .child(one_line_preview(&entry.sql, 120)),
                    )
                    .child(div().truncate().text_size(TEXT_2XS).text_color(theme.muted_foreground).child(meta))
                    .children(error.map(|e| div().truncate().text_size(TEXT_2XS).text_color(theme.danger).child(e))),
            )
            .child(
                self.nav
                    .hover_actions("history-row", ROW_INSET, LIST_INSET, window, cx)
                    .child(
                        Button::new(SharedString::from(format!("history-save-{ix}")))
                            .small()
                            .icon(Icon::new(Lucide::BookmarkPlus))
                            .tooltip(t(cx, "sidebar.saveAsQuery"))
                            .on_click(move |_, window, cx| {
                                cx.stop_propagation();
                                Self::save_as_query(save_conn.clone(), save_sql.clone(), window, cx);
                            }),
                    )
                    .child(
                        Button::new(SharedString::from(format!("history-delete-{ix}")))
                            .small()
                            .danger()
                            .outline()
                            .debug_selector(move || format!("history-delete-{ix}"))
                            .icon(Icon::new(Lucide::Trash))
                            .tooltip(t(cx, "tooltip.deleteHistory"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.delete(&delete_id, cx);
                            })),
                    ),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener({
                    let id = entry.id.clone();
                    move |this, _, window, cx| this.nav.pick(&id, window, cx)
                }),
            )
            .when(ring, |el| list_nav::ring(el, cx))
            .on_click(cx.listener(move |_, _, _, cx| {
                cx.emit(HistoryEvent::Open { connection_id: open_conn.clone(), sql: open_sql.clone() })
            }))
            .context_menu(menu)
            .into_any_element()
    }
}

impl Render for HistoryPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let field = div()
            .debug_selector(|| "history-filter".into())
            .child(form::filter_input(&self.filter, window, cx).cleanable(true));
        let filter_bar = form::filter_bar(field, cx).child(self.options_menu(cx));
        let visible_len = self.visible().len();
        let body = if self.entries.is_empty() {
            form::empty_state(Some(Icon::new(Lucide::Clock)), t(cx, "sidebar.noQueryHistory"), cx).into_any_element()
        } else if visible_len == 0 {
            form::empty_state(None, t(cx, "sidebar.noMatches"), cx).into_any_element()
        } else {
            let now_ms = Timestamp::now().as_millisecond();
            let now = Zoned::now();
            let entries: Vec<HistoryEntry> = self.visible().into_iter().cloned().collect();
            let mut rows: Vec<AnyElement> = Vec::new();
            let mut last = None;
            for (ix, entry) in entries.iter().enumerate() {
                let bucket = time_bucket(&entry.executed_at, &now);
                if last != Some(bucket) {
                    last = Some(bucket);
                    rows.push(form::caption(t(cx, bucket.label_key()), cx).px_2().pt_2().pb_1().into_any_element());
                }
                rows.push(self.render_entry(ix, entry, now_ms, window, cx));
            }
            let list = v_flex()
                .id("history-list")
                .debug_selector(|| "history-list".into())
                .key_context(list_nav::CONTEXT)
                .track_focus(&self.nav.focus)
                .track_scroll(&self.nav.scroll)
                .size_full()
                .px(LIST_INSET)
                .pb(LIST_INSET)
                .overflow_y_scroll()
                .on_action(cx.listener(|this, _: &NavUp, _, cx| this.step(-1, cx)))
                .on_action(cx.listener(|this, _: &NavDown, _, cx| this.step(1, cx)))
                .on_action(cx.listener(|this, _: &NavFirst, _, cx| this.step(isize::MIN, cx)))
                .on_action(cx.listener(|this, _: &NavLast, _, cx| this.step(isize::MAX, cx)))
                .on_action(cx.listener(|this, _: &NavOpen, _, cx| {
                    if let Some(entry) = this.nav_entry() {
                        cx.emit(HistoryEvent::Open { connection_id: entry.connection_id, sql: entry.sql });
                    }
                }))
                .on_action(cx.listener(|this, _: &NavDelete, _, cx| {
                    if let Some(entry) = this.nav_entry() {
                        this.delete(&entry.id, cx);
                    }
                }))
                .children(rows);
            div()
                .relative()
                .size_full()
                .child(list)
                .child(self.nav.clearance.watch(self.nav.scroll.clone()))
                .hover_scrollbar(&self.nav.scroll, ScrollbarAxis::Vertical)
                .into_any_element()
        };
        v_flex().size_full().child(filter_bar).child(div().flex_1().min_h_0().child(body))
    }
}
