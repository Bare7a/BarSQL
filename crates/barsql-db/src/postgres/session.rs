use std::sync::Arc;
use std::time::{Duration, Instant};

use barsql_core::{DriverType, QueryError, Value};
use barsql_sql::quote_ident;
use bytes::BytesMut;
use futures_util::{StreamExt, pin_mut};
use tokio_postgres::SimpleQueryMessage;
use tokio_postgres::types::{Format, IsNull, ToSql, Type, to_sql_checked};

use super::{PgLease, pg_error, type_name};
use crate::event::{ScriptEvent, Sink, StatementResult, emit};
use crate::script::{
    self, Buffered, StatementRun, StatementRunner, summary_for_exec, summary_for_rows, touches_transaction,
};
use crate::{Cancel, ChunkBuilder, ColumnMeta};

use super::values::Decoder;

// A cancelled statement may keep streaming this long before its connection is abandoned.
const CANCEL_GRACE: Duration = Duration::from_secs(5);
const RUN_SAVEPOINT: &str = "barsql_run";

pub struct PgSession {
    lease: PgLease,
    in_transaction: bool,
    broken: bool,
    reporting: bool,
}

impl PgSession {
    pub(crate) async fn new(lease: PgLease) -> Result<Self, QueryError> {
        let schema = lease.engine().default_schema().to_string();
        let session = Self { lease, in_transaction: false, broken: false, reporting: false };
        session.set_search_path(&schema).await?;
        Ok(session)
    }

    async fn set_search_path(&self, schema: &str) -> Result<(), QueryError> {
        let sql = format!("SET search_path TO {}, public", quote_ident(&DriverType::Postgres, schema));
        self.lease.client().batch_execute(&sql).await.map_err(|err| pg_error(&err))
    }

    pub fn is_broken(&self) -> bool {
        self.broken || self.lease.client().is_closed()
    }

    pub fn in_transaction(&self) -> bool {
        self.in_transaction
    }

    pub fn read_only(&self) -> bool {
        self.lease.engine().read_only()
    }

    // Inside a transaction each run gets a savepoint, so a cancel undoes just that run, not the transaction.
    pub async fn run_script(
        &mut self,
        statements: &[String],
        sink: &Sink,
        cancel: &Cancel,
    ) -> Result<usize, QueryError> {
        let guarded = self.in_transaction && self.simple(&format!("SAVEPOINT {RUN_SAVEPOINT}")).await.is_ok();
        let result = script::run_script(self, statements, sink, cancel).await;
        if guarded {
            if cancel.is_cancelled() {
                let _ = self.simple(&format!("ROLLBACK TO SAVEPOINT {RUN_SAVEPOINT}")).await;
            }
            let _ = self.simple(&format!("RELEASE SAVEPOINT {RUN_SAVEPOINT}")).await;
        }
        if self.in_transaction || statements.iter().any(|s| touches_transaction(s)) {
            self.probe_transaction().await;
        }
        result
    }

    pub async fn buffered(&mut self, sql: &str, cancel: &Cancel) -> Result<Buffered, QueryError> {
        script::buffered(self, sql, cancel).await
    }

    pub async fn begin(&mut self) -> Result<(), QueryError> {
        self.simple("BEGIN").await?;
        self.in_transaction = true;
        Ok(())
    }

    // Postgres turns COMMIT of an aborted transaction into a silent rollback, so report that as an error.
    pub async fn commit(&mut self) -> Result<(), QueryError> {
        let aborted = self.transaction_state().await == TxState::Aborted;
        let result = self.simple("COMMIT").await;
        self.probe_transaction().await;
        match result {
            Ok(()) if aborted => Err(QueryError::message("commit unexpectedly resulted in rollback")),
            other => other,
        }
    }

    pub async fn rollback(&mut self) -> Result<(), QueryError> {
        let result = self.simple("ROLLBACK").await;
        self.probe_transaction().await;
        result
    }

    // now() is the transaction start, so it differs from statement_timestamp() inside an explicit transaction.
    // In an aborted one the probe itself fails with 25P02.
    async fn transaction_state(&self) -> TxState {
        match self.lease.client().simple_query("SELECT now() <> statement_timestamp()").await {
            Ok(messages)
                if messages.iter().any(|m| matches!(m, SimpleQueryMessage::Row(row) if row.get(0) == Some("t"))) =>
            {
                TxState::Open
            }
            Err(err) if err.as_db_error().is_some_and(|db| db.code().code() == "25P02") => TxState::Aborted,
            _ => TxState::Idle,
        }
    }

    async fn probe_transaction(&mut self) {
        self.in_transaction = self.transaction_state().await != TxState::Idle;
    }

    async fn simple(&self, sql: &str) -> Result<(), QueryError> {
        self.lease.client().batch_execute(sql).await.map_err(|err| pg_error(&err))
    }

    pub async fn execute_params(&mut self, sql: &str, params: &[Value]) -> Result<u64, QueryError> {
        let params: Vec<TextParam> = params.iter().map(TextParam::from).collect();
        let refs: Vec<&(dyn ToSql + Sync)> = params.iter().map(|p| p as &(dyn ToSql + Sync)).collect();
        self.lease.client().execute(sql, &refs).await.map_err(|err| pg_error(&err))
    }

    // The statement's notices so far. Dropped when no script is running.
    async fn send_notices(&self, result_index: usize, sink: &Sink) {
        let event = self.lease.notices().take(result_index);
        if let Some(event) = event.filter(|_| self.reporting) {
            emit(sink, event).await;
        }
    }
}

impl StatementRunner for PgSession {
    fn driver(&self) -> DriverType {
        DriverType::Postgres
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
        let started = Instant::now();
        let _server_cancel = cancel.on_server(self.lease.engine().server_cancel(self.lease.cancel_token()));
        let local = self.lease.engine().options.local.clone();
        // Ones raised by the session's own setup and probes.
        self.lease.notices().reset();
        let fail = |error: QueryError| async move {
            emit(
                sink,
                ScriptEvent::Result(Box::new(StatementResult {
                    result_index: first_index,
                    statement: stmt.into(),
                    error: Some(error.clone()),
                    ..Default::default()
                })),
            )
            .await;
            StatementRun { sets: 1, error: Some(error) }
        };

        // The simple protocol reports no column types, so prepare the statement to get them.
        let oids: Vec<u32> = match self.lease.client().prepare(stmt).await {
            Ok(prepared) => prepared.columns().iter().map(|c| c.type_().oid()).collect(),
            Err(err) if err.as_db_error().is_some() || self.lease.client().is_closed() => {
                self.send_notices(first_index, sink).await;
                return fail(mark_cancelled(pg_error(&err), cancel)).await;
            }
            Err(_) => Vec::new(),
        };
        let stream = match self.lease.client().simple_query_raw(stmt).await {
            Ok(stream) => stream,
            Err(err) => {
                self.send_notices(first_index, sink).await;
                return fail(mark_cancelled(pg_error(&err), cancel)).await;
            }
        };
        pin_mut!(stream);

        let mut set: Option<SetState> = None;
        let mut cancelled_at: Option<Instant> = None;
        loop {
            let message = match cancelled_at {
                None => tokio::select! {
                    biased;
                    message = stream.next() => message,
                    _ = cancel.cancelled() => {
                        cancelled_at = Some(Instant::now());
                        continue;
                    }
                    // Shown while a long statement still runs.
                    _ = self.lease.notices().arrived(), if self.reporting => {
                        self.send_notices(first_index, sink).await;
                        continue;
                    }
                },
                Some(at) => {
                    match tokio::time::timeout(CANCEL_GRACE.saturating_sub(at.elapsed()), stream.next()).await {
                        Ok(message) => message,
                        Err(_) => {
                            self.broken = true;
                            self.send_notices(first_index, sink).await;
                            return fail(QueryError::cancelled()).await;
                        }
                    }
                }
            };
            match message {
                Some(Ok(SimpleQueryMessage::RowDescription(described))) => {
                    let columns: Arc<[ColumnMeta]> = described
                        .iter()
                        .enumerate()
                        .map(|(i, c)| ColumnMeta {
                            name: c.name().to_string(),
                            type_name: oids.get(i).map(|&oid| type_name(oid)).unwrap_or_default(),
                        })
                        .collect();
                    let decoders = (0..columns.len())
                        .map(|i| oids.get(i).map_or(Decoder::Text, |&o| Decoder::for_oid(o)))
                        .collect();
                    emit(sink, ScriptEvent::Meta { result_index: first_index, columns: columns.clone() }).await;
                    let width = columns.len();
                    set = Some((columns, decoders, ChunkBuilder::new(width, batch_rows.min(1024)), 0));
                }
                Some(Ok(SimpleQueryMessage::Row(row))) => {
                    let Some((columns, decoders, builder, rows)) = set.as_mut() else { continue };
                    for (i, decoder) in decoders.iter().enumerate() {
                        decoder.push(row.get(i), &local, builder);
                    }
                    builder.end_row();
                    *rows += 1;
                    if builder.rows() >= batch_rows {
                        let full = std::mem::replace(builder, ChunkBuilder::new(columns.len(), batch_rows.min(1024)));
                        emit(sink, ScriptEvent::Rows { result_index: first_index, chunk: Arc::new(full.finish()) })
                            .await;
                    }
                }
                Some(Ok(SimpleQueryMessage::CommandComplete(affected))) => {
                    let summary = match set.take() {
                        Some((columns, _, builder, rows)) => {
                            if !builder.is_empty() {
                                emit(
                                    sink,
                                    ScriptEvent::Rows { result_index: first_index, chunk: Arc::new(builder.finish()) },
                                )
                                .await;
                            }
                            summary_for_rows(&columns, rows, started)
                        }
                        None => summary_for_exec(affected, started),
                    };
                    self.send_notices(first_index, sink).await;
                    emit(
                        sink,
                        ScriptEvent::Result(Box::new(StatementResult {
                            result_index: first_index,
                            statement: stmt.into(),
                            summary: Some(summary),
                            ..Default::default()
                        })),
                    )
                    .await;
                    return StatementRun { sets: 1, error: None };
                }
                Some(Ok(_)) => {}
                Some(Err(err)) => {
                    let error = mark_cancelled(pg_error(&err), cancel);
                    let summary = set.map(|(columns, _, _, rows)| summary_for_rows(&columns, rows, started));
                    self.send_notices(first_index, sink).await;
                    emit(
                        sink,
                        ScriptEvent::Result(Box::new(StatementResult {
                            result_index: first_index,
                            statement: stmt.into(),
                            summary,
                            error: Some(error.clone()),
                            ..Default::default()
                        })),
                    )
                    .await;
                    return StatementRun { sets: 1, error: Some(error) };
                }
                None => return StatementRun { sets: 0, error: None },
            }
        }
    }
}

type SetState = (Arc<[ColumnMeta]>, Vec<Decoder>, ChunkBuilder, usize);

#[derive(PartialEq, Eq)]
enum TxState {
    Idle,
    Open,
    Aborted,
}

pub(crate) fn mark_cancelled(mut error: QueryError, cancel: &Cancel) -> QueryError {
    if cancel.is_cancelled() {
        error.cancelled = true;
    }
    error
}

// Sent as text so the server converts the UI's strings and numbers to whatever the statement needs.
#[derive(Debug)]
pub(crate) struct TextParam(Option<String>);

impl From<&Value> for TextParam {
    fn from(value: &Value) -> Self {
        Self(match value {
            Value::Null => None,
            Value::Bool(b) => Some(b.to_string()),
            Value::Int(i) => Some(i.to_string()),
            Value::Float(f) => Some(f.to_string()),
            Value::Text(s) => Some(s.clone()),
        })
    }
}

impl ToSql for TextParam {
    fn to_sql(&self, _: &Type, out: &mut BytesMut) -> Result<IsNull, Box<dyn std::error::Error + Sync + Send>> {
        match &self.0 {
            None => Ok(IsNull::Yes),
            Some(text) => {
                out.extend_from_slice(text.as_bytes());
                Ok(IsNull::No)
            }
        }
    }

    fn accepts(_: &Type) -> bool {
        true
    }

    fn encode_format(&self, _: &Type) -> Format {
        Format::Text
    }

    to_sql_checked!();
}

// Always quoted so the server types it from context. For statements whose rows must come back as text.
pub(crate) fn pg_literal(value: &Value) -> String {
    match value {
        Value::Null => "NULL".into(),
        Value::Bool(b) => format!("'{b}'"),
        Value::Int(i) => format!("'{i}'"),
        Value::Float(f) => format!("'{f}'"),
        Value::Text(s) if s.contains('\\') => format!("E'{}'", s.replace('\\', "\\\\").replace('\'', "''")),
        Value::Text(s) => format!("'{}'", s.replace('\'', "''")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literals_are_quoted_safely() {
        assert_eq!(pg_literal(&Value::Null), "NULL");
        assert_eq!(pg_literal(&Value::Int(4)), "'4'");
        assert_eq!(pg_literal(&Value::from("it's")), "'it''s'");
        assert_eq!(pg_literal(&Value::from(r"a\'b")), r"E'a\\''b'");
    }
}
