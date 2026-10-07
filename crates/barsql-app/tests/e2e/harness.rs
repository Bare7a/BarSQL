use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use barsql_app::{BarApp, BufferedResult, CsvImportRequest, CsvOptions, ImportResult, RunEvent};
use barsql_core::{ConnectionConfig, DriverType, QueryError, ResultSummary, Row, TableDataRequest, Value};
use barsql_db::{Cancel, Cell, Engine, ResultChunk};
use barsql_io::{ExportFormat, export_to_string};
use barsql_sql::qualified_table;
use tempfile::TempDir;

use crate::support::{collect, import_outcome};

const TEST_TIMEOUT: Duration = Duration::from_secs(180);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Postgres,
    MySql,
    MariaDb,
    // sqld from docker-compose.yml, or a Turso database given by BARSQL_E2E_TURSO_URL and _TOKEN.
    Turso,
    ClickHouse,
    // The mssql profile's server, Azure SQL Edge on arm64. Runs only with BARSQL_E2E_MSSQL=1.
    SqlServer,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Postgres => "postgres",
            Self::MySql => "mysql",
            Self::MariaDb => "mariadb",
            Self::Turso => "turso",
            Self::ClickHouse => "clickhouse",
            Self::SqlServer => "sqlserver",
        }
    }

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

    pub fn driver(self) -> DriverType {
        match self {
            Self::Postgres => DriverType::Postgres,
            Self::MySql | Self::MariaDb => DriverType::MySql,
            Self::Turso => DriverType::Turso,
            Self::ClickHouse => DriverType::ClickHouse,
            Self::SqlServer => DriverType::SqlServer,
        }
    }

    pub fn config(self) -> ConnectionConfig {
        let (port, user, password) = match self {
            Self::Postgres => ("55432", "postgres", "postgres"),
            Self::MySql => ("33306", "root", "root"),
            Self::MariaDb => ("33307", "root", "root"),
            Self::ClickHouse => ("38123", "default", "clickhouse"),
            Self::SqlServer => ("31433", "sa", "BarSQL-e2e-Passw0rd"),
            Self::Turso => {
                return ConnectionConfig {
                    name: format!("e2e-{}", self.name()),
                    driver: self.driver(),
                    url: self.env("URL", "http://127.0.0.1:38080"),
                    auth_token: self.env("TOKEN", ""),
                    ..Default::default()
                };
            }
        };
        ConnectionConfig {
            name: format!("e2e-{}", self.name()),
            driver: self.driver(),
            host: self.env("HOST", "127.0.0.1"),
            port: self.env("PORT", port).parse().expect("port"),
            database: self.env("DB", "barsql_test"),
            username: self.env("USER", user),
            password: self.env("PASSWORD", password),
            ssl_mode: match self {
                Self::Postgres | Self::ClickHouse => "disable".into(),
                Self::SqlServer => "require".into(),
                Self::MySql | Self::MariaDb | Self::Turso => String::new(),
            },
            ..Default::default()
        }
    }
}

// The stack's SQL Server starts without barsql_test, and refuses logins for a few seconds after it reports healthy.
// False when the profile is off.
async fn mssql_ready() -> bool {
    if std::env::var("BARSQL_E2E_MSSQL").as_deref() != Ok("1") {
        eprintln!("skipped: set BARSQL_E2E_MSSQL=1 and start the mssql profile");
        return false;
    }
    let cfg = Kind::SqlServer.config();
    let master = ConnectionConfig { database: "master".into(), ..cfg.clone() };
    for _ in 0..30 {
        if let Ok(engine) = Engine::connect(&master).await {
            // Readers see the last committed rows instead of waiting on a writer, as on the other engines.
            let snapshot = format!(
                "IF EXISTS (SELECT 1 FROM sys.databases WHERE name = N'{0}' AND is_read_committed_snapshot_on = 0)
                ALTER DATABASE [{0}] SET READ_COMMITTED_SNAPSHOT ON WITH ROLLBACK IMMEDIATE",
                cfg.database
            );
            let create = format!("IF DB_ID(N'{0}') IS NULL CREATE DATABASE [{0}]", cfg.database);
            let mut session = engine.session().await.unwrap();
            for sql in [create, snapshot] {
                session.buffered(&sql, &Cancel::new()).await.unwrap();
            }
            engine.close().await;
            return true;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    panic!("SQL Server is not reachable; start it with BARSQL_E2E_MSSQL=1 cargo xtask e2e up");
}

// Fresh per run, so repeated or parallel runs never collide with a leftover table.
pub fn unique_table(prefix: &str) -> String {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let micros = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_micros() % 1_000_000;
    format!("{prefix}_{micros}_{}", SEQ.fetch_add(1, Ordering::Relaxed) + 1)
}

pub fn row(pairs: &[(&str, Value)]) -> Row {
    pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
}

pub fn names(values: &[&str]) -> Vec<String> {
    values.iter().map(|s| s.to_string()).collect()
}

pub struct E2e {
    pub app: BarApp,
    pub kind: Kind,
    pub id: String,
    dir: TempDir,
    drops: Mutex<Vec<String>>,
}

// The app's own runtime settings, with two workers.
pub fn block_on<F: Future>(test: F) -> F::Output {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_stack_size(barsql_app::WORKER_STACK)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(test)
}

pub async fn run<F, Fut>(kind: Kind, body: F)
where
    F: FnOnce(Arc<E2e>) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    if kind == Kind::SqlServer && !mssql_ready().await {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let app = BarApp::open(&dir.path().join("data"), tokio::runtime::Handle::current()).unwrap();
    let id = app.save_connection(kind.config()).await.unwrap_or_else(|e| panic!("[{}] save: {e:?}", kind.name())).id;
    if let Err(err) = app.connect(&id).await {
        panic!("{} not reachable ({}) - bring the stack up with `cargo xtask e2e up`", kind.name(), err.message);
    }
    let e = Arc::new(E2e { app, kind, id, dir, drops: Mutex::default() });
    let task = tokio::spawn(body(e.clone()));
    let abort = task.abort_handle();
    let outcome = tokio::time::timeout(TEST_TIMEOUT, task).await;
    abort.abort();
    e.cleanup().await;
    match outcome {
        Err(_) => panic!("[{}] timed out after {TEST_TIMEOUT:?}", kind.name()),
        Ok(Err(err)) if err.is_panic() => std::panic::resume_unwind(err.into_panic()),
        Ok(Err(err)) => panic!("[{}] {err}", kind.name()),
        Ok(Ok(())) => {}
    }
}

impl E2e {
    pub fn driver(&self) -> DriverType {
        self.kind.driver()
    }

    pub fn is_postgres(&self) -> bool {
        self.kind == Kind::Postgres
    }

    pub fn is_mysql(&self) -> bool {
        matches!(self.kind, Kind::MySql | Kind::MariaDb)
    }

    // SQLite's dialect over a server.
    pub fn is_lite(&self) -> bool {
        self.kind == Kind::Turso
    }

    pub fn is_clickhouse(&self) -> bool {
        self.kind == Kind::ClickHouse
    }

    pub fn is_sqlserver(&self) -> bool {
        self.kind == Kind::SqlServer
    }

    pub fn schema(&self) -> String {
        match self.kind {
            Kind::Postgres => "public".into(),
            Kind::MySql | Kind::MariaDb | Kind::ClickHouse => self.kind.config().database,
            Kind::Turso => "main".into(),
            Kind::SqlServer => "dbo".into(),
        }
    }

    // ClickHouse tables need an engine. A table without a key orders by nothing.
    pub fn engine_clause(&self) -> &'static str {
        if self.is_clickhouse() { " ENGINE = MergeTree ORDER BY tuple()" } else { "" }
    }

    // SQLite takes no schema in REFERENCES or CREATE INDEX … ON, and main is the only one anyway.
    pub fn qualified(&self, table: &str) -> String {
        if self.is_lite() {
            return barsql_sql::quote_ident(&self.driver(), table);
        }
        qualified_table(&self.driver(), &self.schema(), table)
    }

    // A lone INTEGER PRIMARY KEY is SQLite's rowid, which fills itself in.
    pub fn pk_column(&self) -> &'static str {
        match self.kind {
            Kind::Postgres => "id SERIAL PRIMARY KEY",
            Kind::MySql | Kind::MariaDb => "id INT AUTO_INCREMENT PRIMARY KEY",
            Kind::Turso => "id INTEGER PRIMARY KEY",
            // No auto-increment, but snowflake ids are unique and grow.
            Kind::ClickHouse => "id UInt64 DEFAULT generateSnowflakeID()",
            Kind::SqlServer => "id INT IDENTITY(1, 1) PRIMARY KEY",
        }
    }

    pub fn auto_pk_table(&self, table: &str) -> String {
        if self.is_clickhouse() {
            return format!("CREATE TABLE {table} ({}, name String) ENGINE = MergeTree ORDER BY id", self.pk_column());
        }
        let name = match self.kind {
            Kind::MySql | Kind::MariaDb => "VARCHAR(255)",
            // text is deprecated there, and can't be compared.
            Kind::SqlServer => "NVARCHAR(255)",
            Kind::Postgres | Kind::Turso | Kind::ClickHouse => "TEXT",
        };
        format!("CREATE TABLE {table} ({}, name {name} NOT NULL)", self.pk_column())
    }

    pub fn json_type(&self) -> &'static str {
        match self.kind {
            Kind::Postgres => "jsonb",
            Kind::MySql | Kind::MariaDb => "json",
            Kind::Turso => "TEXT",
            Kind::ClickHouse => "Nullable(String)",
            Kind::SqlServer => "NVARCHAR(MAX)",
        }
    }

    pub fn bool_type(&self) -> &'static str {
        match self.kind {
            Kind::MySql | Kind::MariaDb => "TINYINT(1)",
            Kind::Postgres | Kind::Turso => "BOOLEAN",
            Kind::ClickHouse => "Nullable(Bool)",
            Kind::SqlServer => "BIT",
        }
    }

    // MySQL answers a killed bare SELECT SLEEP(n) with 1 instead of an error, so it sleeps per row. SQLite has no
    // sleep, so it counts for about as long.
    pub fn sleep_sql(&self, seconds: u32) -> String {
        match self.kind {
            Kind::Postgres => format!("SELECT pg_sleep({seconds})"),
            Kind::MySql => format!("SELECT SLEEP({seconds}) FROM (SELECT 1 AS x UNION ALL SELECT 2) AS t"),
            Kind::MariaDb => format!("SELECT SLEEP({seconds})"),
            Kind::Turso => format!(
                "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c WHERE x < {}) SELECT max(x) FROM c",
                u64::from(seconds) * 2_000_000
            ),
            // sleep() stops at 3 seconds a block, so a second a row.
            Kind::ClickHouse => format!("SELECT sleepEachRow(1) FROM numbers({seconds}) SETTINGS max_block_size = 1"),
            Kind::SqlServer => format!("WAITFOR DELAY '00:00:{seconds:02}'; SELECT 1 AS slept"),
        }
    }

    pub async fn try_query(&self, sql: &str) -> Result<BufferedResult, QueryError> {
        self.app.execute_query(&self.id, sql).await
    }

    pub async fn query(&self, sql: &str) -> BufferedResult {
        self.try_query(sql).await.unwrap_or_else(|err| panic!("[{}] {sql:?}: {err:?}", self.kind.name()))
    }

    pub async fn exec(&self, sql: &str) {
        self.query(sql).await;
    }

    // Registers the matching DROP, so the shared database stays clean when an assertion fails.
    pub async fn create_temp_table(&self, ddl: &str, table: &str) {
        self.exec(ddl).await;
        self.defer(format!("DROP TABLE IF EXISTS {}", self.qualified(table)));
    }

    pub fn defer(&self, sql: impl Into<String>) {
        self.drops.lock().unwrap().push(sql.into());
    }

    pub async fn count(&self, table: &str) -> i64 {
        count_value(&self.query(&format!("SELECT count(*) FROM {}", self.qualified(table))).await)
    }

    pub async fn run_on_tab(&self, tab: &str, sql: &str) -> Vec<RunEvent> {
        self.run_on(&self.id, tab, sql).await
    }

    pub async fn run_on(&self, connection_id: &str, tab: &str, sql: &str) -> Vec<RunEvent> {
        let handle =
            self.app.execute_query_stream(connection_id, tab, sql).await.unwrap_or_else(|e| panic!("{sql:?}: {e:?}"));
        collect(handle).await
    }

    pub async fn exec_on_tab(&self, tab: &str, sql: &str) {
        if let Some(err) = run_error(&self.run_on_tab(tab, sql).await) {
            panic!("[{}] on tab {tab:?} {sql:?}: {err:?}", self.kind.name());
        }
    }

    // First result set only, streamed the way the grid gets it.
    pub async fn stream(&self, sql: &str) -> Streamed {
        let events = self.run_on_tab("e2e-stream", sql).await;
        if let Some(err) = run_error(&events) {
            panic!("[{}] {sql:?}: {err:?}", self.kind.name());
        }
        Streamed::from_events(&events, 0)
    }

    pub async fn table_page(&self, req: TableDataRequest) -> Result<Page, QueryError> {
        let events = collect(self.app.query_table_stream(&self.id, "e2e-table", req).await?).await;
        if let Some(err) = run_error(&events) {
            return Err(err);
        }
        let summary = events
            .iter()
            .find_map(|event| match event {
                RunEvent::Result(result) => result.summary.clone(),
                _ => None,
            })
            .expect("a table page ends with its summary");
        Ok(Page { summary, rows: crate::support::rows(&events, 0) })
    }

    pub fn write(&self, name: &str, content: &str) -> String {
        let path = self.dir.path().join(name);
        std::fs::write(&path, content).unwrap();
        path.display().to_string()
    }

    // Same as the import dialog, mapping each column to itself with the preview's inferred types.
    pub async fn import_csv(&self, table: &str, columns: &[String], path: &str) -> ImportResult {
        let options = CsvOptions { has_header: true, ..Default::default() };
        let preview =
            self.app.preview_import_file(&self.id, path, &options).unwrap_or_else(|e| panic!("preview: {e:?}"));
        let request = CsvImportRequest {
            path: path.into(),
            schema: self.schema(),
            table: table.into(),
            options,
            mapping: columns.to_vec(),
            column_types: preview.inferred_types,
            ..Default::default()
        };
        let handle =
            self.app.import_csv(&self.id, "roundtrip", request).await.unwrap_or_else(|e| panic!("import: {e:?}"));
        let (_, done) = import_outcome(handle).await;
        assert!(done.error.is_empty(), "import aborted: {}", done.error);
        done.result.expect("an import result")
    }

    // Shut down first so tab transactions can't hold locks the drops need. The drops use a fresh
    // connection because the test may have made its own read-only.
    async fn cleanup(&self) {
        self.app.shutdown().await;
        let drops = std::mem::take(&mut *self.drops.lock().unwrap());
        if drops.is_empty() {
            return;
        }
        let mut cfg = self.kind.config();
        cfg.normalize();
        let Ok(engine) = Engine::connect(&cfg).await else { return };
        if let Ok(mut session) = engine.session().await {
            for sql in drops.iter().rev() {
                let _ = tokio::time::timeout(Duration::from_secs(15), session.buffered(sql, &Cancel::new())).await;
            }
        }
        engine.close().await;
    }
}

pub struct Page {
    pub summary: ResultSummary,
    pub rows: Vec<Vec<Option<String>>>,
}

impl Page {
    pub fn column(&self, index: usize) -> Vec<String> {
        self.rows.iter().map(|r| r[index].clone().unwrap_or_else(|| "NULL".into())).collect()
    }
}

pub struct Streamed {
    pub columns: Vec<String>,
    pub types: Vec<String>,
    pub chunks: Vec<Arc<ResultChunk>>,
}

impl Streamed {
    pub fn from_events(events: &[RunEvent], index: usize) -> Self {
        let mut streamed = Self { columns: Vec::new(), types: Vec::new(), chunks: Vec::new() };
        for event in events {
            match event {
                RunEvent::Meta { result_index, columns, .. } if *result_index == index => {
                    streamed.columns = columns.iter().map(|c| c.name.clone()).collect();
                    streamed.types = columns.iter().map(|c| c.type_name.clone()).collect();
                }
                RunEvent::Rows { result_index, chunk } if *result_index == index => streamed.chunks.push(chunk.clone()),
                _ => {}
            }
        }
        streamed
    }

    pub fn rows(&self) -> impl Iterator<Item = Vec<Cell<'_>>> {
        self.chunks
            .iter()
            .flat_map(|chunk| (0..chunk.rows()).map(move |r| (0..chunk.columns()).map(|c| chunk.cell(r, c)).collect()))
    }

    pub fn export(&self, format: ExportFormat) -> String {
        export_to_string(format, &self.columns, &self.types, None, None, self.rows())
    }
}

pub fn run_error(events: &[RunEvent]) -> Option<QueryError> {
    events.iter().find_map(|event| match event {
        RunEvent::Result(result) => result.error.clone(),
        RunEvent::Done { error, .. } => error.clone(),
        _ => None,
    })
}

pub fn meta_columns(events: &[RunEvent], index: usize) -> Vec<String> {
    events
        .iter()
        .find_map(|event| match event {
            RunEvent::Meta { result_index, columns, .. } if *result_index == index => {
                Some(columns.iter().map(|c| c.name.clone()).collect())
            }
            _ => None,
        })
        .unwrap_or_default()
}

// Drivers hand a COUNT(*) back as a number, or as text when a double would lose its precision.
pub fn count_value(result: &BufferedResult) -> i64 {
    match result.rows.first().and_then(|r| r.first()) {
        Some(Value::Int(n)) => *n,
        Some(Value::Float(f)) => *f as i64,
        Some(Value::Text(s)) => s.parse().unwrap_or_else(|_| panic!("count value {s:?}")),
        other => panic!("unexpected count value {other:?}"),
    }
}
