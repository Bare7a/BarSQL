use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::Range;
use std::time::Duration;

use barsql_core::{ColumnInfo, ConnectionConfig, DriverType, ObjectKind, ObjectRef, SqlDialect, TableInfo};
use barsql_sql::alter::ConstraintKind;
use barsql_sql::dml::preview_select;
use barsql_sql::lang::quoting::build_qualified_table;
use barsql_sql::system_views::{ViewScope, views};
use barsql_sql::{TypeFamily, display_type, type_family};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt, DropdownMenu, PopupMenu, PopupMenuItem};
use gpui_kit::component::scroll::ScrollbarAxis;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme, Disableable, Icon, IconName, Sizable, StyledExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::i18n::{I18n, count, t, t_count, t_with};
use crate::list_nav::{self, BarClearance, LIST_INSET};
use crate::schema::{self, Schemas};
use crate::schema_actions::{self, Applied, Change};
use crate::schema_objects::{
    Badge, Group, ObjectRow, constraint_rows, group_key, index_rows, routine_rows, routines_key, trigger_rows,
};
use crate::scrollbars::HoverScrollbar as _;
use crate::spinner::Spinner;
use crate::state::{self, set_setting_json, setting_json};
use crate::toast;
use crate::tokens::{ICON_SM, ICON_XS, RADIUS, RADIUS_SM, TEXT_2XS, TEXT_BASE, TEXT_SM, TEXT_XS, TINT};
use crate::{form, hover_card};

// How far a table's Browse button sits from the end of its row.
const BROWSE_INSET: Rems = rems(0.308);
const SCHEMA_EXPANDED_KEY: &str = "barsql-schema-expanded";
const TABLES_EXPANDED_KEY: &str = "barsql-schema-tables-expanded";
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(200);
const ROW_HEIGHT_REM: f32 = 2.;
const INDENT_REM: f32 = 1.231;
// A row's own inset, before its chevron.
const ROW_PADDING: Rems = rems(0.462);
// Enum values a column's tooltip lists before cutting off.
const TIP_VALUES: usize = 12;
const CONTEXT: &str = "SchemaTree";

actions!(schema_tree, [CursorUp, CursorDown, CursorFirst, CursorLast, ActivateRow, CollapseRow, ExpandRow]);

pub fn init(cx: &mut App) {
    let context = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("up", CursorUp, context),
        KeyBinding::new("down", CursorDown, context),
        KeyBinding::new("home", CursorFirst, context),
        KeyBinding::new("end", CursorLast, context),
        KeyBinding::new("enter", ActivateRow, context),
        KeyBinding::new("space", ActivateRow, context),
        KeyBinding::new("left", CollapseRow, context),
        KeyBinding::new("right", ExpandRow, context),
    ]);
}

pub enum SchemaTreeEvent {
    // Insert at the active editor's caret.
    Insert(String),
    // `run` runs it as soon as its tab opens, as the system views do.
    OpenQuery { sql: String, title: String, run: bool },
    Browse { schema: String, table: String },
    // The ER diagram of a schema, in its own tab.
    OpenDiagram { schema: String },
    // A table's Structure tab, or a new table's without `table`.
    OpenStructure { schema: String, table: Option<String> },
    // SQL for another connection, like a schema comparison's sync script. Opens without running.
    OpenScript { connection: Box<ConnectionConfig>, sql: String, title: String },
    // Connect button succeeded for this connection id, so land in a tab for it.
    Connected(String),
    // A dialog changed a table, so its open table views need to follow.
    TableChanged { connection_id: String, schema: String, table: String, change: TableChange },
}

// The table the active tab shows. Its row is marked.
#[derive(Debug, Clone, PartialEq)]
pub struct ActiveTable {
    pub connection_id: String,
    pub schema: String,
    pub table: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TableChange {
    Renamed(String),
    Dropped,
    // Columns, keys or rows changed.
    Altered,
}

// The type as shown and as the catalog spells it, so `varchar` and `character varying` both find a column.
pub fn column_matches(driver: &DriverType, column: &ColumnInfo, needle: &str) -> bool {
    let shown = display_type(driver.dialect(), &column.data_type);
    format!("{} {} {shown}", column.name, column.data_type).to_lowercase().contains(needle)
}

pub fn table_matches(driver: &DriverType, name: &str, columns: Option<&[ColumnInfo]>, needle: &str) -> bool {
    name.to_lowercase().contains(needle)
        || columns.is_some_and(|cols| cols.iter().any(|c| column_matches(driver, c, needle)))
}

enum GroupState {
    Loading,
    Loaded(Vec<ObjectRow>),
}

#[derive(Clone)]
enum RowKind {
    // `kept_open`: a search holds the row open.
    Schema { name: String, expanded: bool, kept_open: bool, count: Option<String> },
    Table { schema: String, table: TableInfo, expanded: bool, kept_open: bool },
    Column { column: ColumnInfo, schema: String, table: String, view: bool },
    Group { key: String, group: Group, schema: String, table: Option<String>, expanded: bool, count: Option<usize> },
    Object { row: ObjectRow, schema: String, table: Option<String> },
    Note { text: SharedString, spinner: bool },
}

#[derive(Clone)]
struct TreeRow {
    id: SharedString,
    depth: usize,
    kind: RowKind,
}

// Each level loads on first expand. The whole tree is flattened into one virtualised list.
pub struct SchemaTree {
    connection: Option<ConnectionConfig>,
    search: Entity<InputState>,
    needle: String,
    // Keyed `connection:schema` and `connection:schema:table`, and persisted.
    expanded_schemas: BTreeMap<String, bool>,
    expanded_tables: BTreeMap<String, bool>,
    expanded_groups: HashSet<String>,
    groups: HashMap<String, GroupState>,
    error: Option<String>,
    // A load this panel asked for expands the schemas it brings.
    expand_on_load: bool,
    connecting: bool,
    rows: Vec<TreeRow>,
    scroll: UniformListScrollHandle,
    clearance: BarClearance,
    focus: FocusHandle,
    // Kept by row id so it survives rows opening and closing around it. The ring shows only after a key moves it.
    cursor: Option<SharedString>,
    keyed: bool,
    active_table: Option<ActiveTable>,
    search_task: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<SchemaTreeEvent> for SchemaTree {}

impl SchemaTree {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let placeholder = t(cx, "sidebar.searchTablesColumns");
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        let subscriptions = vec![
            cx.subscribe_in(&search, window, |this, search, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    let value = search.read(cx).value().trim().to_lowercase();
                    this.search_task = cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(SEARCH_DEBOUNCE).await;
                        let _ = this.update(cx, |this, cx| this.set_needle(value, cx));
                    });
                }
            }),
            cx.observe_global::<Schemas>(|this, cx| this.schema_changed(cx)),
            cx.observe_global_in::<I18n>(window, |this, window, cx| {
                let placeholder = t(cx, "sidebar.searchTablesColumns");
                this.search.update(cx, |search, cx| search.set_placeholder(placeholder, window, cx));
                this.rebuild(cx);
            }),
        ];
        Self {
            connection: None,
            search,
            needle: String::new(),
            expanded_schemas: setting_json(cx, SCHEMA_EXPANDED_KEY),
            expanded_tables: setting_json(cx, TABLES_EXPANDED_KEY),
            expanded_groups: HashSet::new(),
            groups: HashMap::new(),
            error: None,
            expand_on_load: false,
            connecting: false,
            rows: Vec::new(),
            scroll: UniformListScrollHandle::new(),
            clearance: BarClearance::default(),
            focus: cx.focus_handle(),
            cursor: None,
            keyed: false,
            active_table: None,
            search_task: Task::ready(()),
            _subscriptions: subscriptions,
        }
    }

    pub fn set_connection(&mut self, connection: Option<ConnectionConfig>, cx: &mut Context<Self>) {
        let same = self.connection.as_ref().map(|c| &c.id) == connection.as_ref().map(|c| &c.id);
        self.connection = connection;
        if !same {
            self.error = None;
            self.connecting = false;
        }
        // A live connection loads its schema on first view, even if it connected after the tree showed it.
        if let Some(conn) = self.connection.clone().filter(|c| state::bar(cx).is_connected(&c.id)) {
            self.load(&conn, false, cx);
        }
        if !same {
            self.schema_changed(cx);
        }
    }

    pub fn set_active_table(&mut self, table: Option<ActiveTable>, cx: &mut Context<Self>) {
        if self.active_table != table {
            self.active_table = table;
            cx.notify();
        }
    }

    fn load(&mut self, conn: &ConnectionConfig, force: bool, cx: &mut Context<Self>) {
        let data = schema::get(cx, &conn.id);
        if !force && data.is_some_and(|d| d.schemas().is_some() || d.loading()) {
            return;
        }
        self.expand_on_load = true;
        self.error = None;
        if force {
            self.groups.clear();
            self.expanded_groups.clear();
            schema::reload(&conn.id, conn.driver.clone(), cx);
        } else {
            schema::ensure_loaded(&conn.id, conn.driver.clone(), cx);
        }
    }

    // Toolbar import, into the connection's first schema.
    pub fn import_default(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let schema = self.connection.as_ref().and_then(|c| {
            schema::get(cx, &c.id).and_then(|d| d.schemas()).and_then(|s| s.first()).map(|s| s.name.clone())
        });
        self.import(schema.unwrap_or_default(), None, window, cx);
    }

    // A finished import reloads the schema.
    fn import(&mut self, schema: String, table: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(connection) = self.connection.clone() else { return };
        let tables = schema::get(cx, &connection.id)
            .and_then(|entry| entry.tables(&schema))
            .map(|tables| tables.iter().map(|t| t.name.clone()).collect())
            .unwrap_or_default();
        let (id, driver) = (connection.id.clone(), connection.driver.clone());
        crate::import_dialog::open(connection, schema, tables, table, window, cx, move |cx| {
            schema::reload(&id, driver.clone(), cx);
        });
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        if let Some(conn) = self.connection.clone() {
            self.load(&conn, true, cx);
            self.rebuild(cx);
        }
    }

    // True when loaded, loading or the pool is live, so the tree shows instead of the Connect button.
    fn connected(&self, cx: &App) -> bool {
        let Some(conn) = &self.connection else { return false };
        state::bar(cx).is_connected(&conn.id)
            || schema::get(cx, &conn.id).is_some_and(|d| d.schemas().is_some() || d.loading())
    }

    fn connect(&mut self, cx: &mut Context<Self>) {
        let Some(conn) = self.connection.clone() else { return };
        self.connecting = true;
        self.error = None;
        let bar = state::bar(cx);
        let id = conn.id.clone();
        let task = state::spawn(cx, async move { bar.connect(&id).await });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.connecting = false;
                match result {
                    Some(Ok(())) => {
                        this.load(&conn, false, cx);
                        cx.emit(SchemaTreeEvent::Connected(conn.id.clone()));
                    }
                    Some(Err(error)) => this.error = Some(error.message),
                    None => {}
                }
                this.rebuild(cx);
            });
        })
        .detach();
        cx.notify();
    }

    fn set_needle(&mut self, needle: String, cx: &mut Context<Self>) {
        if self.needle != needle {
            self.needle = needle;
            self.schema_changed(cx);
        }
    }

    fn schema_changed(&mut self, cx: &mut Context<Self>) {
        if let Some(conn) = self.connection.clone()
            && let Some(data) = schema::get(cx, &conn.id)
        {
            if let Some(error) = data.error() {
                self.error = Some(error.to_string());
            }
            if self.expand_on_load && data.schemas().is_some() && !data.loading() {
                self.expand_on_load = false;
                let loaded: Vec<String> = data
                    .schemas()
                    .unwrap_or_default()
                    .iter()
                    .filter(|s| data.tables(&s.name).is_some())
                    .map(|s| format!("{}:{}", conn.id, s.name))
                    .collect();
                for key in loaded {
                    self.expanded_schemas.insert(key, true);
                }
                set_setting_json(cx, SCHEMA_EXPANDED_KEY, &self.expanded_schemas);
            }
            self.hydrate(&conn, cx);
        }
        self.rebuild(cx);
    }

    // Loads tables of open schemas and columns of open tables. A search loads every schema's tables, plus the
    // columns of tables whose names don't match.
    fn hydrate(&mut self, conn: &ConnectionConfig, cx: &mut Context<Self>) {
        let Some(data) = schema::get(cx, &conn.id) else { return };
        let Some(schemas) = data.schemas() else { return };
        let searching = !self.needle.is_empty();
        let mut tables_to_load = Vec::new();
        let mut columns_to_load = Vec::new();
        for sch in schemas {
            let key = format!("{}:{}", conn.id, sch.name);
            let open = self.expanded_schemas.get(&key) == Some(&true);
            let Some(tables) = data.tables(&sch.name) else {
                if (open || searching) && !data.tables_loading(&sch.name) {
                    tables_to_load.push(sch.name.clone());
                }
                continue;
            };
            for table in tables {
                let schema_name = if table.schema.is_empty() { &sch.name } else { &table.schema };
                let loaded = data.columns(schema_name, &table.name).is_some();
                if loaded || data.columns_loading(schema_name, &table.name) {
                    continue;
                }
                let tk = format!("{}:{schema_name}:{}", conn.id, table.name);
                let open_table = open && self.expanded_tables.get(&tk) == Some(&true);
                let name_misses = searching && !table.name.to_lowercase().contains(&self.needle);
                if open_table || name_misses {
                    columns_to_load.push((schema_name.clone(), table.name.clone()));
                }
            }
        }
        for name in tables_to_load {
            schema::load_tables(&conn.id, &name, cx);
        }
        for (schema_name, table) in columns_to_load {
            self.load_columns(&conn.id, &schema_name, &table, cx);
        }
    }

    // Column load failures show here. The editors ignore them.
    fn load_columns(&mut self, connection_id: &str, schema_name: &str, table: &str, cx: &mut Context<Self>) {
        let load = schema::columns(connection_id, schema_name, table, cx);
        cx.spawn(async move |this, cx| {
            if let Err(error) = load.await
                && !error.is_empty()
            {
                let _ = this.update(cx, |this, cx| {
                    this.error = Some(error);
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn toggle_schema(&mut self, name: &str, cx: &mut Context<Self>) {
        let Some(conn) = self.connection.clone() else { return };
        let key = format!("{}:{name}", conn.id);
        let open = self.expanded_schemas.get(&key) == Some(&true);
        self.expanded_schemas.insert(key, !open);
        set_setting_json(cx, SCHEMA_EXPANDED_KEY, &self.expanded_schemas);
        if !open {
            schema::load_tables(&conn.id, name, cx);
        }
        self.schema_changed(cx);
    }

    fn toggle_table(&mut self, schema_name: &str, table: &str, cx: &mut Context<Self>) {
        let Some(conn) = self.connection.clone() else { return };
        let key = format!("{}:{schema_name}:{table}", conn.id);
        let open = self.expanded_tables.get(&key) == Some(&true);
        self.expanded_tables.insert(key, !open);
        set_setting_json(cx, TABLES_EXPANDED_KEY, &self.expanded_tables);
        if !open {
            self.load_columns(&conn.id, schema_name, table, cx);
        }
        self.rebuild(cx);
    }

    // Loads on first expand. A failed load collapses the group again.
    fn toggle_group(
        &mut self,
        key: String,
        group: Group,
        schema_name: String,
        table: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(conn) = self.connection.clone() else { return };
        if self.expanded_groups.remove(&key) {
            self.rebuild(cx);
            return;
        }
        self.expanded_groups.insert(key.clone());
        if self.groups.contains_key(&key) {
            self.rebuild(cx);
            return;
        }
        self.groups.insert(key.clone(), GroupState::Loading);
        let bar = state::bar(cx);
        let id = conn.id.clone();
        let table = table.unwrap_or_default();
        let load = state::spawn(cx, async move {
            match group {
                Group::Indexes => bar.list_indexes(&id, &schema_name, &table).await.map(index_rows),
                Group::Constraints => bar.list_constraints(&id, &schema_name, &table).await.map(constraint_rows),
                Group::Triggers => bar.list_triggers(&id, &schema_name, &table).await.map(trigger_rows),
                Group::Routines => bar.list_routines(&id, &schema_name).await.map(routine_rows),
            }
        });
        cx.spawn(async move |this, cx| {
            let result = load.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Some(Ok(rows)) => {
                        this.groups.insert(key, GroupState::Loaded(rows));
                    }
                    Some(Err(error)) => {
                        this.groups.remove(&key);
                        this.expanded_groups.remove(&key);
                        this.error = Some(error.message);
                    }
                    None => {
                        this.groups.remove(&key);
                    }
                }
                this.rebuild(cx);
            });
        })
        .detach();
        self.rebuild(cx);
    }

    fn rebuild(&mut self, cx: &mut Context<Self>) {
        self.rows = self.build_rows(cx);
        cx.notify();
    }

    fn build_rows(&self, cx: &App) -> Vec<TreeRow> {
        let mut rows = Vec::new();
        let Some(conn) = &self.connection else { return rows };
        let Some(data) = schema::get(cx, &conn.id) else { return rows };
        let Some(schemas) = data.schemas() else { return rows };
        let needle = self.needle.as_str();
        for sch in schemas {
            let key = format!("{}:{}", conn.id, sch.name);
            let all = data.tables(&sch.name).unwrap_or_default();
            let loading = data.tables_loading(&sch.name);
            let visible: Vec<&TableInfo> = all
                .iter()
                .filter(|t| {
                    let schema_name = if t.schema.is_empty() { &sch.name } else { &t.schema };
                    let columns = data.columns(schema_name, &t.name).map(|c| c.as_slice());
                    needle.is_empty() || table_matches(&conn.driver, &t.name, columns, needle)
                })
                .collect();
            if !needle.is_empty() && visible.is_empty() && !loading {
                continue;
            }
            let kept_open = !needle.is_empty();
            let expanded = kept_open || self.expanded_schemas.get(&key) == Some(&true);
            let count = (!all.is_empty()).then(|| {
                if needle.is_empty() { all.len().to_string() } else { format!("{}/{}", visible.len(), all.len()) }
            });
            rows.push(TreeRow {
                id: format!("s:{}", sch.name).into(),
                depth: 0,
                kind: RowKind::Schema { name: sch.name.clone(), expanded, kept_open, count },
            });
            if !expanded {
                continue;
            }
            if loading {
                rows.push(note(&key, 1, t(cx, "sidebar.loadingTables"), true));
            } else if all.is_empty() {
                rows.push(note(&key, 1, t(cx, "sidebar.noTablesRefresh"), false));
            } else {
                for table in visible {
                    let schema_name = if table.schema.is_empty() { &sch.name } else { &table.schema };
                    self.push_table(&mut rows, conn, schema_name, table, cx);
                }
            }
            if !loading && needle.is_empty() && Group::Routines.listed(&conn.driver) {
                let group = routines_key(&conn.id, &sch.name);
                self.push_group(&mut rows, 1, group, Group::Routines, &sch.name, None, cx);
            }
        }
        rows
    }

    fn push_table(
        &self,
        rows: &mut Vec<TreeRow>,
        conn: &ConnectionConfig,
        schema_name: &str,
        table: &TableInfo,
        cx: &App,
    ) {
        let data = schema::get(cx, &conn.id);
        let columns = data.and_then(|d| d.columns(schema_name, &table.name));
        let loading = data.is_some_and(|d| d.columns_loading(schema_name, &table.name));
        let needle = self.needle.as_str();
        let name_matches = needle.is_empty() || table.name.to_lowercase().contains(needle);
        let matches = |column: &ColumnInfo| column_matches(&conn.driver, column, needle);
        let column_hit = !needle.is_empty() && columns.is_some_and(|c| c.iter().any(matches));
        let tk = format!("{}:{schema_name}:{}", conn.id, table.name);
        let kept_open = !needle.is_empty() && !name_matches && (column_hit || loading);
        let expanded = kept_open || self.expanded_tables.get(&tk) == Some(&true);
        let id = format!("{schema_name}.{}", table.name);
        rows.push(TreeRow {
            id: format!("t:{id}").into(),
            depth: 1,
            kind: RowKind::Table { schema: schema_name.to_string(), table: table.clone(), expanded, kept_open },
        });
        if !expanded {
            return;
        }
        let shown: Vec<&ColumnInfo> = match columns {
            Some(cols) if name_matches => cols.iter().collect(),
            Some(cols) => cols.iter().filter(|c| matches(c)).collect(),
            None => Vec::new(),
        };
        if loading {
            rows.push(note(&id, 2, t(cx, "sidebar.loadingColumns"), true));
        } else if shown.is_empty() {
            let key =
                if !needle.is_empty() && !name_matches { "sidebar.searchingColumns" } else { "sidebar.noColumnsFound" };
            rows.push(note(&id, 2, t(cx, key), false));
        } else {
            let view = ObjectKind::for_relation(&table.kind) != ObjectKind::Table;
            for column in shown {
                rows.push(TreeRow {
                    id: format!("c:{id}.{}", column.name).into(),
                    depth: 2,
                    kind: RowKind::Column {
                        column: column.clone(),
                        schema: schema_name.to_string(),
                        table: table.name.clone(),
                        view,
                    },
                });
            }
        }
        // Searching is a column hunt, so hide the object groups.
        if needle.is_empty() {
            for group in Group::TABLE.into_iter().filter(|g| g.listed(&conn.driver)) {
                let key = group_key(&conn.id, schema_name, &table.name, group);
                self.push_group(rows, 2, key, group, schema_name, Some(&table.name), cx);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn push_group(
        &self,
        rows: &mut Vec<TreeRow>,
        depth: usize,
        key: String,
        group: Group,
        schema_name: &str,
        table: Option<&str>,
        cx: &App,
    ) {
        let expanded = self.expanded_groups.contains(&key);
        let state = self.groups.get(&key);
        let count = match state {
            Some(GroupState::Loaded(list)) => Some(list.len()),
            _ => None,
        };
        rows.push(TreeRow {
            id: format!("g:{key}").into(),
            depth,
            kind: RowKind::Group {
                key: key.clone(),
                group,
                schema: schema_name.to_string(),
                table: table.map(str::to_string),
                expanded,
                count,
            },
        });
        if !expanded {
            return;
        }
        match state {
            Some(GroupState::Loading) => rows.push(note(&key, depth + 1, t(cx, "sidebar.loadingObjects"), true)),
            Some(GroupState::Loaded(list)) if list.is_empty() => {
                rows.push(note(&key, depth + 1, t(cx, &format!("sidebar.empty.{}", group.key())), false))
            }
            Some(GroupState::Loaded(list)) => rows.extend(list.iter().map(|row| TreeRow {
                id: format!("o:{key}:{}", row.key).into(),
                depth: depth + 1,
                kind: RowKind::Object {
                    row: row.clone(),
                    schema: schema_name.to_string(),
                    table: table.map(str::to_string),
                },
            })),
            None => {}
        }
    }

    fn copy(text: String, message: SharedString, cx: &mut App) {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        toast::success(message, cx);
    }

    // `open` puts the DDL in a new tab instead of copying it.
    fn with_ddl(&self, object: ObjectRef, open: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(conn) = self.connection.clone() else { return };
        let bar = state::bar(cx);
        let target = object.clone();
        let ddl = state::spawn(cx, async move { bar.object_ddl(&conn.id, &target).await });
        cx.spawn_in(window, async move |this, cx| {
            let result = ddl.await;
            let _ = this.update(cx, |_, cx| match result {
                Some(Ok(ddl)) if open => cx.emit(SchemaTreeEvent::OpenQuery {
                    sql: ddl,
                    title: t_with(cx, "sidebar.ddlTabTitle", &[("name", &object.name)]).to_string(),
                    run: false,
                }),
                Some(Ok(ddl)) => Self::copy(ddl, t(cx, "toast.copiedDDL"), cx),
                Some(Err(error)) => {
                    toast::error_in(t(cx, "errors.ddlFailed"), error.message, cx);
                }
                None => {}
            });
        })
        .detach();
    }

    fn open_change(&mut self, change: Change, window: &mut Window, cx: &mut Context<Self>) {
        let Some(connection) = self.connection.clone() else { return };
        let this = cx.entity().downgrade();
        schema_actions::open_change(connection, change, window, cx, move |applied, cx| {
            let _ = this.update(cx, |tree, cx| tree.changed(applied, cx));
        });
    }

    // Works on the change's own connection, which the tree may no longer be showing.
    fn changed(&mut self, applied: &Applied, cx: &mut Context<Self>) {
        let id = applied.connection_id.as_str();
        // A cascade can reach any object, so everything loads again.
        let cascaded = applied.options.cascade;
        if cascaded {
            let prefix = format!("{id}:");
            self.groups.retain(|key, _| !key.starts_with(&prefix));
            self.expanded_groups.retain(|key| !key.starts_with(&prefix));
            schema::reload(id, applied.driver.clone(), cx);
        }
        let expanded = |schema: &str, table: &str| format!("{id}:{schema}:{table}");
        let (schema, table, table_change) = match &applied.change {
            Change::RenameTable { schema, table, .. } | Change::DropTable { schema, table, .. } => {
                let renamed = matches!(applied.change, Change::RenameTable { .. }).then(|| applied.new_name.clone());
                if let Some(open) = self.expanded_tables.remove(&expanded(schema, table)) {
                    if let Some(name) = &renamed {
                        self.expanded_tables.insert(expanded(schema, name), open);
                    }
                    set_setting_json(cx, TABLES_EXPANDED_KEY, &self.expanded_tables);
                }
                self.forget_groups(id, schema, table, false, cx);
                if !cascaded {
                    schema::reload_tables(id, schema, cx);
                }
                (schema, table, renamed.map_or(TableChange::Dropped, TableChange::Renamed))
            }
            Change::Truncate { schema, table } => (schema, table, TableChange::Altered),
            Change::RenameColumn { schema, table, .. } | Change::DropColumn { schema, table, .. } => {
                self.forget_groups(id, schema, table, true, cx);
                if !cascaded {
                    schema::reload_columns(id, schema, table, cx);
                }
                (schema, table, TableChange::Altered)
            }
            Change::DropObject { object, .. } if object.table.is_empty() => {
                let key = routines_key(id, &object.schema);
                self.forget_group(id, key, Group::Routines, &object.schema, None, true, cx);
                self.rebuild(cx);
                return;
            }
            // Constraints drive column badges and table view editing. Indexes and triggers affect neither.
            Change::DropObject { object, .. } => {
                self.forget_groups(id, &object.schema, &object.table, true, cx);
                if object.kind != ObjectKind::Constraint {
                    self.rebuild(cx);
                    return;
                }
                if !cascaded {
                    schema::reload_columns(id, &object.schema, &object.table, cx);
                }
                (&object.schema, &object.table, TableChange::Altered)
            }
        };
        cx.emit(SchemaTreeEvent::TableChanged {
            connection_id: id.to_string(),
            schema: schema.clone(),
            table: table.clone(),
            change: table_change,
        });
        self.rebuild(cx);
    }

    // Reloads now if the group is open, `reload` is set and its connection is still shown. Otherwise it loads on
    // the next expand.
    #[allow(clippy::too_many_arguments)]
    fn forget_group(
        &mut self,
        connection_id: &str,
        key: String,
        group: Group,
        schema: &str,
        table: Option<&str>,
        reload: bool,
        cx: &mut Context<Self>,
    ) {
        self.groups.remove(&key);
        let shown = self.connection.as_ref().is_some_and(|c| c.id == connection_id);
        if self.expanded_groups.remove(&key) && reload && shown {
            self.toggle_group(key, group, schema.to_string(), table.map(str::to_string), cx);
        }
    }

    fn forget_groups(&mut self, connection_id: &str, schema: &str, table: &str, reload: bool, cx: &mut Context<Self>) {
        for group in Group::TABLE {
            let key = group_key(connection_id, schema, table, group);
            self.forget_group(connection_id, key, group, schema, Some(table), reload, cx);
        }
    }

    // Object groups close and reload on their next expand.
    fn refresh_schema(&mut self, name: &str, cx: &mut Context<Self>) {
        let Some(conn) = self.connection.clone() else { return };
        let prefix = format!("{}:{name}:", conn.id);
        self.groups.retain(|key, _| !key.starts_with(&prefix));
        self.expanded_groups.retain(|key| !key.starts_with(&prefix));
        schema::reload_tables(&conn.id, name, cx);
        self.rebuild(cx);
    }

    fn count_rows(&mut self, schema: String, table: String, cx: &mut Context<Self>) {
        let Some(conn) = self.connection.clone() else { return };
        let bar = state::bar(cx);
        let target = (schema, table.clone());
        let task = state::spawn(cx, async move { bar.count_rows(&conn.id, &target.0, &target.1).await });
        cx.spawn(async move |_, cx| {
            let result = task.await;
            cx.update(|cx| match result {
                Some(Ok(rows)) => {
                    let formatted = count(cx, rows.max(0) as usize);
                    let vars = [("name", table.as_str()), ("formatted", formatted.as_str())];
                    toast::info(t_count(cx, "sidebar.rowCount", rows, &vars), cx);
                }
                Some(Err(error)) => toast::error_in(t(cx, "errors.countFailed"), error.message, cx),
                None => {}
            });
        })
        .detach();
    }

    // Menu items that change anything are disabled on read-only connections.
    fn writable(&self) -> bool {
        self.connection.as_ref().is_some_and(|c| !c.read_only)
    }

    fn change_item(this: &WeakEntity<Self>, label: &str, change: Change, writable: bool, cx: &App) -> PopupMenuItem {
        let this = this.clone();
        PopupMenuItem::new(t(cx, label)).disabled(!writable).on_click(move |_, window, cx| {
            let _ = this.update(cx, |tree, cx| tree.open_change(change.clone(), window, cx));
        })
    }

    fn copy_item(name: String, cx: &App) -> PopupMenuItem {
        PopupMenuItem::new(t(cx, "sidebar.copyName"))
            .on_click(move |_, _, cx| Self::copy(name.clone(), t(cx, "toast.copiedClipboard"), cx))
    }

    fn schema_menu(
        &self,
        name: String,
        cx: &mut Context<Self>,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        let this = cx.entity().downgrade();
        let writable = self.writable();
        move |menu, _, cx| {
            let refresh = {
                let (this, name) = (this.clone(), name.clone());
                move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                    let _ = this.update(cx, |tree, cx| tree.refresh_schema(&name, cx));
                }
            };
            let import = {
                let (this, name) = (this.clone(), name.clone());
                move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                    let _ = this.update(cx, |tree, cx| tree.import(name.clone(), None, window, cx));
                }
            };
            let diagram = {
                let (this, name) = (this.clone(), name.clone());
                move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                    let _ = this.update(cx, |tree, cx| tree.open_er_diagram(name.clone(), cx));
                }
            };
            let compare = {
                let (this, name) = (this.clone(), name.clone());
                move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                    let _ = this.update(cx, |tree, cx| tree.open_schema_diff(name.clone(), window, cx));
                }
            };
            menu.item(PopupMenuItem::new(t(cx, "erDiagram.menu")).on_click(diagram))
                .item(PopupMenuItem::new(t(cx, "schemaDiff.menu")).on_click(compare))
                .separator()
                .item(PopupMenuItem::new(t(cx, "sidebar.refresh")).on_click(refresh))
                .item(PopupMenuItem::new(t(cx, "sidebar.importIntoSchema")).disabled(!writable).on_click(import))
                .separator()
                .item(Self::copy_item(name.clone(), cx))
        }
    }

    // The diagram opens in a tab of its own, on the workspace's side.
    pub(crate) fn open_er_diagram(&mut self, schema: String, cx: &mut Context<Self>) {
        if self.connection.is_some() {
            cx.emit(SchemaTreeEvent::OpenDiagram { schema });
        }
    }

    // The sync script opens in a tab of the compared connection.
    fn open_schema_diff(&mut self, schema: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(connection) = self.connection.clone() else { return };
        let this = cx.entity().downgrade();
        crate::schema_diff_view::open(connection, schema, window, cx, move |connection, sql, _, cx| {
            let title = t(cx, "schemaDiff.scriptTitle").to_string();
            let connection = Box::new(connection);
            let _ = this.update(cx, |_, cx| cx.emit(SchemaTreeEvent::OpenScript { connection, sql, title }));
        });
    }

    fn table_menu(
        &self,
        schema_name: String,
        table: TableInfo,
        cx: &mut Context<Self>,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        let this = cx.entity().downgrade();
        let connection = self.connection.clone().unwrap_or_default();
        let writable = self.writable();
        move |menu, window, cx| {
            let driver = &connection.driver;
            let qualified = build_qualified_table(driver, &schema_name, &table.name);
            let kind = ObjectKind::for_relation(&table.kind);
            // Foreign tables and ClickHouse dictionaries list as tables, but take neither rows nor these changes.
            let foreign = matches!(table.kind.to_lowercase().as_str(), "foreign table" | "dictionary");
            let is_table = kind == ObjectKind::Table && !foreign;
            let table_views = is_table && views(driver, ViewScope::Table).next().is_some();
            let object = ObjectRef {
                schema: schema_name.clone(),
                name: table.name.clone(),
                kind: kind.clone(),
                ..Default::default()
            };
            let emit = |event: SchemaTreeEvent| {
                let this = this.clone();
                let event = std::rc::Rc::new(std::cell::Cell::new(Some(event)));
                move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                    if let Some(event) = event.take() {
                        let _ = this.update(cx, |_, cx| cx.emit(event));
                    }
                }
            };
            let ddl = |open: bool| {
                let (this, object) = (this.clone(), object.clone());
                move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                    let _ = this.update(cx, |this, cx| this.with_ddl(object.clone(), open, window, cx));
                }
            };
            let copy = |text: String| {
                move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                    Self::copy(text.clone(), t(cx, "toast.copiedClipboard"), cx)
                }
            };
            let count = {
                let (this, schema, name) = (this.clone(), schema_name.clone(), table.name.clone());
                move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                    let _ = this.update(cx, |tree, cx| tree.count_rows(schema.clone(), name.clone(), cx));
                }
            };
            let import = {
                let (this, schema, name) = (this.clone(), schema_name.clone(), table.name.clone());
                move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                    let _ = this.update(cx, |this, cx| this.import(schema.clone(), Some(name.clone()), window, cx));
                }
            };
            let backup = {
                let (connection, schema, name) = (connection.clone(), schema_name.clone(), table.name.clone());
                move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                    schema_actions::open_backup(connection.clone(), schema.clone(), name.clone(), window, cx);
                }
            };
            let select = SchemaTreeEvent::OpenQuery {
                sql: preview_select(driver, &schema_name, &table.name, 100),
                title: table.name.clone(),
                run: false,
            };
            let browse = SchemaTreeEvent::Browse { schema: schema_name.clone(), table: table.name.clone() };
            let (schema, name) = (schema_name.clone(), table.name.clone());
            let rename = Change::RenameTable { schema: schema.clone(), table: name.clone(), kind: kind.clone() };
            let truncate = Change::Truncate { schema: schema.clone(), table: name.clone() };
            let drop = Change::DropTable { schema, table: name, kind };
            let system_views = {
                let (this, driver, schema, name) =
                    (this.clone(), driver.clone(), schema_name.clone(), table.name.clone());
                move |menu, _: &mut Window, cx: &mut Context<PopupMenu>| {
                    Self::view_items(menu, &this, &driver, Some((&schema, &name)), cx)
                }
            };
            let structure =
                SchemaTreeEvent::OpenStructure { schema: schema_name.clone(), table: Some(table.name.clone()) };
            let structure_label = if writable { "sidebar.editStructure" } else { "sidebar.viewStructure" };
            menu.item(PopupMenuItem::new(t(cx, "sidebar.browseData")).on_click(emit(browse)))
                .when(is_table, |menu| menu.item(PopupMenuItem::new(t(cx, structure_label)).on_click(emit(structure))))
                .item(PopupMenuItem::new(t(cx, "sidebar.selectInNewTab")).on_click(emit(select)))
                .item(PopupMenuItem::new(t(cx, "sidebar.countRows")).on_click(count))
                .when(table_views, |menu| menu.submenu(t(cx, "sidebar.systemViews"), window, cx, system_views))
                .when(is_table, |menu| {
                    menu.separator()
                        .item(PopupMenuItem::new(t(cx, "sidebar.importIntoTable")).disabled(!writable).on_click(import))
                        .item(PopupMenuItem::new(t(cx, "sidebar.backup")).on_click(backup))
                })
                .separator()
                .item(PopupMenuItem::new(t(cx, "sidebar.copyDDL")).on_click(ddl(false)))
                .item(PopupMenuItem::new(t(cx, "sidebar.openDDLInTab")).on_click(ddl(true)))
                .separator()
                .item(
                    PopupMenuItem::new(t(cx, "sidebar.insertName"))
                        .on_click(emit(SchemaTreeEvent::Insert(qualified.clone()))),
                )
                .item(PopupMenuItem::new(t(cx, "sidebar.copyName")).on_click(copy(table.name.clone())))
                .item(PopupMenuItem::new(t(cx, "sidebar.copyQualifiedName")).on_click(copy(qualified.clone())))
                .when(!foreign && rename.supported(driver), |menu| {
                    menu.separator().item(Self::change_item(&this, "sidebar.rename", rename.clone(), writable, cx))
                })
                .when(!foreign, |menu| {
                    menu.separator()
                        .when(is_table, |menu| {
                            menu.item(Self::change_item(&this, "sidebar.truncate", truncate.clone(), writable, cx))
                        })
                        .item(Self::change_item(&this, "sidebar.drop", drop.clone(), writable, cx))
                })
        }
    }

    // One item per system view, run in a new tab. With a table, the views are scoped to it.
    fn view_items(
        mut menu: PopupMenu,
        this: &WeakEntity<Self>,
        driver: &DriverType,
        table: Option<(&str, &str)>,
        cx: &mut Context<PopupMenu>,
    ) -> PopupMenu {
        let scope = if table.is_some() { ViewScope::Table } else { ViewScope::Server };
        let (schema, name) = table.unwrap_or_default();
        for view in views(driver, scope) {
            let label = t(cx, &view.title_key());
            let title = match table {
                Some(_) => t_with(cx, "systemViews.tableTabTitle", &[("view", label.as_ref()), ("name", name)]),
                None => label.clone(),
            };
            let (this, sql, title) = (this.clone(), view.sql(driver, schema, name), title.to_string());
            menu = menu.item(PopupMenuItem::new(label).on_click(move |_, _, cx| {
                let event = SchemaTreeEvent::OpenQuery { sql: sql.clone(), title: title.clone(), run: true };
                let _ = this.update(cx, |_, cx| cx.emit(event));
            }));
        }
        menu
    }

    // A view's columns only offer Insert and Copy. SQLite and ClickHouse won't drop a key column.
    fn column_menu(
        &self,
        schema: String,
        table: String,
        column: String,
        view: bool,
        primary: bool,
        cx: &mut Context<Self>,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        let this = cx.entity().downgrade();
        let writable = self.writable();
        let driver = self.connection.as_ref().map(|c| c.driver.clone()).unwrap_or_default();
        let droppable = !(primary && matches!(driver, DriverType::Sqlite | DriverType::Turso | DriverType::ClickHouse));
        move |menu, _, cx| {
            let insert = {
                let (this, name) = (this.clone(), column.clone());
                move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                    let _ = this.update(cx, |_, cx| cx.emit(SchemaTreeEvent::Insert(name.clone())));
                }
            };
            let menu = menu
                .item(PopupMenuItem::new(t(cx, "sidebar.insertName")).on_click(insert))
                .item(Self::copy_item(column.clone(), cx));
            if view {
                return menu;
            }
            let (schema, table, column) = (schema.clone(), table.clone(), column.clone());
            let rename = Change::RenameColumn { schema: schema.clone(), table: table.clone(), column: column.clone() };
            let drop = Change::DropColumn { schema, table, column };
            menu.separator()
                .item(Self::change_item(&this, "sidebar.rename", rename, writable, cx))
                .when(droppable, |menu| {
                    menu.separator().item(Self::change_item(&this, "sidebar.drop", drop, writable, cx))
                })
        }
    }

    // A primary key's index goes with its constraint, so only the constraint offers Drop.
    fn object_menu(
        &self,
        row: &ObjectRow,
        schema: String,
        table: Option<String>,
        cx: &mut Context<Self>,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        let this = cx.entity().downgrade();
        let writable = self.writable();
        let driver = self.connection.as_ref().map(|c| c.driver.clone()).unwrap_or_default();
        let object = row.object.clone();
        let constraint = match row.badge {
            Some(Badge::Pk) => ConstraintKind::PrimaryKey,
            Some(Badge::Fk) => ConstraintKind::ForeignKey,
            Some(Badge::Unique) => ConstraintKind::Unique,
            Some(Badge::Check) => ConstraintKind::Check,
            _ => ConstraintKind::Other,
        };
        let target = ObjectRef { schema, table: table.unwrap_or_default(), ..object.clone() };
        let drop = Change::DropObject { object: target, constraint };
        let primary_index = object.kind == ObjectKind::Index && row.badge == Some(Badge::Pk);
        let has_ddl = !(primary_index && driver == DriverType::ClickHouse);
        let drop = (!primary_index && drop.supported(&driver)).then_some(drop);
        move |menu, _, cx| {
            let ddl = |open: bool| {
                let (this, object) = (this.clone(), object.clone());
                move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                    let _ = this.update(cx, |this, cx| this.with_ddl(object.clone(), open, window, cx));
                }
            };
            menu.when(has_ddl, |menu| {
                menu.item(PopupMenuItem::new(t(cx, "sidebar.copyDDL")).on_click(ddl(false)))
                    .item(PopupMenuItem::new(t(cx, "sidebar.openDDLInTab")).on_click(ddl(true)))
                    .separator()
            })
            .item(Self::copy_item(object.name.clone(), cx))
            .when_some(drop.clone(), |menu, drop| {
                menu.separator().item(Self::change_item(&this, "sidebar.drop", drop, writable, cx))
            })
        }
    }

    fn focusable(row: &TreeRow) -> bool {
        !matches!(row.kind, RowKind::Note { .. })
    }

    // None for a row that doesn't expand.
    fn open_state(kind: &RowKind) -> Option<bool> {
        match kind {
            RowKind::Schema { expanded, .. } | RowKind::Table { expanded, .. } | RowKind::Group { expanded, .. } => {
                Some(*expanded)
            }
            _ => None,
        }
    }

    fn cursor_index(&self) -> Option<usize> {
        let id = self.cursor.as_ref()?;
        self.rows.iter().position(|row| &row.id == id)
    }

    fn put_cursor(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.cursor = Some(self.rows[ix].id.clone());
        self.keyed = true;
        self.scroll.scroll_to_item(ix, ScrollStrategy::Nearest);
        cx.notify();
    }

    // Right opens a closed row, then steps onto its first child.
    fn expand_cursor(&mut self, cx: &mut Context<Self>) {
        let Some(ix) = self.cursor_index() else { return };
        let (kind, depth) = (self.rows[ix].kind.clone(), self.rows[ix].depth);
        match Self::open_state(&kind) {
            Some(false) => self.activate(kind, cx),
            Some(true) if self.rows.get(ix + 1).is_some_and(|next| next.depth > depth && Self::focusable(next)) => {
                self.put_cursor(ix + 1, cx)
            }
            _ => {}
        }
    }

    // Left closes an open row, or steps up to its parent. A row a search keeps open can't close.
    fn collapse_cursor(&mut self, cx: &mut Context<Self>) {
        let Some(ix) = self.cursor_index() else { return };
        let (kind, depth) = (self.rows[ix].kind.clone(), self.rows[ix].depth);
        let kept_open =
            matches!(kind, RowKind::Schema { kept_open: true, .. } | RowKind::Table { kept_open: true, .. });
        if Self::open_state(&kind) == Some(true) && !kept_open {
            return self.activate(kind, cx);
        }
        if let Some(parent) = self.rows[..ix].iter().rposition(|row| row.depth < depth) {
            self.put_cursor(parent, cx);
        }
    }

    // Closes and forgets every open row of the shown connection.
    fn collapse_all(&mut self, cx: &mut Context<Self>) {
        let Some(conn) = &self.connection else { return };
        let prefix = format!("{}:", conn.id);
        self.expanded_schemas.retain(|key, _| !key.starts_with(&prefix));
        self.expanded_tables.retain(|key, _| !key.starts_with(&prefix));
        self.expanded_groups.retain(|key| !key.starts_with(&prefix));
        set_setting_json(cx, SCHEMA_EXPANDED_KEY, &self.expanded_schemas);
        set_setting_json(cx, TABLES_EXPANDED_KEY, &self.expanded_tables);
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
        self.rebuild(cx);
    }

    // Includes tables hidden under a closed schema.
    fn has_open(&self) -> bool {
        let Some(conn) = &self.connection else { return false };
        let prefix = format!("{}:", conn.id);
        let schemas_and_tables = self.expanded_schemas.iter().chain(&self.expanded_tables);
        schemas_and_tables.filter(|(_, open)| **open).any(|(key, _)| key.starts_with(&prefix))
            || self.expanded_groups.iter().any(|key| key.starts_with(&prefix))
    }

    // With no cursor yet, Down starts at the top and Up at the bottom.
    fn move_cursor(&mut self, step: isize, cx: &mut Context<Self>) {
        let focusable: Vec<usize> = (0..self.rows.len()).filter(|&ix| Self::focusable(&self.rows[ix])).collect();
        let (Some(&first), Some(&last)) = (focusable.first(), focusable.last()) else { return };
        let current = self.cursor.as_ref().and_then(|id| focusable.iter().position(|&ix| &self.rows[ix].id == id));
        let target = match (current, step) {
            (_, isize::MIN) => first,
            (_, isize::MAX) => last,
            (None, step) if step > 0 => first,
            (None, _) => last,
            (Some(at), step) => focusable[(at as isize + step).clamp(0, focusable.len() as isize - 1) as usize],
        };
        self.put_cursor(target, cx);
    }

    fn activate_cursor(&mut self, cx: &mut Context<Self>) {
        if let Some(ix) = self.cursor_index() {
            self.activate(self.rows[ix].kind.clone(), cx);
        }
    }

    fn activate(&mut self, kind: RowKind, cx: &mut Context<Self>) {
        match kind {
            RowKind::Schema { name, .. } => self.toggle_schema(&name, cx),
            RowKind::Table { schema, table, .. } => self.toggle_table(&schema, &table.name, cx),
            RowKind::Column { column, .. } => cx.emit(SchemaTreeEvent::Insert(column.name)),
            RowKind::Group { key, group, schema, table, .. } => self.toggle_group(key, group, schema, table, cx),
            RowKind::Object { .. } | RowKind::Note { .. } => {}
        }
    }

    fn render_rows(&mut self, range: Range<usize>, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let rows = self.rows.get(range).map(<[TreeRow]>::to_vec).unwrap_or_default();
        let focused = self.focus.is_focused(window);
        let clearance = self.clearance.get(&self.scroll, rems(LIST_INSET.0 + BROWSE_INSET.0), window);
        rows.into_iter()
            .map(|row| {
                let selected = self.cursor.as_ref() == Some(&row.id);
                self.render_row(row, selected, focused && self.keyed && selected, clearance, cx)
            })
            .collect()
    }

    // A nested row sits inside its parent's indent, so its hover background starts there. `selected` marks the
    // cursor's row, and `clearance` moves a table's Browse button clear of the tree's bar.
    fn render_row(
        &self,
        row: TreeRow,
        selected: bool,
        ring: bool,
        clearance: Pixels,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let muted = theme.muted_foreground;
        // Every chevron takes the larger one's width, so the indent guides run under their centres.
        let chevron = |open: bool, size: Rems| {
            h_flex()
                .flex_none()
                .w(ICON_SM)
                .justify_center()
                .child(Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight }).size(size))
        };
        let icon = |name: Lucide| Icon::new(name).size(ICON_SM).text_color(muted);
        let row_id = row.id.clone();
        let selector = format!("tree:{}", scenario_id(&row.id, self.connection.as_ref()));
        let depth = row.depth;
        let base = h_flex()
            .id(row.id.clone())
            .debug_selector(move || selector)
            .relative()
            .h(rems(ROW_HEIGHT_REM))
            .w_full()
            .px(ROW_PADDING)
            .gap(rems(0.308))
            .rounded(RADIUS)
            .text_size(TEXT_BASE)
            .overflow_hidden()
            .when(selected, |el| el.bg(theme.sidebar_accent))
            .when(Self::focusable(&row), |el| {
                // A right-click rings its row, as a key move does, so it's clear which one the menu is for.
                let (left, right) = (row_id.clone(), row_id);
                el.on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, window, cx| {
                        this.cursor = Some(left.clone());
                        this.keyed = false;
                        this.focus.focus(window, cx);
                        cx.notify();
                    }),
                )
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, _, window, cx| {
                        this.cursor = Some(right.clone());
                        this.keyed = true;
                        this.focus.focus(window, cx);
                        cx.notify();
                    }),
                )
            });
        let hover = theme.sidebar_accent;
        let clickable = |el: Stateful<Div>| el.cursor_pointer().hover(|s| s.bg(hover));
        let column_row = |el: Stateful<Div>| el.text_size(TEXT_SM).gap(rems(0.462));
        let mono = |el: Div| el.font_family(theme.mono_font_family.clone());
        let badge = |text: SharedString, color: Hsla| form::badge(text, color);
        let count = |text: String| div().flex_none().text_size(TEXT_2XS).text_color(muted).child(text);
        // A search marks what it matched in names and types, so the rows it keeps read as they do unsearched.
        let highlight = HighlightStyle {
            color: Some(theme.primary),
            background_color: Some(theme.primary.opacity(TINT)),
            font_weight: Some(FontWeight::MEDIUM),
            ..Default::default()
        };
        let marked = |text: SharedString, needle: &str| {
            let ranges = match_ranges(&text, needle);
            StyledText::new(text).with_highlights(ranges.into_iter().map(|range| (range, highlight)))
        };
        let element =
            match row.kind {
                RowKind::Schema { name, expanded, count: tables, .. } => {
                    let menu = self.schema_menu(name.clone(), cx);
                    clickable(base)
                        .child(chevron(expanded, ICON_SM))
                        .child(icon(Lucide::FolderOpen))
                        .child(div().flex_1().min_w_0().truncate().child(name.clone()))
                        .children(tables.map(count))
                        .on_click(cx.listener(move |this, _, _, cx| this.toggle_schema(&name, cx)))
                        .context_menu(menu)
                        .into_any_element()
                }
                RowKind::Table { schema, table, expanded, .. } => {
                    let (relation_icon, relation_badge) = relation_look(&ObjectKind::for_relation(&table.kind));
                    let active = self.active_table.as_ref().is_some_and(|active| {
                        self.connection.as_ref().is_some_and(|c| c.id == active.connection_id)
                            && active.schema == schema
                            && active.table == table.name
                    });
                    let menu = self.table_menu(schema.clone(), table.clone(), cx);
                    let name = table.name.clone();
                    let group = SharedString::from(format!("schema-table-{}-{}", schema, table.name));
                    let browse = self.browse_action(&schema, &table.name, group.clone(), ring, clearance, cx);
                    clickable(base)
                        .group(group)
                        .child(chevron(expanded, ICON_SM))
                        .child(Icon::new(relation_icon).size(ICON_SM).text_color(if relation_badge.is_some() {
                            theme.primary
                        } else {
                            muted
                        }))
                        .child(div().flex_1().min_w_0().truncate().child(marked(name.clone().into(), &self.needle)))
                        .children(relation_badge.map(|key| badge(t(cx, key), theme.primary)))
                        .when(active, |el| {
                            el.child(
                                div()
                                    .debug_selector(|| "open-table".into())
                                    .flex_none()
                                    .size(rems(0.462))
                                    .rounded_full()
                                    .bg(theme.primary),
                            )
                        })
                        .child(browse)
                        .on_click(cx.listener(move |this, _, _, cx| this.toggle_table(&schema, &name, cx)))
                        .context_menu(menu)
                        .into_any_element()
                }
                RowKind::Column { column, schema, table, view } => {
                    let menu = self.column_menu(schema, table, column.name.clone(), view, column.is_primary, cx);
                    let tip = column.clone();
                    let dialect = self.connection.as_ref().and_then(|c| c.driver.dialect());
                    let icon_color = match (column.is_primary, column.is_foreign) {
                        (true, _) => theme.warning,
                        (false, true) => theme.primary,
                        _ => muted,
                    };
                    column_row(clickable(base))
                        .child(Icon::new(column_icon(&column, dialect)).size(ICON_SM).text_color(icon_color))
                        .child(
                            mono(div())
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(theme.foreground)
                                .child(marked(column.name.clone().into(), &self.needle)),
                        )
                        .child(mono(div()).flex_none().text_size(TEXT_2XS).text_color(muted).child(marked(
                            type_label(&column, dialect).into(),
                            type_needle(&column.name, &self.needle),
                        )))
                        .tooltip(move |window, cx| {
                            let column = tip.clone();
                            Tooltip::element(move |_, cx| column_tip(&column, cx)).build(window, cx)
                        })
                        .context_menu(menu)
                        .into_any_element()
                }
                RowKind::Group { key, group, schema, table, expanded, count: objects } => clickable(base)
                    .text_size(TEXT_SM)
                    .text_color(muted)
                    .child(chevron(expanded, ICON_XS))
                    .child(icon(group_icon(group)))
                    .child(div().flex_1().min_w_0().truncate().child(t(cx, &format!("sidebar.group.{}", group.key()))))
                    .children(objects.map(|n| count(n.to_string())))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.toggle_group(key.clone(), group, schema.clone(), table.clone(), cx)
                    }))
                    .into_any_element(),
                RowKind::Object { row, schema, table } => {
                    let menu = self.object_menu(&row, schema, table, cx);
                    let badge_color = match row.badge {
                        Some(Badge::Pk) => theme.warning,
                        Some(Badge::Fk) => theme.primary,
                        Some(Badge::Unique) => theme.success,
                        _ => muted,
                    };
                    // The group's row already shows its icon, and every index would say IDX, so neither repeats here.
                    let shown_badge = row.badge.filter(|badge| *badge != Badge::Index);
                    column_row(base)
                        .hover(|s| s.bg(hover))
                        .child(mono(div()).flex_shrink_0().max_w(relative(0.7)).truncate().child(row.label.clone()))
                        .children(shown_badge.map(|b| badge(t(cx, &format!("sidebar.badge.{}", b.key())), badge_color)))
                        .child(
                            mono(div())
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(TEXT_2XS)
                                .text_color(muted)
                                .child(row.detail.clone()),
                        )
                        .context_menu(menu)
                        .into_any_element()
                }
                RowKind::Note { text, spinner } => column_row(base)
                    .text_color(muted)
                    .when(spinner, |el| el.child(Spinner::new(ICON_XS)))
                    .child(div().min_w_0().truncate().child(text))
                    .into_any_element(),
            };
        let element = div().w_full().child(element).when(ring, |el| list_nav::ring(el, cx));
        // Each open parent's guide runs down from its chevron, past the rows it holds.
        let guide_left = |level: usize| rems(INDENT_REM * level as f32 + ROW_PADDING.0 + ICON_SM.0 / 2.);
        let guides = (0..depth)
            .map(|level| div().absolute().top_0().bottom_0().left(guide_left(level)).w(px(1.)).bg(theme.border));
        div().relative().w_full().pl(rems(INDENT_REM * depth as f32)).children(guides).child(element).into_any_element()
    }

    // Sits over the row's end on a gradient that fades out the truncated name.
    // Shown on hover or with the keyboard ring.
    fn browse_action(
        &self,
        schema: &str,
        table: &str,
        group: SharedString,
        ring: bool,
        clearance: Pixels,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let (hover, elevated) = (theme.sidebar_accent, theme.secondary);
        let (muted, foreground) = (theme.muted_foreground, theme.foreground);
        let id = SharedString::from(format!("browse-{schema}-{table}"));
        let action_group = SharedString::from(format!("{id}-action"));
        let (browse_schema, browse_table) = (schema.to_string(), table.to_string());
        let selector = id.to_string();
        h_flex()
            .absolute()
            .right(BROWSE_INSET)
            .pr(clearance)
            .top_0()
            .bottom_0()
            .invisible()
            .group_hover(group, |style| style.visible())
            .when(ring, |el| el.visible())
            .child(div().w(rems(1.231)).h(rems(1.385)).bg(linear_gradient(
                90.,
                linear_color_stop(hover.opacity(0.), 0.),
                linear_color_stop(hover, 1.),
            )))
            .child(
                div()
                    .id(id)
                    .group(action_group.clone())
                    .debug_selector(move || selector)
                    .flex()
                    .items_center()
                    .justify_center()
                    .py(rems(0.231))
                    .px(rems(0.308))
                    .rounded(RADIUS_SM)
                    .bg(hover)
                    .hover(|style| style.bg(elevated))
                    .child(
                        svg()
                            .path(Lucide::Eye.path())
                            .size(ICON_XS)
                            .text_color(muted)
                            .group_hover(action_group, |style| style.text_color(foreground)),
                    )
                    .tooltip({
                        let hint = t(cx, "sidebar.browseData");
                        move |window, cx| Tooltip::new(hint.clone()).build(window, cx)
                    })
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.stop_propagation();
                        cx.emit(SchemaTreeEvent::Browse { schema: browse_schema.clone(), table: browse_table.clone() });
                    })),
            )
    }

    fn filter_bar(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let enabled = self.connected(cx);
        let loading = self.connection.as_ref().is_some_and(|c| schema::get(cx, &c.id).is_some_and(|d| d.loading()));
        let field = div()
            .debug_selector(|| "schema-search".into())
            .child(form::filter_input(&self.search, window, cx).cleanable(true).disabled(!enabled));
        form::filter_bar(field, cx)
            .child(
                form::filter_button("refresh-schema", Icon::new(Lucide::RefreshCw))
                    .debug_selector(|| "refresh-schema".into())
                    .tooltip(t(cx, "sidebar.refreshSchema"))
                    .disabled(self.connection.is_none() || loading)
                    .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
            )
            .child(self.options_menu(enabled, cx))
    }

    fn options_menu(&self, enabled: bool, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let this = cx.entity().downgrade();
        let (open, writable) = (self.has_open(), self.writable());
        let driver = self.connection.as_ref().map(|c| c.driver.clone()).unwrap_or_default();
        let server_views = views(&driver, ViewScope::Server).next().is_some();
        form::filter_button("schema-options", Icon::new(Lucide::SlidersHorizontal))
            .debug_selector(|| "schema-options".into())
            .tooltip(t(cx, "sidebar.schemaOptions"))
            .disabled(!enabled)
            .dropdown_menu(move |menu: PopupMenu, window, cx| {
                let (collapse, import, views_for) = (this.clone(), this.clone(), this.clone());
                let driver = driver.clone();
                menu.item(
                    PopupMenuItem::new(t(cx, "sidebar.collapseAll"))
                        .icon(Icon::new(Lucide::ChevronsDownUp))
                        .disabled(!open)
                        .on_click(move |_, _, cx| {
                            let _ = collapse.update(cx, |tree, cx| tree.collapse_all(cx));
                        }),
                )
                .separator()
                .item(
                    PopupMenuItem::new(t(cx, "sidebar.importData"))
                        .icon(Icon::new(Lucide::Upload))
                        .disabled(!writable)
                        .on_click(move |_, window, cx| {
                            let _ = import.update(cx, |tree, cx| tree.import_default(window, cx));
                        }),
                )
                .when(server_views, |menu| {
                    let label = t(cx, "sidebar.serverViews");
                    menu.submenu_with_icon(Some(Icon::new(Lucide::Activity)), label, window, cx, move |menu, _, cx| {
                        Self::view_items(menu, &views_for, &driver, None, cx)
                    })
                })
            })
    }

    fn error_banner(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let error = self.error.clone()?;
        let theme = cx.theme();
        let copy_text = error.clone();
        Some(
            form::alert(theme.danger)
                .flex()
                .flex_col()
                .mx_2()
                .mb_1()
                .gap_1()
                .child(
                    h_flex()
                        .gap_1p5()
                        .text_size(TEXT_XS)
                        .child(Icon::new(Lucide::CircleAlert).size(ICON_XS).text_color(theme.danger))
                        .child(div().flex_1().font_semibold().child(t(cx, "errors.generic")))
                        .child(
                            Button::new("copy-schema-error")
                                .ghost()
                                .xsmall()
                                .icon(Icon::new(Lucide::Copy))
                                .tooltip(t(cx, "common.copy"))
                                .on_click(move |_, _, cx| {
                                    Self::copy(copy_text.clone(), t(cx, "toast.copiedClipboard"), cx)
                                }),
                        ),
                )
                .child(div().text_size(TEXT_XS).text_color(theme.muted_foreground).child(error)),
        )
    }

    fn message(&self, icon: Option<Lucide>, text: SharedString, cx: &App) -> Div {
        form::empty_state(icon.map(Icon::new), text, cx)
    }
}

fn note(parent: &str, depth: usize, text: SharedString, spinner: bool) -> TreeRow {
    TreeRow { id: format!("n:{parent}").into(), depth, kind: RowKind::Note { text, spinner } }
}

// Tooltip lines: FK target, NOT NULL and default as the hover card words them, auto-generated or computed, and
// enum values.
fn column_details(column: &ColumnInfo, cx: &App) -> Vec<String> {
    let labels = cx.global::<I18n>().sql_labels();
    let flag = |on: bool, key: &str| if on { t(cx, key).to_string() } else { String::new() };
    let mut lines = vec![hover_card::joined([
        hover_card::column_note(column, &labels, cx),
        flag(column.is_identity, "sidebar.columnTip.identity"),
        flag(column.is_computed, "sidebar.columnTip.computed"),
    ])];
    if !column.enum_values.is_empty() {
        let mut values = column.enum_values.iter().take(TIP_VALUES).map(String::as_str).collect::<Vec<_>>().join(", ");
        if column.enum_values.len() > TIP_VALUES {
            values.push_str(", …");
        }
        lines.push(t_with(cx, "sidebar.columnTip.values", &[("values", &values)]).to_string());
    }
    lines.retain(|line| !line.is_empty());
    lines
}

// Full name and type, which the row may truncate, then the details.
fn column_tip(column: &ColumnInfo, cx: &App) -> Div {
    let theme = cx.theme();
    v_flex()
        .py(rems(0.231))
        .gap(rems(0.154))
        .child(
            h_flex()
                .gap(rems(0.462))
                .font_family(theme.mono_font_family.clone())
                .child(div().font_semibold().child(column.name.clone()))
                .child(div().text_color(theme.muted_foreground).child(column.data_type.clone())),
        )
        .children(
            column_details(column, cx)
                .into_iter()
                .map(|line| div().text_size(TEXT_XS).text_color(theme.muted_foreground).child(line)),
        )
}

// Each place a search's needle shows up in `text`, as byte ranges of `text`. The needle comes lowercased, and a
// letter can lowercase to more bytes, so each lowercased byte keeps the span of the letter it came from.
fn match_ranges(text: &str, needle: &str) -> Vec<Range<usize>> {
    if needle.is_empty() {
        return Vec::new();
    }
    let (mut lower, mut spans) = (String::with_capacity(text.len()), Vec::with_capacity(text.len()));
    for (at, letter) in text.char_indices() {
        for lowered in letter.to_lowercase() {
            lower.push(lowered);
            spans.extend(std::iter::repeat_n(at..at + letter.len_utf8(), lowered.len_utf8()));
        }
    }
    lower.match_indices(needle).map(|(at, found)| spans[at].start..spans[at + found.len() - 1].end).collect()
}

// What a column's type is marked with: the needle, unless its name already shows why the column is listed.
fn type_needle<'a>(name: &str, needle: &'a str) -> &'a str {
    if match_ranges(name, needle).is_empty() { needle } else { "" }
}

// Keys look as they do in the ER diagram and the grid's headers: a gold key, a blue link. Other columns show what
// their type holds, or a list when they take their values from a fixed one.
fn column_icon(column: &ColumnInfo, dialect: Option<SqlDialect>) -> Lucide {
    if column.is_primary {
        return Lucide::KeyRound;
    }
    if column.is_foreign {
        return Lucide::Link;
    }
    if !column.enum_values.is_empty() {
        return Lucide::List;
    }
    match type_family(dialect, &column.data_type) {
        TypeFamily::Number => Lucide::Hash,
        TypeFamily::Text => Lucide::CaseSensitive,
        TypeFamily::Boolean => Lucide::ToggleLeft,
        TypeFamily::Date => Lucide::Calendar,
        TypeFamily::Time => Lucide::Clock,
        TypeFamily::DateTime => Lucide::CalendarClock,
        TypeFamily::Json => Lucide::Braces,
        TypeFamily::Uuid => Lucide::FingerprintPattern,
        TypeFamily::Binary => Lucide::Binary,
        TypeFamily::Array => Lucide::Brackets,
        TypeFamily::Enum => Lucide::List,
        TypeFamily::Other => Lucide::Columns3,
    }
}

// A nullable column's type ends in `?`, unless the type says so itself, as ClickHouse's Nullable(…) does.
fn type_label(column: &ColumnInfo, dialect: Option<SqlDialect>) -> String {
    let shown = display_type(dialect, &column.data_type);
    if column.is_nullable && !shown.contains("Nullable(") { format!("{shown}?") } else { shown }
}

// A materialized view stores its rows, so it doesn't pass for a view.
fn relation_look(kind: &ObjectKind) -> (Lucide, Option<&'static str>) {
    match kind {
        ObjectKind::View => (Lucide::View, Some("sidebar.badge.view")),
        ObjectKind::MaterializedView => (Lucide::Layers, Some("sidebar.badge.matview")),
        _ => (Lucide::Table2, None),
    }
}

// Indexes don't take `#`, which marks number columns.
fn group_icon(group: Group) -> Lucide {
    match group {
        Group::Indexes => Lucide::ListTree,
        Group::Constraints => Lucide::KeyRound,
        Group::Triggers => Lucide::Zap,
        Group::Routines => Lucide::SquareFunction,
    }
}

impl Render for SchemaTree {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(conn) = self.connection.clone() else {
            return v_flex().size_full().child(self.message(
                Some(Lucide::Database),
                t(cx, "sidebar.addConnectionFirst"),
                cx,
            ));
        };
        let data = schema::get(cx, &conn.id);
        let loading = data.is_some_and(|d| d.loading());
        let schemas_empty = data.and_then(|d| d.schemas()).is_some_and(<[_]>::is_empty);
        let body = if loading {
            h_flex()
                .px_3()
                .py_2()
                .gap_2()
                .text_size(TEXT_SM)
                .text_color(cx.theme().muted_foreground)
                .child(Spinner::new(ICON_SM))
                .child(t(cx, "sidebar.loadingSchemasTables"))
                .into_any_element()
        } else if !self.connected(cx) {
            self.message(Some(Lucide::Plug), t(cx, "sidebar.notConnected"), cx)
                .child(
                    Button::new("connect")
                        .primary()
                        .small()
                        .icon(Icon::new(Lucide::Plug))
                        .label(t(cx, "sidebar.connectButton"))
                        .disabled(self.connecting)
                        .on_click(cx.listener(|this, _, _, cx| this.connect(cx))),
                )
                .into_any_element()
        } else if schemas_empty && self.error.is_none() {
            self.message(None, t(cx, "sidebar.noSchemasRefresh"), cx).into_any_element()
        } else if self.rows.is_empty() && !self.needle.is_empty() {
            self.message(None, t(cx, "sidebar.noMatches"), cx).into_any_element()
        } else {
            div()
                .key_context(CONTEXT)
                .track_focus(&self.focus)
                .relative()
                .size_full()
                .on_action(cx.listener(|this, _: &CursorUp, _, cx| this.move_cursor(-1, cx)))
                .on_action(cx.listener(|this, _: &CursorDown, _, cx| this.move_cursor(1, cx)))
                .on_action(cx.listener(|this, _: &CursorFirst, _, cx| this.move_cursor(isize::MIN, cx)))
                .on_action(cx.listener(|this, _: &CursorLast, _, cx| this.move_cursor(isize::MAX, cx)))
                .on_action(cx.listener(|this, _: &ActivateRow, _, cx| this.activate_cursor(cx)))
                .on_action(cx.listener(|this, _: &CollapseRow, _, cx| this.collapse_cursor(cx)))
                .on_action(cx.listener(|this, _: &ExpandRow, _, cx| this.expand_cursor(cx)))
                .child(
                    uniform_list("schema-tree", self.rows.len(), cx.processor(Self::render_rows))
                        .debug_selector(|| "schema-tree".into())
                        .track_scroll(&self.scroll)
                        .size_full()
                        .px(LIST_INSET)
                        .pb(LIST_INSET),
                )
                .child(self.clearance.watch(self.scroll.clone()))
                .hover_scrollbar(&self.scroll, ScrollbarAxis::Vertical)
                .into_any_element()
        };
        v_flex()
            .size_full()
            .child(self.filter_bar(window, cx))
            .children(self.error_banner(cx))
            .child(div().flex_1().min_h_0().child(body))
    }
}

// Strips the connection id that group and object row ids carry.
fn scenario_id(id: &str, connection: Option<&ConnectionConfig>) -> String {
    connection.map_or_else(|| id.to_string(), |c| id.replacen(&format!("{}:", c.id), "", 1))
}

#[cfg(any(test, feature = "snapshot"))]
impl SchemaTree {
    // Each row as (scenario id, label, extra), where extra is the kind, badge or count.
    pub(crate) fn listed(&self) -> Vec<(String, String, String)> {
        self.rows
            .iter()
            .map(|row| {
                let (label, extra) = match &row.kind {
                    RowKind::Schema { name, .. } => (name.clone(), String::new()),
                    RowKind::Table { table, .. } => (table.name.clone(), table.kind.clone()),
                    RowKind::Column { column, .. } => (column.name.clone(), column.data_type.clone()),
                    RowKind::Group { group, count, .. } => {
                        (group.key().to_string(), count.map(|c| c.to_string()).unwrap_or_default())
                    }
                    RowKind::Object { row, .. } => {
                        (row.label.clone(), format!("{} {}", row.badge.map_or("", |b| b.key()), row.detail))
                    }
                    RowKind::Note { text, .. } => (text.to_string(), "note".into()),
                };
                (scenario_id(&row.id, self.connection.as_ref()), label, extra)
            })
            .collect()
    }
}

#[cfg(test)]
impl SchemaTree {
    pub(crate) fn says_no_matches(&self) -> bool {
        self.rows.is_empty() && !self.needle.is_empty()
    }

    #[cfg(feature = "e2e")]
    pub(crate) fn reveal(&mut self, id: &str, cx: &mut Context<Self>) {
        let connection = self.connection.as_ref();
        if let Some(ix) = self.rows.iter().position(|row| scenario_id(&row.id, connection) == id) {
            self.scroll.scroll_to_item(ix, ScrollStrategy::Nearest);
            cx.notify();
        }
    }
}

// For the README screenshots.
#[cfg(feature = "snapshot")]
impl SchemaTree {
    pub(crate) fn search_for(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |search, cx| search.replace_all(text.to_string(), window, cx));
    }

    pub(crate) fn expand_group(&mut self, schema: &str, table: &str, group: Group, cx: &mut Context<Self>) {
        let Some(conn) = &self.connection else { return };
        let key = group_key(&conn.id, schema, table, group);
        if !self.expanded_groups.contains(&key) {
            self.toggle_group(key, group, schema.to_string(), Some(table.to_string()), cx);
        }
    }

    pub(crate) fn open_table_ddl(&mut self, schema: &str, table: &str, window: &mut Window, cx: &mut Context<Self>) {
        let object =
            ObjectRef { schema: schema.into(), name: table.into(), kind: ObjectKind::Table, ..Default::default() };
        self.with_ddl(object, true, window, cx);
    }

    pub(crate) fn import_into(&mut self, schema: &str, table: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.import(schema.to_string(), Some(table.to_string()), window, cx);
    }
}

#[cfg(test)]
mod tests;
