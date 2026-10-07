// ClickHouse over its HTTP interface. Every statement is one POST, its rows streamed back as
// TabSeparatedWithNamesAndTypes. A tab's statements share a server session, so SET and USE last between runs.
// ClickHouse has no transactions to hold open, and its keys aren't unique, so rows can't be edited by key.

mod schema;
#[cfg(test)]
mod tests;
mod tsv;
mod types;

use std::sync::Arc;
use std::time::{Duration, Instant};

use barsql_core::{ConnectionConfig, DriverType, QueryError, Value};
use barsql_sql::lang::tokens::{TokenKind, tokenize};
use barsql_sql::{quote_ident, strip_leading_comments, to_upper};
use url::form_urlencoded;

use crate::event::{ScriptEvent, Sink, StatementResult, emit_notes};
use crate::http::{HttpClient, Origin, Request, Response};
use crate::script::{self, Buffered, StatementRun, StatementRunner, summary_for_exec, summary_for_rows};
use crate::ssh::{SshOptions, TunnelSlot};
use crate::tls::{TlsMode, http_client_config};
use crate::{Cancel, ChunkBuilder, ColumnMeta};

const CH: DriverType = DriverType::ClickHouse;
const MAX_LINE: usize = 256 * 1024 * 1024;
const MAX_ERROR: usize = 1024 * 1024;
// Idle seconds before the server drops a tab's session.
const SESSION_TIMEOUT: &str = "600";
const SESSION_NOT_FOUND: &str = "372";
const SESSION_IS_LOCKED: &str = "373";
// How long a statement waits for its session while a cancelled one still runs there.
const LOCKED_WAIT: Duration = Duration::from_secs(5);
const NO_TRANSACTIONS: &str = "ClickHouse has no transactions to hold open. Each statement takes effect on its own.";
const LOST_STATE: &str = "Settings, USE and temporary tables from earlier runs are gone.";

#[derive(Debug, Clone)]
pub struct ChOptions {
    host: String,
    port: u16,
    tls: Option<TlsMode>,
    user: String,
    password: String,
    database: String,
    read_only: bool,
    ssh: Option<SshOptions>,
}

impl ChOptions {
    // disable is plain http on 8123. require encrypts without checking the certificate and verify-full checks it,
    // both on 8443.
    pub fn from_config(cfg: &ConnectionConfig) -> Result<Self, QueryError> {
        let tls = match cfg.ssl_mode.as_str() {
            "" | "disable" => None,
            "require" => Some(TlsMode::Require),
            _ => Some(TlsMode::VerifyFull),
        };
        let default_port = if tls.is_some() { 8443 } else { 8123 };
        let ssh = if cfg.ssh.enabled { Some(SshOptions::from_config(&cfg.ssh)?) } else { None };
        let or = |value: &str, default: &str| if value.is_empty() { default.to_string() } else { value.to_string() };
        Ok(Self {
            host: cfg.host.clone(),
            port: u16::try_from(cfg.port).ok().filter(|p| *p != 0).unwrap_or(default_port),
            tls,
            user: or(&cfg.username, "default"),
            password: cfg.password.clone(),
            // Like MySQL, the database to browse is the one statements run in.
            database: or(&cfg.schema, &or(&cfg.database, "default")),
            read_only: cfg.read_only,
            ssh,
        })
    }
}

pub struct ChEngine {
    options: ChOptions,
    client: HttpClient,
    // The server keeps this user read-only (readonly=1), which refuses every setting a request carries.
    minimal: bool,
}

// One statement's request.
struct Call<'a> {
    sql: &'a str,
    query_id: &'a str,
    // The session's id, and whether the server already has it.
    session: Option<(&'a str, bool)>,
    // `{name:Type}` placeholders.
    params: &'a [(&'a str, &'a str)],
}

impl ChEngine {
    pub async fn connect(options: ChOptions) -> Result<Arc<Self>, QueryError> {
        let tunnel = options.ssh.clone().map(TunnelSlot::new);
        let tls = match options.tls {
            Some(mode) => Some(
                http_client_config(mode)
                    .ok_or_else(|| QueryError::message("can't load the system's root certificates"))?,
            ),
            None => None,
        };
        let origin = Origin { tls: options.tls.is_some(), host: options.host.clone(), port: options.port };
        let client = HttpClient::new(origin, tls, tunnel);
        // Asked without settings, since a read-only user would refuse them.
        let mut engine = Self { options, client, minimal: true };
        let readonly = engine.rows("SELECT getSetting('readonly')", &[]).await?;
        engine.minimal = readonly.first().and_then(|r| r.first()).and_then(|v| v.as_deref()) == Some("1");
        Ok(Arc::new(engine))
    }

    pub fn read_only(&self) -> bool {
        self.options.read_only
    }

    pub fn default_schema(&self) -> &str {
        &self.options.database
    }

    pub(crate) fn schema_or<'a>(&'a self, schema: &'a str) -> &'a str {
        if schema.is_empty() { &self.options.database } else { schema }
    }

    pub async fn session(self: &Arc<Self>) -> Result<ChSession, QueryError> {
        Ok(ChSession {
            engine: self.clone(),
            session: None,
            started: false,
            stateful: false,
            stateless: false,
            notes: Vec::new(),
        })
    }

    // Without a server session, for the app's own statements.
    pub async fn pooled_session(self: &Arc<Self>) -> Result<ChSession, QueryError> {
        Ok(ChSession { stateless: true, ..self.session().await? })
    }

    fn target(&self, call: &Call) -> String {
        let mut query = form_urlencoded::Serializer::new(String::new());
        query.append_pair("query_id", call.query_id);
        query.append_pair("default_format", "TabSeparatedWithNamesAndTypes");
        if !self.minimal {
            query.append_pair("cancel_http_readonly_queries_on_client_close", "1");
            query.append_pair("date_time_output_format", "iso");
            query.append_pair("output_format_decimal_trailing_zeros", "1");
            query.append_pair("date_time_input_format", "best_effort");
            // 2 still lets requests carry settings. 1 would refuse the ones above.
            if self.options.read_only {
                query.append_pair("readonly", "2");
            }
        }
        match call.session {
            // A known session keeps the database USE chose.
            Some((id, true)) => {
                query.append_pair("session_id", id);
                query.append_pair("session_timeout", SESSION_TIMEOUT);
                query.append_pair("session_check", "1");
            }
            Some((id, false)) => {
                query.append_pair("session_id", id);
                query.append_pair("session_timeout", SESSION_TIMEOUT);
                query.append_pair("database", &self.options.database);
            }
            None => {
                query.append_pair("database", &self.options.database);
            }
        }
        for (name, value) in call.params {
            query.append_pair(&format!("param_{name}"), value);
        }
        format!("/?{}", query.finish())
    }

    async fn send(&self, call: &Call<'_>) -> Result<Response, QueryError> {
        let target = self.target(call);
        let req = Request::post(&target, call.sql.as_bytes())
            .header("User-Agent", concat!("BarSQL/", env!("CARGO_PKG_VERSION")))
            .header("X-ClickHouse-User", self.options.user.clone())
            .header("X-ClickHouse-Key", self.options.password.clone());
        let mut res = self.client.send(&req).await?;
        if res.status != 200 {
            let code = res.header("X-ClickHouse-Exception-Code").map(str::to_string);
            let body = res.read_to_end(MAX_ERROR).await.unwrap_or_default();
            return Err(ch_error(code.as_deref(), &String::from_utf8_lossy(&body), call.sql));
        }
        Ok(res)
    }

    // Every row of one statement, without a session. For the catalog and other reads of the app's own.
    async fn rows(&self, sql: &str, params: &[(&str, &str)]) -> Result<Vec<Vec<Option<String>>>, QueryError> {
        let query_id = uuid::Uuid::new_v4().to_string();
        let mut res = self.send(&Call { sql, query_id: &query_id, session: None, params }).await?;
        // Names, then types.
        for _ in 0..2 {
            if res.next_line(MAX_LINE).await?.is_none() {
                return Ok(Vec::new());
            }
        }
        let mut rows = Vec::new();
        while let Some(line) = res.next_line(MAX_LINE).await? {
            if tsv::opens_exception(&line) {
                return Err(exception(&mut res, sql).await);
            }
            rows.push(tsv::text_fields(&line));
        }
        Ok(rows)
    }

    // A statement of the app's own that returns nothing, like KILL QUERY.
    async fn execute(&self, sql: &str) -> Result<(), QueryError> {
        let query_id = uuid::Uuid::new_v4().to_string();
        let mut res = self.send(&Call { sql, query_id: &query_id, session: None, params: &[] }).await?;
        res.read_to_end(MAX_ERROR).await?;
        Ok(())
    }

    // KILL QUERY reaches what dropping the request can't, like an INSERT … SELECT. ASYNC, so this doesn't wait.
    fn kill(self: &Arc<Self>, query_id: String) {
        let engine = self.clone();
        tokio::spawn(async move {
            let sql = format!("KILL QUERY WHERE query_id = {} ASYNC", literal(&Value::Text(query_id)));
            let _ = engine.execute(&sql).await;
        });
    }
}

// Reads the rest of the body for the message an exception block carries.
async fn exception(res: &mut Response, sql: &str) -> QueryError {
    let mut lines = Vec::new();
    while let Ok(Some(line)) = res.next_line(MAX_LINE).await {
        lines.push(line);
    }
    let message = tsv::exception_message(lines.iter().map(Vec::as_slice));
    ch_error(None, &message, sql)
}

// `Code: 60. DB::Exception: <message>. (UNKNOWN_TABLE) (version 26.8.19.9 (official build))`. The name goes in
// detail. A syntax error's `failed at position N` counts bytes from 1, which becomes a character position.
fn ch_error(header_code: Option<&str>, body: &str, sql: &str) -> QueryError {
    let mut text = body.trim();
    let mut code = header_code.unwrap_or_default().to_string();
    if let Some((number, rest)) = text.strip_prefix("Code: ").and_then(|rest| rest.split_once(". DB::Exception: ")) {
        code = number.to_string();
        text = rest;
    }
    if let Some(at) = text.rfind(" (version ") {
        text = &text[..at];
    }
    let mut name = "";
    if let Some(open) = text.rfind(" (")
        && text.ends_with(')')
    {
        let candidate = &text[open + 2..text.len() - 1];
        if !candidate.is_empty() && candidate.bytes().all(|b| b.is_ascii_uppercase() || b == b'_' || b.is_ascii_digit())
        {
            name = candidate;
            text = &text[..open];
        }
    }
    let message = text.strip_suffix('.').unwrap_or(text).trim().to_string();
    let position = message
        .split_once("failed at position ")
        .and_then(|(_, rest)| rest.split(|c: char| !c.is_ascii_digit()).next()?.parse::<usize>().ok())
        .and_then(|byte| sql.get(..byte.saturating_sub(1)).map(|before| before.chars().count() as u32 + 1))
        .unwrap_or(0);
    QueryError { message, code, detail: name.to_string(), position, ..Default::default() }
}

// A value inlined as a ClickHouse literal. Strings escape backslashes as well as quotes.
pub(crate) fn literal(value: &Value) -> String {
    match value {
        Value::Null => "NULL".into(),
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) if f.is_nan() => "nan".into(),
        Value::Float(f) if f.is_infinite() => if *f > 0.0 { "inf" } else { "-inf" }.into(),
        Value::Float(f) => format!("{f:?}"),
        Value::Text(s) => format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'")),
    }
}

// The HTTP interface takes no bound parameters for `?`, so each one outside strings and comments becomes a
// literal, in order.
pub(crate) fn inline_params(sql: &str, params: &[Value]) -> Result<String, String> {
    let mut out = String::with_capacity(sql.len());
    let mut values = params.iter();
    let mut last = 0;
    for token in tokenize(sql, Some(&CH)) {
        if token.kind == TokenKind::Op && token.text == "?" {
            let value = values.next().ok_or("more placeholders than values")?;
            out.push_str(&sql[last..token.start]);
            out.push_str(&literal(value));
            last = token.end;
        }
    }
    if values.next().is_some() {
        return Err("more values than placeholders".into());
    }
    out.push_str(&sql[last..]);
    Ok(out)
}

// A top-level FORMAT clause asks for the server's own output, like JSON or CSV, which the grid shows as text, a
// line per row. INSERT … FORMAT names the input instead.
pub(crate) fn output_format(sql: &str) -> Option<String> {
    let tokens: Vec<_> = tokenize(sql, Some(&CH)).into_iter().filter(|t| t.kind != TokenKind::Comment).collect();
    if tokens.first().is_some_and(|t| t.kind == TokenKind::Ident && t.lower == "insert") {
        return None;
    }
    let mut depth = 0i32;
    for (i, token) in tokens.iter().enumerate() {
        match (token.kind, token.text) {
            (TokenKind::Punct, "(") => depth += 1,
            (TokenKind::Punct, ")") => depth -= 1,
            // The clause ends the query, before SETTINGS at most. An alias named format doesn't.
            (TokenKind::Ident, _) if depth == 0 && token.lower == "format" => {
                let name = tokens.get(i + 1).filter(|t| t.kind == TokenKind::Ident);
                let after = tokens.get(i + 2);
                let ends = after.is_none_or(|t| t.text == ";" || (t.kind == TokenKind::Ident && t.lower == "settings"));
                if let Some(name) = name.filter(|_| ends) {
                    return Some(name.text.to_string());
                }
            }
            _ => {}
        }
    }
    None
}

// X-ClickHouse-Summary's numbers are strings.
fn written_rows(res: &Response) -> u64 {
    res.header("X-ClickHouse-Summary")
        .and_then(|summary| serde_json::from_str::<serde_json::Value>(summary).ok())
        .and_then(|summary| summary["written_rows"].as_str().and_then(|n| n.parse().ok()))
        .unwrap_or(0)
}

pub struct ChSession {
    engine: Arc<ChEngine>,
    // Created with the tab's first statement.
    session: Option<String>,
    // The server has the session, so later requests check for it rather than recreate it.
    started: bool,
    // The session holds something a new one wouldn't, like a setting the user changed.
    stateful: bool,
    stateless: bool,
    // Said once, with the next statement's result.
    notes: Vec<String>,
}

// One statement's result as it streams in.
struct Streaming<'s> {
    index: usize,
    stmt: &'s str,
    batch_rows: usize,
    sink: &'s Sink,
    started: Instant,
    columns: Option<Arc<[ColumnMeta]>>,
    kinds: Vec<types::Kind>,
    builder: ChunkBuilder,
    rows: usize,
}

impl Streaming<'_> {
    async fn begin(&mut self, columns: Vec<ColumnMeta>) {
        self.kinds = columns.iter().map(|c| types::kind(&c.type_name)).collect();
        self.builder = ChunkBuilder::new(columns.len(), self.batch_rows.min(1024));
        let columns: Arc<[ColumnMeta]> = columns.into();
        let _ = self.sink.send(ScriptEvent::Meta { result_index: self.index, columns: columns.clone() }).await;
        self.columns = Some(columns);
    }

    async fn row(&mut self, fields: &[Option<Vec<u8>>]) {
        for (i, kind) in self.kinds.iter().enumerate() {
            types::push(*kind, fields.get(i).and_then(|f| f.as_deref()), &mut self.builder);
        }
        self.builder.end_row();
        self.rows += 1;
        if self.builder.rows() >= self.batch_rows {
            self.flush().await;
        }
    }

    async fn flush(&mut self) {
        if self.builder.is_empty() {
            return;
        }
        let width = self.kinds.len();
        let full = std::mem::replace(&mut self.builder, ChunkBuilder::new(width, self.batch_rows.min(1024)));
        let _ = self.sink.send(ScriptEvent::Rows { result_index: self.index, chunk: Arc::new(full.finish()) }).await;
    }

    async fn end(&mut self, written: u64, notes: Vec<String>, error: Option<QueryError>) -> StatementRun {
        self.flush().await;
        let summary = match &self.columns {
            Some(columns) => Some(summary_for_rows(columns, self.rows, self.started)),
            None if error.is_none() => Some(summary_for_exec(written, self.started)),
            None => None,
        };
        emit_notes(self.sink, self.index, notes).await;
        let result = StatementResult {
            result_index: self.index,
            statement: self.stmt.into(),
            summary,
            error: error.clone(),
            ..Default::default()
        };
        let _ = self.sink.send(ScriptEvent::Result(Box::new(result))).await;
        StatementRun { sets: 1, error }
    }
}

impl ChSession {
    pub fn is_broken(&self) -> bool {
        false
    }

    pub fn in_transaction(&self) -> bool {
        false
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
        Err(QueryError::message(NO_TRANSACTIONS))
    }

    pub async fn commit(&mut self) -> Result<(), QueryError> {
        Err(QueryError::message(NO_TRANSACTIONS))
    }

    pub async fn rollback(&mut self) -> Result<(), QueryError> {
        Err(QueryError::message(NO_TRANSACTIONS))
    }

    // Inlined, since the HTTP interface binds no `?`. Returns the rows written.
    pub async fn execute_params(&mut self, sql: &str, params: &[Value]) -> Result<u64, QueryError> {
        let sql = inline_params(sql, params).map_err(QueryError::message)?;
        let query_id = uuid::Uuid::new_v4().to_string();
        let mut res = self.send(&sql, &query_id).await?;
        let written = written_rows(&res);
        res.read_to_end(MAX_ERROR).await?;
        Ok(written)
    }

    // Sends in the tab's session, opening a new one when the server dropped it or another statement holds it.
    async fn send(&mut self, sql: &str, query_id: &str) -> Result<Response, QueryError> {
        let waiting = Instant::now();
        loop {
            if self.stateless {
                return self.engine.send(&Call { sql, query_id, session: None, params: &[] }).await;
            }
            let id = self.session.get_or_insert_with(|| uuid::Uuid::new_v4().to_string()).clone();
            // A request's database lasts for that request only, so a new session starts with USE. After that the
            // session keeps whichever database the tab's own USE picks.
            if !self.started {
                let database = format!("USE {}", quote_ident(&CH, &self.engine.options.database));
                let start_id = uuid::Uuid::new_v4().to_string();
                let start = Call { sql: &database, query_id: &start_id, session: Some((&id, false)), params: &[] };
                self.engine.send(&start).await?.read_to_end(MAX_ERROR).await?;
                self.started = true;
            }
            let call = Call { sql, query_id, session: Some((&id, true)), params: &[] };
            match self.engine.send(&call).await {
                Ok(res) => return Ok(res),
                Err(error) if error.code == SESSION_NOT_FOUND && self.started => {
                    self.drop_session("The server had ended this tab's session, so the statement ran in a new one.");
                }
                Err(error) if error.code == SESSION_IS_LOCKED && waiting.elapsed() < LOCKED_WAIT => {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
                Err(error) if error.code == SESSION_IS_LOCKED => {
                    self.drop_session(
                        "This tab's session was still busy with an earlier statement, so this one ran in a new session.",
                    );
                }
                Err(error) => return Err(error),
            }
        }
    }

    // Moves to a new session, and says so when the old one held something the user set. Sessions time out after
    // ten idle minutes, so losing one that holds nothing is routine.
    fn drop_session(&mut self, why: &str) {
        (self.session, self.started) = (None, false);
        if std::mem::take(&mut self.stateful) {
            self.notes.push(format!("{why} {LOST_STATE}"));
        }
    }

    async fn execute(&mut self, out: &mut Streaming<'_>, query_id: &str) -> StatementRun {
        let mut res = match self.send(out.stmt, query_id).await {
            Ok(res) => res,
            Err(error) => return out.end(0, std::mem::take(&mut self.notes), Some(error)).await,
        };
        let notes = std::mem::take(&mut self.notes);
        let written = written_rows(&res);
        let failed = |error: crate::http::HttpError| Some(QueryError::from(error));
        if let Some(format) = output_format(out.stmt) {
            out.begin(vec![ColumnMeta { name: format, type_name: "String".into() }]).await;
            loop {
                match res.next_line(MAX_LINE).await {
                    Ok(Some(line)) if tsv::opens_exception(&line) => {
                        let error = exception(&mut res, out.stmt).await;
                        return out.end(written, notes, Some(error)).await;
                    }
                    Ok(Some(line)) => out.row(&[Some(line)]).await,
                    Ok(None) => return out.end(written, notes, None).await,
                    Err(error) => return out.end(written, notes, failed(error)).await,
                }
            }
        }
        let names = match res.next_line(MAX_LINE).await {
            Ok(Some(names)) => names,
            // A statement without rows answers with an empty body.
            Ok(None) => return out.end(written, notes, None).await,
            Err(error) => return out.end(written, notes, failed(error)).await,
        };
        let types = match res.next_line(MAX_LINE).await {
            Ok(types) => types.unwrap_or_default(),
            Err(error) => return out.end(written, notes, failed(error)).await,
        };
        let columns: Vec<ColumnMeta> = tsv::text_fields(&names)
            .into_iter()
            .zip(tsv::text_fields(&types).into_iter().chain(std::iter::repeat(None)))
            .map(|(name, type_name)| ColumnMeta {
                name: name.unwrap_or_default(),
                type_name: type_name.unwrap_or_default(),
            })
            .collect();
        let width = columns.len();
        out.begin(columns).await;
        loop {
            match res.next_line(MAX_LINE).await {
                Ok(Some(line)) if tsv::opens_exception(&line) => {
                    let error = exception(&mut res, out.stmt).await;
                    return out.end(written, notes, Some(error)).await;
                }
                // Servers before exception tags wrote the error in as a line of its own.
                Ok(Some(line)) if line.starts_with(b"Code: ") && tsv::fields(&line).len() != width => {
                    let mut message = String::from_utf8_lossy(&line).into_owned();
                    while let Ok(Some(more)) = res.next_line(MAX_LINE).await {
                        message.push('\n');
                        message.push_str(&String::from_utf8_lossy(&more));
                    }
                    return out.end(written, notes, Some(ch_error(None, &message, out.stmt))).await;
                }
                Ok(Some(line)) => out.row(&tsv::fields(&line)).await,
                Ok(None) => return out.end(written, notes, None).await,
                Err(error) => return out.end(written, notes, failed(error)).await,
            }
        }
    }
}

impl StatementRunner for ChSession {
    fn driver(&self) -> DriverType {
        DriverType::ClickHouse
    }

    // Cancelling drops the request, which stops a read, and KILL QUERY stops the rest.
    async fn stream_statement(
        &mut self,
        stmt: &str,
        first_index: usize,
        batch_rows: usize,
        sink: &Sink,
        cancel: &Cancel,
    ) -> StatementRun {
        let mut out = Streaming {
            index: first_index,
            stmt,
            batch_rows,
            sink,
            started: Instant::now(),
            columns: None,
            kinds: Vec::new(),
            builder: ChunkBuilder::new(0, 1),
            rows: 0,
        };
        if cancel.is_cancelled() {
            return out.end(0, Vec::new(), Some(QueryError::cancelled())).await;
        }
        let query_id = uuid::Uuid::new_v4().to_string();
        let engine = self.engine.clone();
        let run = tokio::select! {
            biased;
            _ = cancel.cancelled() => None,
            run = self.execute(&mut out, &query_id) => Some(run),
        };
        match run {
            Some(run) => {
                self.stateful |= run.error.is_none() && sets_state(stmt);
                run
            }
            None => {
                engine.kill(query_id);
                out.end(0, Vec::new(), Some(QueryError::cancelled())).await
            }
        }
    }
}

// What lasts for the session: settings, the current database and temporary tables.
fn sets_state(stmt: &str) -> bool {
    let code = to_upper(strip_leading_comments(stmt));
    let mut words = code.split_whitespace();
    match words.next() {
        Some("SET" | "USE") => true,
        Some("CREATE") => words.next() == Some("TEMPORARY"),
        _ => false,
    }
}
