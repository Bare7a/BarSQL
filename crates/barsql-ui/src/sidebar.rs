use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use barsql_app::ConnectionFolder;
use barsql_core::{ConnectionConfig, DriverType, SavedQuery, scope};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::{ElementExt as _, resize_handle};
use gpui_kit::component::button::{Button, ButtonRounded, ButtonVariants};
use gpui_kit::component::{ActiveTheme, Icon, IconName, StyledExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::assets::engine_icon;
use crate::connection_dialog::{self, ConnectionForm};
use crate::connections_panel::{ConnectionsEvent, ConnectionsPanel, connection_subtitle, parse_color};
use crate::database_pane::{DatabasePane, DatabasePaneEvent};

use crate::form;
use crate::history_panel::{HistoryEvent, HistoryPanel};
use crate::i18n::t;
use crate::saved_panel::{SavedEvent, SavedPanel};
use crate::schema;
use crate::schema_actions::{Applied, Change};
use crate::schema_tree::{ActiveTable, SchemaTree, SchemaTreeEvent, TableChange};
use crate::state;
use crate::tokens::{DOT, ICON_SM, ICON_XS, RADIUS, STRIP, TEXT_BASE, TEXT_SM, TOP_BAR};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Panel {
    Schema,
    Saved,
    Recent,
}

const PANELS: [Panel; 3] = [Panel::Schema, Panel::Saved, Panel::Recent];
// The database each connection that lists them all last showed, when it isn't the connection's own.
const DATABASES_KEY: &str = "barsql-sidebar-databases";

#[derive(Clone)]
struct DatabasesDrag;

impl Render for DatabasesDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

pub enum SidebarEvent {
    Selected(ConnectionConfig),
    Insert(String),
    // Opens a new tab for SQL from the tree, like SELECT, COUNT or DDL. `run` runs it there, as system views do.
    OpenQuery { connection: ConnectionConfig, sql: String, title: String, run: bool },
    Browse { connection: ConnectionConfig, schema: String, table: String },
    OpenDiagram { connection: ConnectionConfig, schema: String },
    // No `table` for a new one.
    OpenStructure { connection: ConnectionConfig, schema: String, table: Option<String> },
    // SQL from history. Goes into the connection's plain query tab.
    OpenSql { connection: ConnectionConfig, sql: String },
    OpenSaved(SavedQuery),
    SavedRenamed { id: String, name: String },
    // Open tabs pick up the new settings.
    ConnectionsChanged(Vec<ConnectionConfig>),
    TableChanged { connection_id: String, schema: String, table: String, change: TableChange },
    // New, Rename or Drop on the selected connection's server, from the Databases list.
    DatabaseChange { base: ConnectionConfig, change: Change },
}

pub struct Sidebar {
    connections: Vec<ConnectionConfig>,
    folders: Vec<ConnectionFolder>,
    // The saved connection. `databases` holds the one its tree shows, if not its own.
    selected: Option<String>,
    databases: HashMap<String, String>,
    switcher_open: bool,
    // Lets a click on the switcher toggle the menu instead of counting as a click outside.
    switcher_bounds: Rc<Cell<Bounds<Pixels>>>,
    menu_focus: FocusHandle,
    connections_panel: Entity<ConnectionsPanel>,
    panel: Panel,
    database_pane: Entity<DatabasePane>,
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
        let database_pane = cx.new(|cx| DatabasePane::new(window, cx));
        let saved = cx.new(|cx| SavedPanel::new(window, cx));
        let history = cx.new(|cx| HistoryPanel::new(window, cx));
        let subscriptions = vec![
            cx.subscribe(&schema_tree, |this, _, event: &SchemaTreeEvent, cx| {
                let event = match event {
                    SchemaTreeEvent::Insert(text) => SidebarEvent::Insert(text.clone()),
                    SchemaTreeEvent::Connected(id) => {
                        let Some(connection) = this.connection(id) else { return };
                        SidebarEvent::Selected(connection)
                    }
                    SchemaTreeEvent::OpenQuery { sql, title, run } => {
                        let Some(connection) = this.selected() else { return };
                        SidebarEvent::OpenQuery { connection, sql: sql.clone(), title: title.clone(), run: *run }
                    }
                    SchemaTreeEvent::Browse { schema, table } => {
                        let Some(connection) = this.selected() else { return };
                        SidebarEvent::Browse { connection, schema: schema.clone(), table: table.clone() }
                    }
                    SchemaTreeEvent::OpenDiagram { schema } => {
                        let Some(connection) = this.selected() else { return };
                        SidebarEvent::OpenDiagram { connection, schema: schema.clone() }
                    }
                    SchemaTreeEvent::OpenStructure { schema, table } => {
                        let Some(connection) = this.selected() else { return };
                        SidebarEvent::OpenStructure { connection, schema: schema.clone(), table: table.clone() }
                    }
                    SchemaTreeEvent::OpenScript { connection, sql, title } => SidebarEvent::OpenQuery {
                        connection: (**connection).clone(),
                        sql: sql.clone(),
                        title: title.clone(),
                        run: false,
                    },
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
            cx.subscribe(&database_pane, |this, _, event: &DatabasePaneEvent, cx| match event {
                DatabasePaneEvent::Select(name) => this.show_database(name.clone(), cx),
                DatabasePaneEvent::Open(name) => {
                    if let Some(base) = this.selected_base().filter(|base| base.shows_all_databases()) {
                        cx.emit(SidebarEvent::Selected(base.in_database(name)));
                    }
                }
                DatabasePaneEvent::Change(change) => {
                    if let Some(base) = this.selected_base().cloned() {
                        cx.emit(SidebarEvent::DatabaseChange { base, change: change.clone() });
                    }
                }
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
                if let Some(connection) = this.connection(connection_id) {
                    cx.emit(SidebarEvent::OpenSql { connection, sql: sql.clone() });
                }
            }),
        ];
        let this = Self {
            connections,
            folders,
            selected,
            databases: state::setting_json(cx, DATABASES_KEY),
            switcher_open: false,
            switcher_bounds: Rc::default(),
            menu_focus: cx.focus_handle(),
            connections_panel,
            panel: Panel::Schema,
            database_pane,
            schema_tree,
            saved,
            history,
            _subscriptions: subscriptions,
        };
        this.sync_tree(cx);
        this
    }

    fn sync_tree(&self, cx: &mut Context<Self>) {
        let connection = self.selected();
        let listing = self.selected_base().filter(|base| base.shows_all_databases()).cloned();
        let shown = connection.as_ref().map(|c| c.scoped_database().unwrap_or_else(|| c.default_database()));
        self.database_pane.update(cx, |pane, cx| pane.set_connection(listing, shown.unwrap_or_default(), cx));
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

    #[cfg(test)]
    pub(crate) fn database_pane(&self) -> Entity<DatabasePane> {
        self.database_pane.clone()
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
            "connection-dialog" => {
                self.edit_connection(self.selected_base().cloned(), window, cx);
            }
            "import" => self.schema_tree.update(cx, |tree, cx| tree.import_default(window, cx)),
            _ => self.panel = Panel::Schema,
        }
        cx.notify();
    }

    pub(crate) fn reload_connections(&mut self, cx: &mut Context<Self>) {
        let bar = state::bar(cx);
        self.connections = bar.list_connections();
        self.folders = bar.list_folders();
        if self.selected_base().is_none() {
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

    // From the status bar.
    pub(crate) fn open_switcher(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.switcher_open {
            self.toggle_switcher(window, cx);
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
                let id = self.remembered(id);
                if state::bar(cx).is_connected(&id) {
                    self.select(id, cx);
                } else {
                    self.set_selected(&id, cx);
                }
            }
            ConnectionsEvent::Connected(id) => {
                self.set_switcher_open(false, cx);
                self.panel = Panel::Schema;
                let id = self.remembered(id);
                self.select(id, cx);
            }
            ConnectionsEvent::Edit(connection) => {
                self.set_switcher_open(false, cx);
                // A blank config is the New button, a copy with a name is Duplicate.
                let config = (!connection.name.is_empty() || !connection.id.is_empty()).then(|| (**connection).clone());
                self.edit_connection(config, window, cx);
            }
            ConnectionsEvent::Changed => self.reload_connections(cx),
            ConnectionsEvent::OpenQuery { connection_id, sql, title } => {
                self.set_switcher_open(false, cx);
                let Some(connection) = self.connection(connection_id) else { return };
                cx.emit(SidebarEvent::OpenQuery { connection, sql: sql.clone(), title: title.clone(), run: true });
            }
        }
    }

    pub fn has_connections(&self) -> bool {
        !self.connections.is_empty()
    }

    // The welcome screen's New connection.
    pub fn add_connection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.edit_connection(None, window, cx);
    }

    // The welcome screen's engines.
    pub fn add_connection_for(&mut self, driver: DriverType, window: &mut Window, cx: &mut Context<Self>) {
        let form = self.edit_connection(None, window, cx);
        form.update(cx, |form, cx| form.set_driver(driver, window, cx));
    }

    pub(crate) fn edit_connection(
        &mut self,
        config: Option<ConnectionConfig>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<ConnectionForm> {
        // A database of a connection edits the saved connection, so saving never adds a copy under its id.
        let config = config.map(|cfg| match cfg.scoped_database() {
            Some(_) => self.connections.iter().find(|c| c.id == scope::base_id(&cfg.id)).cloned().unwrap_or(cfg),
            None => cfg,
        });
        let this = cx.entity().downgrade();
        connection_dialog::open(config, window, cx, move |_, _, cx| {
            let _ = this.update(cx, |sidebar, cx| sidebar.reload_connections(cx));
        })
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

    // The saved connection the header names.
    pub fn selected_base(&self) -> Option<&ConnectionConfig> {
        self.selected.as_ref().and_then(|id| self.connections.iter().find(|c| &c.id == id))
    }

    // What the tree shows: the saved connection, or the database picked under it.
    pub fn selected(&self) -> Option<ConnectionConfig> {
        let base = self.selected_base()?;
        Some(match self.databases.get(&base.id).filter(|_| base.shows_all_databases()) {
            Some(database) => base.in_database(database),
            None => base.clone(),
        })
    }

    pub fn connections(&self) -> &[ConnectionConfig] {
        &self.connections
    }

    pub fn folders(&self) -> &[ConnectionFolder] {
        &self.folders
    }

    // A saved connection or one of its databases.
    pub fn connection(&self, id: &str) -> Option<ConnectionConfig> {
        scope::resolve(&self.connections, id)
    }

    // The database the connection last showed, so the switcher returns to it.
    fn remembered(&self, id: &str) -> String {
        let shows_all = self.connections.iter().any(|c| c.id == id && c.shows_all_databases());
        match self.databases.get(id).filter(|_| shows_all) {
            Some(database) => scope::scoped_id(id, database),
            None => id.to_string(),
        }
    }

    // Follows the active tab without emitting Selected.
    pub fn set_selected(&mut self, id: &str, cx: &mut Context<Self>) {
        let before = (self.selected.clone(), self.selected().map(|c| c.id));
        self.point_at(id, cx);
        if before != (self.selected.clone(), self.selected().map(|c| c.id)) {
            self.sync_tree(cx);
            cx.notify();
        }
    }

    // Selects `id`'s saved connection, and its database where the connection lists them all.
    fn point_at(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(cfg) = self.connection(id) else {
            self.selected = Some(id.to_string());
            return;
        };
        let (base, database) = match cfg.scoped_database() {
            Some(database) => (scope::base_id(&cfg.id).to_string(), database),
            None => (cfg.id.clone(), cfg.default_database()),
        };
        self.selected = Some(base.clone());
        self.remember_database(&base, database, cx);
    }

    // The tree shows the database picked in the Databases list, and loads it without opening a tab.
    fn show_database(&mut self, name: String, cx: &mut Context<Self>) {
        let Some(base) = self.selected.clone() else { return };
        self.remember_database(&base, name, cx);
        self.sync_tree(cx);
        self.schema_tree.update(cx, |tree, cx| tree.load_now(cx));
        cx.notify();
    }

    // A new database opens in the tree. One renamed or dropped while shown gives way to its new name or the
    // connection's own.
    pub(crate) fn database_changed(&mut self, applied: &Applied, cx: &mut Context<Self>) {
        let Some(base) = self.connections.iter().find(|c| c.id == applied.connection_id).cloned() else { return };
        let selected = self.selected.as_deref() == Some(base.id.as_str());
        let shown = self.databases.get(&base.id).cloned().unwrap_or_else(|| base.default_database());
        let next = match &applied.change {
            Change::CreateDatabase => selected.then(|| applied.new_name.clone()),
            Change::RenameDatabase { database } | Change::DropDatabase { database } => {
                schema::forget(&base.in_database(database).id, cx);
                let renamed = matches!(applied.change, Change::RenameDatabase { .. });
                (shown == *database).then(|| if renamed { applied.new_name.clone() } else { base.default_database() })
            }
            _ => None,
        };
        match next {
            Some(next) if selected => self.show_database(next, cx),
            Some(next) => self.remember_database(&base.id, next, cx),
            None => {}
        }
        let (id, change, new_name) = (base.id.clone(), applied.change.clone(), applied.new_name.clone());
        self.database_pane.update(cx, |pane, cx| pane.database_changed(&id, &change, &new_name, cx));
    }

    fn remember_database(&mut self, base: &str, database: String, cx: &mut Context<Self>) {
        let Some(saved) = self.connections.iter().find(|c| c.id == base && c.shows_all_databases()) else { return };
        let changed = match database == saved.default_database() {
            true => self.databases.remove(base).is_some(),
            false => self.databases.insert(base.to_string(), database.clone()).as_ref() != Some(&database),
        };
        if changed {
            state::set_setting_json(cx, DATABASES_KEY, &self.databases);
        }
    }

    pub fn set_active_table(&mut self, table: Option<ActiveTable>, cx: &mut Context<Self>) {
        self.schema_tree.update(cx, |tree, cx| tree.set_active_table(table, cx));
    }

    fn select(&mut self, id: String, cx: &mut Context<Self>) {
        self.point_at(&id, cx);
        self.sync_tree(cx);
        if let Some(config) = self.connection(&id) {
            cx.emit(SidebarEvent::Selected(config));
        }
        cx.notify();
    }

    fn switcher(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let current = self.selected_base().cloned();
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
                .on_click(cx.listener(|this, _, window, cx| {
                    this.edit_connection(None, window, cx);
                }))
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
                    .child(engine_icon(&current.driver).size(ICON_SM).text_color(color))
                    .child(div().flex_1().min_w_0().truncate().font_semibold().child(current.name.clone()))
                    .child(div().flex_none().size(DOT).rounded_full().bg(if connected {
                        theme.success
                    } else {
                        theme.danger
                    }))
                    .child(Icon::new(IconName::ChevronDown).size(ICON_XS).text_color(theme.muted_foreground)),
            )
            .on_click(cx.listener(|this, _, window, cx| this.toggle_switcher(window, cx)))
            .into_any_element()
    }

    pub(crate) fn schema_tree(&self) -> Entity<SchemaTree> {
        self.schema_tree.clone()
    }

    fn tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let tabs = [
            ("sidebar.schema", Lucide::Table2),
            ("sidebar.saved", Lucide::Bookmark),
            ("sidebar.recent", Lucide::Clock),
        ];
        h_flex()
            .debug_selector(|| "sidebar-tabs".into())
            .flex_none()
            .h(STRIP)
            .border_b_1()
            .border_color(theme.border)
            .children(tabs.into_iter().enumerate().map(|(ix, (key, icon))| {
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
            }))
    }

    // The Databases list, then the tree of the one picked, under a row naming it.
    fn schema_with_databases(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let collapsed = self.database_pane.read(cx).collapsed();
        v_flex()
            .size_full()
            .on_drag_move(cx.listener(|this, event: &DragMoveEvent<DatabasesDrag>, window, cx| {
                let height = event.event.position.y - event.bounds.top();
                this.database_pane.update(cx, |pane, cx| pane.resize(height, window.rem_size(), cx));
            }))
            .child(self.database_pane.clone())
            .child(
                v_flex()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .border_t_1()
                    .border_color(theme.border)
                    .child(div().flex_1().min_h_0().child(self.schema_tree.clone()))
                    .when(!collapsed, |el| {
                        el.child(
                            resize_handle::<DatabasesDrag, DatabasesDrag>("databases-splitter", Axis::Vertical)
                                .on_drag(DatabasesDrag, |drag, _, _, cx| {
                                    cx.stop_propagation();
                                    cx.new(|_| drag.clone())
                                }),
                        )
                    }),
            )
            .into_any_element()
    }

    fn empty(&self, message: SharedString, cx: &App) -> impl IntoElement {
        form::empty_state(Some(Icon::new(Lucide::Database)), message, cx)
    }
}

impl EventEmitter<SidebarEvent> for Sidebar {}

impl Render for Sidebar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        // Without connections the welcome screen leads, and New connection sits at the top, so this only says so.
        let body = if self.connections.is_empty() {
            self.empty(t(cx, "sidebar.noConnectionsYet"), cx).into_any_element()
        } else {
            match self.panel {
                Panel::Schema if self.selected_base().is_some_and(|base| base.shows_all_databases()) => {
                    self.schema_with_databases(cx)
                }
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
                    .h(TOP_BAR)
                    .border_b_1()
                    .border_color(theme.border)
                    .child(self.switcher(cx))
                    .on_prepaint(move |rect, _, _| bounds.set(rect)),
            )
            .when(!self.connections.is_empty(), |el| el.child(self.tabs(cx)))
            .child(div().flex_1().min_h_0().child(body))
            .when(self.switcher_open && !self.connections.is_empty(), |el| el.child(self.switcher_menu(window, cx)))
    }
}
