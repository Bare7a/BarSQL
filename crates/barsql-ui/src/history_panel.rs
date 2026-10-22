use std::rc::Rc;
use std::time::Duration;

use barsql_core::{ConnectionConfig, HistoryEntry};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::ButtonVariants;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt, DropdownMenu, PopupMenu, PopupMenuItem};
use gpui_kit::component::scroll::ScrollbarAxis;
use gpui_kit::component::{ActiveTheme, Icon, IconName, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use jiff::civil::Date;
use jiff::{Timestamp, Zoned};

use crate::dialogs::{self, Confirm};
use crate::form;
use crate::i18n::{I18n, t, t_with};
use crate::list_nav::{self, LIST_INSET, ListNav, NavDelete, NavDown, NavFirst, NavLast, NavOpen, NavUp, ROW_INSET};
use crate::relative_time::{TimeBucket, format_relative_time, one_line_preview, time_bucket};
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

// A row of the list: a day's caption, or entry `entry` at place `ix` among the runs shown.
#[derive(Clone, Copy)]
enum Row {
    Day(TimeBucket),
    Entry { entry: usize, ix: usize },
}

// Shown as the Recent tab. Newest runs first, grouped by day.
pub struct HistoryPanel {
    connection_id: Option<String>,
    connections: Vec<ConnectionConfig>,
    scope: Scope,
    entries: Vec<HistoryEntry>,
    // The runs the filter lets through, under their days as of `day`. The list builds only the rows on screen.
    rows: Vec<Row>,
    day: Date,
    filter: Entity<InputState>,
    needle: String,
    filter_task: Task<()>,
    nav: ListNav<ListState>,
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
                            this.rebuild();
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
            rows: Vec::new(),
            day: Zoned::now().date(),
            filter,
            needle: String::new(),
            filter_task: Task::ready(()),
            // Measuring every row once a change lays them out sizes the bar right from the start.
            nav: ListNav::with_scroll(ListState::new(0, ListAlignment::Top, px(200.)).measure_all(), cx),
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
        self.rebuild();
        cx.notify();
    }

    fn set_scope(&mut self, scope: Scope, cx: &mut Context<Self>) {
        self.scope = scope;
        self.reload(cx);
    }

    // Lays out the runs the filter lets through under their days, and keeps the list where it was.
    fn rebuild(&mut self) {
        self.rebuild_as_of(Zoned::now());
    }

    fn rebuild_as_of(&mut self, now: Zoned) {
        self.day = now.date();
        let needle = &self.needle;
        let shown = self.entries.iter().enumerate().filter(|(_, e)| {
            needle.is_empty() || e.sql.to_lowercase().contains(needle) || e.error.to_lowercase().contains(needle)
        });
        let (mut rows, mut last) = (Vec::new(), None);
        for (ix, (entry, e)) in shown.enumerate() {
            let bucket = time_bucket(&e.executed_at, &now);
            if last != Some(bucket) {
                last = Some(bucket);
                rows.push(Row::Day(bucket));
            }
            rows.push(Row::Entry { entry, ix });
        }
        self.rows = rows;
        let top = self.nav.scroll.logical_scroll_top();
        self.nav.scroll.reset(self.rows.len());
        self.nav.scroll.scroll_to(top);
    }

    fn visible(&self) -> impl Iterator<Item = &HistoryEntry> {
        self.rows.iter().filter_map(|row| match *row {
            Row::Entry { entry, .. } => Some(&self.entries[entry]),
            Row::Day(_) => None,
        })
    }

    // At once, with an Undo on the toast.
    fn delete(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(entry) = self.entries.iter().find(|entry| entry.id == id).cloned() else { return };
        let result = state::bar(cx).delete_query_history_entry(id);
        self.reload(cx);
        if let Err(error) = result {
            return toast::error_in(t(cx, "errors.deleteHistoryFailed"), error.message, cx);
        }
        let this = cx.entity().downgrade();
        let undo = toast::ToastAction {
            label: t(cx, "common.undo"),
            on_click: Rc::new(move |_, cx| {
                if let Err(error) = state::bar(cx).restore_query_history_entry(entry.clone()) {
                    toast::error(error.message, cx);
                }
                let _ = this.update(cx, |this, cx| this.reload(cx));
            }),
        };
        toast::push(t(cx, "toast.historyDeleted"), toast::ToastKind::Success, None, Some(undo), cx);
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
                    PopupMenuItem::new(t(cx, "sidebar.clearHistory"))
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
            .item(PopupMenuItem::new(t(cx, "sidebar.copySql")).on_click(move |_, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(copy_sql.clone()));
                toast::success(t(cx, "toast.copiedClipboard"), cx);
            }))
            .separator()
            .item(PopupMenuItem::new(t(cx, "common.delete")).on_click(move |_, _, cx| {
                let _ = delete.update(cx, |this, cx| this.delete(&delete_id, cx));
            }))
        }
    }

    // Entry ids with their row in the list. Day captions count as rows.
    fn nav_rows(&self) -> Vec<(String, usize)> {
        let rows = self.rows.iter().enumerate();
        rows.filter_map(|(row, kind)| match *kind {
            Row::Entry { entry, .. } => Some((self.entries[entry].id.clone(), row)),
            Row::Day(_) => None,
        })
        .collect()
    }

    #[cfg(test)]
    pub fn focus_list(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.nav.focus_list(window, cx);
    }

    #[cfg(test)]
    pub(crate) fn listed(&self) -> Vec<String> {
        self.visible().map(|entry| entry.sql.clone()).collect()
    }

    #[cfg(test)]
    pub fn show_entries(&mut self, entries: Vec<HistoryEntry>, cx: &mut Context<Self>) {
        self.scope = Scope::All;
        self.entries = entries;
        self.rebuild();
        cx.notify();
    }

    fn step(&mut self, step: isize, cx: &mut Context<Self>) {
        let rows = self.nav_rows();
        self.nav.step(&rows, step);
        cx.notify();
    }

    fn nav_entry(&self) -> Option<HistoryEntry> {
        let id = self.nav.cursor()?;
        self.visible().find(|entry| entry.id == id).cloned()
    }

    // Row `row` of the list, padded at its sides as the list's own padding would: a `list` pads only its ends.
    fn render_row(
        &self,
        row: usize,
        now_ms: i64,
        clearance: Pixels,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let element = match self.rows.get(row) {
            Some(Row::Day(bucket)) => {
                form::caption(t(cx, bucket.label_key()), cx).px_2().pt_2().pb_1().into_any_element()
            }
            Some(&Row::Entry { entry, ix }) => {
                self.render_entry(ix, &self.entries[entry], now_ms, clearance, window, cx)
            }
            None => return div().into_any_element(),
        };
        let last = row + 1 == self.rows.len();
        div().px(LIST_INSET).when(last, |el| el.pb(LIST_INSET)).child(element).into_any_element()
    }

    fn render_entry(
        &self,
        ix: usize,
        entry: &HistoryEntry,
        now_ms: i64,
        clearance: Pixels,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let ring = self.nav.ringed(&entry.id, window);
        let theme = cx.theme().clone();
        let lang = cx.global::<I18n>().lang();
        let mut meta = format!("{} · {} ms", format_relative_time(&entry.executed_at, lang, now_ms), entry.duration_ms);
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
                list_nav::hover_actions("history-row", ROW_INSET, cx)
                    .pr(clearance)
                    .child(
                        list_nav::row_button(SharedString::from(format!("history-save-{ix}")), Lucide::BookmarkPlus)
                            .tooltip(t(cx, "sidebar.saveAsQuery"))
                            .on_click(move |_, window, cx| {
                                cx.stop_propagation();
                                Self::save_as_query(save_conn.clone(), save_sql.clone(), window, cx);
                            }),
                    )
                    .child(
                        list_nav::row_button(SharedString::from(format!("history-delete-{ix}")), Lucide::Trash)
                            .danger()
                            .outline()
                            .debug_selector(move || format!("history-delete-{ix}"))
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
            .on_mouse_down(
                MouseButton::Right,
                cx.listener({
                    let id = entry.id.clone();
                    move |this, _, window, cx| {
                        this.nav.point_at(&id, window, cx);
                        cx.notify();
                    }
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
        // The captions go by today's date, so rows laid out before midnight are laid out again.
        if Zoned::now().date() != self.day {
            self.rebuild();
        }
        let body = if self.entries.is_empty() {
            form::empty_state(Some(Icon::new(Lucide::Clock)), t(cx, "sidebar.noQueryHistory"), cx).into_any_element()
        } else if self.rows.is_empty() {
            form::empty_state(None, t(cx, "sidebar.noMatches"), cx).into_any_element()
        } else {
            let now_ms = Timestamp::now().as_millisecond();
            // Read now: the list can't be read while it builds its rows.
            let clearance = self.nav.clearance(ROW_INSET, LIST_INSET, window);
            let rows = list(
                self.nav.scroll.clone(),
                cx.processor(move |this, row, window, cx| this.render_row(row, now_ms, clearance, window, cx)),
            )
            .size_full();
            div()
                .debug_selector(|| "history-list".into())
                .key_context(list_nav::CONTEXT)
                .track_focus(&self.nav.focus)
                .relative()
                .size_full()
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
                .child(rows)
                .child(self.nav.clearance.watch(self.nav.scroll.clone()))
                .hover_scrollbar(&self.nav.scroll, ScrollbarAxis::Vertical)
                .into_any_element()
        };
        v_flex().size_full().child(filter_bar).child(div().flex_1().min_h_0().child(body))
    }
}

#[cfg(test)]
mod tests {
    use barsql_core::HistoryEntry;
    use gpui_kit::{App, TestAppContext};
    use jiff::Zoned;

    use super::{HistoryPanel, Row};
    use crate::relative_time::TimeBucket;
    use crate::test_support::{Env, in_root};

    // A list laid out before midnight is laid out again when it next draws, so its captions say what day it is.
    #[gpui_kit::test]
    fn the_day_captions_move_on_at_midnight(cx: &mut TestAppContext) {
        let env = Env::new(cx);
        let (panel, cx) = in_root(cx, HistoryPanel::new);
        let yesterday = Zoned::now().yesterday().unwrap();
        let entry = HistoryEntry {
            id: "h1".into(),
            connection_id: env.connection.id.clone(),
            sql: "SELECT 1".into(),
            executed_at: yesterday.timestamp().to_string(),
            success: true,
            ..Default::default()
        };
        let days = |panel: &HistoryPanel, _: &App| -> Vec<TimeBucket> {
            panel.rows.iter().filter_map(|row| if let Row::Day(day) = row { Some(*day) } else { None }).collect()
        };
        // Read before the update ends, since the window draws then.
        let before = panel.update(cx, |panel, cx| {
            panel.show_entries(vec![entry], cx);
            panel.rebuild_as_of(yesterday);
            days(panel, cx)
        });
        assert_eq!(before, [TimeBucket::Today], "laid out the day before");
        panel.update(cx, |_, cx| cx.notify());
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert_eq!(panel.read_with(cx, days), [TimeBucket::Yesterday]);
    }
}
