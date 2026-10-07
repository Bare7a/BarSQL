// Turso and other libSQL servers, over Hrana on HTTP. Each tab runs on a stream the server identifies by a baton.
// The server drops a stream after seconds idle, which is why a transaction can't stay open between runs. The
// catalog is SQLite's, read through stateless requests.
//
// Read-only connections rely on the client-side classifier. sqld refuses PRAGMA query_only, so the server's own
// guard is a read-only token.

mod hrana;
#[cfg(test)]
mod tests;

use std::sync::Arc;
use std::time::Instant;

use barsql_core::schema::primary_keys;
use barsql_core::{
    ColumnInfo, ConnectionConfig, ConnectionStatus, ConstraintInfo, DriverType, FunctionList, IndexInfo, ObjectRef,
    QueryError, RoutineInfo, SchemaInfo, TableInfo, TriggerInfo, Value,
};
use barsql_sql::{first_keyword, strip_leading_comments};

use crate::dump::DumpColumn;
use crate::event::{ScriptEvent, Sink, StatementResult, emit_notes};
use crate::http::{HttpClient, Origin, Request, Response};
use crate::lite::{LiteRows, LiteSource, LiteValue, catalog, values};
use crate::script::{self, Buffered, StatementRun, StatementRunner, summary_for_exec, summary_for_rows};
use crate::ssh::{SshOptions, TunnelSlot};
use crate::tls::{TlsMode, http_client_config};
use crate::{Cancel, ChunkBuilder, ColumnMeta};
use hrana::{
    CursorBatch, CursorBody, CursorEntry, CursorHead, CursorStep, Endpoint, HError, PipelineBody, PipelineOk,
    PipelineRequest, PipelineResponse, PipelineResult, Stmt, StmtResult, request_error,
};

// Error bodies and v2 results are read whole. Cursor lines hold one row each.
const MAX_BODY: usize = 512 * 1024 * 1024;
const MAX_LINE: usize = 256 * 1024 * 1024;
const NO_TRANSACTIONS: &str = "Turso can't keep a transaction open between runs, since the server drops an idle \
                               stream after a few seconds. Run BEGIN and COMMIT together.";
const LOST_STATE: &str = "PRAGMAs and attached databases from earlier runs are gone.";

#[derive(Debug, Clone)]
pub struct TursoOptions {
    endpoint: Endpoint,
    token: String,
    read_only: bool,
    tls: TlsMode,
    ssh: Option<SshOptions>,
}

impl TursoOptions {
    // sslmode=require accepts a self-signed sqld. Anything else verifies the certificate.
    pub fn from_config(cfg: &ConnectionConfig) -> Result<Self, QueryError> {
        let endpoint = hrana::endpoint(&cfg.url).map_err(QueryError::message)?;
        let tls = if cfg.ssl_mode == "require" { TlsMode::Require } else { TlsMode::VerifyFull };
        let ssh = if cfg.ssh.enabled { Some(SshOptions::from_config(&cfg.ssh)?) } else { None };
        Ok(Self { endpoint, token: cfg.auth_token.clone(), read_only: cfg.read_only, tls, ssh })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Protocol {
    // Streams rows from /v3/cursor.
    V3,
    // Older servers. /v2/pipeline answers with every row at once.
    V2,
}

pub struct TursoEngine {
    options: TursoOptions,
    client: HttpClient,
    tunnel: Option<Arc<TunnelSlot>>,
    protocol: Protocol,
}

fn hrana_error(error: HError) -> QueryError {
    statement_error(error, "")
}

// With the error's place in `stmt` when the server says where.
fn statement_error(error: HError, stmt: &str) -> QueryError {
    let (message, loc) = error.cleaned();
    let position = loc.map_or(0, |loc| loc.position(stmt));
    QueryError { message, code: error.code.unwrap_or_default(), position, ..Default::default() }
}

impl TursoEngine {
    pub async fn connect(options: TursoOptions) -> Result<Arc<Self>, QueryError> {
        let tunnel = options.ssh.clone().map(TunnelSlot::new);
        let client = client_for(&options, &options.endpoint, tunnel.clone())?;
        let protocol = detect(&client, &options).await?;
        let engine = Arc::new(Self { options, client, tunnel, protocol });
        engine.stateless("SELECT 1", &[]).await?;
        Ok(engine)
    }

    pub fn read_only(&self) -> bool {
        self.options.read_only
    }

    pub fn default_schema(&self) -> &str {
        "main"
    }

    pub async fn session(self: &Arc<Self>) -> Result<TursoSession, QueryError> {
        Ok(TursoSession {
            engine: self.clone(),
            client: self.client.clone(),
            path: self.options.endpoint.path.clone(),
            baton: None,
            stateful: false,
            notes: Vec::new(),
        })
    }

    fn authorized<'a>(&self, req: Request<'a>) -> Request<'a> {
        let req = req.header("User-Agent", concat!("BarSQL/", env!("CARGO_PKG_VERSION")));
        if self.options.token.is_empty() {
            req
        } else {
            req.header("Authorization", format!("Bearer {}", self.options.token))
        }
    }

    async fn post(&self, client: &HttpClient, target: &str, body: &[u8]) -> Result<Response, HError> {
        let req = self.authorized(Request::post(target, body)).header("Content-Type", "application/json");
        let mut res = client.send(&req).await.map_err(|err| HError { message: err.message, code: None })?;
        if !res.is_success() {
            let body = res.read_to_end(MAX_BODY).await.unwrap_or_default();
            return Err(request_error(res.status, &body));
        }
        Ok(res)
    }

    async fn pipeline(
        &self,
        client: &HttpClient,
        path: &str,
        baton: Option<String>,
        requests: Vec<PipelineRequest>,
    ) -> Result<PipelineResponse, HError> {
        let version = match self.protocol {
            Protocol::V3 => "v3",
            Protocol::V2 => "v2",
        };
        let body = serde_json::to_vec(&PipelineBody { baton, requests }).expect("serializable");
        let mut res = self.post(client, &format!("{path}/{version}/pipeline"), &body).await?;
        let body = res.read_to_end(MAX_BODY).await.map_err(|err| HError { message: err.message, code: None })?;
        serde_json::from_slice(&body)
            .map_err(|err| HError { message: format!("the server's answer isn't Hrana: {err}"), code: None })
    }

    // One statement on a stream of its own, closed with it. For the catalog and other reads of the app's own.
    async fn stateless(&self, sql: &str, params: &[LiteValue]) -> Result<LiteRows, QueryError> {
        let requests = vec![PipelineRequest::Execute { stmt: Stmt::new(sql, params) }, PipelineRequest::Close];
        let res =
            self.pipeline(&self.client, &self.options.endpoint.path, None, requests).await.map_err(hrana_error)?;
        match res.results.into_iter().next() {
            Some(PipelineResult::Ok { response: PipelineOk::Execute { result } }) => Ok(lite_rows(result)),
            Some(PipelineResult::Error { error }) => Err(hrana_error(error)),
            _ => Err(QueryError::message("the server's answer is missing the statement's result")),
        }
    }

    // The catalog's queries run synchronously, off the async threads, each as its own request.
    async fn catalog<T: Send + 'static>(
        self: &Arc<Self>,
        work: impl FnOnce(&mut dyn LiteSource) -> Result<T, QueryError> + Send + 'static,
    ) -> Result<T, QueryError> {
        let engine = self.clone();
        let handle = tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || work(&mut HranaSource { engine: &engine, handle }))
            .await
            .map_err(|err| QueryError::message(err.to_string()))?
    }

    pub async fn connection_info(self: &Arc<Self>) -> Result<ConnectionStatus, QueryError> {
        Ok(ConnectionStatus {
            connected: true,
            database: "main".into(),
            schema: "main".into(),
            host: self.options.endpoint.host.clone(),
            ..Default::default()
        })
    }

    pub async fn list_schemas(self: &Arc<Self>) -> Result<Vec<SchemaInfo>, QueryError> {
        Ok(vec![SchemaInfo { name: "main".into() }])
    }

    pub(crate) async fn dump_columns(self: &Arc<Self>, table: &str) -> Result<Vec<DumpColumn>, QueryError> {
        let table = table.to_string();
        self.catalog(move |src| catalog::dump_columns(src, &table)).await
    }

    pub async fn list_tables(self: &Arc<Self>, _schema: &str) -> Result<Vec<TableInfo>, QueryError> {
        self.catalog(catalog::tables).await
    }

    pub async fn list_columns(self: &Arc<Self>, _schema: &str, table: &str) -> Result<Vec<ColumnInfo>, QueryError> {
        let table = table.to_string();
        self.catalog(move |src| catalog::columns(src, &table)).await
    }

    pub async fn list_indexes(self: &Arc<Self>, _schema: &str, table: &str) -> Result<Vec<IndexInfo>, QueryError> {
        let table = table.to_string();
        self.catalog(move |src| catalog::indexes(src, &table)).await
    }

    pub async fn list_constraints(
        self: &Arc<Self>,
        _schema: &str,
        table: &str,
    ) -> Result<Vec<ConstraintInfo>, QueryError> {
        let table = table.to_string();
        self.catalog(move |src| catalog::constraints(src, &table)).await
    }

    pub async fn list_triggers(self: &Arc<Self>, _schema: &str, table: &str) -> Result<Vec<TriggerInfo>, QueryError> {
        let table = table.to_string();
        self.catalog(move |src| catalog::triggers(src, &table)).await
    }

    // libSQL has no routines.
    pub async fn list_routines(self: &Arc<Self>, _schema: &str) -> Result<Vec<RoutineInfo>, QueryError> {
        Ok(Vec::new())
    }

    pub async fn list_functions(self: &Arc<Self>) -> Result<FunctionList, QueryError> {
        self.catalog(catalog::functions).await
    }

    pub async fn object_ddl(self: &Arc<Self>, object: &ObjectRef) -> Result<String, QueryError> {
        let object = object.clone();
        self.catalog(move |src| catalog::object_ddl(src, &object)).await
    }

    pub async fn primary_keys(
        self: &Arc<Self>,
        schema: &str,
        table: &str,
    ) -> Result<(Vec<ColumnInfo>, Vec<String>), QueryError> {
        let cols = self.list_columns(schema, table).await?;
        let pks = primary_keys(&cols);
        Ok((cols, pks))
    }
}

fn client_for(
    options: &TursoOptions,
    endpoint: &Endpoint,
    tunnel: Option<Arc<TunnelSlot>>,
) -> Result<HttpClient, QueryError> {
    let tls = match endpoint.tls {
        true => Some(
            http_client_config(options.tls)
                .ok_or_else(|| QueryError::message("can't load the system's root certificates"))?,
        ),
        false => None,
    };
    let origin = Origin { tls: endpoint.tls, host: endpoint.host.clone(), port: endpoint.port };
    Ok(HttpClient::new(origin, tls, tunnel))
}

// v3 servers answer GET /v3, older ones only GET /v2.
async fn detect(client: &HttpClient, options: &TursoOptions) -> Result<Protocol, QueryError> {
    let path = &options.endpoint.path;
    let mut last = 0;
    for (version, protocol) in [("v3", Protocol::V3), ("v2", Protocol::V2)] {
        let target = format!("{path}/{version}");
        let mut req = Request::get(&target).header("User-Agent", concat!("BarSQL/", env!("CARGO_PKG_VERSION")));
        if !options.token.is_empty() {
            req = req.header("Authorization", format!("Bearer {}", options.token));
        }
        let mut res = client.send(&req).await?;
        let body = res.read_to_end(1024 * 1024).await?;
        if res.is_success() {
            return Ok(protocol);
        }
        if matches!(res.status, 401 | 403) {
            return Err(hrana_error(request_error(res.status, &body)));
        }
        last = res.status;
    }
    Err(QueryError::message(format!(
        "{}:{} doesn't speak libSQL's Hrana protocol (HTTP {last})",
        options.endpoint.host, options.endpoint.port
    )))
}

fn lite_rows(result: StmtResult) -> LiteRows {
    LiteRows {
        columns: result.cols.into_iter().map(|c| c.name.unwrap_or_default()).collect(),
        rows: result.rows.into_iter().map(|row| row.iter().map(|v| v.to_lite()).collect()).collect(),
    }
}

struct HranaSource<'a> {
    engine: &'a TursoEngine,
    handle: tokio::runtime::Handle,
}

impl LiteSource for HranaSource<'_> {
    fn query(&mut self, sql: &str, params: &[LiteValue]) -> Result<LiteRows, QueryError> {
        self.handle.block_on(self.engine.stateless(sql, params))
    }
}

pub struct TursoSession {
    engine: Arc<TursoEngine>,
    // The stream's server and path, which the server can move with base_url.
    client: HttpClient,
    path: String,
    // None until the first statement, and again once the stream ends or a run is cancelled.
    baton: Option<String>,
    // The stream holds something a new one wouldn't, like a PRAGMA the user set.
    stateful: bool,
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
    time_columns: Vec<bool>,
    builder: ChunkBuilder,
    rows: usize,
}

impl Streaming<'_> {
    async fn begin(&mut self, cols: Vec<hrana::Col>) {
        if cols.is_empty() {
            return;
        }
        let columns: Arc<[ColumnMeta]> = cols
            .into_iter()
            .map(|c| ColumnMeta {
                name: c.name.unwrap_or_default(),
                type_name: c.decltype.unwrap_or_default().to_uppercase(),
            })
            .collect();
        self.time_columns = columns.iter().map(|c| values::is_time_type(&c.type_name)).collect();
        self.builder = ChunkBuilder::new(columns.len(), self.batch_rows.min(1024));
        let _ = self.sink.send(ScriptEvent::Meta { result_index: self.index, columns: columns.clone() }).await;
        self.columns = Some(columns);
    }

    async fn row(&mut self, row: &[hrana::HValue]) {
        for (i, time) in self.time_columns.iter().enumerate() {
            match row.get(i) {
                Some(value) => value.with_ref(|v| values::push(v, *time, &mut self.builder)),
                None => self.builder.push_null(),
            }
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
        let width = self.time_columns.len();
        let full = std::mem::replace(&mut self.builder, ChunkBuilder::new(width, self.batch_rows.min(1024)));
        let _ = self.sink.send(ScriptEvent::Rows { result_index: self.index, chunk: Arc::new(full.finish()) }).await;
    }

    async fn end(&mut self, affected: u64, notes: Vec<String>, error: Option<QueryError>) -> StatementRun {
        self.flush().await;
        let summary = match &self.columns {
            Some(columns) => summary_for_rows(columns, self.rows, self.started),
            None if error.is_none() => summary_for_exec(affected, self.started),
            None => Default::default(),
        };
        emit_notes(self.sink, self.index, notes).await;
        let has_summary = self.columns.is_some() || error.is_none();
        let result = StatementResult {
            result_index: self.index,
            statement: self.stmt.into(),
            summary: has_summary.then_some(summary),
            error: error.clone(),
            ..Default::default()
        };
        let _ = self.sink.send(ScriptEvent::Result(Box::new(result))).await;
        StatementRun { sets: 1, error }
    }
}

impl TursoSession {
    pub fn is_broken(&self) -> bool {
        false
    }

    pub fn in_transaction(&self) -> bool {
        false
    }

    pub fn read_only(&self) -> bool {
        self.engine.read_only()
    }

    // Forgets the stream, and says so when it held something the user set. An idle stream expires after a few
    // seconds, so losing one that holds nothing is routine.
    fn drop_stream(&mut self, why: &str) {
        self.baton = None;
        if std::mem::take(&mut self.stateful) {
            self.notes.push(format!("{why} {LOST_STATE}"));
        }
    }

    // Follows the server to the base_url it names for the rest of the stream.
    fn follow(&mut self, baton: Option<String>, base_url: Option<String>) {
        self.baton = baton;
        let Some(base) = base_url.filter(|b| !b.is_empty()) else { return };
        let Ok(endpoint) = hrana::endpoint(&base) else { return };
        let origin = Origin { tls: endpoint.tls, host: endpoint.host.clone(), port: endpoint.port };
        if &origin != self.client.origin()
            && let Ok(client) = client_for(&self.engine.options, &endpoint, self.engine.tunnel.clone())
        {
            self.client = client;
        }
        self.path = endpoint.path;
    }

    pub async fn run_script(
        &mut self,
        statements: &[String],
        sink: &Sink,
        cancel: &Cancel,
    ) -> Result<usize, QueryError> {
        let result = script::run_script(self, statements, sink, cancel).await;
        let next = *result.as_ref().unwrap_or(&0);
        match self.close_open_transaction().await {
            Some(error) if result.is_ok() => {
                let result = StatementResult { result_index: next, error: Some(error.clone()), ..Default::default() };
                let _ = sink.send(ScriptEvent::Result(Box::new(result))).await;
                Err(error)
            }
            _ => result,
        }
    }

    // Rolls back a transaction the run left open, and says so.
    async fn close_open_transaction(&mut self) -> Option<QueryError> {
        if self.engine.protocol != Protocol::V3 || self.baton.is_none() {
            return None;
        }
        let requests = vec![PipelineRequest::GetAutocommit];
        let res = self.engine.pipeline(&self.client, &self.path, self.baton.clone(), requests).await.ok()?;
        self.follow(res.baton, res.base_url);
        let open = matches!(
            res.results.first(),
            Some(PipelineResult::Ok { response: PipelineOk::GetAutocommit { is_autocommit: false } })
        );
        if !open {
            return None;
        }
        let rollback = vec![PipelineRequest::Execute { stmt: Stmt::new("ROLLBACK", &[]) }];
        match self.engine.pipeline(&self.client, &self.path, self.baton.clone(), rollback).await {
            Ok(res) => self.follow(res.baton, res.base_url),
            Err(_) => self.baton = None,
        }
        Some(QueryError::message(format!("Rolled back. {NO_TRANSACTIONS}")))
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

    async fn execute_args(&mut self, sql: &str, params: &[Value]) -> Result<StmtResult, QueryError> {
        let args: Vec<LiteValue> = params.iter().map(LiteValue::from_value).collect();
        let requests = vec![PipelineRequest::Execute { stmt: Stmt::new(sql, &args) }];
        let res =
            self.engine.pipeline(&self.client, &self.path, self.baton.clone(), requests).await.map_err(hrana_error)?;
        self.follow(res.baton, res.base_url);
        match res.results.into_iter().next() {
            Some(PipelineResult::Ok { response: PipelineOk::Execute { result } }) => Ok(result),
            Some(PipelineResult::Error { error }) => Err(hrana_error(error)),
            _ => Err(QueryError::message("the server's answer is missing the statement's result")),
        }
    }

    pub async fn execute_params(&mut self, sql: &str, params: &[Value]) -> Result<u64, QueryError> {
        Ok(self.execute_args(sql, params).await?.affected_row_count)
    }

    // With the rows it returns, for INSERT … RETURNING.
    pub async fn buffered_params(&mut self, sql: &str, params: &[Value]) -> Result<Buffered, QueryError> {
        let result = self.execute_args(sql, params).await?;
        let columns: Arc<[ColumnMeta]> = result
            .cols
            .iter()
            .map(|c| ColumnMeta {
                name: c.name.clone().unwrap_or_default(),
                type_name: c.decltype.clone().unwrap_or_default().to_uppercase(),
            })
            .collect();
        let times: Vec<bool> = columns.iter().map(|c| values::is_time_type(&c.type_name)).collect();
        let mut builder = ChunkBuilder::new(columns.len(), result.rows.len().max(1));
        for row in &result.rows {
            for (i, time) in times.iter().enumerate() {
                match row.get(i) {
                    Some(value) => value.with_ref(|v| values::push(v, *time, &mut builder)),
                    None => builder.push_null(),
                }
            }
            builder.end_row();
        }
        let summary = summary_for_rows(&columns, result.rows.len(), Instant::now());
        Ok(Buffered { columns, chunk: builder.finish(), summary: Some(summary) })
    }

    // Sends the statement, retrying once on a new stream when the server had dropped this one.
    async fn execute(&mut self, out: &mut Streaming<'_>) -> StatementRun {
        for attempt in 0..2 {
            let result = match self.engine.protocol {
                Protocol::V3 => self.cursor(out).await,
                Protocol::V2 => self.pipeline_statement(out).await,
            };
            match result {
                Err(error) if attempt == 0 && error.lost_stream() && self.baton.is_some() => {
                    self.drop_stream("The server had closed this tab's stream, so the statement ran on a new one.");
                }
                // A request the server refused, like one it couldn't parse, ends the stream.
                Err(error) => {
                    self.drop_stream("The server closed this tab's stream after the error.");
                    let error = statement_error(error, out.stmt);
                    return out.end(0, std::mem::take(&mut self.notes), Some(error)).await;
                }
                Ok(run) => {
                    self.stateful |= run.error.is_none() && sets_state(out.stmt);
                    return run;
                }
            }
        }
        unreachable!("the second attempt returns")
    }

    async fn cursor(&mut self, out: &mut Streaming<'_>) -> Result<StatementRun, HError> {
        let steps = vec![CursorStep { stmt: Stmt::new(out.stmt, &[]) }];
        let body = serde_json::to_vec(&CursorBody { baton: self.baton.clone(), batch: CursorBatch { steps } })
            .expect("serializable");
        let target = format!("{}/v3/cursor", self.path);
        let mut res = self.engine.post(&self.client, &target, &body).await?;
        let lost = |err: crate::http::HttpError| HError { message: err.message, code: None };
        let head = res.next_line(MAX_LINE).await.map_err(lost)?.unwrap_or_default();
        let head: CursorHead = serde_json::from_slice(&head)
            .map_err(|err| HError { message: format!("the server's answer isn't Hrana: {err}"), code: None })?;
        self.follow(head.baton, head.base_url);
        while let Some(line) = res.next_line(MAX_LINE).await.map_err(lost)? {
            if line.is_empty() {
                continue;
            }
            let entry: CursorEntry = serde_json::from_slice(&line)
                .map_err(|err| HError { message: format!("the server's answer isn't Hrana: {err}"), code: None })?;
            match entry {
                CursorEntry::StepBegin { cols } => out.begin(cols).await,
                CursorEntry::Row { row } => out.row(&row).await,
                CursorEntry::StepEnd { affected_row_count } => {
                    return Ok(out.end(affected_row_count, std::mem::take(&mut self.notes), None).await);
                }
                CursorEntry::StepError { error, .. } | CursorEntry::Error { error } => {
                    let error = statement_error(error, out.stmt);
                    return Ok(out.end(0, std::mem::take(&mut self.notes), Some(error)).await);
                }
                CursorEntry::Other => {}
            }
        }
        Err(HError { message: "the server ended its answer before the statement finished".into(), code: None })
    }

    async fn pipeline_statement(&mut self, out: &mut Streaming<'_>) -> Result<StatementRun, HError> {
        let requests = vec![PipelineRequest::Execute { stmt: Stmt::new(out.stmt, &[]) }];
        let res = self.engine.pipeline(&self.client, &self.path, self.baton.clone(), requests).await?;
        self.follow(res.baton, res.base_url);
        match res.results.into_iter().next() {
            Some(PipelineResult::Ok { response: PipelineOk::Execute { result } }) => {
                out.begin(result.cols).await;
                for row in &result.rows {
                    out.row(row).await;
                }
                Ok(out.end(result.affected_row_count, std::mem::take(&mut self.notes), None).await)
            }
            Some(PipelineResult::Error { error }) => {
                let error = statement_error(error, out.stmt);
                Ok(out.end(0, std::mem::take(&mut self.notes), Some(error)).await)
            }
            _ => Err(HError { message: "the server's answer is missing the statement's result".into(), code: None }),
        }
    }
}

// What lasts for the stream: PRAGMA assignments and attached databases.
fn sets_state(stmt: &str) -> bool {
    let code = strip_leading_comments(stmt);
    match first_keyword(code).as_str() {
        "ATTACH" | "DETACH" => true,
        "PRAGMA" => code.contains('='),
        _ => false,
    }
}

// The server drops an unused stream on its own after a few seconds, but closing it frees it now.
impl Drop for TursoSession {
    fn drop(&mut self) {
        let (Some(baton), Ok(handle)) = (self.baton.take(), tokio::runtime::Handle::try_current()) else { return };
        let (engine, client, path) = (self.engine.clone(), self.client.clone(), self.path.clone());
        handle.spawn(async move {
            let _ = engine.pipeline(&client, &path, Some(baton), vec![PipelineRequest::Close]).await;
        });
    }
}

impl StatementRunner for TursoSession {
    fn driver(&self) -> DriverType {
        DriverType::Turso
    }

    // Cancelling drops the request. The server finishes or abandons the statement on its own, and the stream's
    // state is unknown afterwards, so the next statement starts a new one.
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
            time_columns: Vec::new(),
            builder: ChunkBuilder::new(0, 1),
            rows: 0,
        };
        if cancel.is_cancelled() {
            return out.end(0, Vec::new(), Some(QueryError::cancelled())).await;
        }
        let run = tokio::select! {
            biased;
            _ = cancel.cancelled() => None,
            run = self.execute(&mut out) => Some(run),
        };
        match run {
            Some(run) => run,
            None => {
                self.drop_stream("Cancelling closed this tab's stream.");
                out.end(0, std::mem::take(&mut self.notes), Some(QueryError::cancelled())).await
            }
        }
    }
}
