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
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Postgres => "postgres",
            Self::MySql => "mysql",
            Self::MariaDb => "mariadb",
        }
    }

    fn env(self, key: &str, default: &str) -> String {
        let prefix = match self {
            Self::Postgres => "PG",
            Self::MySql => "MYSQL",
            Self::MariaDb => "MARIADB",
        };
        std::env::var(format!("BARSQL_E2E_{prefix}_{key}")).unwrap_or_else(|_| default.to_string())
    }

    pub fn driver(self) -> DriverType {
        if self == Self::Postgres { DriverType::Postgres } else { DriverType::MySql }
    }

    pub fn config(self) -> ConnectionConfig {
        let (port, user, password) = match self {
            Self::Postgres => ("55432", "postgres", "postgres"),
            Self::MySql => ("33306", "root", "root"),
            Self::MariaDb => ("33307", "root", "root"),
        };
        ConnectionConfig {
            name: format!("e2e-{}", self.name()),
            driver: self.driver(),
            host: self.env("HOST", "127.0.0.1"),
            port: self.env("PORT", port).parse().expect("port"),
            database: self.env("DB", "barsql_test"),
            username: self.env("USER", user),
            password: self.env("PASSWORD", password),
            ssl_mode: if self == Self::Postgres { "disable".into() } else { String::new() },
            ..Default::default()
        }
    }
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

pub async fn run<F, Fut>(kind: Kind, body: F)
where
    F: FnOnce(Arc<E2e>) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
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

    pub fn schema(&self) -> String {
        if self.is_postgres() { "public".into() } else { self.kind.config().database }
    }

    pub fn qualified(&self, table: &str) -> String {
        qualified_table(&self.driver(), &self.schema(), table)
    }

    pub fn pk_column(&self) -> &'static str {
        if self.is_postgres() { "id SERIAL PRIMARY KEY" } else { "id INT AUTO_INCREMENT PRIMARY KEY" }
    }

    pub fn auto_pk_table(&self, table: &str) -> String {
        let name = if self.is_postgres() { "TEXT" } else { "VARCHAR(255)" };
        format!("CREATE TABLE {table} ({}, name {name} NOT NULL)", self.pk_column())
    }

    pub fn json_type(&self) -> &'static str {
        if self.is_postgres() { "jsonb" } else { "json" }
    }

    pub fn bool_type(&self) -> &'static str {
        if self.is_postgres() { "BOOLEAN" } else { "TINYINT(1)" }
    }

    // MySQL answers a killed bare SELECT SLEEP(n) with 1 instead of an error, so it sleeps per row.
    pub fn sleep_sql(&self, seconds: u32) -> String {
        match self.kind {
            Kind::Postgres => format!("SELECT pg_sleep({seconds})"),
            Kind::MySql => format!("SELECT SLEEP({seconds}) FROM (SELECT 1 AS x UNION ALL SELECT 2) AS t"),
            Kind::MariaDb => format!("SELECT SLEEP({seconds})"),
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
        export_to_string(format, &self.columns, &self.types, None, self.rows())
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
