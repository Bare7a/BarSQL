use std::cell::Cell;
use std::rc::Rc;

use barsql_app::ConnectionFolder;
use barsql_core::{ConnectionConfig, DriverType, SavedQuery};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonRounded, ButtonVariants};
use gpui_kit::component::{ActiveTheme, Icon, IconName, Sizable, StyledExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::connection_dialog;
use crate::connections_panel::{ConnectionsEvent, ConnectionsPanel, connection_subtitle, parse_color};

use crate::history_panel::{HistoryEvent, HistoryPanel};
use crate::i18n::t;
use crate::saved_panel::{SavedEvent, SavedPanel};
use crate::schema_tree::{SchemaTree, SchemaTreeEvent, TableChange};
use crate::state;
use crate::tokens::{ICON_SM, ICON_XS, RADIUS, TEXT_BASE, TEXT_SM};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Panel {
    Schema,
    Saved,
    Recent,
}

const PANELS: [Panel; 3] = [Panel::Schema, Panel::Saved, Panel::Recent];

pub enum SidebarEvent {
    Selected(ConnectionConfig),
    Insert(String),
    // Opens a new tab for SQL from the tree, like SELECT, COUNT or DDL.
    OpenQuery { connection: ConnectionConfig, sql: String, title: String },
    Browse { connection: ConnectionConfig, schema: String, table: String },
    // SQL from history. Goes into the connection's plain query tab.
    OpenSql { connection: ConnectionConfig, sql: String },
    OpenSaved(SavedQuery),
    SavedRenamed { id: String, name: String },
    // Open tabs pick up the new settings.
    ConnectionsChanged(Vec<ConnectionConfig>),
    TableChanged { connection_id: String, schema: String, table: String, change: TableChange },
}

pub struct Sidebar {
    connections: Vec<ConnectionConfig>,
    folders: Vec<ConnectionFolder>,
    selected: Option<String>,
    switcher_open: bool,
    // Lets a click on the switcher toggle the menu instead of counting as a click outside.
    switcher_bounds: Rc<Cell<Bounds<Pixels>>>,
    menu_focus: FocusHandle,
    connections_panel: Entity<ConnectionsPanel>,
    panel: Panel,
    schema_tree: Entity<SchemaTree>,
    saved: Entity<SavedPanel>,
    history: Entity<HistoryPanel>,
    _subscriptions: Vec<Subscription>,
}

impl Sidebar {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let connections = state::bar(cx).list_connections();
        let folders = state::bar(cx).list_folders();
        let selected = connections.first().map(|c| c.id.clone());
        let connections_panel = cx.new(ConnectionsPanel::new);
        let schema_tree = cx.new(|cx| SchemaTree::new(window, cx));
        let saved = cx.new(|cx| SavedPanel::new(window, cx));
        let history = cx.new(|cx| HistoryPanel::new(window, cx));
        let subscriptions = vec![
            cx.subscribe(&schema_tree, |this, _, event: &SchemaTreeEvent, cx| {
                let event = match event {
                    SchemaTreeEvent::Insert(text) => SidebarEvent::Insert(text.clone()),
                    SchemaTreeEvent::Connected(id) => {
                        let Some(connection) = this.connection(id).cloned() else { return };
                        SidebarEvent::Selected(connection)
                    }
                    SchemaTreeEvent::OpenQuery { sql, title } => {
                        let Some(connection) = this.selected().cloned() else { return };
                        SidebarEvent::OpenQuery { connection, sql: sql.clone(), title: title.clone() }
                    }
                    SchemaTreeEvent::Browse { schema, table } => {
                        let Some(connection) = this.selected().cloned() else { return };
                        SidebarEvent::Browse { connection, schema: schema.clone(), table: table.clone() }
                    }
                    SchemaTreeEvent::TableChanged { connection_id, schema, table, change } => {
                        SidebarEvent::TableChanged {
                            connection_id: connection_id.clone(),
                            schema: schema.clone(),
                            table: table.clone(),
                            change: change.clone(),
                        }
                    }
                };
                cx.emit(event);
            }),
            cx.subscribe(&saved, |_, _, event: &SavedEvent, cx| {
                cx.emit(match event {
                    SavedEvent::Open(query) => SidebarEvent::OpenSaved(query.clone()),
                    SavedEvent::Renamed { id, name } => {
                        SidebarEvent::SavedRenamed { id: id.clone(), name: name.clone() }
                    }
                })
            }),
            cx.subscribe_in(&connections_panel, window, |this, _, event: &ConnectionsEvent, window, cx| {
                this.connections_event(event, window, cx)
            }),
            cx.subscribe(&history, |this, _, event: &HistoryEvent, cx| {
                let HistoryEvent::Open { connection_id, sql } = event;
                if let Some(connection) = this.connection(connection_id).cloned() {
                    cx.emit(SidebarEvent::OpenSql { connection, sql: sql.clone() });
                }
            }),
        ];
        let this = Self {
            connections,
            folders,
            selected,
            switcher_open: false,
            switcher_bounds: Rc::default(),
            menu_focus: cx.focus_handle(),
            connections_panel,
            panel: Panel::Schema,
            schema_tree,
            saved,
            history,
            _subscriptions: subscriptions,
        };
        this.sync_tree(cx);
        this
    }

    fn sync_tree(&self, cx: &mut Context<Self>) {
        let connection = self.selected().cloned();
        let (id, connections) = (self.selected.clone(), self.connections.clone());
        let (panel_connections, folders, selected) =
            (self.connections.clone(), self.folders.clone(), self.selected.clone());
        self.connections_panel.update(cx, |panel, cx| panel.set_state(panel_connections, folders, selected, cx));
        self.schema_tree.update(cx, |tree, cx| tree.set_connection(connection, cx));
        self.saved.update(cx, |saved, cx| saved.set_connection(id.clone(), connections.clone(), cx));
        self.history.update(cx, |history, cx| history.set_connection(id, connections, cx));
    }

    #[cfg(test)]
    pub(crate) fn choose(&mut self, id: &str, cx: &mut Context<Self>) {
        self.select(id.to_string(), cx);
    }

    #[cfg(any(test, feature = "snapshot"))]
    pub(crate) fn panels(&self) -> (Entity<SchemaTree>, Entity<SavedPanel>, Entity<HistoryPanel>) {
        (self.schema_tree.clone(), self.saved.clone(), self.history.clone())
    }

    #[cfg(feature = "snapshot")]
    pub(crate) fn close_switcher(&mut self, cx: &mut Context<Self>) {
        self.set_switcher_open(false, cx);
    }

    pub fn show_panel(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        match name {
            "saved" => self.panel = Panel::Saved,
            "recent" => self.panel = Panel::Recent,
            "connections" => self.switcher_open = true,
            "connection-dialog" => self.edit_connection(self.selected().cloned(), window, cx),
            "import" => self.schema_tree.update(cx, |tree, cx| tree.import_default(window, cx)),
            _ => self.panel = Panel::Schema,
        }
        cx.notify();
    }

    fn reload_connections(&mut self, cx: &mut Context<Self>) {
        let bar = state::bar(cx);
        self.connections = bar.list_connections();
        self.folders = bar.list_folders();
        if self.selected.as_ref().is_none_or(|id| self.connection(id).is_none()) {
            self.selected = self.connections.first().map(|c| c.id.clone());
        }
        self.sync_tree(cx);
        cx.emit(SidebarEvent::ConnectionsChanged(self.connections.clone()));
        cx.notify();
    }

    fn set_switcher_open(&mut self, open: bool, cx: &mut Context<Self>) {
        self.switcher_open = open;
        cx.notify();
    }

    fn toggle_switcher(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_switcher_open(!self.switcher_open, cx);
        if self.switcher_open {
            self.connections_panel.update(cx, |panel, cx| panel.focus_list(window, cx));
        }
    }

    // A click outside closes it, but not one on a popup the panel opened. A row's context menu acts on
    // mouse-up, after this handler. The panel shrinks to the height cap, and its list scrolls.
    fn switcher_menu(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let bounds = self.switcher_bounds.clone();
        deferred(
            v_flex()
                .id("connection-switcher-menu")
                .debug_selector(|| "connection-switcher-menu".into())
                .track_focus(&self.menu_focus)
                .absolute()
                .top(rems(2.692 + 0.308))
                .left_0()
                .right_0()
                .max_h(window.viewport_size().height * 0.6)
                .overflow_hidden()
                .occlude()
                .p(rems(0.615))
                .bg(theme.secondary)
                .border_1()
                .border_color(theme.border)
                .rounded(RADIUS)
                .shadow(vec![BoxShadow {
                    color: hsla(0., 0., 0., 0.35),
                    offset: point(px(0.), px(8.)),
                    blur_radius: px(24.),
                    spread_radius: px(0.),
                    inset: false,
                }])
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                    if event.keystroke.key == "escape" {
                        cx.stop_propagation();
                        this.set_switcher_open(false, cx);
                    }
                }))
                .on_mouse_down_out(cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    let list = this.connections_panel.read(cx).list_focus().clone();
                    let popup = this.menu_focus.contains_focused(window, cx)
                        && !this.menu_focus.is_focused(window)
                        && !list.is_focused(window);
                    if !popup && !bounds.get().contains(&event.position) {
                        this.set_switcher_open(false, cx);
                    }
                }))
                .child(self.connections_panel.clone()),
        )
        .with_priority(1)
    }

    fn connections_event(&mut self, event: &ConnectionsEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            ConnectionsEvent::Activate(id) => {
                self.set_switcher_open(false, cx);
                if state::bar(cx).is_connected(id) {
                    self.select(id.clone(), cx);
                } else {
                    self.set_selected(id, cx);
                }
            }
            ConnectionsEvent::Connected(id) => {
                self.set_switcher_open(false, cx);
                self.panel = Panel::Schema;
                self.select(id.clone(), cx);
            }
            ConnectionsEvent::Edit(connection) => {
                self.set_switcher_open(false, cx);
                // A blank config is the New button, a copy with a name is Duplicate.
                let config = (!connection.name.is_empty() || !connection.id.is_empty()).then(|| (**connection).clone());
                self.edit_connection(config, window, cx);
            }
            ConnectionsEvent::Changed => self.reload_connections(cx),
        }
    }

    fn edit_connection(&mut self, config: Option<ConnectionConfig>, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        connection_dialog::open(config, window, cx, move |_, _, cx| {
            let _ = this.update(cx, |sidebar, cx| sidebar.reload_connections(cx));
        });
    }

    // SQLite files opened from the OS or dropped on the window become a prefilled new connection.
    pub fn open_sqlite(&mut self, file_path: String, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let config = ConnectionConfig {
            name,
            driver: DriverType::Sqlite,
            color: connection_dialog::DEFAULT_COLOR.into(),
            file_path,
            host: "localhost".into(),
            port: 5432,
            ssl_mode: "disable".into(),
            ..Default::default()
        };
        self.edit_connection(Some(config), window, cx);
    }

    pub fn refresh_history(&self, cx: &mut Context<Self>) {
        self.history.update(cx, |history, cx| history.reload(cx));
    }

    pub fn selected(&self) -> Option<&ConnectionConfig> {
        self.selected.as_ref().and_then(|id| self.connections.iter().find(|c| &c.id == id))
    }

    pub fn connections(&self) -> &[ConnectionConfig] {
        &self.connections
    }

    pub fn connection(&self, id: &str) -> Option<&ConnectionConfig> {
        self.connections.iter().find(|c| c.id == id)
    }

    // Follows the active tab without emitting Selected.
    pub fn set_selected(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.selected.as_deref() != Some(id) {
            self.selected = Some(id.to_string());
            self.sync_tree(cx);
            cx.notify();
        }
    }

    fn select(&mut self, id: String, cx: &mut Context<Self>) {
        self.selected = Some(id.clone());
        self.sync_tree(cx);
        if let Some(config) = self.connection(&id).cloned() {
            cx.emit(SidebarEvent::Selected(config));
        }
        cx.notify();
    }

    fn switcher(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let current = self.selected().cloned();
        let connected = current.as_ref().is_some_and(|c| state::bar(cx).is_connected(&c.id));
        let detail: Option<SharedString> =
            current.as_ref().filter(|_| connected).map(|c| connection_subtitle(c).into());
        let button = Button::new("connection-switcher")
            .ghost()
            .w_full()
            .h(rems(2.692))
            .px(rems(0.769))
            .rounded(ButtonRounded::None)
            .debug_selector(|| "connection-switcher".into());
        let Some(current) = current else {
            return button
                .text_color(theme.muted_foreground)
                .child(
                    h_flex()
                        .w_full()
                        .justify_center()
                        .gap(rems(0.308))
                        .text_size(TEXT_BASE)
                        .child(Icon::new(IconName::Plus).size(ICON_XS))
                        .child(t(cx, "sidebar.newConnection")),
                )
                .on_click(cx.listener(|this, _, window, cx| this.edit_connection(None, window, cx)))
                .into_any_element();
        };
        let color = parse_color(&current.color).unwrap_or(theme.primary);
        button
            .when_some(detail, |button, detail| button.tooltip(detail))
            .child(
                h_flex()
                    .w_full()
                    .gap(rems(0.462))
                    .text_size(TEXT_BASE)
                    .text_color(theme.foreground)
                    .child(Icon::new(Lucide::Database).size(ICON_SM).text_color(color))
                    .child(div().flex_1().min_w_0().truncate().font_semibold().child(current.name.clone()))
                    .child(div().flex_none().size(rems(0.538)).rounded_full().bg(if connected {
                        theme.success
                    } else {
                        theme.danger
                    }))
                    .child(Icon::new(IconName::ChevronDown).size(ICON_XS).text_color(theme.muted_foreground)),
            )
            .on_click(cx.listener(|this, _, window, cx| this.toggle_switcher(window, cx)))
            .into_any_element()
    }

    fn tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let tabs = [
            ("sidebar.schema", Lucide::Table2),
            ("sidebar.saved", Lucide::Bookmark),
            ("sidebar.recent", Lucide::Clock),
        ];
        h_flex().flex_none().h(rems(3.077)).border_b_1().border_color(theme.border).children(
            tabs.into_iter().enumerate().map(|(ix, (key, icon))| {
                let active = PANELS[ix] == self.panel;
                h_flex()
                    .id(key)
                    .debug_selector(move || key.into())
                    .relative()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .justify_center()
                    .gap(rems(0.385))
                    .px(rems(0.308))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_size(TEXT_SM)
                    .text_color(if active { theme.foreground } else { theme.muted_foreground })
                    .cursor_pointer()
                    .when(ix + 1 < tabs.len(), |el| el.border_r_1().border_color(theme.border))
                    .child(Icon::new(icon).size(ICON_XS))
                    .child(t(cx, key))
                    .when(active, |el| {
                        el.child(div().absolute().left_0().right_0().bottom_0().h(rems(0.154)).bg(theme.primary))
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.panel = PANELS[ix];
                        cx.notify();
                    }))
            }),
        )
    }

    fn empty(&self, message: SharedString, cx: &App) -> impl IntoElement {
        div().p_4().text_color(cx.theme().muted_foreground).child(message)
    }
}

impl EventEmitter<SidebarEvent> for Sidebar {}

impl Render for Sidebar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let body = if self.connections.is_empty() {
            v_flex()
                .gap_3()
                .child(self.empty(t(cx, "sidebar.noConnectionsYet"), cx))
                .child(
                    div().px_4().child(
                        Button::new("add-connection")
                            .primary()
                            .small()
                            .label(t(cx, "sidebar.addConnectionButton"))
                            .on_click(cx.listener(|this, _, window, cx| this.edit_connection(None, window, cx))),
                    ),
                )
                .into_any_element()
        } else {
            match self.panel {
                Panel::Schema => self.schema_tree.clone().into_any_element(),
                Panel::Saved => self.saved.clone().into_any_element(),
                Panel::Recent => self.history.clone().into_any_element(),
            }
        };
        let bounds = self.switcher_bounds.clone();
        v_flex()
            .relative()
            .size_full()
            .debug_selector(|| "sidebar".into())
            .bg(theme.sidebar)
            .text_color(theme.sidebar_foreground)
            .child(
                div()
                    .relative()
                    .flex_none()
                    .h(rems(2.769))
                    .border_b_1()
                    .border_color(theme.border)
                    .child(self.switcher(cx))
                    // Not GPUI Kit's on_prepaint: its canvas has no insets, so after the button it lands one row
                    // lower, and a click on the button counts as outside the menu and reopens it.
                    .child(
                        canvas(move |rect, _, _| bounds.set(rect), |_, _, _, _| {})
                            .absolute()
                            .top_0()
                            .left_0()
                            .size_full(),
                    ),
            )
            .child(self.tabs(cx))
            .child(div().flex_1().min_h_0().child(body))
            .when(self.switcher_open && !self.connections.is_empty(), |el| el.child(self.switcher_menu(window, cx)))
    }
}
