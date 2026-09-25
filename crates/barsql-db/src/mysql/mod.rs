mod schema;
mod values;

use std::sync::Arc;
use std::time::{Duration, Instant};

use barsql_core::{ConnectionConfig, DriverType, QueryError, Value};
use barsql_sql::{first_keyword, strip_leading_comments};
use mysql_async::prelude::Queryable;
use mysql_async::{Column, Conn, Opts, OptsBuilder, Pool, PoolConstraints, PoolOpts, Row, SslOpts, Value as MyValue};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::event::{ScriptEvent, Sink, StatementResult, emit};
use crate::postgres::port_or;
use crate::script::{self, Buffered, StatementRun, StatementRunner, summary_for_exec, summary_for_rows};
use crate::ssh::{Forward, SshOptions, TunnelSlot};
use crate::tls::TlsMode;
use crate::{Cancel, ChunkBuilder, ColumnMeta};

pub(crate) use values::push_binary as push_binary_value;
pub use values::{Decoder, type_name};

const MAX_CONNECTIONS: usize = 10;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

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
            .into();
        let pool = Pool::new(opts.clone());
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
        let permit =
            self.permits.clone().acquire_owned().await.map_err(|_| QueryError::message("connection is closed"))?;
        let conn = self.get_conn().await?;
        Ok(MyLease { conn, _permit: permit })
    }

    pub async fn session(self: &Arc<Self>) -> Result<MySession, QueryError> {
        let lease = self.lease().await?;
        let id = lease.conn.id();
        Ok(MySession { lease, id, engine: self.clone(), in_transaction: false })
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
}

impl MySession {
    pub fn connection_id(&self) -> u32 {
        self.id
    }

    pub fn is_broken(&self) -> bool {
        false
    }

    pub fn in_transaction(&self) -> bool {
        self.in_transaction
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
        self.in_transaction = false;
        self.lease.conn.query_drop("COMMIT").await.map_err(my_error)
    }

    pub async fn rollback(&mut self) -> Result<(), QueryError> {
        self.in_transaction = false;
        self.lease.conn.query_drop("ROLLBACK").await.map_err(my_error)
    }

    pub async fn execute_params(&mut self, sql: &str, params: &[Value]) -> Result<u64, QueryError> {
        let params: Vec<MyValue> = params.iter().map(my_value).collect();
        self.lease.conn.exec_drop(sql, params).await.map_err(my_error)?;
        Ok(self.lease.conn.affected_rows())
    }

    pub(crate) fn conn(&mut self) -> &mut Conn {
        &mut self.lease.conn
    }

    // Implicit-commit statements like DDL end the transaction too.
    fn track_transaction(&mut self, stmt: &str) {
        let keyword = first_keyword(strip_leading_comments(stmt));
        let upper = keyword.as_str();
        let rest = barsql_sql::to_upper(strip_leading_comments(stmt));
        match upper {
            "BEGIN" | "START" => self.in_transaction = true,
            "COMMIT" => self.in_transaction = false,
            "ROLLBACK" if !rest.contains(" TO ") => self.in_transaction = false,
            "CREATE" | "ALTER" | "DROP" | "TRUNCATE" | "RENAME" | "LOCK" | "UNLOCK" | "GRANT" | "REVOKE" => {
                self.in_transaction = false
            }
            _ => {}
        }
    }
}

impl StatementRunner for MySession {
    fn driver(&self) -> DriverType {
        DriverType::MySql
    }

    async fn stream_statement(
        &mut self,
        stmt: &str,
        first_index: usize,
        batch_rows: usize,
        sink: &Sink,
        cancel: &Cancel,
    ) -> StatementRun {
        let _server_cancel = cancel.on_server(self.engine.server_cancel(self.id));
        let run = stream_query(&mut self.lease.conn, stmt, first_index, batch_rows, sink, cancel).await;
        if run.error.is_none() {
            self.track_transaction(stmt);
        }
        run
    }
}

async fn stream_query(
    conn: &mut Conn,
    stmt: &str,
    first_index: usize,
    batch_rows: usize,
    sink: &Sink,
    cancel: &Cancel,
) -> StatementRun {
    let started = Instant::now();
    let mut index = first_index;
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
            if let Err(err) = result.next().await {
                return fail(index, mark_cancelled(my_error(err), cancel)).await;
            }
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
                .map(|c| ColumnMeta { name: c.name_str().into_owned(), type_name: type_name(c) })
                .collect();
            let decoders: Vec<Decoder> = columns.iter().map(Decoder::for_column).collect();
            emit(sink, ScriptEvent::Meta { result_index: index, columns: meta.clone() }).await;
            let mut builder = ChunkBuilder::new(meta.len(), batch_rows.min(1024));
            let mut rows = 0usize;
            loop {
                match result.next().await {
                    Ok(Some(row)) => {
                        push_text_row(&row, &decoders, &mut builder);
                        rows += 1;
                        if builder.rows() >= batch_rows {
                            let full =
                                std::mem::replace(&mut builder, ChunkBuilder::new(meta.len(), batch_rows.min(1024)));
                            emit(sink, ScriptEvent::Rows { result_index: index, chunk: Arc::new(full.finish()) }).await;
                        }
                    }
                    Ok(None) => break,
                    Err(err) => {
                        let error = mark_cancelled(my_error(err), cancel);
                        emit(
                            sink,
                            ScriptEvent::Result(Box::new(StatementResult {
                                result_index: index,
                                statement: stmt.into(),
                                summary: Some(summary_for_rows(&meta, rows, started)),
                                error: Some(error.clone()),
                                ..Default::default()
                            })),
                        )
                        .await;
                        return StatementRun { sets: index - first_index + 1, error: Some(error) };
                    }
                }
            }
            if !builder.is_empty() {
                emit(sink, ScriptEvent::Rows { result_index: index, chunk: Arc::new(builder.finish()) }).await;
            }
            emit(
                sink,
                ScriptEvent::Result(Box::new(StatementResult {
                    result_index: index,
                    statement: stmt.into(),
                    summary: Some(summary_for_rows(&meta, rows, started)),
                    ..Default::default()
                })),
            )
            .await;
        }
        index += 1;
        if result.is_empty() {
            break;
        }
    }
    StatementRun { sets: index - first_index, error: None }
}

fn push_text_row(row: &Row, decoders: &[Decoder], builder: &mut ChunkBuilder) {
    for (i, decoder) in decoders.iter().enumerate() {
        match row.as_ref(i) {
            Some(MyValue::Bytes(bytes)) => decoder.push(bytes, builder),
            Some(MyValue::NULL) | None => builder.push_null(),
            Some(other) => values::push_binary(other, decoder, builder),
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
}
