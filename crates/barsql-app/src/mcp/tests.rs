use std::cell::Cell;
use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use barsql_core::{ConnectionConfig, DriverType};
use serde_json::{Value, json};

use super::{MAX_ROWS, Server, TOOLS, lock};
use crate::BarApp;

const ENDLESS: &str = "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n) SELECT count(*) FROM n";

struct Setup {
    runtime: tokio::runtime::Runtime,
    dir: tempfile::TempDir,
    // The user's app, on the server's data folder.
    app: BarApp,
    shared: ConnectionConfig,
    private: ConnectionConfig,
    server: Arc<Server>,
    replies: mpsc::Receiver<Value>,
    list_changes: Cell<usize>,
}

fn setup() -> Setup {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_stack_size(crate::WORKER_STACK)
        .enable_all()
        .build()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let app = BarApp::open(&data, runtime.handle().clone()).unwrap();
    let sqlite = |name: &str, mcp: bool| ConnectionConfig {
        name: name.into(),
        driver: DriverType::Sqlite,
        file_path: dir.path().join(format!("{name}.db")).display().to_string(),
        password: "hunter2".into(),
        mcp,
        ..Default::default()
    };
    let shared = runtime.block_on(app.save_connection(sqlite("shop", true))).unwrap();
    let private = runtime.block_on(app.save_connection(sqlite("payroll", false))).unwrap();
    let seed = "CREATE TABLE orders AS WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 500) \
                SELECT i AS id, 'order ' || i AS label FROM n";
    runtime.block_on(app.execute_query(&shared.id, seed)).unwrap();
    let mcp = BarApp::open_for_mcp(&data, runtime.handle().clone()).unwrap();
    let (replies_tx, replies) = mpsc::channel();
    let server = Arc::new(Server::new(mcp, data.join("mcp.log"), replies_tx));
    Setup { runtime, dir, app, shared, private, server, replies, list_changes: Cell::new(0) }
}

impl Setup {
    fn data(&self, file: &str) -> PathBuf {
        self.dir.path().join("data").join(file)
    }

    fn send(&self, msg: Value) {
        self.server.receive(&msg.to_string());
    }

    // Skips tool-list notices, counting them.
    fn next(&self) -> Value {
        loop {
            let msg = self.replies.recv_timeout(Duration::from_secs(20)).expect("a message");
            if msg["method"] != "notifications/tools/list_changed" {
                return msg;
            }
            self.list_changes.set(self.list_changes.get() + 1);
        }
    }

    fn request(&self, id: i64, method: &str, params: Value) -> Value {
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        let reply = self.next();
        assert_eq!(reply["id"], id, "{reply}");
        reply
    }

    fn allow_changes(&self, ask: bool) {
        let cfg = ConnectionConfig { mcp_changes: true, confirm_changes: ask, ..self.shared.clone() };
        self.runtime.block_on(self.app.save_connection(cfg)).unwrap();
    }

    fn change(&self, mut args: Value) -> (String, bool) {
        args["connection"] = json!(self.shared.id);
        self.call("run_change", args)
    }

    fn call(&self, tool: &str, args: Value) -> (String, bool) {
        let reply = self.request(1, "tools/call", json!({ "name": tool, "arguments": args }));
        let result = &reply["result"];
        (result["content"][0]["text"].as_str().expect("text content").to_string(), result["isError"] == true)
    }

    fn rows(&self, sql: &str) -> Value {
        let (text, failed) = self.call("run_query", json!({ "connection": self.shared.id, "sql": sql }));
        assert!(!failed, "{text}");
        serde_json::from_str(&text).unwrap()
    }

    // Without waiting for the answer.
    fn query(&self, id: i64, sql: &str) {
        let args = json!({ "connection": self.shared.id, "sql": sql });
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call", "params": { "name": "run_query", "arguments": args } }));
    }

    fn cancel(&self, id: i64) {
        self.send(json!({ "jsonrpc": "2.0", "method": "notifications/cancelled", "params": { "requestId": id } }));
    }

    fn settled(&self) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !lock(&self.server.running).is_empty() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        lock(&self.server.running).is_empty()
    }
}

// So a failing test ends instead of the runtime waiting on an endless query.
impl Drop for Setup {
    fn drop(&mut self) {
        self.runtime.block_on(self.server.bar.shutdown());
    }
}

#[test]
fn initialize_answers_with_the_clients_version_or_the_newest() {
    let s = setup();
    let hello =
        json!({ "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "t", "version": "1" } });
    let result = &s.request(1, "initialize", hello)["result"];
    assert_eq!(result["protocolVersion"], "2025-06-18");
    assert_eq!(result["serverInfo"]["name"], "BarSQL");
    assert!(result["capabilities"]["tools"].is_object());
    assert_eq!(
        s.request(2, "initialize", json!({ "protocolVersion": "2099-01-01" }))["result"]["protocolVersion"],
        "2025-11-25"
    );
}

#[test]
fn only_read_tools_are_listed_until_a_connection_allows_changes() {
    let s = setup();
    let listed = |s: &Setup| s.request(1, "tools/list", json!({}))["result"]["tools"].as_array().unwrap().clone();
    let tools = listed(&s);
    let names: Vec<&str> = tools.iter().map(|tool| tool["name"].as_str().unwrap()).collect();
    assert_eq!(names, TOOLS[..6]);
    assert!(tools.iter().all(|tool| tool["annotations"]["readOnlyHint"] == true), "{tools:?}");

    s.allow_changes(false);
    let (text, _) = s.call("list_connections", json!({}));
    assert!(text.contains(r#""canChange":true"#), "{text}");
    assert_eq!(s.list_changes.get(), 1, "the client hears its tools changed");
    let change = listed(&s).into_iter().find(|tool| tool["name"] == "run_change").expect("run_change is listed");
    assert_eq!(change["annotations"]["destructiveHint"], true);
}

#[test]
fn protocol_mistakes_get_json_rpc_errors() {
    let s = setup();
    s.server.receive("{not json");
    let reply = s.next();
    assert_eq!((reply["id"].clone(), reply["error"]["code"].clone()), (Value::Null, json!(-32700)));
    // A newer client probes with server/discover and falls back to initialize on this error.
    assert_eq!(s.request(2, "server/discover", json!({}))["error"]["code"], -32601);
    assert_eq!(s.request(3, "tools/call", json!({ "name": "drop_everything" }))["error"]["code"], -32602);
    // Notifications get no reply, so the next one is the ping's.
    s.send(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
    assert_eq!(s.request(4, "ping", json!({}))["result"], json!({}));
}

#[test]
fn only_shared_connections_are_listed_and_without_secrets() {
    let s = setup();
    let (text, failed) = s.call("list_connections", json!({}));
    assert!(!failed);
    let expected = json!([{
        "id": s.shared.id, "name": "shop", "engine": "sqlite", "database": "shop.db", "defaultSchema": "main", "canChange": false,
    }]);
    assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), expected);
    assert!(!text.contains("hunter2") && !text.contains(s.dir.path().to_str().unwrap()), "{text}");
    let (text, failed) = s.call("run_query", json!({ "connection": s.private.id, "sql": "SELECT 1" }));
    assert!(failed && text.contains("no shared connection"), "{text}");
}

#[test]
fn queries_return_rows_up_to_the_limit() {
    let s = setup();
    let answer = s.rows("SELECT id, label FROM orders ORDER BY id LIMIT 2");
    assert_eq!(
        (answer["columns"].clone(), answer["rows"].clone()),
        (json!(["id", "label"]), json!([[1, "order 1"], [2, "order 2"]]))
    );
    assert!(answer.get("note").is_none(), "{answer}");
    let answer = s.rows("SELECT * FROM orders");
    assert_eq!(answer["rows"].as_array().unwrap().len(), MAX_ROWS);
    assert!(answer["note"].as_str().unwrap().contains("first 200 rows"), "{answer}");
    let (text, failed) = s.call("run_query", json!({ "connection": s.shared.id }));
    assert!(failed && text.contains("`sql` is required"), "{text}");
}

#[test]
fn writes_are_refused_and_the_data_folder_is_left_alone() {
    let s = setup();
    let saved = ["connections.json", "query_history.json"].map(|file| std::fs::read(s.data(file)).unwrap());
    for sql in ["DELETE FROM orders", "SELECT 1; DROP TABLE orders", "CREATE TABLE more (x INTEGER)"] {
        let (text, failed) = s.call("run_query", json!({ "connection": s.shared.id, "sql": sql }));
        assert!(failed, "{sql}: {text}");
    }
    assert_eq!(s.rows("SELECT count(*) AS n FROM orders")["rows"], json!([[500]]));
    let now = ["connections.json", "query_history.json"].map(|file| std::fs::read(s.data(file)).unwrap());
    assert!(saved == now, "the server changed the app's files");
}

#[test]
fn schema_tools_describe_what_is_there() {
    let s = setup();
    let id = &s.shared.id;
    let (text, failed) = s.call("list_tables", json!({ "connection": id }));
    assert!(!failed && text.contains(r#""name":"orders""#), "{text}");
    let (text, failed) = s.call("describe_table", json!({ "connection": id, "table": "orders" }));
    let columns = serde_json::from_str::<Value>(&text).unwrap()["columns"].as_array().map_or(0, Vec::len);
    assert!(!failed && columns == 2, "{text}");
    let (text, failed) = s.call("describe_table", json!({ "connection": id, "table": "missing" }));
    assert!(failed, "{text}");
    let (text, failed) = s.call("show_ddl", json!({ "connection": id, "name": "orders" }));
    assert!(!failed && text.to_uppercase().contains("CREATE TABLE"), "{text}");
    let (text, failed) =
        s.call("explain_query", json!({ "connection": id, "sql": "SELECT * FROM orders WHERE id = 3" }));
    assert!(!failed && serde_json::from_str::<Value>(&text).unwrap()["nodes"].is_array(), "{text}");
}

#[test]
fn every_call_is_logged() {
    let s = setup();
    s.call("run_query", json!({ "connection": s.shared.id, "sql": "SELECT 1" }));
    s.call("run_query", json!({ "connection": s.shared.id, "sql": "DELETE FROM orders" }));
    let log = std::fs::read_to_string(s.data("mcp.log")).unwrap();
    let lines: Vec<Value> = log.lines().map(|line| serde_json::from_str(line).unwrap()).collect();
    assert_eq!(lines.len(), 2, "{log}");
    assert_eq!(
        (lines[0]["tool"].clone(), lines[0]["arguments"]["sql"].clone()),
        (json!("run_query"), json!("SELECT 1"))
    );
    assert!(lines[0].get("error").is_none() && lines[1]["error"].is_string(), "{log}");
}

#[test]
fn a_cancelled_call_stops_and_gets_no_reply() {
    let s = setup();
    s.query(7, ENDLESS);
    std::thread::sleep(Duration::from_millis(200));
    s.cancel(7);
    // Right behind its call, before the call has started.
    s.query(8, "SELECT 1");
    s.cancel(8);
    assert!(s.settled(), "the cancelled query kept running");
    assert_eq!(s.request(9, "ping", json!({}))["id"], 9);
    assert!(s.replies.try_recv().is_err(), "a cancelled call answered");
}

// Against JSON-RPC's rules, but it mustn't break cancelling.
#[test]
fn a_reused_request_id_leaves_the_newer_call_cancellable() {
    let s = setup();
    s.allow_changes(true);
    s.request(9, "initialize", json!({ "protocolVersion": "2025-11-25", "capabilities": { "elicitation": {} } }));
    // A change waiting for approval holds no connection, so the query can start meanwhile.
    let args = json!({ "connection": s.shared.id, "sql": "DELETE FROM orders WHERE id = 1" });
    s.send(json!({ "jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": { "name": "run_change", "arguments": args } }));
    let question = s.next();
    s.query(4, ENDLESS);
    s.send(json!({ "jsonrpc": "2.0", "id": question["id"], "result": { "action": "decline" } }));
    assert_eq!(s.next()["id"], 4, "the change answers");
    assert!(lock(&s.server.running).contains_key("4"), "the endless query is still registered");
    s.cancel(4);
    assert!(s.settled(), "the cancel reached it");
}

#[test]
fn cancelling_a_change_takes_its_question_down_and_runs_nothing() {
    let s = setup();
    s.allow_changes(true);
    s.request(9, "initialize", json!({ "protocolVersion": "2025-11-25", "capabilities": { "elicitation": {} } }));
    let args = json!({ "connection": s.shared.id, "sql": "DELETE FROM orders WHERE id = 1" });
    s.send(json!({ "jsonrpc": "2.0", "id": 5, "method": "tools/call", "params": { "name": "run_change", "arguments": args } }));
    let question = s.next();
    assert_eq!(question["method"], "elicitation/create", "{question}");
    s.cancel(5);
    let withdrawn = s.next();
    assert_eq!(withdrawn["method"], "notifications/cancelled", "{withdrawn}");
    assert_eq!(withdrawn["params"]["requestId"], question["id"]);
    // Too late: the call is gone.
    let approve = json!({ "action": "accept", "content": { "approve": true } });
    s.send(json!({ "jsonrpc": "2.0", "id": question["id"], "result": approve }));
    assert_eq!(s.rows("SELECT count(*) FROM orders")["rows"], json!([[500]]));
    assert!(s.replies.try_recv().is_err(), "the cancelled call answered");
}

#[test]
fn sharing_in_the_app_reaches_a_running_server() {
    let s = setup();
    s.runtime.block_on(s.app.save_connection(ConnectionConfig { mcp: true, ..s.private.clone() })).unwrap();
    let (text, _) = s.call("list_connections", json!({}));
    assert!(text.contains(&s.private.id) && text.contains(&s.shared.id), "{text}");
    for cfg in [&s.shared, &s.private] {
        s.runtime.block_on(s.app.save_connection(ConnectionConfig { mcp: false, ..cfg.clone() })).unwrap();
    }
    let (text, failed) = s.call("list_connections", json!({}));
    assert!(!failed && text.contains("Allow AI assistants (MCP)"), "{text}");
    let (text, failed) = s.call("run_query", json!({ "connection": s.shared.id, "sql": "SELECT 1" }));
    assert!(failed && text.contains("no shared connection"), "{text}");
}

#[test]
fn the_setup_names_this_executable_and_data_folder() {
    let setup: Value = serde_json::from_str(&super::setup_json()).unwrap();
    let server = &setup["mcpServers"]["barsql"];
    let exe = std::env::current_exe().unwrap();
    assert_eq!(server["command"], json!(dunce::canonicalize(&exe).unwrap_or(exe)));
    assert_eq!(server["args"], json!(["--mcp"]));
    assert_eq!(server["env"]["BARSQL_DATA_DIR"], json!(dunce::simplified(&barsql_core::paths::data_dir())));
    for path in [&server["command"], &server["env"]["BARSQL_DATA_DIR"]] {
        assert!(!path.as_str().unwrap().starts_with(r"\\?\"), "an AI app can't start {path}");
    }
}

#[test]
fn a_change_commits_and_a_preview_rolls_back() {
    let s = setup();
    s.allow_changes(false);
    let (text, failed) = s.change(json!({ "sql": "UPDATE orders SET label = 'done' WHERE id <= 3" }));
    assert!(!failed, "{text}");
    let done = json!({ "statements": 1, "affectedRows": 3, "committed": true });
    assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), done);
    let (text, failed) = s.change(json!({ "sql": "DELETE FROM orders WHERE id <= 10", "preview": true }));
    assert!(!failed && text.contains(r#""affectedRows":10"#) && text.contains(r#""committed":false"#), "{text}");
    assert_eq!(s.rows("SELECT count(*), sum(label = 'done') FROM orders")["rows"], json!([[500, 3]]));
}

#[test]
fn risky_or_unallowed_changes_are_refused() {
    let s = setup();
    let (text, failed) = s.change(json!({ "sql": "DELETE FROM orders WHERE id = 1" }));
    assert!(failed && text.contains("doesn't let AI assistants change data"), "{text}");
    s.allow_changes(false);
    let (text, failed) = s.change(json!({ "sql": "DELETE FROM orders" }));
    assert!(failed && text.contains("every row"), "{text}");
    let (text, failed) = s.change(json!({ "sql": "DELETE FROM orders", "allRows": true, "preview": true }));
    assert!(!failed && text.contains(r#""affectedRows":500"#), "{text}");
    for sql in ["DELETE FROM orders WHERE id = 1; COMMIT", "BEGIN; DELETE FROM orders WHERE id = 1"] {
        let (text, failed) = s.change(json!({ "sql": sql }));
        assert!(failed && text.contains("BEGIN, COMMIT, ROLLBACK"), "{sql}: {text}");
    }
    let other = s.dir.path().join("elsewhere.db");
    let attach = format!("ATTACH '{}' AS o; CREATE TABLE o.loot (x INTEGER)", other.display());
    for sql in [attach, "/* tidy */ DETACH o".into()] {
        let (text, failed) = s.change(json!({ "sql": sql }));
        assert!(failed && text.contains("ATTACH and DETACH are refused"), "{sql}: {text}");
    }
    assert!(!other.exists(), "a change reached another file");
    let (text, failed) = s.call("run_query", json!({ "connection": s.shared.id, "sql": "SELECT 1; COMMIT" }));
    assert!(failed && text.contains("BEGIN, COMMIT, ROLLBACK"), "{text}");
    let (text, failed) = s.call("run_query", json!({ "connection": s.shared.id, "sql": "DELETE FROM orders" }));
    assert!(failed, "reads stay read-only where changes are allowed: {text}");
    // A read-only connection never takes changes, whatever else it says.
    let cfg = ConnectionConfig { read_only: true, mcp_changes: true, ..s.shared.clone() };
    s.runtime.block_on(s.app.save_connection(cfg)).unwrap();
    let (text, failed) = s.change(json!({ "sql": "DELETE FROM orders WHERE id = 1" }));
    assert!(failed, "{text}");
    assert_eq!(s.rows("SELECT count(*) FROM orders")["rows"], json!([[500]]));
}

#[test]
fn a_connection_that_asks_first_needs_the_users_approval() {
    let s = setup();
    s.allow_changes(true);
    let hello = |s: &Setup, capabilities: Value| {
        s.request(9, "initialize", json!({ "protocolVersion": "2025-11-25", "capabilities": capabilities }));
    };
    hello(&s, json!({}));
    let (text, failed) = s.change(json!({ "sql": "DELETE FROM orders WHERE id = 1" }));
    assert!(failed && text.contains("can't ask the user"), "{text}");

    hello(&s, json!({ "elicitation": {} }));
    let answers =
        [(json!({ "action": "decline" }), 0), (json!({ "action": "accept", "content": { "approve": true } }), 1)];
    for (answer, deleted) in answers {
        let args = json!({ "connection": s.shared.id, "sql": "DELETE FROM orders WHERE id = 1" });
        let params = json!({ "name": "run_change", "arguments": args });
        s.send(json!({ "jsonrpc": "2.0", "id": 5, "method": "tools/call", "params": params }));
        let question = s.next();
        assert_eq!(question["method"], "elicitation/create", "{question}");
        assert!(question["params"]["message"].as_str().unwrap().contains("DELETE FROM orders WHERE id = 1"));
        s.send(json!({ "jsonrpc": "2.0", "id": question["id"], "result": answer }));
        let reply = s.next();
        assert_eq!(
            (reply["id"].clone(), reply["result"]["isError"].clone()),
            (json!(5), json!(deleted == 0)),
            "{reply}"
        );
        assert_eq!(s.rows("SELECT count(*) FROM orders")["rows"], json!([[500 - deleted]]));
    }
}
