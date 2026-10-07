use std::borrow::Cow;
use std::sync::Arc;
use std::time::{Duration, Instant};

use barsql_core::{DriverType, MessageLevel, QueryError, ResultSummary, ServerMessage, Value};
use barsql_sql::lang::tokens::{TokenKind, tokenize};
use futures_util::TryStreamExt;
use tiberius::{ColumnData, ColumnType, QueryItem, ToSql};

use super::capture::{DONE_COUNT, DONE_ERROR, DONE_SERVER_ERROR, Token, captured};
use super::{MsLease, ms_error, values};
use crate::event::{ScriptEvent, Sink, StatementResult, emit};
use crate::script::{
    self, Buffered, StatementRun, StatementRunner, summary_for_exec, summary_for_rows, touches_transaction,
};
use crate::{Cancel, ChunkBuilder, ColumnMeta};

// After a cancel, the token being read may finish, so the stream stops between tokens.
const SETTLE: Duration = Duration::from_millis(250);
// The server acknowledges a cancel within this, or the connection is given up.
const ATTENTION_WAIT: Duration = Duration::from_secs(5);
const RUN_SAVEPOINT: &str = "barsql_run";
const MS: DriverType = DriverType::SqlServer;

pub struct MsSession {
    lease: MsLease,
    in_transaction: bool,
    reporting: bool,
}

enum Ended {
    Done,
    Failed(tiberius::error::Error),
    Cancelled,
}

impl MsSession {
    pub(crate) fn new(lease: MsLease) -> Self {
        Self { lease, in_transaction: false, reporting: false }
    }

    pub fn is_broken(&self) -> bool {
        self.lease.broken
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
        let guarded = self.in_transaction && self.simple(&format!("SAVE TRANSACTION {RUN_SAVEPOINT}")).await.is_ok();
        let result = script::run_script(self, statements, sink, cancel).await;
        if guarded && cancel.is_cancelled() && !self.lease.broken {
            let _ = self.simple(&format!("IF XACT_STATE() = 1 ROLLBACK TRANSACTION {RUN_SAVEPOINT}")).await;
            self.probe_transaction().await;
        }
        result
    }

    pub async fn buffered(&mut self, sql: &str, cancel: &Cancel) -> Result<Buffered, QueryError> {
        script::buffered(self, sql, cancel).await
    }

    pub async fn begin(&mut self) -> Result<(), QueryError> {
        self.simple("BEGIN TRANSACTION").await?;
        self.in_transaction = true;
        Ok(())
    }

    // A doomed transaction can only roll back, so commit rolls it back and says so.
    pub async fn commit(&mut self) -> Result<(), QueryError> {
        if self.scalar("SELECT XACT_STATE()").await? == Some(-1) {
            let _ = self.simple("ROLLBACK TRANSACTION").await;
            self.probe_transaction().await;
            return Err(QueryError::message("the transaction had failed, so it was rolled back instead"));
        }
        let result = self.simple("IF @@TRANCOUNT > 0 COMMIT TRANSACTION").await;
        self.probe_transaction().await;
        result
    }

    pub async fn rollback(&mut self) -> Result<(), QueryError> {
        let result = self.simple("IF @@TRANCOUNT > 0 ROLLBACK TRANSACTION").await;
        self.probe_transaction().await;
        result
    }

    async fn probe_transaction(&mut self) {
        if let Ok(count) = self.scalar("SELECT @@TRANCOUNT").await {
            self.in_transaction = count.unwrap_or(0) > 0;
        }
    }

    // NULLs go in as literals: a NULL parameter would be an nvarchar, which some columns won't take.
    pub async fn execute_params(&mut self, sql: &str, params: &[Value]) -> Result<u64, QueryError> {
        let (sql, params) = inline_nulls(sql, params);
        let params: Vec<MsParam> = params.iter().map(MsParam::from).collect();
        let refs: Vec<&dyn ToSql> = params.iter().map(|p| p as &dyn ToSql).collect();
        let mut tokens = Vec::new();
        let client = self.lease.client();
        let result = captured(&mut tokens, client.execute(sql.as_str(), &refs)).await;
        result.map(|done| done.total()).map_err(|error| self.failed(error, &sql))
    }

    // A statement with parameters and the first rows it returns, like INSERT … OUTPUT.
    pub async fn query_params(&mut self, sql: &str, params: &[Value]) -> Result<Buffered, QueryError> {
        let (sql, params) = inline_nulls(sql, params);
        let params: Vec<MsParam> = params.iter().map(MsParam::from).collect();
        let refs: Vec<&dyn ToSql> = params.iter().map(|p| p as &dyn ToSql).collect();
        let mut tokens = Vec::new();
        let client = self.lease.client();
        let read = async { client.query(sql.as_str(), &refs).await?.into_first_result().await };
        let rows = captured(&mut tokens, read).await.map_err(|error| self.failed(error, &sql))?;
        let Some(first) = rows.first() else { return Ok(Buffered::default()) };
        let cells: Vec<&ColumnData<'static>> = first.cells().map(|(_, data)| data).collect();
        let named: Vec<(String, ColumnType)> =
            first.columns().iter().map(|c| (c.name().to_string(), c.column_type())).collect();
        let columns = meta(&named, &cells);
        let mut builder = ChunkBuilder::new(named.len(), rows.len());
        for row in &rows {
            for ((_, data), (_, column)) in row.cells().zip(&named) {
                values::push(data, *column, &mut builder);
            }
            builder.end_row();
        }
        let summary = summary_for_rows(&columns, rows.len(), Instant::now());
        Ok(Buffered { columns, chunk: builder.finish(), summary: Some(summary) })
    }

    // A batch whose rows nobody reads.
    async fn simple(&mut self, sql: &str) -> Result<(), QueryError> {
        self.first_value(sql).await.map(|_| ())
    }

    async fn scalar(&mut self, sql: &str) -> Result<Option<i64>, QueryError> {
        Ok(match self.first_value(sql).await? {
            Some(ColumnData::U8(v)) => v.map(i64::from),
            Some(ColumnData::I16(v)) => v.map(i64::from),
            Some(ColumnData::I32(v)) => v.map(i64::from),
            Some(ColumnData::I64(v)) => v,
            _ => None,
        })
    }

    async fn first_value(&mut self, sql: &str) -> Result<Option<ColumnData<'static>>, QueryError> {
        let mut tokens = Vec::new();
        let client = self.lease.client();
        let read = async move {
            let rows = client.simple_query(sql).await?.into_first_result().await?;
            Ok::<_, tiberius::error::Error>(rows.into_iter().next().and_then(|row| row.into_iter().next()))
        };
        captured(&mut tokens, read).await.map_err(|error| self.failed(error, sql))
    }

    // Anything but the server's own error leaves the connection out of step.
    fn failed(&mut self, error: tiberius::error::Error, sql: &str) -> QueryError {
        if !matches!(error, tiberius::error::Error::Server(_)) {
            self.lease.broken = true;
        }
        ms_error(error, sql)
    }

    // Reads the batch to its end, or until the cancel.
    async fn drive(&mut self, batch: &str, out: &mut Batch<'_>, cancel: &Cancel) -> Ended {
        let mut tokens = Vec::new();
        let client = self.lease.client();
        let sent = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Ended::Cancelled,
            sent = captured(&mut tokens, client.simple_query(batch)) => sent,
        };
        out.absorb(&mut tokens).await;
        let mut stream = match sent {
            Ok(stream) => stream,
            Err(error) => return Ended::Failed(error),
        };
        loop {
            let next = tokio::select! {
                biased;
                _ = cancel.cancelled() => None,
                item = captured(&mut tokens, stream.try_next()) => Some(item),
            };
            let item = match next {
                Some(item) => item,
                None => {
                    let settled = tokio::time::timeout(SETTLE, captured(&mut tokens, stream.try_next())).await;
                    out.absorb(&mut tokens).await;
                    // It ended on its own, so there's nothing left to cancel.
                    return if matches!(settled, Ok(Ok(None))) { Ended::Done } else { Ended::Cancelled };
                }
            };
            out.absorb(&mut tokens).await;
            match item {
                Ok(Some(QueryItem::Metadata(meta))) => out.open(meta.columns()).await,
                Ok(Some(QueryItem::Row(row))) => out.row(&row).await,
                Ok(None) => return Ended::Done,
                Err(error) => return Ended::Failed(error),
            }
        }
    }
}

impl StatementRunner for MsSession {
    fn driver(&self) -> DriverType {
        MS
    }

    fn set_reporting(&mut self, on: bool) {
        self.reporting = on;
    }

    fn mark_broken(&mut self) {
        self.lease.broken = true;
    }

    // `stmt` is a whole batch, which can hold any number of statements and result sets.
    async fn stream_statement(
        &mut self,
        stmt: &str,
        first_index: usize,
        batch_rows: usize,
        sink: &Sink,
        cancel: &Cancel,
    ) -> StatementRun {
        let mut out = Batch::new(stmt, first_index, batch_rows, sink, self.reporting);
        // Boxed, like the other tiberius futures here, to keep this one small: debug builds give every future a
        // stack slot of its own in each caller.
        let ended = match Box::pin(self.drive(stmt, &mut out, cancel)).await {
            Ended::Done => Ok(()),
            Ended::Failed(error) => Err(self.failed(error, stmt)),
            Ended::Cancelled => {
                // Attention, then the server's acknowledgement.
                let mut tokens = Vec::new();
                let client = self.lease.client();
                let attention = Box::pin(captured(&mut tokens, client.cancel_query()));
                let acked = tokio::time::timeout(ATTENTION_WAIT, attention).await;
                // A statement stopped mid-run ends the batch's answer, and the server acknowledges in a message of its
                // own, which tiberius 0.13 stops short of. The connection can't be trusted with another request then.
                if !matches!(acked, Ok(Ok(()))) {
                    self.lease.broken = true;
                    out.say(ServerMessage::new(
                        MessageLevel::Warning,
                        "Cancelling closed this tab's connection, so its temporary tables, SET options and any open \
                         transaction are gone. The next run opens a new one.",
                    ));
                }
                Err(QueryError::cancelled())
            }
        };
        match out.transaction {
            Some(open) => self.in_transaction = open,
            // Without the batch's tokens, ask.
            None if out.dones == 0 && (self.in_transaction || touches_transaction(stmt)) => {
                Box::pin(self.probe_transaction()).await
            }
            None => {}
        }
        // The server rolls back what a closed connection had open.
        if self.lease.broken {
            self.in_transaction = false;
        }
        // Every batch ends with a DONE. None seen means the tokens weren't captured, so the count is asked for.
        if out.dones == 0 && ended.is_ok() && !out.produced() {
            let count = Box::pin(self.scalar("SELECT @@ROWCOUNT")).await;
            out.affected = count.ok().flatten().map(|n| n.max(0) as u64);
        }
        out.finish(ended).await
    }
}

// One batch's results as they stream in.
struct Batch<'s> {
    sql: &'s str,
    sink: &'s Sink,
    first_index: usize,
    // The next result's index.
    index: usize,
    batch_rows: usize,
    reporting: bool,
    started: Instant,
    set: Option<Set>,
    // Rows the statements without result sets touched, one count each.
    counts: Vec<u64>,
    affected: Option<u64>,
    dones: usize,
    messages: Vec<ServerMessage>,
    errors: Vec<(u32, String)>,
    // Where the batch left the transaction: Some(true) after a BEGIN, Some(false) after its end.
    transaction: Option<bool>,
}

struct Set {
    columns: Vec<(String, ColumnType)>,
    // Sent with the first row, which names the types the metadata leaves open.
    meta: Option<Arc<[ColumnMeta]>>,
    builder: ChunkBuilder,
    rows: usize,
}

impl<'s> Batch<'s> {
    fn new(sql: &'s str, first_index: usize, batch_rows: usize, sink: &'s Sink, reporting: bool) -> Self {
        Self {
            sql,
            sink,
            first_index,
            index: first_index,
            batch_rows,
            reporting,
            started: Instant::now(),
            set: None,
            counts: Vec::new(),
            affected: None,
            dones: 0,
            messages: Vec::new(),
            errors: Vec::new(),
            transaction: None,
        }
    }

    fn produced(&self) -> bool {
        self.index > self.first_index
    }

    async fn absorb(&mut self, tokens: &mut Vec<Token>) {
        for token in tokens.drain(..) {
            match token {
                // The server says these after every USE and login.
                Token::Info(text)
                    if text.starts_with("Changed database context to")
                        || text.starts_with("Changed language setting to") => {}
                Token::Info(text) => self.say(ServerMessage::new(MessageLevel::Notice, text)),
                Token::Error { code, message } => self.errors.push((code, message)),
                Token::Done { status, rows, .. } => self.done(status, rows).await,
                Token::EnvChange(change) => match change.as_str() {
                    "Begin transaction" => self.transaction = Some(true),
                    "Commit transaction" | "Rollback transaction" | "Defect transaction" => {
                        self.transaction = Some(false)
                    }
                    _ => {}
                },
            }
        }
        // Messages go with the latest result, or the coming one before the first.
        let index = if self.produced() { self.index - 1 } else { self.index };
        self.send_messages(index).await;
    }

    fn say(&mut self, message: ServerMessage) {
        if self.reporting {
            self.messages.push(message);
        }
    }

    async fn send_messages(&mut self, result_index: usize) {
        if !self.messages.is_empty() {
            let messages = std::mem::take(&mut self.messages);
            emit(self.sink, ScriptEvent::Messages { result_index, messages, dropped: 0 }).await;
        }
    }

    async fn done(&mut self, status: u16, rows: u64) {
        self.dones += 1;
        let failed = status & (DONE_ERROR | DONE_SERVER_ERROR) != 0;
        match self.set.take() {
            // A statement that failed before its first row leaves just its error.
            Some(set) if failed && set.rows == 0 => {}
            Some(set) => self.close(set).await,
            None if status & DONE_COUNT != 0 && !failed => self.counts.push(rows),
            None => {}
        }
    }

    async fn open(&mut self, columns: &[tiberius::Column]) {
        // Without the tokens nothing closed the last one.
        if let Some(set) = self.set.take() {
            self.close(set).await;
        }
        let columns: Vec<(String, ColumnType)> =
            columns.iter().map(|c| (c.name().to_string(), c.column_type())).collect();
        let builder = ChunkBuilder::new(columns.len(), self.batch_rows.min(1024));
        self.set = Some(Set { columns, meta: None, builder, rows: 0 });
    }

    async fn row(&mut self, row: &tiberius::Row) {
        let Some(set) = self.set.as_mut() else { return };
        if set.meta.is_none() {
            let first: Vec<&ColumnData<'static>> = row.cells().map(|(_, data)| data).collect();
            let meta = meta(&set.columns, &first);
            set.meta = Some(meta.clone());
            emit(self.sink, ScriptEvent::Meta { result_index: self.index, columns: meta }).await;
        }
        for ((_, data), (_, column)) in row.cells().zip(&set.columns) {
            values::push(data, *column, &mut set.builder);
        }
        set.builder.end_row();
        set.rows += 1;
        if set.builder.rows() >= self.batch_rows {
            let width = set.columns.len();
            let full = std::mem::replace(&mut set.builder, ChunkBuilder::new(width, self.batch_rows.min(1024)));
            emit(self.sink, ScriptEvent::Rows { result_index: self.index, chunk: Arc::new(full.finish()) }).await;
        }
    }

    async fn close(&mut self, set: Set) {
        let meta = match set.meta {
            Some(meta) => meta,
            None => {
                let meta = meta(&set.columns, &[]);
                emit(self.sink, ScriptEvent::Meta { result_index: self.index, columns: meta.clone() }).await;
                meta
            }
        };
        if !set.builder.is_empty() {
            emit(self.sink, ScriptEvent::Rows { result_index: self.index, chunk: Arc::new(set.builder.finish()) })
                .await;
        }
        self.send_messages(self.index).await;
        let summary = summary_for_rows(&meta, set.rows, self.started);
        self.result(Some(summary), None).await;
    }

    async fn result(&mut self, summary: Option<ResultSummary>, error: Option<QueryError>) {
        let result = StatementResult {
            result_index: self.index,
            statement: self.sql.to_string(),
            summary,
            error,
            ..Default::default()
        };
        emit(self.sink, ScriptEvent::Result(Box::new(result))).await;
        self.index += 1;
    }

    async fn finish(mut self, ended: Result<(), QueryError>) -> StatementRun {
        if let Some(set) = self.set.take() {
            self.close(set).await;
        }
        let affected = self.affected.or_else(|| (!self.counts.is_empty()).then(|| self.counts.iter().sum()));
        // One count needs no list, since the result says it.
        if self.counts.len() > 1 {
            for rows in std::mem::take(&mut self.counts) {
                let text = if rows == 1 { "(1 row affected)".to_string() } else { format!("({rows} rows affected)") };
                self.say(ServerMessage::new(MessageLevel::Info, text));
            }
        }
        let error = match ended {
            Ok(()) => None,
            Err(error) => {
                // The first is the result's own. The rest join the messages.
                let more = if error.cancelled { Vec::new() } else { self.errors.split_off(self.errors.len().min(1)) };
                for (code, message) in more {
                    let message = ServerMessage::new(MessageLevel::Warning, message);
                    self.say(ServerMessage { code: code.to_string(), ..message });
                }
                Some(error)
            }
        };
        if error.is_none() && self.produced() {
            self.send_messages(self.index - 1).await;
            return StatementRun { sets: self.index - self.first_index, error: None };
        }
        let summary = match (&error, affected) {
            (Some(_), _) => None,
            (None, Some(rows)) => Some(summary_for_exec(rows, self.started)),
            (None, None) => Some(ResultSummary {
                duration_ms: self.started.elapsed().as_millis() as i64,
                message: "Commands completed successfully.".into(),
                ..Default::default()
            }),
        };
        self.send_messages(self.index).await;
        self.result(summary, error.clone()).await;
        StatementRun { sets: self.index - self.first_index, error }
    }
}

fn meta(columns: &[(String, ColumnType)], first: &[&ColumnData<'_>]) -> Arc<[ColumnMeta]> {
    columns
        .iter()
        .enumerate()
        .map(|(i, (name, column))| ColumnMeta {
            name: name.clone(),
            type_name: values::type_name(*column, first.get(i).copied()).to_string(),
        })
        .collect()
}

// Writes NULL for each `@Pn` whose value is NULL and numbers the rest from @P1 again.
pub(crate) fn inline_nulls(sql: &str, params: &[Value]) -> (String, Vec<Value>) {
    if !params.iter().any(|p| matches!(p, Value::Null)) {
        return (sql.to_string(), params.to_vec());
    }
    let mut kept = Vec::new();
    let renumbered: Vec<Option<usize>> = params
        .iter()
        .map(|p| match p {
            Value::Null => None,
            value => {
                kept.push(value.clone());
                Some(kept.len())
            }
        })
        .collect();
    let mut out = String::with_capacity(sql.len());
    let mut last = 0;
    for token in tokenize(sql, Some(&MS)) {
        let n = token.text.strip_prefix("@P").or_else(|| token.text.strip_prefix("@p"));
        let Some(n) = n.and_then(|n| n.parse::<usize>().ok()).filter(|n| (1..=params.len()).contains(n)) else {
            continue;
        };
        if token.kind != TokenKind::Ident {
            continue;
        }
        out.push_str(&sql[last..token.start]);
        match renumbered[n - 1] {
            Some(k) => out.push_str(&format!("@P{k}")),
            None => out.push_str("NULL"),
        }
        last = token.end;
    }
    out.push_str(&sql[last..]);
    (out, kept)
}

// Sent with its own type, so the server converts it as the statement needs.
enum MsParam {
    Bit(bool),
    Int(i64),
    Float(f64),
    Text(String),
}

impl From<&Value> for MsParam {
    fn from(value: &Value) -> Self {
        match value {
            Value::Bool(b) => Self::Bit(*b),
            Value::Int(i) => Self::Int(*i),
            Value::Float(f) => Self::Float(*f),
            Value::Text(s) => Self::Text(s.clone()),
            // Inlined before this, by `inline_nulls`.
            Value::Null => Self::Text(String::new()),
        }
    }
}

impl ToSql for MsParam {
    fn to_sql(&self) -> ColumnData<'_> {
        match self {
            Self::Bit(b) => ColumnData::Bit(Some(*b)),
            Self::Int(i) => ColumnData::I64(Some(*i)),
            Self::Float(f) => ColumnData::F64(Some(*f)),
            Self::Text(s) => ColumnData::String(Some(Cow::Borrowed(s))),
        }
    }
}
