// The databases of a connection that lists them all, above its tree. A click shows one in the tree, a double-click
// opens a query tab on it.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::rc::Rc;

use barsql_core::{ConnectionConfig, DatabaseInfo};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::ElementExt as _;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt, DropdownMenu, PopupMenu, PopupMenuItem};
use gpui_kit::component::scroll::ScrollbarAxis;
use gpui_kit::component::{ActiveTheme, Icon, IconName, Side, Sizable, StyledExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::form;
use crate::grid;
use crate::i18n::{I18n, bytes, t};
use crate::list_nav::{self, LIST_INSET, ListNav, NavDown, NavFirst, NavLast, NavOpen, NavScroll as _, NavUp};
use crate::schema::{self, Schemas};
use crate::schema_actions::Change;
use crate::schema_tree::{ROW_GAP, ROW_HEIGHT_REM, ROW_PADDING};
use crate::scrollbars::HoverScrollbar as _;
use crate::state::{self, set_setting_bool, set_setting_json, setting_bool, setting_json};
use crate::toast;
use crate::tokens::{ICON_SM, ICON_XS, RADIUS, TEXT_2XS, TEXT_BASE, TEXT_SM};

const PINS_KEY: &str = "barsql-database-pins";
const COLLAPSED_KEY: &str = "barsql-databases-collapsed";
const SYSTEM_KEY: &str = "barsql-databases-system";
// Past this many, the list gets a filter.
const FILTER_AFTER: usize = 8;
const ROW_REM: f32 = ROW_HEIGHT_REM;
const HEADER_REM: f32 = grid::TOOLBAR_REM;
// The tree's filter bar: its padding and its buttons' width, so the options buttons share a column.
const BAR_PADDING: Rems = rems(0.615);
const BAR_BUTTON: Rems = rems(2.308);
const FILTER_REM: f32 = 3.231;
// The list's height in rows as it opens, and the fewest and most a drag leaves. Kept for the session only.
const ROWS: (f32, f32, f32) = (6., 2., 40.);

// The last list per saved connection, for the query toolbar's database picker.
#[derive(Default)]
struct Lists {
    listed: HashMap<String, Vec<DatabaseInfo>>,
    loading: HashSet<String>,
}

impl Global for Lists {}

pub fn listed<'a>(cx: &'a App, base: &str) -> Option<&'a [DatabaseInfo]> {
    cx.try_global::<Lists>()?.listed.get(base).map(Vec::as_slice)
}

fn keep_list(base: &str, listed: &[DatabaseInfo], cx: &mut App) {
    let lists = cx.default_global::<Lists>();
    lists.loading.remove(base);
    lists.listed.insert(base.to_string(), listed.to_vec());
}

// Lists the connection's databases once, for a picker opened before the Databases list showed them. A failure is
// kept as an empty list, so redraws don't ask again until the Databases list refreshes.
pub fn ensure_listed(base: &str, cx: &mut App) {
    if cx.try_global::<Lists>().is_some_and(|lists| lists.listed.contains_key(base) || lists.loading.contains(base)) {
        return;
    }
    cx.default_global::<Lists>().loading.insert(base.to_string());
    let (bar, id) = (state::bar(cx), base.to_string());
    let listing = state::spawn(cx, async move { bar.database_list(&id).await });
    let base = base.to_string();
    cx.spawn(async move |cx| {
        let listed = listing.await;
        cx.update(|cx| match listed {
            Some(Ok(listed)) => keep_list(&base, &listed, cx),
            _ => keep_list(&base, &[], cx),
        });
    })
    .detach();
}

// A header's or row's first column, where the tree keeps its chevrons, so the list, the headers and the tree line up.
fn lead(child: impl IntoElement) -> Div {
    h_flex().flex_none().w(ICON_SM).justify_center().child(child)
}

pub enum DatabasePaneEvent {
    Select(String),
    Open(String),
    // New, Rename or Drop, which the workspace opens a dialog for.
    Change(Change),
}

enum Listed {
    // Lists once the connection or one of its databases is connected.
    Waiting,
    Loading(u64),
    Ready(Vec<DatabaseInfo>),
    Failed(SharedString),
}

pub struct DatabasePane {
    connection: Option<ConnectionConfig>,
    shown: String,
    listed: Listed,
    sizes: HashMap<String, u64>,
    pins: HashMap<String, Vec<String>>,
    filter: Entity<InputState>,
    needle: String,
    collapsed: bool,
    show_system: bool,
    rows: f32,
    next_load: u64,
    nav: ListNav<UniformListScrollHandle>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<DatabasePaneEvent> for DatabasePane {}

impl DatabasePane {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let placeholder = t(cx, "sidebar.filterDatabases");
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        let subscriptions = vec![
            cx.subscribe(&filter, |this, filter, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.needle = filter.read(cx).value().to_string();
                    cx.notify();
                }
            }),
            // Connects and disconnects show on the open dots, and the first connect lists the databases.
            cx.observe_global::<Schemas>(|this, cx| {
                this.list_if_connected(cx);
                cx.notify();
            }),
            cx.observe_global_in::<I18n>(window, |this, window, cx| {
                let placeholder = t(cx, "sidebar.filterDatabases");
                this.filter.update(cx, |filter, cx| filter.set_placeholder(placeholder, window, cx));
            }),
        ];
        Self {
            connection: None,
            shown: String::new(),
            listed: Listed::Waiting,
            sizes: HashMap::new(),
            pins: setting_json(cx, PINS_KEY),
            filter,
            needle: String::new(),
            collapsed: setting_bool(cx, COLLAPSED_KEY, false),
            show_system: setting_bool(cx, SYSTEM_KEY, false),
            rows: ROWS.0,
            next_load: 0,
            nav: ListNav::with_scroll(UniformListScrollHandle::new(), cx),
            _subscriptions: subscriptions,
        }
    }

    // The saved connection, and the database its tree shows.
    pub fn set_connection(&mut self, connection: Option<ConnectionConfig>, shown: String, cx: &mut Context<Self>) {
        let key = |c: &Option<ConnectionConfig>| c.as_ref().map(|c| (c.id.clone(), c.fingerprint()));
        let same = key(&self.connection) == key(&connection);
        let moved = shown != self.shown;
        self.connection = connection;
        self.shown = shown;
        if !same {
            self.listed = Listed::Waiting;
            self.sizes.clear();
            self.list_if_connected(cx);
        } else if moved {
            self.reveal_shown();
        }
        cx.notify();
    }

    // Brings the database the tree shows into view as the list loads, opens, or follows another tab.
    fn reveal_shown(&self) {
        self.reveal(&self.shown);
    }

    pub(crate) fn reveal(&self, name: &str) {
        if let Some(ix) = self.visible().iter().position(|db| db.name == name) {
            self.nav.scroll.reveal(ix);
        }
    }

    pub fn collapsed(&self) -> bool {
        self.collapsed
    }

    // `height` runs from the pane's top to the pointer.
    pub fn resize(&mut self, height: Pixels, rem: Pixels, cx: &mut Context<Self>) {
        let filter = if self.filtered() { FILTER_REM } else { 0. };
        let rows = (height / rem - HEADER_REM - filter) / ROW_REM;
        self.rows = rows.clamp(ROWS.1, ROWS.2);
        cx.notify();
    }

    fn connected(&self, cx: &App) -> bool {
        let Some(base) = &self.connection else { return false };
        let bar = state::bar(cx);
        bar.is_connected(&base.id) || bar.is_connected(&base.in_database(&self.shown).id)
    }

    fn list_if_connected(&mut self, cx: &mut Context<Self>) {
        if matches!(self.listed, Listed::Waiting) && self.connected(cx) {
            self.refresh(cx);
        }
    }

    // Names first, sizes after, since sizing can take a while on a big server.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(base) = self.connection.clone() else { return };
        self.next_load += 1;
        let load = self.next_load;
        self.listed = Listed::Loading(load);
        let bar = state::bar(cx);
        let id = base.id.clone();
        let listing = state::spawn(cx, async move { bar.database_list(&id).await });
        cx.spawn(async move |this, cx| {
            let listed = listing.await;
            let _ = this.update(cx, |this, cx| {
                if !matches!(this.listed, Listed::Loading(at) if at == load) {
                    return;
                }
                this.listed = match listed {
                    Some(Ok(listed)) => {
                        this.load_sizes(load, cx);
                        if let Some(base) = &this.connection {
                            keep_list(&base.id, &listed, cx);
                        }
                        Listed::Ready(listed)
                    }
                    Some(Err(error)) => Listed::Failed(error.message.into()),
                    None => Listed::Waiting,
                };
                this.reveal_shown();
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn load_sizes(&self, load: u64, cx: &mut Context<Self>) {
        let Some(base) = self.connection.clone() else { return };
        let bar = state::bar(cx);
        let sizing = state::spawn(cx, async move { bar.database_sizes(&base.id).await });
        cx.spawn(async move |this, cx| {
            let Some(Ok(sizes)) = sizing.await else { return };
            let _ = this.update(cx, |this, cx| {
                if this.next_load == load {
                    this.sizes = sizes;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn pinned(&self) -> &[String] {
        self.connection.as_ref().and_then(|base| self.pins.get(&base.id)).map_or(&[], Vec::as_slice)
    }

    fn toggle_pin(&mut self, name: &str, cx: &mut Context<Self>) {
        let Some(base) = &self.connection else { return };
        let pins = self.pins.entry(base.id.clone()).or_default();
        match pins.iter().position(|pin| pin == name) {
            Some(ix) => {
                pins.remove(ix);
            }
            None => pins.push(name.to_string()),
        }
        self.pins.retain(|_, pins| !pins.is_empty());
        set_setting_json(cx, PINS_KEY, &self.pins);
        cx.notify();
    }

    fn set_collapsed(&mut self, collapsed: bool, cx: &mut Context<Self>) {
        self.collapsed = collapsed;
        set_setting_bool(cx, COLLAPSED_KEY, collapsed);
        if !collapsed {
            self.reveal_shown();
        }
        cx.notify();
    }

    fn set_show_system(&mut self, show: bool, cx: &mut Context<Self>) {
        self.show_system = show;
        set_setting_bool(cx, SYSTEM_KEY, show);
        cx.notify();
    }

    // Closes that database's pool. The connection's own closes from the connection switcher.
    fn disconnect(&mut self, name: &str, cx: &mut Context<Self>) {
        let Some(base) = &self.connection else { return };
        let id = base.in_database(name).id;
        let (bar, target) = (state::bar(cx), id.clone());
        let task = state::spawn(cx, async move { bar.disconnect(&target).await });
        cx.spawn(async move |this, cx| {
            task.await;
            let _ = this.update(cx, |_, cx| {
                schema::forget(&id, cx);
                cx.notify();
            });
        })
        .detach();
    }

    // The connection's own database and the one shown stay, even when they're system ones.
    fn visible(&self) -> Vec<DatabaseInfo> {
        let (Listed::Ready(all), Some(base)) = (&self.listed, &self.connection) else { return Vec::new() };
        let own = base.default_database();
        let needle = self.needle.trim().to_lowercase();
        let pinned = self.pinned();
        let mut shown: Vec<DatabaseInfo> = all
            .iter()
            .filter(|db| self.show_system || !db.system || db.name == own || db.name == self.shown)
            .filter(|db| needle.is_empty() || db.name.to_lowercase().contains(&needle))
            .cloned()
            .collect();
        shown.sort_by_key(|db| !pinned.contains(&db.name));
        shown
    }

    fn filtered(&self) -> bool {
        matches!(&self.listed, Listed::Ready(all) if all.iter().filter(|db| !db.system).count() > FILTER_AFTER)
    }

    fn step(&mut self, step: isize, cx: &mut Context<Self>) {
        let rows: Vec<(String, usize)> = self.visible().into_iter().enumerate().map(|(ix, db)| (db.name, ix)).collect();
        self.nav.step(&rows, step);
        cx.notify();
    }

    fn row_menu(
        &self,
        db: &DatabaseInfo,
        open: bool,
        cx: &mut Context<Self>,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        let this = cx.entity().downgrade();
        let name = db.name.clone();
        let own = self.connection.as_ref().is_some_and(|base| base.default_database() == name);
        let (pinned, can_connect) = (self.pinned().contains(&name), db.can_connect);
        // The connection's own database and the server's stay as they are.
        let editable = self.connection.as_ref().is_some_and(|base| !base.read_only) && !own && !db.system;
        let changes: Vec<(&str, Change)> = [
            ("sidebar.rename", Change::RenameDatabase { database: name.clone() }),
            ("sidebar.drop", Change::DropDatabase { database: name.clone() }),
        ]
        .into_iter()
        .filter(|(_, change)| self.connection.as_ref().is_some_and(|base| change.supported(&base.driver)))
        .collect();
        move |menu, _, cx| {
            let act = |f: fn(&mut DatabasePane, &str, &mut Context<DatabasePane>)| {
                let (this, name) = (this.clone(), name.clone());
                move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                    let _ = this.update(cx, |this, cx| f(this, &name, cx));
                }
            };
            let copy = name.clone();
            let menu = menu
                .item(
                    PopupMenuItem::new(t(cx, "app.newQuery"))
                        .disabled(!can_connect)
                        .on_click(act(|_, name, cx| cx.emit(DatabasePaneEvent::Open(name.to_string())))),
                )
                .item(
                    PopupMenuItem::new(t(cx, if pinned { "sidebar.unpin" } else { "sidebar.pin" }))
                        .on_click(act(Self::toggle_pin)),
                )
                .item(PopupMenuItem::new(t(cx, "sidebar.copyName")).on_click(move |_, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
                    toast::success(t(cx, "toast.copiedClipboard"), cx);
                }))
                .separator()
                .when(open && !own, |menu| {
                    menu.item(PopupMenuItem::new(t(cx, "tooltip.disconnect")).on_click(act(Self::disconnect)))
                })
                .item(PopupMenuItem::new(t(cx, "sidebar.refresh")).on_click(act(|this, _, cx| this.refresh(cx))))
                .when(!changes.is_empty(), |menu| menu.separator());
            changes.iter().fold(menu, |menu, (label, change)| {
                let (this, change) = (this.clone(), change.clone());
                menu.item(PopupMenuItem::new(t(cx, label)).disabled(!editable).on_click(move |_, _, cx| {
                    let _ = this.update(cx, |_, cx| cx.emit(DatabasePaneEvent::Change(change.clone())));
                }))
            })
        }
    }

    pub(crate) fn database_changed(&mut self, base: &str, change: &Change, new_name: &str, cx: &mut Context<Self>) {
        if let Some(pins) = self.pins.get_mut(base) {
            let before = pins.clone();
            match change {
                Change::RenameDatabase { database } => {
                    for pin in pins.iter_mut().filter(|pin| pin.as_str() == database.as_str()) {
                        *pin = new_name.to_string();
                    }
                }
                Change::DropDatabase { database } => pins.retain(|pin| pin != database),
                _ => {}
            }
            if *pins != before {
                self.pins.retain(|_, pins| !pins.is_empty());
                set_setting_json(cx, PINS_KEY, &self.pins);
            }
        }
        if self.connection.as_ref().is_some_and(|shown| shown.id == base) {
            self.refresh(cx);
        } else if cx.try_global::<Lists>().is_some_and(|lists| lists.listed.contains_key(base)) {
            // The toolbar's picker lists again when it next opens.
            cx.global_mut::<Lists>().listed.remove(base);
        }
    }

    fn options_menu(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let this = cx.entity().downgrade();
        let show_system = self.show_system;
        // Whether the connection may make databases, where its engine can.
        let create = self
            .connection
            .as_ref()
            .filter(|base| Change::CreateDatabase.supported(&base.driver))
            .map(|base| !base.read_only);
        Button::new("database-options")
            .debug_selector(|| "database-options".into())
            .ghost()
            .small()
            .w(BAR_BUTTON)
            .h(rems(form::TOOL_REM))
            .p_0()
            .child(Icon::new(Lucide::Ellipsis).size(ICON_XS))
            .tooltip(t(cx, "sidebar.databaseOptions"))
            // Opens into the sidebar, its right edge on the button's.
            .dropdown_menu_with_anchor(Anchor::TopRight, move |menu: PopupMenu, _, cx| {
                let (made, system, refresh) = (this.clone(), this.clone(), this.clone());
                // Every item has an icon, which would take the check's place on the left.
                menu.check_side(Side::Right)
                    .when_some(create, |menu, writable| {
                        menu.item(
                            PopupMenuItem::new(t(cx, "sidebar.newDatabase"))
                                .icon(Icon::new(Lucide::Plus))
                                .disabled(!writable)
                                .on_click(move |_, _, cx| {
                                    let _ = made
                                        .update(cx, |_, cx| cx.emit(DatabasePaneEvent::Change(Change::CreateDatabase)));
                                }),
                        )
                        .separator()
                    })
                    .item(
                        PopupMenuItem::new(t(cx, "sidebar.showSystemDatabases"))
                            .icon(Icon::new(Lucide::Eye))
                            .checked(show_system)
                            .on_click(move |_, _, cx| {
                                let _ = system.update(cx, |this, cx| this.set_show_system(!show_system, cx));
                            }),
                    )
                    .item(PopupMenuItem::new(t(cx, "sidebar.refresh")).icon(Icon::new(Lucide::RefreshCw)).on_click(
                        move |_, _, cx| {
                            let _ = refresh.update(cx, |this, cx| this.refresh(cx));
                        },
                    ))
            })
    }

    fn header(&self, count: Option<usize>, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let collapsed = self.collapsed;
        let chevron = if collapsed { IconName::ChevronRight } else { IconName::ChevronDown };
        // Collapsed, it names the tree's database as the open list's highlighted row would. Open or not, its chevron,
        // icon and title sit in the rows' columns.
        let named = collapsed && !self.shown.is_empty();
        let (tint, title) = match named {
            true => (theme.primary, SharedString::from(self.shown.clone())),
            false => (theme.muted_foreground, t(cx, "sidebar.databases")),
        };
        // Everything but the options button toggles.
        let toggle = h_flex()
            .id("databases-toggle")
            .debug_selector(|| "databases-toggle".into())
            .flex_1()
            .min_w_0()
            .h_full()
            .gap(ROW_GAP)
            .cursor_pointer()
            .child(lead(Icon::new(chevron).size(ICON_SM)))
            .child(Icon::new(Lucide::Database).size(ICON_SM).text_color(tint))
            .child(
                div()
                    .debug_selector(|| "databases-title".into())
                    .min_w_0()
                    .truncate()
                    .font_semibold()
                    .text_color(theme.foreground)
                    .child(title),
            )
            .when(!named, |el| {
                el.children(count.map(|count| div().flex_none().text_size(TEXT_2XS).child(count.to_string())))
            })
            .on_click(cx.listener(move |this, _, _, cx| this.set_collapsed(!collapsed, cx)));
        h_flex()
            .debug_selector(|| "databases-header".into())
            .flex_none()
            .h(rems(HEADER_REM))
            .pl(LIST_INSET + ROW_PADDING)
            .pr(BAR_PADDING)
            .gap(ROW_GAP)
            .text_size(TEXT_BASE)
            .text_color(theme.muted_foreground)
            .child(toggle)
            .child(self.options_menu(cx))
    }

    fn render_row(&self, ix: usize, db: &DatabaseInfo, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let Some(base) = &self.connection else { return Empty.into_any_element() };
        let open = state::bar(cx).is_connected(&base.in_database(&db.name).id);
        let (current, own) = (db.name == self.shown, db.name == base.default_database());
        let pinned = self.pinned().contains(&db.name);
        let icon = match (db.can_connect, pinned) {
            (false, _) => Lucide::Lock,
            (true, true) => Lucide::Pin,
            (true, false) => Lucide::Database,
        };
        let detail = match (db.can_connect, self.sizes.get(&db.name)) {
            (false, _) => Some(t(cx, "sidebar.databaseNoAccess").to_string()),
            (true, size) => size.map(|size| bytes(cx, *size as i64)),
        };
        let name = db.name.clone();
        let (select, double, pick, point) = (name.clone(), name.clone(), name.clone(), name.clone());
        h_flex()
            .id(SharedString::from(format!("database-{ix}")))
            .debug_selector(move || format!("database:{select}"))
            .h(rems(ROW_REM))
            .w_full()
            .px(ROW_PADDING)
            .gap(ROW_GAP)
            .rounded(RADIUS)
            .text_size(TEXT_BASE)
            .when(current, |el| el.bg(theme.sidebar_accent))
            .when(!db.can_connect, |el| el.text_color(theme.muted_foreground))
            .when(db.can_connect, |el| el.cursor_pointer().hover(|s| s.bg(theme.sidebar_accent)))
            .child(lead(div().size(rems(0.462)).rounded_full().when(open, |el| el.bg(theme.success))))
            .child(Icon::new(icon).size(ICON_SM).text_color(if current {
                theme.primary
            } else {
                theme.muted_foreground
            }))
            .child(div().flex_1().min_w_0().truncate().child(name))
            .child(
                h_flex()
                    .flex_none()
                    .gap(rems(0.462))
                    .text_size(TEXT_2XS)
                    .text_color(theme.muted_foreground)
                    .when(own, |el| el.child(form::badge(t(cx, "sidebar.databaseDefault"), theme.muted_foreground)))
                    .children(detail),
            )
            .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, window, cx| this.nav.pick(&pick, window, cx)))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, _, window, cx| {
                    this.nav.point_at(&point, window, cx);
                    cx.notify();
                }),
            )
            .when(self.nav.ringed(&db.name, window), |el| list_nav::ring(el, cx))
            .when(db.can_connect, |el| {
                el.on_click(cx.listener(move |_, event: &ClickEvent, _, cx| {
                    let name = double.clone();
                    cx.emit(match event.click_count() {
                        2.. => DatabasePaneEvent::Open(name),
                        _ => DatabasePaneEvent::Select(name),
                    });
                }))
            })
            .context_menu(self.row_menu(db, open, cx))
            .into_any_element()
    }

    fn body(&self, shown: Rc<[DatabaseInfo]>, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let note = |text: SharedString| {
            div()
                .h(rems(ROW_REM))
                .flex()
                .items_center()
                // Under the rows' names, past their dot and icon.
                .pl(LIST_INSET + ROW_PADDING + ICON_SM + ROW_GAP + ICON_SM + ROW_GAP)
                .text_size(TEXT_SM)
                .text_color(theme.muted_foreground)
                .child(text)
                .into_any_element()
        };
        match &self.listed {
            Listed::Waiting => note(t(cx, "sidebar.connectToListDatabases")),
            Listed::Loading(_) => note(t(cx, "connection.loadingDatabases")),
            Listed::Failed(error) => note(format!("{} - {error}", t(cx, "connection.databasesFailed")).into()),
            Listed::Ready(_) if shown.is_empty() => note(t(cx, "sidebar.noMatches")),
            Listed::Ready(_) => {
                let height = rems(ROW_REM * (shown.len() as f32).min(self.rows) + 0.308);
                div()
                    .key_context(list_nav::CONTEXT)
                    .track_focus(&self.nav.focus)
                    .relative()
                    .w_full()
                    .h(height)
                    .on_action(cx.listener(|this, _: &NavUp, _, cx| this.step(-1, cx)))
                    .on_action(cx.listener(|this, _: &NavDown, _, cx| this.step(1, cx)))
                    .on_action(cx.listener(|this, _: &NavFirst, _, cx| this.step(isize::MIN, cx)))
                    .on_action(cx.listener(|this, _: &NavLast, _, cx| this.step(isize::MAX, cx)))
                    .on_action(cx.listener(|this, _: &NavOpen, _, cx| {
                        if let Some(name) = this.nav.cursor().map(str::to_string) {
                            cx.emit(DatabasePaneEvent::Select(name));
                        }
                    }))
                    .child(
                        uniform_list(
                            "database-list",
                            shown.len(),
                            cx.processor(move |this, range: Range<usize>, window, cx| {
                                range.map(|ix| this.render_row(ix, &shown[ix], window, cx)).collect::<Vec<_>>()
                            }),
                        )
                        .debug_selector(|| "database-list".into())
                        .track_scroll(&self.nav.scroll)
                        .size_full()
                        .px(LIST_INSET),
                    )
                    .on_prepaint(self.nav.clearance.watch(self.nav.scroll.clone()))
                    .hover_scrollbar(&self.nav.scroll, ScrollbarAxis::Vertical)
                    .into_any_element()
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn show_list(&mut self, listed: Vec<DatabaseInfo>, cx: &mut Context<Self>) {
        self.listed = Listed::Ready(listed);
        self.reveal_shown();
        cx.notify();
    }

    #[cfg(test)]
    pub(crate) fn listed(&self) -> Vec<String> {
        self.visible().into_iter().map(|db| db.name).collect()
    }
}

impl Render for DatabasePane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let shown: Rc<[DatabaseInfo]> = self.visible().into();
        let count = match &self.listed {
            Listed::Ready(all) => Some(all.iter().filter(|db| self.show_system || !db.system).count()),
            _ => None,
        };
        let filter = (!self.collapsed && self.filtered()).then(|| {
            let field = div()
                .debug_selector(|| "database-filter".into())
                .child(form::search_input(&self.filter, window, cx).cleanable(true));
            form::filter_bar(field)
        });
        v_flex()
            .debug_selector(|| "databases".into())
            .flex_none()
            .w_full()
            .child(self.header(count, cx))
            .children(filter)
            .when(!self.collapsed, |el| el.child(self.body(shown, cx)))
    }
}

#[cfg(test)]
mod tests {
    use barsql_core::{ConnectionConfig, DatabaseInfo, DriverType};
    use gpui_kit::{TestAppContext, VisualTestContext};

    use super::DatabasePane;
    use crate::test_support::Env;

    fn db(name: &str, can_connect: bool, system: bool) -> DatabaseInfo {
        DatabaseInfo { name: name.into(), can_connect, system }
    }

    #[gpui_kit::test]
    fn pinned_databases_lead_and_system_ones_wait_for_the_toggle(cx: &mut TestAppContext) {
        let _env = Env::new(cx);
        let window = cx.add_window(DatabasePane::new);
        let pane = window.root(cx).unwrap();
        let cx = VisualTestContext::from_window(window.into(), cx).into_mut();
        let server = ConnectionConfig {
            id: "server".into(),
            driver: DriverType::Postgres,
            database: "forum".into(),
            show_all_databases: true,
            ..Default::default()
        };
        let listed = vec![
            db("forum", true, false),
            db("hr", true, false),
            db("payroll", false, false),
            db("postgres", true, true),
            db("sales", true, false),
        ];
        pane.update(cx, |pane, cx| {
            pane.set_connection(Some(server), "forum".into(), cx);
            pane.show_list(listed, cx);
        });
        let names = |cx: &mut VisualTestContext| pane.read_with(cx, |pane, _| pane.listed());
        assert_eq!(names(cx), ["forum", "hr", "payroll", "sales"], "postgres waits for the toggle");
        pane.update(cx, |pane, cx| pane.toggle_pin("sales", cx));
        assert_eq!(names(cx), ["sales", "forum", "hr", "payroll"], "a pinned database leads");
        pane.update(cx, |pane, cx| pane.set_show_system(true, cx));
        assert_eq!(names(cx), ["sales", "forum", "hr", "payroll", "postgres"]);
        pane.update(cx, |pane, _| pane.needle = "r".into());
        assert_eq!(names(cx), ["forum", "hr", "payroll", "postgres"]);
        pane.update(cx, |pane, cx| {
            pane.toggle_pin("sales", cx);
            pane.set_show_system(false, cx);
        });
    }

    #[gpui_kit::test]
    fn the_database_the_tree_shows_scrolls_into_view(cx: &mut TestAppContext) {
        let _env = Env::new(cx);
        let window = cx.add_window(DatabasePane::new);
        let pane = window.root(cx).unwrap();
        let cx = VisualTestContext::from_window(window.into(), cx).into_mut();
        let server = ConnectionConfig {
            id: "server".into(),
            driver: DriverType::Postgres,
            database: "db00".into(),
            show_all_databases: true,
            ..Default::default()
        };
        let listed = (0..30).map(|n| db(&format!("db{n:02}"), true, false)).collect();
        pane.update(cx, |pane, cx| {
            pane.set_connection(Some(server.clone()), "db24".into(), cx);
            pane.show_list(listed, cx);
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("database:db24").is_some(), "the list opens on it");
        assert!(cx.debug_bounds("database:db00").is_none(), "the top scrolled away");
        pane.update(cx, |pane, cx| pane.set_connection(Some(server), "db02".into(), cx));
        cx.run_until_parked();
        assert!(cx.debug_bounds("database:db02").is_some(), "another tab's database comes into view");
    }
}
