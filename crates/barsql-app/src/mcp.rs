// `BarSQL --mcp`: an MCP server over stdio for the connections shared with AI assistants.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use barsql_core::{ConnectionConfig, DriverType, ObjectKind, ObjectRef, QueryError};
use barsql_db::{ColumnMeta, ResultChunk};
use serde_json::{Value, json};
use tokio::sync::{oneshot, watch};

use crate::{BarApp, ChangeOutcome, RunEvent, lock};

// Newest first; other versions get the newest.
const PROTOCOLS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
const TOOLS: [&str; 7] =
    ["list_connections", "list_tables", "describe_table", "show_ddl", "run_query", "explain_query", "run_change"];
const MAX_ROWS: usize = 200;
const MAX_CELL_CHARS: usize = 2_000;
// Rough cap on row bytes per answer, so wide rows can't flood the assistant's context.
const MAX_RESULT_BYTES: usize = 100_000;
const QUERY_TIMEOUT: Duration = Duration::from_secs(30);
// How long a stopped change gets to roll back before it's left running.
const STOP_GRACE: Duration = Duration::from_secs(5);
const CONFIRM_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const MAX_CONFIRM_CHARS: usize = 4_000;
const LOG_LIMIT: u64 = 1 << 20;

const PARSE_ERROR: i64 = -32700;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

const INSTRUCTIONS: &str = "Access to the databases the user shared from BarSQL. Start with list_connections, then \
    list_tables and describe_table before writing SQL in the connection's dialect. run_query only reads, and returns at \
    most 200 rows, so filter, aggregate or LIMIT in SQL. Only connections with canChange take run_change: preview \
    anything risky first.";
const NOTHING_SHARED: &str = "No connections are shared with AI assistants yet. In BarSQL, edit a connection and \
    tick \"Allow AI assistants (MCP)\".";
const CANCELLED: &str = "The call was cancelled before it ran.";

pub fn serve(data_dir: &Path, runtime: tokio::runtime::Handle) -> io::Result<()> {
    let bar = BarApp::open_for_mcp(data_dir, runtime)?;
    let (replies, outbox) = mpsc::channel::<Value>();
    // One writer, so replies from concurrent calls never interleave.
    std::thread::spawn(move || {
        let mut out = io::stdout().lock();
        for reply in outbox {
            if out.write_all(format!("{reply}\n").as_bytes()).and_then(|()| out.flush()).is_err() {
                return;
            }
        }
    });
    let server = Arc::new(Server::new(bar.clone(), data_dir.join("mcp.log"), replies));
    for line in io::stdin().lock().split(b'\n') {
        server.receive(&String::from_utf8_lossy(&line?));
    }
    // A connection the database won't let go of mustn't keep this process alive once the client has quit.
    bar.runtime().block_on(async {
        let _ = tokio::time::timeout(STOP_GRACE, bar.shutdown()).await;
    });
    Ok(())
}

// An AppImage runs from a temporary mount, so it's named by its file. Windows' canonical paths start with \\?\, 
// which AI apps can't start a program from, so dunce gives the plain form.
pub fn setup_json() -> String {
    let install = crate::update::install();
    let command = match install.kind {
        crate::update::Kind::AppImage => install.target.clone(),
        _ => std::env::current_exe().map(|exe| dunce::canonicalize(&exe).unwrap_or(exe)).unwrap_or_default(),
    };
    let server = json!({
        "command": command,
        "args": ["--mcp"],
        "env": { "BARSQL_DATA_DIR": dunce::simplified(&barsql_core::paths::data_dir()) },
    });
    serde_json::to_string_pretty(&json!({ "mcpServers": { "barsql": server } })).unwrap_or_default()
}

pub(crate) struct Server {
    bar: BarApp,
    log: PathBuf,
    replies: mpsc::Sender<Value>,
    next_tab: AtomicU64,
    running: Mutex<HashMap<String, Call>>,
    elicitation: AtomicBool,
    next_question: AtomicU64,
    questions: Mutex<HashMap<String, oneshot::Sender<Value>>>,
    // What the last tools/list offered, so the client hears when that changes.
    listed_changes: AtomicBool,
}

struct Call {
    tab: String,
    cancelled: watch::Sender<bool>,
}

impl Server {
    pub(crate) fn new(bar: BarApp, log: PathBuf, replies: mpsc::Sender<Value>) -> Self {
        Self {
            bar,
            log,
            replies,
            next_tab: AtomicU64::new(1),
            running: Mutex::default(),
            elicitation: AtomicBool::new(false),
            next_question: AtomicU64::new(1),
            questions: Mutex::default(),
            listed_changes: AtomicBool::new(false),
        }
    }

    pub(crate) fn receive(self: &Arc<Self>, line: &str) {
        if line.trim().is_empty() {
            return;
        }
        let msg: Value = match serde_json::from_str(line) {
            Ok(msg) => msg,
            Err(err) => return self.error(Value::Null, PARSE_ERROR, &err.to_string()),
        };
        let method = msg["method"].as_str().unwrap_or_default();
        // A message without a method answers one of this server's questions.
        if method.is_empty() {
            let waiting = msg["id"].as_str().and_then(|id| lock(&self.questions).remove(id));
            if let Some(waiting) = waiting {
                let _ = waiting.send(msg);
            }
            return;
        }
        let Some(id) = msg.get("id").cloned() else {
            if method == "notifications/cancelled" {
                self.cancel(&msg["params"]["requestId"]);
            }
            return;
        };
        let params = &msg["params"];
        match method {
            "initialize" => {
                self.elicitation.store(params["capabilities"]["elicitation"].is_object(), Ordering::Relaxed);
                self.reply(id, initialize(params));
            }
            "ping" => self.reply(id, json!({})),
            "tools/list" => {
                let _ = self.bar.reload_connections();
                let changes = self.bar.list_connections().iter().any(|cfg| cfg.mcp_changes);
                self.listed_changes.store(changes, Ordering::Relaxed);
                self.reply(id, json!({ "tools": tools(changes) }));
            }
            "tools/call" if TOOLS.contains(&params["name"].as_str().unwrap_or_default()) => {
                // Own tab, so calls don't cancel each other. Registered now, so an immediate cancel finds it.
                let tab = format!("mcp-{}", self.next_tab.fetch_add(1, Ordering::Relaxed));
                let (cancelled, watching) = watch::channel(false);
                lock(&self.running).insert(id.to_string(), Call { tab: tab.clone(), cancelled });
                let (server, params) = (self.clone(), params.clone());
                self.bar.runtime().spawn(async move { server.call(id, params, tab, watching).await });
            }
            "tools/call" => self.error(id, INVALID_PARAMS, &format!("unknown tool: {}", params["name"])),
            _ => self.error(id, METHOD_NOT_FOUND, &format!("method not found: {method}")),
        }
    }

    async fn call(self: Arc<Self>, id: Value, params: Value, tab: String, cancelled: watch::Receiver<bool>) {
        let tool = params["name"].as_str().unwrap_or_default();
        let args = &params["arguments"];
        let started = Instant::now();
        let result = if *cancelled.borrow() {
            Err(CANCELLED.into())
        } else {
            self.run(tool, args, &tab, cancelled.clone()).await
        };
        // Gives the connection back at once: SQLite has only one.
        self.bar.cleanup_tab(&tab).await;
        self.log(tool, args, started.elapsed(), &result);
        // Unless a newer call took the id.
        if let Entry::Occupied(call) = lock(&self.running).entry(id.to_string())
            && call.get().tab == tab
        {
            call.remove();
        }
        // A cancelled call gets no answer.
        if *cancelled.borrow() {
            return;
        }
        let (text, failed) = match result {
            Ok(text) => (text, false),
            Err(text) => (text, true),
        };
        self.reply(id, json!({ "content": [{ "type": "text", "text": text }], "isError": failed }));
    }

    async fn run(
        &self,
        tool: &str,
        args: &Value,
        tab: &str,
        cancelled: watch::Receiver<bool>,
    ) -> Result<String, String> {
        self.bar.reload_connections().map_err(failed)?;
        let shared = self.bar.list_connections();
        let changes = shared.iter().any(|cfg| cfg.mcp_changes);
        if self.listed_changes.swap(changes, Ordering::Relaxed) != changes {
            let _ = self.replies.send(json!({ "jsonrpc": "2.0", "method": "notifications/tools/list_changed" }));
        }
        if tool == "list_connections" {
            return Ok(list_connections(&shared));
        }
        let cfg = connection(&shared, args)?;
        match tool {
            "list_tables" => self.list_tables(cfg, args).await,
            "describe_table" => self.describe_table(cfg, args).await,
            "show_ddl" => self.show_ddl(cfg, args).await,
            "run_query" => self.run_query(cfg, args, tab).await,
            "explain_query" => self.explain_query(cfg, args, tab).await,
            "run_change" => self.run_change(cfg, args, tab, cancelled).await,
            _ => Err(format!("unknown tool: {tool}")),
        }
    }

    async fn list_tables(&self, cfg: &ConnectionConfig, args: &Value) -> Result<String, String> {
        let schema = optional(args, "schema");
        if !schema.is_empty() {
            let tables = self.bar.list_tables(&cfg.id, schema).await.map_err(failed)?;
            return Ok(json!({ "tables": tables }).to_string());
        }
        let bundle = self.bar.load_schema_data(&cfg.id).await.map_err(failed)?;
        let schemas: Vec<&str> = bundle.schemas.iter().map(|schema| schema.name.as_str()).collect();
        let tables: Vec<_> = bundle.loaded_tables.iter().flat_map(|loaded| &loaded.tables).collect();
        Ok(json!({ "schemas": schemas, "tables": tables }).to_string())
    }

    // Views have no structure on some engines, and their columns still help.
    async fn describe_table(&self, cfg: &ConnectionConfig, args: &Value) -> Result<String, String> {
        let (schema, table) = (schema_of(args, cfg), required(args, "table")?);
        if let Ok(mut structure) = self.bar.table_structure(&cfg.id, &schema, table).await
            && !structure.columns.is_empty()
        {
            structure.create_sql.clear();
            return Ok(json!(structure).to_string());
        }
        let columns = self.bar.list_columns(&cfg.id, &schema, table).await.map_err(failed)?;
        if columns.is_empty() {
            return Err(format!("no table or view named {table} in schema {schema}"));
        }
        Ok(json!({ "schema": schema, "name": table, "columns": columns }).to_string())
    }

    async fn show_ddl(&self, cfg: &ConnectionConfig, args: &Value) -> Result<String, String> {
        let kind = optional(args, "kind");
        let object = ObjectRef {
            schema: schema_of(args, cfg),
            name: required(args, "name")?.into(),
            kind: if kind.is_empty() { ObjectKind::Table } else { ObjectKind::parse(kind) },
            table: optional(args, "table").into(),
            ..Default::default()
        };
        self.bar.object_ddl(&cfg.id, &object).await.map_err(failed)
    }

    // Returning early drops the run; cleaning up its tab cancels the rest.
    async fn run_query(&self, cfg: &ConnectionConfig, args: &Value, tab: &str) -> Result<String, String> {
        let sql = required(args, "sql")?;
        let run = self.bar.execute_query_stream(&cfg.id, tab, sql).await.map_err(failed)?;
        let mut rows = Rows::default();
        let read = async {
            while let Ok(event) = run.events.recv().await {
                match event {
                    RunEvent::Meta { columns, .. } => rows = Rows::new(&columns),
                    RunEvent::Rows { chunk, .. } => {
                        if !rows.add(&chunk) {
                            break;
                        }
                    }
                    RunEvent::Result(result) => {
                        if let Some(error) = result.error {
                            return Err(failed(error));
                        }
                    }
                    RunEvent::Done { error: Some(error), .. } => return Err(failed(error)),
                    _ => {}
                }
            }
            Ok(())
        };
        tokio::time::timeout(QUERY_TIMEOUT, read).await.map_err(|_| stopped())??;
        Ok(rows.to_text())
    }

    async fn explain_query(&self, cfg: &ConnectionConfig, args: &Value, tab: &str) -> Result<String, String> {
        let (sql, analyze) = (required(args, "sql")?, args["analyze"].as_bool().unwrap_or(false));
        let plan = tokio::time::timeout(QUERY_TIMEOUT, self.bar.explain_query(&cfg.id, tab, sql, analyze))
            .await
            .map_err(|_| stopped())?
            .map_err(failed)?;
        Ok(json!({
            "explainSql": plan.explain_sql,
            "analyzed": plan.analyzed,
            "totalCost": plan.total_cost,
            "planningMs": plan.planning_ms,
            "executionMs": plan.execution_ms,
            "nodes": plan.nodes,
        })
        .to_string())
    }

    async fn run_change(
        &self,
        cfg: &ConnectionConfig,
        args: &Value,
        tab: &str,
        mut cancelled: watch::Receiver<bool>,
    ) -> Result<String, String> {
        let sql = required(args, "sql")?;
        let (preview, all_rows) = (args["preview"] == true, args["allRows"] == true);
        self.bar.check_change(&cfg.id, sql, preview).map_err(failed)?;
        let unbounded = barsql_sql::unbounded_writes(&cfg.driver, sql);
        if !unbounded.is_empty() && !all_rows {
            let verbs = unbounded.join(" and ");
            return Err(format!(
                "{verbs} without a WHERE changes every row. Add a WHERE, or set allRows if that's meant."
            ));
        }
        if cfg.confirm_changes {
            self.confirm(cfg, sql, preview, &mut cancelled).await?;
        }
        // The client may have cancelled while the user decided.
        if *cancelled.borrow() {
            return Err(CANCELLED.into());
        }
        // Owned, so a change that won't stop can be left running instead of dropped halfway.
        let bar = self.bar.clone();
        let (id, job, sql, confirmed) = (cfg.id.clone(), tab.to_string(), sql.to_string(), cfg.confirm_changes);
        let mut apply = Box::pin(async move { bar.apply_change(&id, &job, &sql, preview, confirmed).await });
        // Biased: the change's first poll registers its job, so a cancel always finds it.
        let stop = tokio::select! {
            biased;
            outcome = &mut apply => return changed(outcome),
            () = cancellation(&mut cancelled) => "cancelled".to_string(),
            () = tokio::time::sleep(QUERY_TIMEOUT) => stopped(),
        };
        // A cancelled change rolls back before it returns.
        self.bar.cancel_query(tab);
        match tokio::time::timeout(STOP_GRACE, &mut apply).await {
            Ok(outcome) => changed(outcome).map_err(|error| format!("{stop}: {error}")),
            Err(_) => {
                self.bar.runtime().spawn(apply);
                Err(format!(
                    "{stop}, but the database hasn't stopped the change yet. Check what it changed before retrying."
                ))
            }
        }
    }

    // No window here, so the AI app asks the user.
    async fn confirm(
        &self,
        cfg: &ConnectionConfig,
        sql: &str,
        preview: bool,
        cancelled: &mut watch::Receiver<bool>,
    ) -> Result<(), String> {
        if !self.elicitation.load(Ordering::Relaxed) {
            return Err(format!(
                "{} asks before every change, and this AI app can't ask the user. Turn off \"Ask before running \
                 changes\" for it in BarSQL, or use an AI app that supports MCP elicitation.",
                cfg.name
            ));
        }
        let id = format!("barsql-{}", self.next_question.fetch_add(1, Ordering::Relaxed));
        let (answer, answered) = oneshot::channel();
        lock(&self.questions).insert(id.clone(), answer);
        let sql = shorten(sql.to_string(), MAX_CONFIRM_CHARS);
        let message = if preview {
            format!("Try this change on {} and roll it back?\n\n{sql}", cfg.name)
        } else {
            format!("Run this change on {}?\n\n{sql}", cfg.name)
        };
        let approve = json!({ "type": "boolean", "title": "Approve", "default": false });
        let schema = json!({ "type": "object", "properties": { "approve": approve }, "required": ["approve"] });
        let params = json!({ "message": message, "requestedSchema": schema });
        let _ =
            self.replies.send(json!({ "jsonrpc": "2.0", "id": id, "method": "elicitation/create", "params": params }));
        let reply = tokio::select! {
            reply = tokio::time::timeout(CONFIRM_TIMEOUT, answered) => Ok(reply.ok().and_then(Result::ok)),
            () = cancellation(cancelled) => Err(CANCELLED.to_string()),
        };
        // Unanswered: let the AI app take the question down.
        if lock(&self.questions).remove(&id).is_some() {
            let params = json!({ "requestId": id });
            let _ =
                self.replies.send(json!({ "jsonrpc": "2.0", "method": "notifications/cancelled", "params": params }));
        }
        let reply = reply?.unwrap_or_default();
        if let Some(error) = reply["error"]["message"].as_str() {
            return Err(format!("The AI app couldn't ask the user: {error}"));
        }
        let result = &reply["result"];
        if result["action"] == "accept" && result["content"]["approve"] == true {
            Ok(())
        } else {
            Err("The user didn't approve the change.".into())
        }
    }

    fn cancel(&self, request: &Value) {
        if let Some(call) = lock(&self.running).get(&request.to_string()) {
            call.cancelled.send_replace(true);
            self.bar.cancel_query(&call.tab);
        }
    }

    // One line per call in a single write, so concurrent calls and servers don't interleave.
    fn log(&self, tool: &str, args: &Value, took: Duration, result: &Result<String, String>) {
        let mut entry = json!({
            "at": barsql_core::clock::now_rfc3339(),
            "tool": tool,
            "arguments": args,
            "ms": took.as_millis() as u64,
        });
        match result {
            Err(error) => entry["error"] = json!(error),
            // Only a change's outcome: query results would be large.
            Ok(outcome) if tool == "run_change" => {
                entry["result"] = serde_json::from_str(outcome).unwrap_or_else(|_| json!(outcome));
            }
            Ok(_) => {}
        }
        if fs::metadata(&self.log).is_ok_and(|meta| meta.len() > LOG_LIMIT) {
            let _ = fs::rename(&self.log, self.log.with_extension("log.1"));
        }
        let mut options = OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        if let Ok(mut file) = options.open(&self.log) {
            let _ = file.write_all(format!("{entry}\n").as_bytes());
        }
    }

    fn reply(&self, id: Value, result: Value) {
        let _ = self.replies.send(json!({ "jsonrpc": "2.0", "id": id, "result": result }));
    }

    fn error(&self, id: Value, code: i64, message: &str) {
        let error = json!({ "code": code, "message": message });
        let _ = self.replies.send(json!({ "jsonrpc": "2.0", "id": id, "error": error }));
    }
}

// The last result set of a run, cut to the answer's limits.
#[derive(Default)]
struct Rows {
    columns: Vec<String>,
    types: Vec<String>,
    rows: Vec<Vec<barsql_core::Value>>,
    bytes: usize,
    truncated: bool,
    seen: bool,
}

impl Rows {
    fn new(columns: &[ColumnMeta]) -> Self {
        Self {
            columns: columns.iter().map(|column| column.name.clone()).collect(),
            types: columns.iter().map(|column| column.type_name.clone()).collect(),
            seen: true,
            ..Default::default()
        }
    }

    // False once a limit is reached.
    fn add(&mut self, chunk: &ResultChunk) -> bool {
        for row in 0..chunk.rows() {
            if self.rows.len() == MAX_ROWS || self.bytes > MAX_RESULT_BYTES {
                self.truncated = true;
                return false;
            }
            let values: Vec<_> = (0..chunk.columns()).map(|col| clip(chunk.cell(row, col).to_value())).collect();
            self.bytes += values.iter().map(|value| value.as_text().map_or(8, str::len)).sum::<usize>();
            self.rows.push(values);
        }
        true
    }

    fn to_text(&self) -> String {
        if !self.seen {
            return "The statement returned no rows.".into();
        }
        let mut answer = json!({ "columns": self.columns, "types": self.types, "rows": self.rows });
        if self.truncated {
            answer["note"] = json!(format!(
                "Only the first {} rows are shown. Filter, aggregate or LIMIT in SQL to see the rest.",
                self.rows.len()
            ));
        }
        answer.to_string()
    }
}

fn clip(value: barsql_core::Value) -> barsql_core::Value {
    match value {
        barsql_core::Value::Text(text) => barsql_core::Value::Text(shorten(text, MAX_CELL_CHARS)),
        other => other,
    }
}

fn shorten(mut text: String, max_chars: usize) -> String {
    if let Some((end, _)) = text.char_indices().nth(max_chars) {
        text.truncate(end);
        text.push('…');
    }
    text
}

fn initialize(params: &Value) -> Value {
    let asked = params["protocolVersion"].as_str().unwrap_or_default();
    json!({
        "protocolVersion": PROTOCOLS.into_iter().find(|version| *version == asked).unwrap_or(PROTOCOLS[0]),
        "capabilities": { "tools": { "listChanged": true } },
        "serverInfo": { "name": "BarSQL", "version": crate::VERSION },
        "instructions": INSTRUCTIONS,
    })
}

fn tools(changes: bool) -> Value {
    let connection = json!({ "type": "string", "description": "A connection id from list_connections." });
    let schema =
        json!({ "type": "string", "description": "Schema name. Defaults to the connection's default schema." });
    let sql = json!({ "type": "string", "description": "One read-only statement in the connection's SQL dialect." });
    let kinds = ["table", "view", "materialized view", "function", "procedure", "trigger", "index"];
    let mut tools = vec![
        tool(
            "list_connections",
            "List the database connections the user shared with AI assistants. Every other tool takes a connection id \
             from this list.",
            json!({}),
            &[],
        ),
        tool(
            "list_tables",
            "List tables and views. Without a schema, it also lists the names of all schemas.",
            json!({ "connection": connection, "schema": schema }),
            &["connection"],
        ),
        tool(
            "describe_table",
            "Describe a table or view: its columns with types, nullability and defaults, and for a table its primary \
             key, indexes, foreign keys and checks.",
            json!({ "connection": connection, "schema": schema, "table": { "type": "string" } }),
            &["connection", "table"],
        ),
        tool(
            "show_ddl",
            "Show the CREATE statement of a table, view, function, procedure, trigger or index.",
            json!({
                "connection": connection,
                "schema": schema,
                "name": { "type": "string" },
                "kind": { "type": "string", "enum": kinds, "description": "Defaults to table." },
                "table": { "type": "string", "description": "The table a trigger or index belongs to." },
            }),
            &["connection", "name"],
        ),
        tool(
            "run_query",
            "Run read-only SQL and return the rows of its last result, at most 200. Statements that change data or \
             schema are refused.",
            json!({ "connection": connection, "sql": sql }),
            &["connection", "sql"],
        ),
        tool(
            "explain_query",
            "Show how the database would run a read-only statement. With analyze, the statement runs and the plan \
             includes actual rows and timings.",
            json!({ "connection": connection, "sql": sql, "analyze": { "type": "boolean" } }),
            &["connection", "sql"],
        ),
    ];
    if changes {
        let mut change = tool(
            "run_change",
            "Change data or schema (INSERT, UPDATE, DELETE, CREATE, ALTER...) on a connection whose canChange is true. \
             It runs in one transaction where the database has them, and reports the rows affected. Set preview to \
             run it and roll it back. An UPDATE or DELETE without a WHERE needs allRows.",
            json!({
                "connection": connection,
                "sql": { "type": "string", "description": "The change in the connection's SQL dialect, with no \
                         BEGIN, COMMIT or ROLLBACK." },
                "preview": { "type": "boolean", "description": "Run it, report what it would change, and roll back." },
                "allRows": { "type": "boolean", "description": "Confirms an UPDATE or DELETE that has no WHERE." },
            }),
            &["connection", "sql"],
        );
        change["annotations"] = json!({ "readOnlyHint": false, "destructiveHint": true });
        tools.push(change);
    }
    Value::from(tools)
}

fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": { "type": "object", "properties": properties, "required": required },
        "annotations": { "readOnlyHint": true },
    })
}

fn list_connections(shared: &[ConnectionConfig]) -> String {
    let listed: Vec<Value> = (shared.iter())
        .map(|cfg| {
            json!({
                "id": cfg.id,
                "name": cfg.name,
                "engine": cfg.driver.as_str(),
                "database": database_name(cfg),
                "defaultSchema": cfg.default_browse_schema(),
                "canChange": cfg.mcp_changes,
            })
        })
        .collect();
    if listed.is_empty() { NOTHING_SHARED.into() } else { Value::from(listed).to_string() }
}

fn connection<'a>(shared: &'a [ConnectionConfig], args: &Value) -> Result<&'a ConnectionConfig, String> {
    let id = required(args, "connection")?;
    (shared.iter().find(|cfg| cfg.id == id))
        .ok_or_else(|| format!("no shared connection has the id {id}; list_connections lists them"))
}

// Resolves on cancel; never once the call is unregistered.
async fn cancellation(cancelled: &mut watch::Receiver<bool>) {
    if cancelled.wait_for(|cancelled| *cancelled).await.is_err() {
        std::future::pending::<()>().await;
    }
}

// A SQLite file's name, never its path.
fn database_name(cfg: &ConnectionConfig) -> String {
    if cfg.driver != DriverType::Sqlite {
        return cfg.database.clone();
    }
    Path::new(&cfg.file_path).file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default()
}

fn required<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    Some(optional(args, key)).filter(|value| !value.is_empty()).ok_or_else(|| format!("`{key}` is required"))
}

fn optional<'a>(args: &'a Value, key: &str) -> &'a str {
    args[key].as_str().map(str::trim).unwrap_or_default()
}

fn schema_of(args: &Value, cfg: &ConnectionConfig) -> String {
    match optional(args, "schema") {
        "" => cfg.default_browse_schema(),
        schema => schema.into(),
    }
}

fn changed(outcome: Result<ChangeOutcome, QueryError>) -> Result<String, String> {
    outcome.map(|outcome| json!(outcome).to_string()).map_err(failed)
}

fn failed(error: QueryError) -> String {
    error.message
}

fn stopped() -> String {
    format!("stopped after {} seconds", QUERY_TIMEOUT.as_secs())
}

#[cfg(test)]
mod tests;
