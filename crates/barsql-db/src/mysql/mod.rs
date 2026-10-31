mod schema;
mod values;

use std::sync::Arc;
use std::time::{Duration, Instant};

use barsql_core::{ConnectionConfig, DriverType, MessageLevel, QueryError, ServerMessage, Value};
use barsql_sql::{first_keyword, strip_leading_comments};
use jiff::tz::{Offset, TimeZone};
use mysql_async::consts::StatusFlags;
use mysql_async::prelude::Queryable;
use mysql_async::{Column, Conn, Opts, OptsBuilder, Pool, PoolConstraints, PoolOpts, Row, SslOpts, Value as MyValue};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::event::{MAX_MESSAGES, ScriptEvent, Sink, StatementResult, emit};
use crate::postgres::port_or;
use crate::script::{self, Buffered, StatementRun, StatementRunner, summary_for_exec, summary_for_rows};
use crate::ssh::{Forward, SshOptions, TunnelSlot};
use crate::temporal::parse_offset;
use crate::tls::TlsMode;
use crate::{Cancel, ChunkBuilder, ColumnMeta, ColumnOrigin};

pub(crate) use values::push_binary as push_binary_value;
pub use values::{Decoder, type_name};

const MAX_CONNECTIONS: usize = 10;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
// Sessions start in UTC, as dumps do, whatever the server's zone. NOW() and TIMESTAMP literals follow it too.
const SESSION_SETUP: &str = "SET time_zone = '+00:00'";
// Makes the server refuse writes the SQL check can't see, like one inside a function.
const READ_ONLY_SETUP: &str = "SET SESSION TRANSACTION READ ONLY";

#[derive(Debug, Clone)]
pub struct MyConnectOptions {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
    pub database: String,
    pub schema: String,
    pub ssl_mode: String,
    pub ssh: Option<SshOptions>,
    pub read_only: bool,
}

// Verify against the real host name, even through a tunnel.
pub(crate) fn mysql_ssl_opts(ssl_mode: &str, host: &str) -> Option<SslOpts> {
    match TlsMode::mysql(ssl_mode) {
        TlsMode::Require => Some(SslOpts::default().with_danger_accept_invalid_certs(true)),
        TlsMode::VerifyFull => Some(SslOpts::default().with_danger_tls_hostname_override(Some(host.to_string()))),
        _ => None,
    }
}

impl MyConnectOptions {
    pub fn from_config(cfg: &ConnectionConfig) -> Result<Self, QueryError> {
        Ok(Self {
            host: cfg.host.clone(),
            port: port_or(cfg.port, 3306)?,
            user: cfg.username.clone(),
            password: cfg.password.clone(),
            database: cfg.database.clone(),
            schema: if cfg.schema.is_empty() { cfg.database.clone() } else { cfg.schema.clone() },
            ssl_mode: cfg.ssl_mode.clone(),
            ssh: if cfg.ssh.enabled { Some(SshOptions::from_config(&cfg.ssh)?) } else { None },
            read_only: cfg.read_only,
        })
    }
}

pub struct MyEngine {
    pub(crate) options: MyConnectOptions,
    opts: Opts,
    pool: Pool,
    permits: Arc<Semaphore>,
    _forward: Option<Forward>,
}

impl MyEngine {
    pub async fn connect(options: MyConnectOptions) -> Result<Arc<Self>, QueryError> {
        let forward = match &options.ssh {
            Some(ssh) => Some(TunnelSlot::new(ssh.clone()).forward(&options.host, options.port).await?),
            None => None,
        };
        let (host, port) = match &forward {
            Some(forward) => ("127.0.0.1".to_string(), forward.addr.port()),
            None => (options.host.clone(), options.port),
        };
        let ssl = mysql_ssl_opts(&options.ssl_mode, &options.host);
        let pool_opts = PoolOpts::default()
            .with_constraints(PoolConstraints::new(0, MAX_CONNECTIONS).expect("valid constraints"))
            .with_reset_connection(true)
            .with_inactive_connection_ttl(Duration::from_secs(60));
        let opts: Opts = OptsBuilder::default()
            .ip_or_hostname(host)
            .tcp_port(port)
            .user(Some(options.user.clone()))
            .pass(Some(options.password.clone()))
            .db_name(Some(options.schema.clone()).filter(|s| !s.is_empty()))
            .prefer_socket(false)
            .ssl_opts(ssl)
            .pool_opts(pool_opts)
            // Runs again each time the pool resets a connection.
            .setup(vec![SESSION_SETUP])
            .into();
        let pooled: Opts = if options.read_only {
            OptsBuilder::from_opts(opts.clone()).setup(vec![SESSION_SETUP, READ_ONLY_SETUP]).into()
        } else {
            opts.clone()
        };
        let pool = Pool::new(pooled);
        let engine = Arc::new(Self {
            options,
            opts,
            pool,
            permits: Arc::new(Semaphore::new(MAX_CONNECTIONS)),
            _forward: forward,
        });
        let conn = engine.get_conn().await?;
        drop(conn);
        Ok(engine)
    }

    pub fn read_only(&self) -> bool {
        self.options.read_only
    }

    pub fn default_schema(&self) -> &str {
        &self.options.schema
    }

    pub(crate) fn schema_or<'a>(&'a self, schema: &'a str) -> &'a str {
        if schema.is_empty() { &self.options.schema } else { schema }
    }

    pub(crate) async fn lease(self: &Arc<Self>) -> Result<MyLease, QueryError> {
        let permit = crate::pool::acquire(&self.permits, MAX_CONNECTIONS, crate::pool::LEASE_TIMEOUT).await?;
        let conn = self.get_conn().await?;
        Ok(MyLease { conn, _permit: permit })
    }

    pub async fn session(self: &Arc<Self>) -> Result<MySession, QueryError> {
        let lease = self.lease().await?;
        let id = lease.conn.id();
        Ok(MySession {
            lease,
            id,
            engine: self.clone(),
            in_transaction: false,
            reporting: false,
            time_zone: TimeZone::UTC,
        })
    }

    pub async fn close(&self) {
        self.permits.close();
        let _ = self.pool.clone().disconnect().await;
    }

    async fn get_conn(&self) -> Result<Conn, QueryError> {
        tokio::time::timeout(CONNECT_TIMEOUT, self.pool.get_conn())
            .await
            .map_err(|_| QueryError::message("dial tcp: i/o timeout"))?
            .map_err(my_error)
    }

    // Uses a fresh connection outside the pool so KILL works even when every pooled one is busy.
    fn server_cancel(self: &Arc<Self>, thread_id: u32) -> impl Fn() + Send + Sync + 'static {
        let opts = self.opts.clone();
        let runtime = tokio::runtime::Handle::try_current().ok();
        move || {
            let Some(runtime) = runtime.clone() else { return };
            let opts = opts.clone();
            runtime.spawn(async move {
                if let Ok(mut conn) = Conn::new(opts).await {
                    let _ = conn.query_drop(format!("KILL QUERY {thread_id}")).await;
                    let _ = conn.disconnect().await;
                }
            });
        }
    }
}

pub(crate) struct MyLease {
    pub(crate) conn: Conn,
    _permit: OwnedSemaphorePermit,
}

pub struct MySession {
    lease: MyLease,
    id: u32,
    engine: Arc<MyEngine>,
    in_transaction: bool,
    reporting: bool,
    time_zone: TimeZone,
}

impl MySession {
    pub fn connection_id(&self) -> u32 {
        self.id
    }

    // The driver drops the connection on an I/O error. MySQL's notice that it closed an idle one arrives out of
    // turn, which counts as one.
    pub fn is_broken(&self) -> bool {
        self.lease.conn.is_disconnected()
    }

    // The server rolls back what a closed connection had open.
    pub fn in_transaction(&self) -> bool {
        self.in_transaction && !self.is_broken()
    }

    pub fn read_only(&self) -> bool {
        self.engine.read_only()
    }

    pub async fn run_script(
        &mut self,
        statements: &[String],
        sink: &Sink,
        cancel: &Cancel,
    ) -> Result<usize, QueryError> {
        script::run_script(self, statements, sink, cancel).await
    }

    pub async fn buffered(&mut self, sql: &str, cancel: &Cancel) -> Result<Buffered, QueryError> {
        script::buffered(self, sql, cancel).await
    }

    pub async fn begin(&mut self) -> Result<(), QueryError> {
        self.lease.conn.query_drop("BEGIN").await.map_err(my_error)?;
        self.in_transaction = true;
        Ok(())
    }

    pub async fn commit(&mut self) -> Result<(), QueryError> {
        self.end_transaction("COMMIT").await
    }

    pub async fn rollback(&mut self) -> Result<(), QueryError> {
        self.end_transaction("ROLLBACK").await
    }

    // A refused COMMIT may end the transaction (read-only server) or not (XA), so the status flags decide.
    async fn end_transaction(&mut self, sql: &str) -> Result<(), QueryError> {
        let result = self.lease.conn.query_drop(sql).await.map_err(my_error);
        self.sync_transaction().await;
        result
    }

    // An UPDATE counts the rows it matched, not changed, as on the other engines.
    pub async fn execute_params(&mut self, sql: &str, params: &[Value]) -> Result<u64, QueryError> {
        let params: Vec<MyValue> = params.iter().map(my_value).collect();
        self.lease.conn.exec_drop(sql, params).await.map_err(my_error)?;
        Ok(matched_rows(&self.lease.conn.info()).unwrap_or_else(|| self.lease.conn.affected_rows()))
    }

    pub(crate) fn conn(&mut self) -> &mut Conn {
        &mut self.lease.conn
    }

    pub(crate) fn time_zone(&self) -> &TimeZone {
        &self.time_zone
    }

    // The status flags track every way a transaction opens or ends, like XA, AND CHAIN, DDL or a deadlock. An error
    // carries none and clears the driver's, so a ping fetches them.
    async fn sync_transaction(&mut self) {
        if self.lease.conn.last_ok_packet().is_none() && !self.is_broken() {
            let _ = self.lease.conn.ping().await;
        }
        self.in_transaction = self
            .lease
            .conn
            .last_ok_packet()
            .is_some_and(|ok| ok.status_flags().contains(StatusFlags::SERVER_STATUS_IN_TRANS));
    }

    async fn read_time_zone(&mut self) {
        if let Ok(Some(name)) = self.lease.conn.query_first::<String, _>("SELECT @@session.time_zone").await {
            self.time_zone = parse_time_zone(&name);
        }
    }
}

// SET time_zone, and code CALL or EXECUTE runs, can change the zone TIMESTAMPs come back in. Naming time_zone covers
// every form of SET and MariaDB's compound statements. Queries and writes can't set it, so they skip the scan.
fn may_set_time_zone(stmt: &str) -> bool {
    match first_keyword(strip_leading_comments(stmt)).as_str() {
        "CALL" | "EXECUTE" => true,
        "SELECT" | "WITH" | "TABLE" | "VALUES" | "INSERT" | "REPLACE" | "UPDATE" | "DELETE" | "LOAD" => false,
        _ => stmt.as_bytes().windows(9).any(|word| word.eq_ignore_ascii_case(b"time_zone")),
    }
}

// SHOW WARNINGS or ERRORS, which can't run code.
fn reads_diagnostics(stmt: &str) -> bool {
    let stmt = strip_leading_comments(stmt).trim_end_matches(|c: char| c == ';' || c.is_whitespace()).to_uppercase();
    let words: Vec<&str> = stmt.split_whitespace().collect();
    let (kind, limit) = match words.as_slice() {
        ["SHOW", "COUNT(*)", kind] => (*kind, &[][..]),
        ["SHOW", kind, limit @ ..] => (*kind, limit),
        _ => return false,
    };
    let number = |word: &&str| word.bytes().all(|b| b.is_ascii_digit() || b == b',');
    matches!(kind, "WARNINGS" | "ERRORS")
        && match limit {
            [] => true,
            ["LIMIT", numbers @ ..] => (1..=2).contains(&numbers.len()) && numbers.iter().all(number),
            _ => false,
        }
}

// An UPDATE's info line, "Rows matched: 1  Changed: 0  Warnings: 0".
fn matched_rows(info: &str) -> Option<u64> {
    info.strip_prefix("Rows matched:")?.split_whitespace().next()?.parse().ok()
}

// @@time_zone is an offset like +05:00, a name from the server's time zone tables, or SYSTEM, the server OS's zone.
// That one, and a name jiff doesn't know, give UTC.
fn parse_time_zone(name: &str) -> TimeZone {
    let offset = name.starts_with(['+', '-']).then(|| parse_offset(name)).flatten();
    match offset.and_then(|seconds| Offset::from_seconds(seconds).ok()) {
        Some(offset) => TimeZone::fixed(offset),
        None => TimeZone::get(name).unwrap_or(TimeZone::UTC),
    }
}

impl StatementRunner for MySession {
    fn driver(&self) -> DriverType {
        DriverType::MySql
    }

    fn set_reporting(&mut self, on: bool) {
        self.reporting = on;
    }

    async fn stream_statement(
        &mut self,
        stmt: &str,
        first_index: usize,
        batch_rows: usize,
        sink: &Sink,
        cancel: &Cancel,
    ) -> StatementRun {
        // A function can make the session writable, so each statement resets it. Not SHOW WARNINGS: SET clears them.
        if self.read_only() && !reads_diagnostics(stmt) {
            let _ = self.lease.conn.query_drop(READ_ONLY_SETUP).await;
        }
        let run = {
            // Only while the statement runs, so a late cancel can't kill the queries after it.
            let _server_cancel = cancel.on_server(self.engine.server_cancel(self.id));
            let conn = &mut self.lease.conn;
            stream_query(conn, stmt, first_index, batch_rows, self.reporting, &self.time_zone, sink, cancel).await
        };
        self.sync_transaction().await;
        if may_set_time_zone(stmt) && !self.is_broken() {
            self.read_time_zone().await;
        }
        run
    }
}

// With `reporting`, a result's info line (like "Rows matched: 1  Changed: 1  Warnings: 0") comes before it as a
// message, and the statement's warnings and notes after its last result.
#[allow(clippy::too_many_arguments)]
async fn stream_query(
    conn: &mut Conn,
    stmt: &str,
    first_index: usize,
    batch_rows: usize,
    reporting: bool,
    zone: &TimeZone,
    sink: &Sink,
    cancel: &Cancel,
) -> StatementRun {
    let started = Instant::now();
    let mut index = first_index;
    let mut warnings = 0;
    let info = |result_index: usize, text: &str| {
        let message = ServerMessage::new(MessageLevel::Info, text);
        let event = ScriptEvent::Messages { result_index, messages: vec![message], dropped: 0 };
        let shown = reporting && !text.is_empty();
        async move {
            if shown {
                emit(sink, event).await;
            }
        }
    };
    let fail = |index: usize, error: QueryError| async move {
        emit(
            sink,
            ScriptEvent::Result(Box::new(StatementResult {
                result_index: index,
                statement: stmt.into(),
                error: Some(error.clone()),
                ..Default::default()
            })),
        )
        .await;
        StatementRun { sets: index - first_index + 1, error: Some(error) }
    };
    let mut result = match conn.query_iter(stmt).await {
        Ok(result) => result,
        Err(err) => return fail(index, mark_cancelled(my_error(err), cancel)).await,
    };
    loop {
        let columns: Arc<[Column]> = result.columns().unwrap_or_else(|| Arc::from(Vec::new()));
        if columns.is_empty() {
            let affected = result.affected_rows();
            let line = result.info().into_owned();
            warnings = result.warnings().saturating_add(warnings);
            if let Err(err) = result.next().await {
                return fail(index, mark_cancelled(my_error(err), cancel)).await;
            }
            info(index, &line).await;
            emit(
                sink,
                ScriptEvent::Result(Box::new(StatementResult {
                    result_index: index,
                    statement: stmt.into(),
                    summary: Some(summary_for_exec(affected, started)),
                    ..Default::default()
                })),
            )
            .await;
        } else {
            let meta: Arc<[ColumnMeta]> = columns
                .iter()
                .map(|c| ColumnMeta { name: c.name_str().into_owned(), type_name: type_name(c), origin: origin(c) })
                .collect();
            let decoders: Vec<Decoder> = columns.iter().map(Decoder::for_column).collect();
            emit(sink, ScriptEvent::Meta { result_index: index, columns: meta.clone() }).await;
            let mut builder = ChunkBuilder::new(meta.len(), batch_rows.min(1024));
            let mut rows = 0usize;
            let failed = loop {
                match result.next().await {
                    Ok(Some(row)) => {
                        push_text_row(&row, &decoders, zone, &mut builder);
                        rows += 1;
                        if builder.rows() >= batch_rows {
                            let full =
                                std::mem::replace(&mut builder, ChunkBuilder::new(meta.len(), batch_rows.min(1024)));
                            emit(sink, ScriptEvent::Rows { result_index: index, chunk: Arc::new(full.finish()) }).await;
                        }
                    }
                    Ok(None) => break None,
                    Err(err) => break Some(mark_cancelled(my_error(err), cancel)),
                }
            };
            // Rows that came before an error or a cancel still show, so the count matches the grid.
            if !builder.is_empty() {
                emit(sink, ScriptEvent::Rows { result_index: index, chunk: Arc::new(builder.finish()) }).await;
            }
            if failed.is_none() {
                warnings = result.warnings().saturating_add(warnings);
            }
            emit(
                sink,
                ScriptEvent::Result(Box::new(StatementResult {
                    result_index: index,
                    statement: stmt.into(),
                    summary: Some(summary_for_rows(&meta, rows, started)),
                    error: failed.clone(),
                    ..Default::default()
                })),
            )
            .await;
            if let Some(error) = failed {
                return StatementRun { sets: index - first_index + 1, error: Some(error) };
            }
        }
        index += 1;
        if result.is_empty() {
            break;
        }
    }
    drop(result);
    if reporting && warnings > 0 && !cancel.is_cancelled() {
        let messages = show_warnings(conn).await;
        let dropped = usize::from(warnings).saturating_sub(messages.len());
        emit(sink, ScriptEvent::Messages { result_index: index - 1, messages, dropped }).await;
    }
    StatementRun { sets: index - first_index, error: None }
}

// The server only counts warnings and notes in the result. SHOW WARNINGS lists them, up to max_error_count.
async fn show_warnings(conn: &mut Conn) -> Vec<ServerMessage> {
    let rows: Vec<(String, u32, String)> = conn.query("SHOW WARNINGS").await.unwrap_or_default();
    rows.into_iter()
        .take(MAX_MESSAGES)
        .map(|(level, code, text)| ServerMessage {
            level: if level.eq_ignore_ascii_case("note") { MessageLevel::Notice } else { MessageLevel::Warning },
            code: code.to_string(),
            text,
            ..Default::default()
        })
        .collect()
}

fn push_text_row(row: &Row, decoders: &[Decoder], zone: &TimeZone, builder: &mut ChunkBuilder) {
    for (i, decoder) in decoders.iter().enumerate() {
        match row.as_ref(i) {
            Some(MyValue::Bytes(bytes)) => decoder.push(bytes, zone, builder),
            Some(MyValue::NULL) | None => builder.push_null(),
            Some(other) => values::push_binary(other, decoder, zone, builder),
        }
    }
    builder.end_row();
}

fn mark_cancelled(mut error: QueryError, cancel: &Cancel) -> QueryError {
    if cancel.is_cancelled() {
        error.cancelled = true;
    }
    error
}

pub(crate) fn my_value(value: &Value) -> MyValue {
    match value {
        Value::Null => MyValue::NULL,
        Value::Bool(b) => MyValue::Int(i64::from(*b)),
        Value::Int(i) => MyValue::Int(*i),
        Value::Float(f) => MyValue::Double(*f),
        Value::Text(s) => MyValue::Bytes(s.clone().into_bytes()),
    }
}

pub(crate) fn my_error(err: mysql_async::Error) -> QueryError {
    match err {
        mysql_async::Error::Server(server) => QueryError {
            message: server.message,
            code: if server.code != 0 { server.code.to_string() } else { server.state.trim().to_string() },
            ..Default::default()
        },
        other => QueryError::message(other.to_string()),
    }
}

// Empty for an expression.
fn origin(column: &Column) -> Option<ColumnOrigin> {
    let (table, name) = (column.org_table_str(), column.org_name_str());
    (!table.is_empty() && !name.is_empty()).then(|| ColumnOrigin::Named {
        schema: column.schema_str().into_owned(),
        table: table.into_owned(),
        column: name.into_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> ConnectionConfig {
        ConnectionConfig {
            driver: DriverType::MySql,
            host: "db.example.com".into(),
            database: "blog".into(),
            username: "root".into(),
            ..Default::default()
        }
    }

    #[test]
    fn disabled_ssh_opens_no_tunnel() {
        let mut cfg = config();
        cfg.ssh = barsql_core::SshConfig {
            enabled: false,
            host: "unreachable.invalid".into(),
            username: "u".into(),
            auth: barsql_core::config::SSH_AUTH_AGENT.into(),
            ..Default::default()
        };
        assert!(MyConnectOptions::from_config(&cfg).unwrap().ssh.is_none());
        cfg.driver = DriverType::Postgres;
        assert!(crate::postgres::PgConnectOptions::from_config(&cfg).unwrap().ssh.is_none());
    }

    #[test]
    fn tls_modes_map_to_ssl_options() {
        let require = mysql_ssl_opts("require", "db.example.com").expect("require uses TLS");
        assert!(require.accept_invalid_certs());
        let full = mysql_ssl_opts("verify-full", "db.example.com").expect("verify-full uses TLS");
        assert!(!full.accept_invalid_certs());
        assert_eq!(full.tls_hostname_override(), Some("db.example.com"));
        assert!(mysql_ssl_opts("disable", "db.example.com").is_none());
        assert!(mysql_ssl_opts("", "db.example.com").is_none());
    }

    #[test]
    fn set_call_and_execute_may_change_the_time_zone() {
        for stmt in [
            "SET time_zone = '+05:00'",
            "set @@SESSION.Time_Zone = 'UTC'",
            "SET autocommit = 0, time_zone = 'Europe/Sofia'",
            "/* zone */ CALL barsql.set_zone()",
            "execute set_zone",
            "BEGIN NOT ATOMIC SET time_zone = '+01:00'; END",
        ] {
            assert!(may_set_time_zone(stmt), "{stmt}");
        }
        for stmt in [
            "SELECT 1",
            "SET @zone = '+05:00'",
            "BEGIN",
            "INSERT INTO users (id, time_zone) VALUES (1, 'UTC')",
            "SELECT time_zone FROM users",
        ] {
            assert!(!may_set_time_zone(stmt), "{stmt}");
        }
    }

    #[test]
    fn updates_count_the_rows_they_matched() {
        assert_eq!(matched_rows("Rows matched: 1  Changed: 0  Warnings: 0"), Some(1));
        assert_eq!(matched_rows("Rows matched: 12  Changed: 3  Warnings: 1"), Some(12));
        assert_eq!(matched_rows("Records: 3  Duplicates: 0  Warnings: 0"), None);
        assert_eq!(matched_rows(""), None);
    }

    #[test]
    fn time_zones_are_offsets_or_names() {
        // 2024-02-29T13:45:30Z, in winter.
        let instant = jiff::Timestamp::from_second(1_709_214_330).unwrap();
        let offset = |name: &str| parse_time_zone(name).to_offset(instant).seconds();
        assert_eq!(offset("+05:00"), 5 * 3600);
        assert_eq!(offset("-03:30"), -(3 * 3600 + 1800));
        assert_eq!(offset("+00:00"), 0);
        assert_eq!(offset("Europe/Sofia"), 2 * 3600);
        assert_eq!(offset("europe/sofia"), 2 * 3600);
        assert_eq!(offset("UTC"), 0);
        assert_eq!(offset("SYSTEM"), 0);
        assert_eq!(offset("Mars/Olympus_Mons"), 0);
    }

    #[test]
    fn only_warning_lists_skip_the_read_only_reset() {
        for stmt in ["SHOW WARNINGS", "show errors;", "-- why\nSHOW COUNT(*) WARNINGS", "SHOW WARNINGS LIMIT 0, 5"] {
            assert!(reads_diagnostics(stmt), "{stmt}");
        }
        for stmt in ["SHOW TABLES WHERE purge() OR WARNINGS", "SHOW WARNINGS LIMIT f()", "SELECT 1", "SHOW"] {
            assert!(!reads_diagnostics(stmt), "{stmt}");
        }
    }
}
