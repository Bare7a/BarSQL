// Needs the compose databases. Start them with `cargo xtask e2e up`, then run
// `cargo test -p barsql-ui --features e2e engines`.
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use barsql_core::{ConnectionConfig, DriverType};
use barsql_sql::plan::NOTE_NO_METRICS;
use barsql_sql::system_views::{SystemView, ViewScope, views};
use gpui_kit::{Entity, Modifiers, TestAppContext};

use super::driver::{Driver, open_with, selector};
use crate::cell_content::Kind;
use crate::cell_viewer::Opened;
use crate::file_dialogs::Stub;
use crate::grid::Grid;
use crate::i18n::t;
use crate::table_tab::TableTab;

// In a table's menu, after Browse data, SELECT in new tab and Count rows.
const SYSTEM_VIEWS: usize = 3;

#[derive(Clone, Copy, PartialEq)]
enum Engine {
    Postgres,
    MySql,
    MariaDb,
    Turso,
    ClickHouse,
    // The mssql profile's server. Only with BARSQL_E2E_MSSQL=1.
    SqlServer,
}

impl Engine {
    fn env(self, key: &str, default: &str) -> String {
        let prefix = match self {
            Self::Postgres => "PG",
            Self::MySql => "MYSQL",
            Self::MariaDb => "MARIADB",
            Self::Turso => "TURSO",
            Self::ClickHouse => "CH",
            Self::SqlServer => "MSSQL",
        };
        std::env::var(format!("BARSQL_E2E_{prefix}_{key}")).unwrap_or_else(|_| default.to_string())
    }

    fn driver(self) -> DriverType {
        match self {
            Self::Postgres => DriverType::Postgres,
            Self::MySql | Self::MariaDb => DriverType::MySql,
            Self::Turso => DriverType::Turso,
            Self::ClickHouse => DriverType::ClickHouse,
            Self::SqlServer => DriverType::SqlServer,
        }
    }

    // The driver's row in the dialog's menu, which lists the supported drivers in order.
    fn menu_index(self) -> usize {
        let driver = self.driver();
        DriverType::KNOWN.iter().filter(|d| d.is_supported()).position(|d| *d == driver).expect("listed")
    }

    // Keep in sync with the app's e2e harness.
    fn config(self) -> ConnectionConfig {
        let (port, user, password) = match self {
            Self::Postgres => ("55432", "postgres", "postgres"),
            Self::MySql => ("33306", "root", "root"),
            Self::MariaDb => ("33307", "root", "root"),
            Self::ClickHouse => ("38123", "default", "clickhouse"),
            Self::SqlServer => ("31433", "sa", "BarSQL-e2e-Passw0rd"),
            Self::Turso => {
                return ConnectionConfig {
                    name: "E2E Turso".into(),
                    driver: self.driver(),
                    url: self.env("URL", "http://127.0.0.1:38080"),
                    auth_token: self.env("TOKEN", ""),
                    color: "#3b82f6".into(),
                    ..Default::default()
                };
            }
        };
        let name = match self {
            Self::Postgres => "E2E Postgres",
            Self::MySql => "E2E MySQL",
            Self::MariaDb => "E2E MariaDB",
            Self::ClickHouse => "E2E ClickHouse",
            Self::SqlServer => "E2E SQL Server",
            Self::Turso => unreachable!("returned above"),
        };
        ConnectionConfig {
            name: name.into(),
            driver: self.driver(),
            host: self.env("HOST", "127.0.0.1"),
            port: self.env("PORT", port).parse().expect("port"),
            database: self.env("DB", "barsql_test"),
            username: self.env("USER", user),
            password: self.env("PASSWORD", password),
            // SQL Server encrypts with the certificate it made itself.
            ssl_mode: if self == Self::SqlServer { "require".into() } else { "disable".into() },
            color: "#3b82f6".into(),
            ..Default::default()
        }
    }

    // SQL Server runs only with its compose profile, and starts without barsql_test.
    fn ready(self) -> bool {
        if self != Self::SqlServer {
            return true;
        }
        if std::env::var("BARSQL_E2E_MSSQL").as_deref() != Ok("1") {
            return false;
        }
        // Once, with the other tests waiting: switching the snapshot on restarts the database, failing their logins.
        static PREPARED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
        PREPARED.get_or_init(|| prepare_mssql(&self.config()));
        true
    }
}

fn prepare_mssql(config: &ConnectionConfig) {
    let master = ConnectionConfig { database: "master".into(), ..config.clone() };
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let engine = barsql_db::Engine::connect(&master).await.expect("SQL Server is reachable");
        let mut session = engine.session().await.unwrap();
        let cancel = barsql_db::Cancel::new();
        let create = format!("IF DB_ID(N'{0}') IS NULL CREATE DATABASE [{0}]", config.database);
        session.buffered(&create, &cancel).await.unwrap();
        // Only when off: switching ends every other connection to the database, a parallel test's included.
        let snapshot = format!(
            "IF EXISTS (SELECT 1 FROM sys.databases WHERE name = N'{0}' AND is_read_committed_snapshot_on = 0)
            ALTER DATABASE [{0}] SET READ_COMMITTED_SNAPSHOT ON WITH ROLLBACK IMMEDIATE",
            config.database
        );
        session.buffered(&snapshot, &cancel).await.unwrap();
        engine.close().await;
    });
}

// Tables outlive a run on a server, so each gets a fresh name.
fn unique(prefix: &str) -> String {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let micros = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_micros() % 1_000_000_000;
    format!("{prefix}_{micros}_{}", SEQ.fetch_add(1, Ordering::Relaxed))
}

fn open_on(cx: &mut TestAppContext, engine: Engine) -> Driver<'_> {
    open_with(cx, |env| env.runtime.block_on(env.bar.save_connection(engine.config())).unwrap())
}

fn table(app: &mut Driver, name: &str) -> String {
    format!("t:{}.{name}", app.schema_name())
}

fn view(app: &mut Driver) -> Entity<TableTab> {
    let tab = app.cx.update(|_, cx| app.workspace.read(cx).table_tab()).expect("a table view is active");
    app.settle(|cx| cx.update(|_, cx| tab.read(cx).idle()));
    tab
}

fn grid(app: &mut Driver) -> Entity<Grid> {
    let tab = view(app);
    app.cx.update(|_, cx| tab.read(cx).grid())
}

fn column(app: &mut Driver, col: usize) -> Vec<Option<String>> {
    let grid = grid(app);
    app.cx.update(|_, cx| (0..grid.read(cx).rows_shown()).map(|row| grid.read(cx).text_at(row, col)).collect())
}

fn browse(app: &mut Driver, name: &str) {
    let schema = app.schema_name();
    app.hover(selector(format!("tree:t:{schema}.{name}")));
    app.click(selector(format!("browse-{schema}-{name}")));
    view(app);
}

// Drop is a table menu's last item, one further down where the System views submenu comes first.
fn drop_item(engine: Engine) -> usize {
    12 + usize::from(views(&engine.driver(), ViewScope::Table).next().is_some())
}

fn label(app: &mut Driver, view: &SystemView) -> String {
    app.cx.update(|_, cx| t(cx, &view.title_key()).to_string())
}

// A view opened from a menu gets a tab of its own, titled `title`, and runs there.
fn ran_in_new_tab(app: &mut Driver, tabs: usize, title: &str) {
    assert_eq!(app.titles().len(), tabs + 1);
    assert_eq!(app.titles().last().map(String::as_str), Some(title));
    app.wait_idle();
    assert!(!app.status_is_error(), "{:?}", app.error());
    assert!(app.grid().is_some(), "a grid of the view's rows");
}

fn plan_rows(app: &mut Driver) -> Vec<(String, String, f64)> {
    let plan = app.plan().expect("the plan view");
    app.cx.update(|_, cx| plan.read(cx).rows())
}

fn analyzed(app: &mut Driver) -> bool {
    let plan = app.plan().expect("the plan view");
    app.cx.update(|_, cx| plan.read(cx).analyzed())
}

fn simple_query(cx: &mut TestAppContext, engine: Engine) {
    let mut app = open_on(cx, engine);
    app.connect();
    app.run("SELECT 1 AS one;");
    assert_eq!(app.cell(0, 0).as_deref(), Some("1"));
}

fn explain(cx: &mut TestAppContext, engine: Engine) {
    let mut app = open_on(cx, engine);
    let name = unique("plan");
    app.seed(&format!("CREATE TABLE {name} (id INTEGER PRIMARY KEY, name VARCHAR(50)); INSERT INTO {name} VALUES (1, 'Alice'), (2, 'Bob')"));
    app.connect();
    app.set_sql(&format!("SELECT * FROM {name} WHERE id = 1;"));
    app.click("explain");
    app.wait_idle();
    assert!(!analyzed(&mut app), "Estimated");
    assert!(plan_rows(&mut app).iter().all(|(_, label, _)| !label.is_empty()));
    let plan = app.plan().unwrap();
    // A plan without numbers says so.
    let notes: &[&str] = if engine.driver().capabilities().plan_metrics { &[] } else { &[NOTE_NO_METRICS] };
    assert_eq!(app.cx.update(|_, cx| plan.read(cx).notes()), notes);
}

fn typed_explain(cx: &mut TestAppContext, engine: Engine) {
    // T-SQL has no EXPLAIN to type.
    if !engine.driver().capabilities().typed_explain {
        return;
    }
    let mut app = open_on(cx, engine);
    let name = unique("plan_typed");
    app.seed(&format!(
        "CREATE TABLE {name} (id INTEGER PRIMARY KEY, name VARCHAR(50)); INSERT INTO {name} VALUES (1, 'Alice')"
    ));
    app.connect();
    // SQLite's plain EXPLAIN lists bytecode. Its plan is EXPLAIN QUERY PLAN.
    let explain = if engine == Engine::Turso { "EXPLAIN QUERY PLAN" } else { "EXPLAIN" };
    app.run(&format!("{explain} SELECT * FROM {name} WHERE id = 1;"));
    assert!(app.grid().is_none());
    assert!(plan_rows(&mut app).iter().all(|(_, label, _)| !label.is_empty()));
}

fn transactions(cx: &mut TestAppContext, engine: Engine) {
    let mut app = open_on(cx, engine);
    let name = unique("e2e_txn");
    app.seed(&format!("CREATE TABLE {name} (id INTEGER PRIMARY KEY, name VARCHAR(50))"));
    let tab = app.connect();
    // Turso can't hold a transaction between runs, so the toolbar leaves the buttons out.
    if !engine.driver().capabilities().interactive_transactions {
        assert!(app.cx.debug_bounds("begin-txn").is_none(), "no transaction buttons");
        return;
    }
    let in_txn = |app: &mut Driver, expected: bool| {
        app.settle(|cx| cx.update(|_, cx| tab.read(cx).in_transaction() == expected));
    };
    app.click("begin-txn");
    in_txn(&mut app, true);
    app.run(&format!("INSERT INTO {name} (id, name) VALUES (1, 'temp');"));
    app.click("rollback-txn");
    in_txn(&mut app, false);
    app.run(&format!("SELECT COUNT(*) FROM {name};"));
    assert_eq!(app.cell(0, 0).as_deref(), Some("0"));
    app.click("begin-txn");
    in_txn(&mut app, true);
    app.run(&format!("INSERT INTO {name} (id, name) VALUES (2, 'kept');"));
    app.click("commit-txn");
    in_txn(&mut app, false);
    app.run(&format!("SELECT COUNT(*) FROM {name};"));
    assert_eq!(app.cell(0, 0).as_deref(), Some("1"));
}

fn created_table(cx: &mut TestAppContext, engine: Engine) {
    let mut app = open_on(cx, engine);
    let name = unique("e2e_schema");
    app.connect();
    app.run(&format!("CREATE TABLE {name} (id INTEGER PRIMARY KEY, name VARCHAR(50));"));
    app.refresh_schema();
    let row = table(&mut app, &name);
    app.click(selector(format!("tree:{row}")));
    let schema = app.schema_name();
    let sidebar = app.sidebar();
    let tree = app.cx.update(|_, cx| sidebar.read(cx).panels().0);
    let column = format!("c:{schema}.{name}.name");
    app.settle(|cx| cx.update(|_, cx| tree.read(cx).listed().iter().any(|(id, ..)| *id == column)));
}

fn browse_rows(cx: &mut TestAppContext, engine: Engine) {
    let mut app = open_on(cx, engine);
    let name = unique("e2e_tv");
    app.seed(&format!("CREATE TABLE {name} (id INTEGER PRIMARY KEY, name VARCHAR(50)); INSERT INTO {name} VALUES (1, 'Alice'), (2, 'Bob')"));
    app.connect();
    app.refresh_schema();
    browse(&mut app, &name);
    assert_eq!(column(&mut app, 1), [Some("Alice".into()), Some("Bob".into())]);
}

// The filter bar's button lists the server views. SQLite keeps no statistics, so Turso has no button.
fn server_views(cx: &mut TestAppContext, engine: Engine) {
    let mut app = open_on(cx, engine);
    app.connect();
    let Some(first) = views(&engine.driver(), ViewScope::Server).next() else {
        assert!(app.cx.debug_bounds("server-views").is_none(), "no views, so no button");
        return;
    };
    let (tabs, title) = (app.titles().len(), label(&mut app, first));
    app.menu_pick("server-views", 0);
    ran_in_new_tab(&mut app, tabs, &title);
}

fn table_views(cx: &mut TestAppContext, engine: Engine) {
    let Some(first) = views(&engine.driver(), ViewScope::Table).next() else { return };
    let mut app = open_on(cx, engine);
    let name = unique("e2e_stats");
    app.seed(&format!(
        "CREATE TABLE {name} (id INTEGER PRIMARY KEY, name VARCHAR(50)); INSERT INTO {name} VALUES (1, 'Alice')"
    ));
    app.connect();
    app.refresh_schema();
    let (tabs, title) = (app.titles().len(), format!("{}: {name}", label(&mut app, first)));
    let row = table(&mut app, &name);
    app.context_submenu(selector(format!("tree:{row}")), SYSTEM_VIEWS, 0);
    ran_in_new_tab(&mut app, tabs, &title);
}

// The engine names the column's type, which is what makes the viewer read the cell as an array.
fn array_cells(cx: &mut TestAppContext, engine: Engine) {
    let (sql, json) = match engine {
        Engine::Postgres => ("SELECT ARRAY['a', 'b c'] AS a;", "[\n  \"a\",\n  \"b c\"\n]"),
        Engine::ClickHouse => ("SELECT map('k', [1, 2]) AS a;", "{\n  \"k\": [\n    1,\n    2\n  ]\n}"),
        Engine::MySql | Engine::MariaDb | Engine::Turso | Engine::SqlServer => return,
    };
    let mut app = open_on(cx, engine);
    app.connect();
    app.run(sql);
    let grid = app.grid().expect("a grid");
    let at = app.grid_point(&grid, |grid| grid.cell_point(0, 0));
    app.click_at(at, Modifiers::none());
    app.keys("shift-enter");
    let viewer = app.cx.update(|_, cx| cx.try_global::<Opened>().and_then(|opened| opened.0.upgrade()));
    let viewer = viewer.expect("the cell viewer");
    assert_eq!(app.cx.update(|_, cx| viewer.read(cx).shown(cx)), (Kind::Array, json.to_string()));
}

fn foreign_key(cx: &mut TestAppContext, engine: Engine) {
    // ClickHouse parses FOREIGN KEY and keeps nothing of it.
    if !engine.driver().capabilities().catalog.constraints {
        return;
    }
    let mut app = open_on(cx, engine);
    let (parent, child) = (unique("e2e_fk_parent"), unique("e2e_fk_child"));
    app.seed(&format!(
        "CREATE TABLE {parent} (id INTEGER PRIMARY KEY, name VARCHAR(50));
         INSERT INTO {parent} VALUES (1, 'Alice'), (2, 'Bob');
         CREATE TABLE {child} (id INTEGER PRIMARY KEY, parent_id INTEGER, FOREIGN KEY (parent_id) REFERENCES {parent} (id));
         INSERT INTO {child} VALUES (1, 2), (2, NULL)"
    ));
    app.connect();
    app.refresh_schema();
    browse(&mut app, &child);
    let grid = grid(&mut app);
    let at = app.grid_point(&grid, |grid| grid.jump_point(0, 1));
    let buttons = app.cx.update(|_, cx| {
        let grid = grid.read(cx);
        (grid.jumps_at(0, 1), grid.jumps_at(0, 0), grid.jumps_at(1, 1))
    });
    assert_eq!(buttons, (true, false, false));
    app.click_at(at, Modifiers::none());
    let workspace = app.workspace.clone();
    app.settle(|cx| cx.update(|_, cx| workspace.read(cx).active_title(cx) == parent));
    assert_eq!(column(&mut app, 1), [Some("Bob".into())]);
}

fn dialog(cx: &mut TestAppContext, engine: Engine) {
    let mut app = open_on(cx, engine);
    let config = engine.config();
    app.cx.update(|_, cx| cx.set_global(Stub(None)));
    app.click("connection-switcher");
    app.click("new-connection");
    assert!(app.dialog_open());
    app.click("conn-name");
    app.type_text(&format!("{} dialog", config.name));
    app.menu_pick("conn-driver", engine.menu_index());
    let fields = match engine {
        Engine::Turso => vec![("conn-url", config.url.clone())],
        Engine::Postgres | Engine::MySql | Engine::MariaDb | Engine::ClickHouse | Engine::SqlServer => vec![
            ("conn-host", config.host.clone()),
            ("conn-port", config.port.to_string()),
            ("conn-database", config.database.clone()),
            ("conn-username", config.username.clone()),
            ("conn-password", config.password.clone()),
        ],
    };
    for (field, value) in fields {
        app.click(field);
        app.keys("mod-a");
        app.type_text(&value);
    }
    if engine == Engine::Turso {
        // A plain-http sqld, so the certificate check has nothing to check.
        app.menu_pick("conn-ssl", 0);
    }
    app.click("connection-test");
    app.settle(|cx| {
        cx.update(|_, cx| crate::toast::messages(cx).iter().any(|(_, text)| text == "Connection successful!"))
    });
    app.click("connection-save");
    let saved = app.env.bar.list_connections().into_iter().find(|c| c.name.ends_with(" dialog")).expect("saved");
    app.click("connection-switcher");
    app.hover(selector(format!("connection-row-{}", saved.id)));
    app.click(selector(format!("connect-{}", saved.id)));
    let bar = app.env.bar.clone();
    let id = saved.id.clone();
    app.settle(|_| bar.is_connected(&id));
    app.click("connection-switcher");
    app.hover(selector(format!("connection-row-{}", saved.id)));
    app.click(selector(format!("connect-{}", saved.id)));
    app.settle(|_| !bar.is_connected(&id));
}

// Lists databases with the unsaved credentials, before any database is set.
fn database_picker(cx: &mut TestAppContext, engine: Engine) {
    if !engine.driver().capabilities().database_picker {
        return;
    }
    let mut app = open_on(cx, engine);
    let config = engine.config();
    app.click("connection-switcher");
    app.click("new-connection");
    app.menu_pick("conn-driver", engine.menu_index());
    for (field, value) in [
        ("conn-host", config.host.clone()),
        ("conn-port", config.port.to_string()),
        ("conn-username", config.username.clone()),
        ("conn-password", config.password.clone()),
    ] {
        app.click(field);
        app.keys("mod-a");
        app.type_text(&value);
    }
    let form = app.cx.update(|_, cx| cx.global::<crate::connection_dialog::Opened>().0.upgrade()).expect("the form");
    assert_eq!(app.cx.update(|_, cx| form.read(cx).database(cx)), "");
    app.click("conn-database-list");
    app.settle(|cx| cx.update(|_, cx| form.read(cx).databases().is_some()));
    let databases = app.cx.update(|_, cx| form.read(cx).databases().unwrap_or_default().to_vec());
    let at = databases.iter().position(|d| *d == config.database).unwrap_or_else(|| panic!("{databases:?}"));
    app.cx.update(|window, cx| window.draw(cx).clear(cx));
    app.keys(&format!("{}enter", "down ".repeat(at + 1)));
    assert_eq!(app.cx.update(|_, cx| form.read(cx).database(cx)), config.database);
}

macro_rules! per_engine {
    ($($test:ident => $body:ident;)*) => {
        mod postgres {
            $(#[gpui_kit::test] fn $test(cx: &mut gpui_kit::TestAppContext) { super::$body(cx, super::Engine::Postgres) })*
        }
        mod mysql {
            $(#[gpui_kit::test] fn $test(cx: &mut gpui_kit::TestAppContext) { super::$body(cx, super::Engine::MySql) })*
        }
        mod mariadb {
            $(#[gpui_kit::test] fn $test(cx: &mut gpui_kit::TestAppContext) { super::$body(cx, super::Engine::MariaDb) })*
        }
        mod turso {
            $(#[gpui_kit::test] fn $test(cx: &mut gpui_kit::TestAppContext) { super::$body(cx, super::Engine::Turso) })*
        }
        mod clickhouse {
            $(#[gpui_kit::test] fn $test(cx: &mut gpui_kit::TestAppContext) { super::$body(cx, super::Engine::ClickHouse) })*
        }
        mod sqlserver {
            $(#[gpui_kit::test] fn $test(cx: &mut gpui_kit::TestAppContext) {
                if super::Engine::SqlServer.ready() {
                    super::$body(cx, super::Engine::SqlServer)
                }
            })*
        }
    };
}

per_engine! {
    a_connection_is_added_tested_connected_and_disconnected => dialog;
    the_database_picker_lists_the_servers_databases => database_picker;
    a_simple_query_shows_its_row => simple_query;
    explain_shows_an_estimated_plan => explain;
    a_typed_explain_shows_the_plan_viewer => typed_explain;
    the_toolbar_rolls_back_and_commits => transactions;
    a_created_table_shows_in_the_schema_browser => created_table;
    browsing_a_table_shows_its_rows => browse_rows;
    a_foreign_key_cell_jumps_to_the_referenced_row => foreign_key;
    a_server_view_opens_in_a_tab_and_runs => server_views;
    a_tables_system_view_opens_in_a_tab_and_runs => table_views;
    an_array_cell_opens_as_json => array_cells;
}

mod postgres_only {
    use gpui_kit::TestAppContext;

    use barsql_core::DriverType;
    use barsql_sql::system_views::{ViewScope, views};

    use super::{Engine, analyzed, drop_item, label, open_on, plan_rows, ran_in_new_tab, unique};
    use crate::scenarios::driver::{Driver, selector};

    fn seeded(cx: &mut TestAppContext) -> (Driver<'_>, String) {
        let mut app = open_on(cx, Engine::Postgres);
        let name = unique("plan_pg");
        app.seed(&format!("CREATE TABLE {name} (id INTEGER PRIMARY KEY, name VARCHAR(50)); INSERT INTO {name} VALUES (1, 'Alice'), (2, 'Bob')"));
        app.connect();
        (app, name)
    }

    fn metrics(app: &mut Driver) -> Vec<&'static str> {
        let plan = app.plan().unwrap();
        app.cx.update(|_, cx| plan.read(cx).metrics().iter().map(|metric| metric.key()).collect())
    }

    fn columns(app: &mut Driver) -> Vec<&'static str> {
        let plan = app.plan().unwrap();
        app.cx.update(|_, cx| plan.read(cx).columns().iter().map(|metric| metric.key()).collect())
    }

    // A connection's own menu lists the server views too, after Connect, Edit and Duplicate.
    #[gpui_kit::test]
    fn the_connection_menu_opens_a_server_view(cx: &mut TestAppContext) {
        let mut app = open_on(cx, Engine::Postgres);
        app.connect();
        let first = views(&DriverType::Postgres, ViewScope::Server).next().expect("Postgres has server views");
        let (tabs, title, id) = (app.titles().len(), label(&mut app, first), app.connection.id.clone());
        app.click("connection-switcher");
        app.context_submenu(selector(format!("connection-row-{id}")), 3, 0);
        ran_in_new_tab(&mut app, tabs, &title);
    }

    // Postgres won't drop a table a view depends on. Cascade drops the view too.
    #[gpui_kit::test]
    fn a_blocked_drop_goes_through_with_cascade(cx: &mut TestAppContext) {
        let mut app = open_on(cx, Engine::Postgres);
        let (name, view) = (unique("cascade_pg"), unique("cascade_pg_v"));
        app.seed(&format!("CREATE TABLE {name} (id INTEGER PRIMARY KEY); CREATE VIEW {view} AS SELECT id FROM {name}"));
        app.connect();
        app.refresh_schema();
        app.context_menu(selector(format!("tree:t:public.{name}")), drop_item(Engine::Postgres));
        app.click("change-confirm");
        app.settle(|cx| cx.debug_bounds("change-error").is_some());
        app.click("change-cascade");
        app.click("change-confirm");
        app.settle_dialog_closed();
        let left = app.query(&format!("SELECT to_regclass('{name}')::text, to_regclass('{view}')::text"));
        assert_eq!(left, [[barsql_core::Value::Null, barsql_core::Value::Null]]);
    }

    #[gpui_kit::test]
    fn the_error_card_shows_the_code_message_and_hint(cx: &mut TestAppContext) {
        let mut app = open_on(cx, Engine::Postgres);
        app.connect();
        app.run("SELECT nonexistent_func_e2e();");
        assert!(app.status_is_error());
        let info = app.error().and_then(|error| error.info).expect("the error card");
        assert_eq!(info.code, "42883");
        assert!(info.message.contains("nonexistent_func_e2e") && info.hint.contains("No function matches"), "{info:?}");
    }

    #[gpui_kit::test]
    fn jump_to_error_focuses_the_editor_and_marks_the_token(cx: &mut TestAppContext) {
        let mut app = open_on(cx, Engine::Postgres);
        app.connect();
        app.run("SELECT * FROM definitely_missing_table_e2e;");
        assert_eq!(app.error().and_then(|error| error.info).map(|info| info.code).as_deref(), Some("42P01"));
        app.click("jump-to-error");
        let tab = app.tab();
        let squiggle = app.cx.update(|_, cx| tab.read(cx).error_squiggle(cx));
        assert_eq!(squiggle.as_deref(), Some("definitely_missing_table_e2e"));
    }

    #[gpui_kit::test]
    fn the_shortcuts_pick_estimates_or_measurements(cx: &mut TestAppContext) {
        let (mut app, name) = seeded(cx);
        app.set_sql(&format!("SELECT * FROM {name};"));
        app.keys("mod-shift-e");
        app.wait_idle();
        assert!(!analyzed(&mut app), "Estimated");
        app.keys("mod-shift-a");
        app.wait_idle();
        assert!(analyzed(&mut app), "Measured");
    }

    #[gpui_kit::test]
    fn explain_analyze_measures_rows_and_timings(cx: &mut TestAppContext) {
        let (mut app, name) = seeded(cx);
        app.set_sql(&format!("SELECT * FROM {name};"));
        app.click("explain-analyze");
        app.wait_idle();
        assert!(analyzed(&mut app));
        assert!(metrics(&mut app).contains(&"time"));
        let hottest = plan_rows(&mut app).iter().map(|(.., heat)| *heat).fold(0., f64::max);
        assert_eq!(hottest, 1.);
    }

    #[gpui_kit::test]
    fn the_heat_follows_the_chosen_metric(cx: &mut TestAppContext) {
        let (mut app, name) = seeded(cx);
        app.set_sql(&format!("SELECT * FROM {name} ORDER BY name;"));
        app.click("explain");
        app.wait_idle();
        assert_eq!(columns(&mut app), ["rows", "cost"]);
        app.click("plan-metric-rows");
        let hottest = plan_rows(&mut app).iter().map(|(.., heat)| *heat).fold(0., f64::max);
        assert_eq!(hottest, 1.);
    }

    #[gpui_kit::test]
    fn a_typed_explain_analyze_is_measured(cx: &mut TestAppContext) {
        let (mut app, name) = seeded(cx);
        app.run(&format!("EXPLAIN ANALYZE SELECT * FROM {name};"));
        assert!(analyzed(&mut app));
    }

    #[gpui_kit::test]
    fn a_formatted_explain_stays_a_grid(cx: &mut TestAppContext) {
        let (mut app, name) = seeded(cx);
        app.run(&format!("EXPLAIN (FORMAT TEXT) SELECT * FROM {name};"));
        assert!(app.plan().is_none());
        assert!(app.cell(0, 0).is_some());
    }

    // A DO block returns nothing, so a run of one opens on what it said.
    #[gpui_kit::test]
    fn a_do_blocks_notices_open_the_messages_tab(cx: &mut TestAppContext) {
        let mut app = open_on(cx, Engine::Postgres);
        app.connect();
        app.run("DO $$ BEGIN RAISE NOTICE 'hello from the server'; RAISE WARNING 'careful'; END $$;");
        let results = app.results();
        assert!(app.cx.update(|_, cx| results.read(cx).messages_shown()));
        let texts = app.cx.update(|_, cx| results.read(cx).message_texts());
        let texts: Vec<&str> = texts.iter().map(|(_, _, text)| text.as_str()).collect();
        assert_eq!(texts, ["hello from the server", "careful"]);
        assert!(app.cx.debug_bounds("result-tab-messages").is_some());
    }

    #[gpui_kit::test]
    fn schema_functions_are_listed_under_routines(cx: &mut TestAppContext) {
        let mut app = open_on(cx, Engine::Postgres);
        let function = unique("e2e_ddl_fn");
        app.seed(&format!("CREATE FUNCTION {function}(a int) RETURNS int LANGUAGE sql AS 'SELECT a'"));
        app.connect();
        app.refresh_schema();
        let sidebar = app.sidebar();
        let tree = app.cx.update(|_, cx| sidebar.read(cx).panels().0);
        app.cx.update(|_, cx| tree.update(cx, |tree, cx| tree.reveal("g:public:routines", cx)));
        app.click(selector("tree:g:public:routines".into()));
        let wanted = function.clone();
        app.settle(|cx| cx.update(|_, cx| tree.read(cx).listed().iter().any(|(_, label, _)| label.contains(&wanted))));
        app.seed(&format!("DROP FUNCTION {function}(int)"));
    }
}

mod sqlserver_only {
    use barsql_core::MessageLevel;
    use gpui_kit::TestAppContext;

    use super::{Engine, open_on, table, unique};
    use crate::scenarios::driver::selector;

    // A batch of PRINTs returns no rows, so the run opens on its Messages.
    #[gpui_kit::test]
    fn print_shows_in_the_messages_tab(cx: &mut TestAppContext) {
        if !Engine::SqlServer.ready() {
            return;
        }
        let mut app = open_on(cx, Engine::SqlServer);
        app.connect();
        app.run("PRINT 'hello from BarSQL'");
        let results = app.results();
        let (shown, texts) =
            app.cx.update(|_, cx| (results.read(cx).messages_shown(), results.read(cx).message_texts()));
        assert!(shown, "the Messages tab is open");
        assert_eq!(texts, [(MessageLevel::Notice, String::new(), "hello from BarSQL".to_string())]);
    }

    // T-SQL has no LIMIT.
    #[gpui_kit::test]
    fn the_menus_select_takes_the_top_rows(cx: &mut TestAppContext) {
        if !Engine::SqlServer.ready() {
            return;
        }
        let mut app = open_on(cx, Engine::SqlServer);
        let name = unique("e2e_top");
        app.seed(&format!("CREATE TABLE {name} (id INTEGER PRIMARY KEY); INSERT INTO {name} VALUES (1)"));
        app.connect();
        app.refresh_schema();
        let row = table(&mut app, &name);
        app.context_menu(selector(format!("tree:{row}")), 1);
        let sql = app.sql();
        assert!(sql.starts_with("SELECT TOP (100) "), "{sql}");
        app.click("run-all");
        app.wait_idle();
        assert_eq!(app.cell(0, 0).as_deref(), Some("1"));
    }
}

mod turso_only {
    use gpui_kit::TestAppContext;

    use super::{Engine, open_on};
    use crate::scenarios::editor::suggest;

    // libSQL's vector functions come from its own pack, and the server's function list keeps them.
    #[gpui_kit::test]
    fn completion_offers_libsqls_vector_functions(cx: &mut TestAppContext) {
        let mut app = open_on(cx, Engine::Turso);
        app.connect();
        app.refresh_schema();
        suggest(&mut app, "SELECT vector_dist", "vector_distance_cos");
    }
}
