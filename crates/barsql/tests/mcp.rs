// The real binary over its pipes, as an AI app runs it.

use std::io::{BufRead, BufReader, Write};
use std::process::{ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use barsql_app::BarApp;
use barsql_core::{ConnectionConfig, DriverType};
use serde_json::{Value, json};

struct Client {
    stdin: ChildStdin,
    lines: mpsc::Receiver<String>,
}

impl Client {
    fn send(&mut self, msg: Value) {
        writeln!(self.stdin, "{msg}").unwrap();
    }

    fn ask(&mut self, msg: Value) -> Value {
        self.send(msg);
        let line = self.lines.recv_timeout(Duration::from_secs(30)).expect("a reply");
        serde_json::from_str(&line).unwrap_or_else(|_| panic!("stdout carried more than JSON-RPC: {line:?}"))
    }
}

#[test]
fn an_ai_app_queries_a_shared_connection_over_stdio() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let app = BarApp::open(&data, runtime.handle().clone()).unwrap();
    let file = dir.path().join("notes.db").display().to_string();
    let cfg = ConnectionConfig {
        name: "notes".into(),
        driver: DriverType::Sqlite,
        file_path: file,
        mcp: true,
        ..Default::default()
    };
    let id = runtime.block_on(app.save_connection(cfg)).unwrap().id;
    runtime
        .block_on(app.execute_query(&id, "CREATE TABLE notes (body TEXT); INSERT INTO notes VALUES ('hello')"))
        .unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_BarSQL"))
        .arg("--mcp")
        .env("BARSQL_DATA_DIR", &data)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let stdout = BufReader::new(child.stdout.take().unwrap());
    let (tx, lines) = mpsc::channel();
    std::thread::spawn(move || stdout.lines().map_while(Result::ok).try_for_each(|line| tx.send(line)));
    let mut client = Client { stdin: child.stdin.take().unwrap(), lines };

    let hello = json!({ "protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": { "name": "test", "version": "1" } });
    let reply = client.ask(json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": hello }));
    assert_eq!(reply["result"]["serverInfo"]["name"], "BarSQL", "{reply}");
    client.send(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
    let query = json!({ "name": "run_query", "arguments": { "connection": id, "sql": "SELECT body FROM notes" } });
    let reply = client.ask(json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": query }));
    assert_eq!(reply["id"], 2, "{reply}");
    let text = reply["result"]["content"][0]["text"].as_str().unwrap_or_default();
    assert!(reply["result"]["isError"] == false && text.contains("hello"), "{reply}");

    // Closing stdin is how a client says goodbye.
    drop(client);
    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "the server outlived its client");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(status.success(), "{status}");
}
