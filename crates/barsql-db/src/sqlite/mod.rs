mod schema;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use barsql_core::{ConnectionConfig, DriverType, QueryError, Value};
use barsql_sql::{ExplainStrategy, PlanRows};
use rusqlite::types::{ToSqlOutput, ValueRef};
use rusqlite::{Connection, InterruptHandle, OpenFlags, ToSql, params_from_iter};
use tokio::sync::{Mutex, OwnedMutexGuard};

use crate::event::{ScriptEvent, Sink, StatementResult};
use crate::lite::{LiteRef, LiteRows, LiteSource, LiteValue, values};
use crate::script::{self, Buffered, StatementRun, StatementRunner, summary_for_exec, summary_for_rows};
use crate::{Cancel, ChunkBuilder, ColumnMeta};

pub use crate::lite::values::parse_time;

#[derive(Debug, Clone)]
pub struct SqliteConnectOptions {
    pub path: PathBuf,
    pub read_only: bool,
}

impl SqliteConnectOptions {
    // Any `?` query is dropped, so a stored path cannot smuggle in open flags or pragmas.
    pub fn from_config(cfg: &ConnectionConfig) -> Self {
        let path = cfg.file_path.split('?').next().unwrap_or_default();
        Self { path: PathBuf::from(path), read_only: cfg.read_only }
    }
}

type Guard = OwnedMutexGuard<Connection>;

// One connection per file. Runs and transactions hold it exclusively.
pub struct SqliteEngine {
    pub(crate) options: SqliteConnectOptions,
    conn: Arc<Mutex<Connection>>,
    interrupt: Arc<InterruptHandle>,
}

impl SqliteEngine {
    pub async fn connect(options: SqliteConnectOptions) -> Result<Arc<Self>, QueryError> {
        let opened = options.clone();
        let conn = tokio::task::spawn_blocking(move || -> Result<Connection, QueryError> {
            let conn = Connection::open_with_flags(
                &opened.path,
                OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )
            .map_err(|err| lite_error(&err))?;
            conn.execute_batch("PRAGMA foreign_keys = ON").map_err(|err| lite_error(&err))?;
            if opened.read_only {
                conn.execute_batch("PRAGMA query_only = true").map_err(|err| lite_error(&err))?;
            }
            conn.query_row("SELECT 1", [], |_| Ok(())).map_err(|err| lite_error(&err))?;
            Ok(conn)
        })
        .await
        .map_err(|err| QueryError::message(err.to_string()))??;
        let interrupt = Arc::new(conn.get_interrupt_handle());
        Ok(Arc::new(Self { options, conn: Arc::new(Mutex::new(conn)), interrupt }))
    }

    pub fn read_only(&self) -> bool {
        self.options.read_only
    }

    pub fn default_schema(&self) -> &str {
        "main"
    }

    pub async fn session(self: &Arc<Self>) -> Result<SqliteSession, QueryError> {
        Ok(SqliteSession { engine: self.clone(), guard: None, in_transaction: false })
    }

    pub(crate) async fn with_conn<T: Send + 'static>(
        self: &Arc<Self>,
        work: impl FnOnce(&Connection) -> Result<T, QueryError> + Send + 'static,
    ) -> Result<T, QueryError> {
        let guard = self.conn.clone().lock_owned().await;
        tokio::task::spawn_blocking(move || work(&guard)).await.map_err(|err| QueryError::message(err.to_string()))?
    }

    fn server_cancel(&self) -> impl Fn() + Send + Sync + 'static {
        let interrupt = self.interrupt.clone();
        move || interrupt.interrupt()
    }
}

pub struct SqliteSession {
    engine: Arc<SqliteEngine>,
    guard: Option<Guard>,
    in_transaction: bool,
}

impl SqliteSession {
    pub fn is_broken(&self) -> bool {
        false
    }

    pub fn in_transaction(&self) -> bool {
        self.in_transaction
    }

    pub fn read_only(&self) -> bool {
        self.engine.read_only()
    }

    async fn hold(&mut self) {
        if self.guard.is_none() {
            self.guard = Some(self.engine.conn.clone().lock_owned().await);
        }
    }

    fn release(&mut self) {
        if !self.in_transaction {
            self.guard = None;
        }
    }

    async fn blocking<T: Send + 'static>(
        &mut self,
        work: impl FnOnce(&Connection) -> T + Send + 'static,
    ) -> Result<T, QueryError> {
        self.hold().await;
        let guard = self.guard.take().expect("connection held");
        let (guard, out) = tokio::task::spawn_blocking(move || {
            let out = work(&guard);
            (guard, out)
        })
        .await
        .map_err(|err| QueryError::message(err.to_string()))?;
        self.in_transaction = !guard.is_autocommit();
        self.guard = Some(guard);
        Ok(out)
    }

    pub async fn run_script(
        &mut self,
        statements: &[String],
        sink: &Sink,
        cancel: &Cancel,
    ) -> Result<usize, QueryError> {
        self.hold().await;
        let result = script::run_script(self, statements, sink, cancel).await;
        self.release();
        result
    }

    pub async fn buffered(&mut self, sql: &str, cancel: &Cancel) -> Result<Buffered, QueryError> {
        self.hold().await;
        let result = script::buffered(self, sql, cancel).await;
        self.release();
        result
    }

    pub async fn plan_rows(&mut self, strategy: &ExplainStrategy, cancel: &Cancel) -> Result<PlanRows, QueryError> {
        self.hold().await;
        let result = script::plan_rows(self, strategy, cancel).await;
        self.release();
        result
    }

    pub async fn begin(&mut self) -> Result<(), QueryError> {
        self.blocking(|conn| conn.execute_batch("BEGIN")).await?.map_err(|err| lite_error(&err))?;
        self.in_transaction = true;
        Ok(())
    }

    pub async fn commit(&mut self) -> Result<(), QueryError> {
        let result = self.blocking(|conn| conn.execute_batch("COMMIT")).await?.map_err(|err| lite_error(&err));
        self.release();
        result
    }

    pub async fn rollback(&mut self) -> Result<(), QueryError> {
        let result = self.blocking(|conn| conn.execute_batch("ROLLBACK")).await?.map_err(|err| lite_error(&err));
        self.release();
        result
    }

    pub async fn execute_params(&mut self, sql: &str, params: &[Value]) -> Result<u64, QueryError> {
        let sql = sql.to_string();
        let params: Vec<rusqlite::types::Value> = params.iter().map(lite_value).collect();
        let result = self
            .blocking(move |conn| conn.execute(&sql, params_from_iter(params.iter())).map(|n| n as u64))
            .await?
            .map_err(|err| lite_error(&err));
        self.release();
        result
    }
}

impl Drop for SqliteSession {
    fn drop(&mut self) {
        // A session dropped mid-transaction must not leave the shared connection inside it.
        if let Some(guard) = self.guard.take()
            && !guard.is_autocommit()
        {
            let _ = guard.execute_batch("ROLLBACK");
        }
    }
}

impl StatementRunner for SqliteSession {
    fn driver(&self) -> DriverType {
        DriverType::Sqlite
    }

    async fn stream_statement(
        &mut self,
        stmt: &str,
        first_index: usize,
        batch_rows: usize,
        sink: &Sink,
        cancel: &Cancel,
    ) -> StatementRun {
        let _server_cancel = cancel.on_server(self.engine.server_cancel());
        let stmt = stmt.to_string();
        let sink = sink.clone();
        let cancel = cancel.clone();
        match self.blocking(move |conn| stream_statement(conn, &stmt, first_index, batch_rows, &sink, &cancel)).await {
            Ok(run) => run,
            Err(error) => StatementRun { sets: 0, error: Some(error) },
        }
    }
}

fn stream_statement(
    conn: &Connection,
    stmt: &str,
    index: usize,
    batch_rows: usize,
    sink: &Sink,
    cancel: &Cancel,
) -> StatementRun {
    let started = Instant::now();
    // sqlite3_interrupt is cleared when a statement starts, so a cancel landing just before it would be lost.
    // The progress handler polls the token for the whole run instead.
    let watch = cancel.clone();
    let _ = conn.progress_handler(PROGRESS_OPS, Some(move || watch.is_cancelled()));
    let _progress = ProgressGuard(conn);
    let emit = |event: ScriptEvent| {
        let _ = sink.send_blocking(event);
    };
    let fail = |error: QueryError, summary| {
        let error = mark_cancelled(error, cancel);
        emit(ScriptEvent::Result(Box::new(StatementResult {
            result_index: index,
            statement: stmt.into(),
            summary,
            error: Some(error.clone()),
            ..Default::default()
        })));
        StatementRun { sets: 1, error: Some(error) }
    };
    if cancel.is_cancelled() {
        return fail(QueryError::cancelled(), None);
    }
    let mut prepared = match conn.prepare(stmt) {
        Ok(prepared) => prepared,
        Err(err) => return fail(lite_error(&err), None),
    };
    let width = prepared.column_count();
    if width == 0 {
        return match prepared.raw_execute() {
            Ok(_) => {
                emit(ScriptEvent::Result(Box::new(StatementResult {
                    result_index: index,
                    statement: stmt.into(),
                    summary: Some(summary_for_exec(conn.changes(), started)),
                    ..Default::default()
                })));
                StatementRun { sets: 1, error: None }
            }
            Err(err) => fail(lite_error(&err), None),
        };
    }
    let columns: Arc<[ColumnMeta]> = prepared
        .columns()
        .iter()
        .map(|c| ColumnMeta { name: c.name().to_string(), type_name: c.decl_type().unwrap_or("").to_uppercase() })
        .collect();
    emit(ScriptEvent::Meta { result_index: index, columns: columns.clone() });
    let time_columns: Vec<bool> = columns.iter().map(|c| values::is_time_type(&c.type_name)).collect();
    let mut rows = prepared.raw_query();
    let mut builder = ChunkBuilder::new(width, batch_rows.min(1024));
    let mut count = 0usize;
    loop {
        match rows.next() {
            Ok(Some(row)) => {
                for (i, time) in time_columns.iter().enumerate() {
                    match row.get_ref(i) {
                        Ok(value) => values::push(lite_ref(value), *time, &mut builder),
                        Err(_) => builder.push_null(),
                    }
                }
                builder.end_row();
                count += 1;
                if builder.rows() >= batch_rows {
                    let full = std::mem::replace(&mut builder, ChunkBuilder::new(width, batch_rows.min(1024)));
                    emit(ScriptEvent::Rows { result_index: index, chunk: Arc::new(full.finish()) });
                }
            }
            Ok(None) => break,
            Err(err) => return fail(lite_error(&err), Some(summary_for_rows(&columns, count, started))),
        }
    }
    if !builder.is_empty() {
        emit(ScriptEvent::Rows { result_index: index, chunk: Arc::new(builder.finish()) });
    }
    emit(ScriptEvent::Result(Box::new(StatementResult {
        result_index: index,
        statement: stmt.into(),
        summary: Some(summary_for_rows(&columns, count, started)),
        ..Default::default()
    })));
    StatementRun { sets: 1, error: None }
}

const PROGRESS_OPS: i32 = 1000;

struct ProgressGuard<'c>(&'c Connection);

impl Drop for ProgressGuard<'_> {
    fn drop(&mut self) {
        let _ = self.0.progress_handler(0, None::<fn() -> bool>);
    }
}

fn mark_cancelled(mut error: QueryError, cancel: &Cancel) -> QueryError {
    if cancel.is_cancelled() {
        error.cancelled = true;
    }
    error
}

pub(crate) fn lite_value(value: &Value) -> rusqlite::types::Value {
    use rusqlite::types::Value as V;
    match value {
        Value::Null => V::Null,
        Value::Bool(b) => V::Integer(i64::from(*b)),
        Value::Int(i) => V::Integer(*i),
        Value::Float(f) => V::Real(*f),
        Value::Text(s) => V::Text(s.clone()),
    }
}

fn lite_ref(value: ValueRef<'_>) -> LiteRef<'_> {
    match value {
        ValueRef::Null => LiteRef::Null,
        ValueRef::Integer(i) => LiteRef::Integer(i),
        ValueRef::Real(f) => LiteRef::Real(f),
        ValueRef::Text(bytes) => LiteRef::Text(bytes),
        ValueRef::Blob(bytes) => LiteRef::Blob(bytes),
    }
}

fn owned(value: ValueRef<'_>) -> LiteValue {
    match value {
        ValueRef::Null => LiteValue::Null,
        ValueRef::Integer(i) => LiteValue::Integer(i),
        ValueRef::Real(f) => LiteValue::Real(f),
        ValueRef::Text(bytes) => LiteValue::Text(String::from_utf8_lossy(bytes).into_owned()),
        ValueRef::Blob(bytes) => LiteValue::Blob(bytes.to_vec()),
    }
}

struct Param<'a>(&'a LiteValue);

impl ToSql for Param<'_> {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::Borrowed(match self.0 {
            LiteValue::Null => ValueRef::Null,
            LiteValue::Integer(i) => ValueRef::Integer(*i),
            LiteValue::Real(f) => ValueRef::Real(*f),
            LiteValue::Text(s) => ValueRef::Text(s.as_bytes()),
            LiteValue::Blob(b) => ValueRef::Blob(b),
        }))
    }
}

// The file's connection as the catalog's source.
pub(crate) struct Rusqlite<'c>(pub(crate) &'c Connection);

impl LiteSource for Rusqlite<'_> {
    fn query(&mut self, sql: &str, params: &[LiteValue]) -> Result<LiteRows, QueryError> {
        let mut stmt = self.0.prepare(sql).map_err(|e| lite_error(&e))?;
        let columns: Vec<String> = stmt.column_names().into_iter().map(String::from).collect();
        let width = columns.len();
        let mut rows = stmt.query(params_from_iter(params.iter().map(Param))).map_err(|e| lite_error(&e))?;
        let mut out = LiteRows { columns, rows: Vec::new() };
        while let Some(row) = rows.next().map_err(|e| lite_error(&e))? {
            out.rows.push((0..width).map(|i| row.get_ref(i).map_or(LiteValue::Null, owned)).collect());
        }
        Ok(out)
    }
}

// Formatted as "<errstr>: <errmsg> (<extended code>)".
pub(crate) fn lite_error(err: &rusqlite::Error) -> QueryError {
    let (code, text) = match err {
        rusqlite::Error::SqliteFailure(code, message) => (code, message.clone().unwrap_or_else(|| code.to_string())),
        rusqlite::Error::SqlInputError { error, msg, .. } => (error, msg.clone()),
        other => return QueryError::message(other.to_string()),
    };
    let extended = code.extended_code;
    QueryError {
        message: format!("{}: {text} ({extended})", error_string(extended & 0xff)),
        code: extended.to_string(),
        ..Default::default()
    }
}

fn error_string(primary: i32) -> &'static str {
    match primary {
        1 => "SQL logic error",
        2 => "internal logic error",
        3 => "access permission denied",
        4 => "query aborted",
        5 => "database is locked",
        6 => "database table is locked",
        7 => "out of memory",
        8 => "attempt to write a readonly database",
        9 => "interrupted",
        10 => "disk I/O error",
        11 => "database disk image is malformed",
        12 => "unknown operation",
        13 => "database or disk is full",
        14 => "unable to open database file",
        15 => "locking protocol",
        17 => "database schema has changed",
        18 => "string or blob too big",
        19 => "constraint failed",
        20 => "datatype mismatch",
        21 => "bad parameter or other API misuse",
        23 => "authorization denied",
        25 => "column index out of range",
        26 => "file is not a database",
        _ => "unknown error",
    }
}
