use std::ops::Range;
use std::rc::Rc;

use barsql_core::{ConnectionConfig, SavedQuery};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::ButtonVariants;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt, DropdownMenu, PopupMenu, PopupMenuItem};
use gpui_kit::component::scroll::ScrollbarAxis;
use gpui_kit::component::{ActiveTheme, Icon, IconName, StyledExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use jiff::Timestamp;

use crate::dialogs::{self, Confirm, Prompt};
use crate::form;
use crate::history_panel::Scope;
use crate::hover_card::joined;
use crate::i18n::{I18n, t, t_with};
use crate::list_nav::{
    self, LIST_INSET, ListNav, NavDelete, NavDown, NavFirst, NavLast, NavOpen, NavRename, NavUp, ROW_INSET,
};
use crate::relative_time::{format_relative_time, one_line_preview};
use crate::saved_queries::{self, SORT_KEY, SavedQueries, SavedSort};
use crate::scrollbars::HoverScrollbar as _;
use crate::state::{self, set_setting, setting};
use crate::toast;
use crate::tokens::{ICON_2XS, RADIUS, TEXT_2XS, TEXT_SM, TEXT_XS};

pub enum SavedEvent {
    Open(SavedQuery),
    // Tabs linked to the query take the new name.
    Renamed { id: String, name: String },
}

pub struct SavedPanel {
    connection_id: Option<String>,
    connections: Vec<ConnectionConfig>,
    scope: Scope,
    sort: SavedSort,
    filter: Entity<InputState>,
    needle: String,
    // The list builds only the rows on screen.
    nav: ListNav<UniformListScrollHandle>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<SavedEvent> for SavedPanel {}

impl SavedPanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let placeholder = t(cx, "sidebar.filterSaved");
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        let subscriptions = vec![
            cx.subscribe(&filter, |this, filter, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.needle = filter.read(cx).value().to_string();
                    cx.notify();
                }
            }),
            cx.observe_global::<SavedQueries>(|_, cx| cx.notify()),
            cx.observe_global_in::<I18n>(window, |this, window, cx| {
                let placeholder = t(cx, "sidebar.filterSaved");
                this.filter.update(cx, |filter, cx| filter.set_placeholder(placeholder, window, cx));
            }),
        ];
        Self {
            connection_id: None,
            connections: Vec::new(),
            scope: Scope::Connection,
            sort: SavedSort::parse(&setting(cx, SORT_KEY).unwrap_or_default()),
            filter,
            needle: String::new(),
            nav: ListNav::with_scroll(UniformListScrollHandle::new(), cx),
            _subscriptions: subscriptions,
        }
    }

    pub fn set_connection(&mut self, id: Option<String>, connections: Vec<ConnectionConfig>, cx: &mut Context<Self>) {
        self.connection_id = id;
        self.connections = connections;
        cx.notify();
    }

    fn visible<'a>(&self, cx: &'a App) -> Vec<&'a SavedQuery> {
        let scope = match self.scope {
            Scope::Connection => self.connection_id.as_deref(),
            Scope::All => None,
        };
        saved_queries::visible(
            saved_queries::list(cx),
            scope,
            &self.needle,
            self.sort,
            &self.connections,
            saved_queries::pinned(cx),
        )
    }

    fn set_sort(&mut self, sort: SavedSort, cx: &mut Context<Self>) {
        self.sort = sort;
        set_setting(cx, SORT_KEY, sort.key());
        cx.notify();
    }

    fn rename(&mut self, query: SavedQuery, window: &mut Window, cx: &mut Context<Self>) {
        let prompt = Prompt {
            title: t(cx, "dialog.renameQueryTitle"),
            description: None,
            label: t(cx, "dialog.renameQueryLabel"),
            placeholder: SharedString::default(),
            initial: query.name.clone(),
            confirm: t(cx, "common.rename"),
        };
        let this = cx.entity().downgrade();
        dialogs::prompt(prompt, window, cx, move |name, window, cx| {
            if name == query.name {
                return;
            }
            match state::bar(cx).save_saved_query(SavedQuery { name: name.clone(), ..query.clone() }) {
                Ok(saved) => {
                    saved_queries::refresh(cx);
                    let event = SavedEvent::Renamed { id: saved.id, name: saved.name };
                    let _ = this.update(cx, |_, cx| cx.emit(event));
                }
                Err(error) => saved_queries::failed(error.message, "errors.renameQueryFailed", window, cx),
            }
        });
    }

    fn delete(&mut self, query: SavedQuery, window: &mut Window, cx: &mut Context<Self>) {
        let confirm = Confirm {
            title: t(cx, "dialog.deleteSavedTitle"),
            description: t_with(cx, "dialog.deleteSavedDescription", &[("name", &query.name)]),
            detail: None,
            confirm: t(cx, "common.delete"),
            danger: true,
        };
        dialogs::confirm(confirm, window, cx, move |window, cx| {
            let detail = match state::bar(cx).delete_saved_query(&query.id) {
                Ok(true) => None,
                Ok(false) => Some(t(cx, "errors.deleteQueryFailedDetail").to_string()),
                Err(error) => Some(error.message),
            };
            if let Some(detail) = detail {
                saved_queries::failed(detail, "errors.deleteQueryFailed", window, cx);
            }
            saved_queries::refresh(cx);
        });
    }

    fn duplicate(query: &SavedQuery, window: &mut Window, cx: &mut App) {
        let copy = SavedQuery {
            name: t_with(cx, "sidebar.duplicateSuffix", &[("name", &query.name)]).to_string(),
            connection_id: query.connection_id.clone(),
            sql: query.sql.clone(),
            ..Default::default()
        };
        match state::bar(cx).save_saved_query(copy) {
            Ok(_) => saved_queries::refresh(cx),
            Err(error) => saved_queries::failed(error.message, "errors.generic", window, cx),
        }
    }

    fn options_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let this = cx.entity().downgrade();
        let (scope, sort) = (self.scope, self.sort);
        form::filter_button("saved-options", Icon::new(Lucide::SlidersHorizontal))
            .debug_selector(|| "saved-options".into())
            .tooltip(t(cx, "tooltip.queryOptions"))
            .dropdown_menu(move |menu: PopupMenu, _, cx| {
                let pick_scope = |target: Scope| {
                    let this = this.clone();
                    move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                        let _ = this.update(cx, |this, cx| {
                            this.scope = target;
                            cx.notify();
                        });
                    }
                };
                let mut menu = menu
                    .item(
                        PopupMenuItem::new(t(cx, "sidebar.scopeConnection"))
                            .icon(Icon::new(Lucide::Database))
                            .checked(scope == Scope::Connection)
                            .on_click(pick_scope(Scope::Connection)),
                    )
                    .item(
                        PopupMenuItem::new(t(cx, "sidebar.scopeAll"))
                            .icon(Icon::new(IconName::Globe))
                            .checked(scope == Scope::All)
                            .on_click(pick_scope(Scope::All)),
                    )
                    .separator();
                for target in SavedSort::ALL {
                    let (label, icon) = match target {
                        SavedSort::Name => ("sidebar.sortName", Lucide::ArrowDownAZ),
                        SavedSort::UpdatedAt => ("sidebar.sortUpdated", Lucide::Clock),
                        SavedSort::CreatedAt => ("sidebar.sortCreated", Lucide::CalendarPlus),
                    };
                    let this = this.clone();
                    menu = menu.item(
                        PopupMenuItem::new(t(cx, label)).icon(Icon::new(icon)).checked(sort == target).on_click(
                            move |_, _, cx| {
                                let _ = this.update(cx, |this, cx| this.set_sort(target, cx));
                            },
                        ),
                    );
                }
                menu
            })
    }

    fn row_menu(
        &self,
        query: &SavedQuery,
        pinned: bool,
        cx: &mut Context<Self>,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        let this = cx.entity().downgrade();
        let query = query.clone();
        move |menu, _, cx| {
            let with = |f: fn(&mut SavedPanel, SavedQuery, &mut Window, &mut Context<SavedPanel>)| {
                let (this, query) = (this.clone(), query.clone());
                move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                    let _ = this.update(cx, |this, cx| f(this, query.clone(), window, cx));
                }
            };
            let (pin_id, copy_sql, duplicate) = (query.id.clone(), query.sql.clone(), query.clone());
            let open = this.clone();
            let open_query = query.clone();
            menu.item(PopupMenuItem::new(t(cx, "sidebar.openQuery")).on_click(move |_, _, cx| {
                let event = SavedEvent::Open(open_query.clone());
                let _ = open.update(cx, |_, cx| cx.emit(event));
            }))
            .item(
                PopupMenuItem::new(t(cx, if pinned { "sidebar.unpin" } else { "sidebar.pin" }))
                    .on_click(move |_, _, cx| saved_queries::toggle_pin(&pin_id, cx)),
            )
            .item(PopupMenuItem::new(t(cx, "sidebar.rename")).on_click(with(Self::rename)))
            .item(
                PopupMenuItem::new(t(cx, "sidebar.duplicate"))
                    .on_click(move |_, window, cx| Self::duplicate(&duplicate, window, cx)),
            )
            .item(PopupMenuItem::new(t(cx, "sidebar.copySql")).on_click(move |_, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(copy_sql.clone()));
                toast::success(t(cx, "toast.copiedClipboard"), cx);
            }))
            .separator()
            .item(PopupMenuItem::new(t(cx, "sidebar.deleteEllipsis")).on_click(with(Self::delete)))
        }
    }

    #[cfg(test)]
    pub(crate) fn listed(&self, cx: &App) -> Vec<(String, bool)> {
        let pinned = saved_queries::pinned(cx);
        self.visible(cx).into_iter().map(|query| (query.name.clone(), pinned.contains(&query.id))).collect()
    }

    #[cfg(test)]
    pub fn focus_list(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.nav.focus_list(window, cx);
    }

    fn step(&mut self, step: isize, cx: &mut Context<Self>) {
        let rows = self.nav_rows(cx);
        self.nav.step(&rows, step);
        cx.notify();
    }

    fn nav_rows(&self, cx: &App) -> Vec<(String, usize)> {
        self.visible(cx).into_iter().enumerate().map(|(ix, query)| (query.id.clone(), ix)).collect()
    }

    fn nav_query(&self, cx: &App) -> Option<SavedQuery> {
        let id = self.nav.cursor()?;
        self.visible(cx).into_iter().find(|query| query.id == id).cloned()
    }

    fn render_row(
        &self,
        ix: usize,
        query: &SavedQuery,
        now_ms: i64,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let ring = self.nav.ringed(&query.id, window);
        let theme = cx.theme().clone();
        let pinned = saved_queries::pinned(cx).contains(&query.id);
        let connection = (self.scope == Scope::All)
            .then(|| self.connections.iter().find(|c| c.id == query.connection_id))
            .flatten()
            .map(|c| c.name.as_str());
        let meta = row_meta(query, self.sort, connection, cx.global::<I18n>().lang(), now_ms);
        let menu = self.row_menu(query, pinned, cx);
        let (open, pin_id, rename, delete) = (query.clone(), query.id.clone(), query.clone(), query.clone());
        h_flex()
            .id(SharedString::from(format!("saved-{ix}-{}", query.id)))
            .debug_selector(move || format!("saved-row-{ix}"))
            .group("saved-row")
            .relative()
            .w_full()
            .px(ROW_INSET)
            .py_1()
            .rounded(RADIUS)
            .gap_2()
            .cursor_pointer()
            .hover(|s| s.bg(theme.sidebar_accent))
            .child(
                v_flex()
                    .debug_selector(move || format!("saved-text-{ix}"))
                    .flex_1()
                    .min_w_0()
                    .child(
                        h_flex()
                            .gap_1()
                            .when(pinned, |el| {
                                el.child(Icon::new(Lucide::Pin).size(ICON_2XS).text_color(theme.primary))
                            })
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(TEXT_SM)
                                    .font_semibold()
                                    .child(query.name.clone()),
                            )
                            .children(meta.map(|meta| {
                                div().flex_none().text_size(TEXT_2XS).text_color(theme.muted_foreground).child(meta)
                            })),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(TEXT_XS)
                            .line_height(relative(1.45))
                            .font_family(theme.mono_font_family.clone())
                            .text_color(theme.muted_foreground)
                            .child(one_line_preview(&query.sql, 120)),
                    ),
            )
            .child(
                self.nav
                    .hover_actions("saved-row", ROW_INSET, LIST_INSET, window, cx)
                    .child(
                        list_nav::row_button(
                            SharedString::from(format!("saved-pin-{ix}")),
                            if pinned { Lucide::PinOff } else { Lucide::Pin },
                        )
                        .debug_selector(move || format!("saved-pin-{ix}"))
                        .tooltip(t(cx, if pinned { "sidebar.unpin" } else { "sidebar.pin" }))
                        .on_click(move |_, _, cx| {
                            cx.stop_propagation();
                            saved_queries::toggle_pin(&pin_id, cx);
                        }),
                    )
                    .child(
                        list_nav::row_button(SharedString::from(format!("saved-rename-{ix}")), Lucide::SquarePen)
                            .debug_selector(move || format!("saved-rename-{ix}"))
                            .tooltip(t(cx, "tooltip.renameSavedQueryBtn"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.rename(rename.clone(), window, cx);
                            })),
                    )
                    .child(
                        list_nav::row_button(SharedString::from(format!("saved-delete-{ix}")), Lucide::Trash)
                            .danger()
                            .outline()
                            .debug_selector(move || format!("saved-delete-{ix}"))
                            .tooltip(t(cx, "tooltip.deleteSavedQuery"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.delete(delete.clone(), window, cx);
                            })),
                    ),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener({
                    let id = query.id.clone();
                    move |this, _, window, cx| this.nav.pick(&id, window, cx)
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener({
                    let id = query.id.clone();
                    move |this, _, window, cx| {
                        this.nav.point_at(&id, window, cx);
                        cx.notify();
                    }
                }),
            )
            .when(ring, |el| list_nav::ring(el, cx))
            .on_click(cx.listener(move |_, _, _, cx| cx.emit(SavedEvent::Open(open.clone()))))
            .context_menu(menu)
            .into_any_element()
    }
}

// The connection in the All scope, and the query's age when sorted by a date.
fn row_meta(query: &SavedQuery, sort: SavedSort, connection: Option<&str>, lang: &str, now_ms: i64) -> Option<String> {
    let date = match sort {
        SavedSort::Name => "",
        SavedSort::UpdatedAt => &query.updated_at,
        SavedSort::CreatedAt => &query.created_at,
    };
    let meta = joined([connection.unwrap_or_default().to_string(), format_relative_time(date, lang, now_ms)]);
    (!meta.is_empty()).then_some(meta)
}

impl Render for SavedPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let field = div()
            .debug_selector(|| "saved-filter".into())
            .child(form::filter_input(&self.filter, window, cx).cleanable(true));
        let filter_bar = form::filter_bar(field, cx).child(self.options_menu(cx));
        let shown: Rc<[SavedQuery]> = self.visible(cx).into_iter().cloned().collect();
        let now_ms = Timestamp::now().as_millisecond();
        let body = if saved_queries::list(cx).is_empty() {
            form::empty_state(Some(Icon::new(Lucide::Bookmark)), t(cx, "sidebar.noSaved"), cx)
                .child(div().text_size(TEXT_XS).child(t(cx, "sidebar.savedEmptyHint")))
                .into_any_element()
        } else if shown.is_empty() {
            let key = if !self.needle.trim().is_empty() {
                "sidebar.noMatches"
            } else if self.scope == Scope::Connection {
                "sidebar.noSavedForConnection"
            } else {
                "sidebar.noSaved"
            };
            form::empty_state(None, t(cx, key), cx).into_any_element()
        } else {
            div()
                .key_context(list_nav::CONTEXT)
                .track_focus(&self.nav.focus)
                .relative()
                .size_full()
                .on_action(cx.listener(|this, _: &NavUp, _, cx| this.step(-1, cx)))
                .on_action(cx.listener(|this, _: &NavDown, _, cx| this.step(1, cx)))
                .on_action(cx.listener(|this, _: &NavFirst, _, cx| this.step(isize::MIN, cx)))
                .on_action(cx.listener(|this, _: &NavLast, _, cx| this.step(isize::MAX, cx)))
                .on_action(cx.listener(|this, _: &NavOpen, _, cx| {
                    if let Some(query) = this.nav_query(cx) {
                        cx.emit(SavedEvent::Open(query));
                    }
                }))
                .on_action(cx.listener(|this, _: &NavDelete, window, cx| {
                    if let Some(query) = this.nav_query(cx) {
                        this.delete(query, window, cx);
                    }
                }))
                .on_action(cx.listener(|this, _: &NavRename, window, cx| {
                    if let Some(query) = this.nav_query(cx) {
                        this.rename(query, window, cx);
                    }
                }))
                .child(
                    uniform_list(
                        "saved-list",
                        shown.len(),
                        cx.processor(move |this, range: Range<usize>, window, cx| {
                            range.map(|ix| this.render_row(ix, &shown[ix], now_ms, window, cx)).collect::<Vec<_>>()
                        }),
                    )
                    .debug_selector(|| "saved-list".into())
                    .track_scroll(&self.nav.scroll)
                    .size_full()
                    .px(LIST_INSET)
                    .pb(LIST_INSET),
                )
                .child(self.nav.clearance.watch(self.nav.scroll.clone()))
                .hover_scrollbar(&self.nav.scroll, ScrollbarAxis::Vertical)
                .into_any_element()
        };
        v_flex().size_full().child(filter_bar).child(div().flex_1().min_h_0().child(body))
    }
}

#[cfg(test)]
mod tests {
    use barsql_core::SavedQuery;
    use jiff::Timestamp;

    use super::row_meta;
    use crate::saved_queries::SavedSort;

    #[test]
    fn a_date_sort_shows_how_old_each_query_is() {
        let now_ms = "2026-10-25T10:00:00Z".parse::<Timestamp>().unwrap().as_millisecond();
        let query = SavedQuery {
            created_at: "2026-10-22T10:00:00Z".into(),
            updated_at: "2026-10-25T09:00:00Z".into(),
            ..Default::default()
        };
        let meta = |sort, connection| row_meta(&query, sort, connection, "en", now_ms);
        assert_eq!(meta(SavedSort::UpdatedAt, None).as_deref(), Some("1 hour ago"));
        assert_eq!(meta(SavedSort::CreatedAt, None).as_deref(), Some("3 days ago"));
        assert_eq!(meta(SavedSort::CreatedAt, Some("Forum")).as_deref(), Some("Forum · 3 days ago"));
        assert_eq!(meta(SavedSort::Name, Some("Forum")).as_deref(), Some("Forum"));
        assert_eq!(meta(SavedSort::Name, None), None);
        let undated = SavedQuery::default();
        assert_eq!(row_meta(&undated, SavedSort::UpdatedAt, None, "en", now_ms), None, "no date to show");
    }
}
