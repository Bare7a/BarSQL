use std::sync::Arc;

use barsql_core::{ConnectionConfig, DriverType, Value};

use super::{ChEngine, ChOptions, ch_error, inline_params, literal, output_format};
use crate::event::ScriptEvent;
use crate::http::fake::{Reply, fake, response};
use crate::{Cancel, Engine};

#[test]
fn errors_keep_the_message_code_and_name() {
    let body = "Code: 60. DB::Exception: Unknown table expression identifier 'nope' in scope SELECT * FROM nope. \
                (UNKNOWN_TABLE) (version 26.8.19.9 (official build))";
    let error = ch_error(Some("60"), body, "SELECT * FROM nope");
    assert_eq!(error.message, "Unknown table expression identifier 'nope' in scope SELECT * FROM nope");
    assert_eq!((error.code.as_str(), error.detail.as_str(), error.position), ("60", "UNKNOWN_TABLE", 0));

    let sql = "SELECT 'é' AS a, 1 +";
    let body = "Code: 62. DB::Exception: Syntax error: failed at position 22 (end of query): . Expected end of query. \
                (SYNTAX_ERROR) (version 26.8.19.9 (official build))\n";
    let error = ch_error(None, body, sql);
    assert_eq!(error.code, "62");
    assert_eq!(error.position, 21, "byte 22 is after the 20 characters, with é taking two bytes");
    assert!(error.message.starts_with("Syntax error: failed at position 22"), "{error:?}");

    let plain = ch_error(Some("516"), "default: Authentication failed", "");
    assert_eq!((plain.message.as_str(), plain.code.as_str()), ("default: Authentication failed", "516"));
}

#[test]
fn literals_escape_backslashes_and_quotes() {
    assert_eq!(literal(&Value::Text(r"it's a \ path".into())), r"'it\'s a \\ path'");
    assert_eq!(literal(&Value::Null), "NULL");
    assert_eq!(literal(&Value::Bool(true)), "true");
    assert_eq!(literal(&Value::Float(f64::NAN)), "nan");
    assert_eq!(literal(&Value::Float(f64::NEG_INFINITY)), "-inf");
    assert_eq!(literal(&Value::Float(0.1)), "0.1");
}

#[test]
fn placeholders_outside_strings_become_literals() {
    let sql = "INSERT INTO t (a, b, c) VALUES (?, '?', ?) -- ?";
    let inlined = inline_params(sql, &[Value::Int(1), Value::Text("x".into())]).unwrap();
    assert_eq!(inlined, "INSERT INTO t (a, b, c) VALUES (1, '?', 'x') -- ?");
    assert!(inline_params("SELECT ?", &[]).is_err());
    assert!(inline_params("SELECT 1", &[Value::Int(1)]).is_err());
}

#[test]
fn a_top_level_format_asks_for_raw_output() {
    assert_eq!(output_format("SELECT 1 FORMAT JSONEachRow"), Some("JSONEachRow".into()));
    assert_eq!(output_format("SELECT 1 AS format FROM t"), None);
    assert_eq!(output_format("SELECT * FROM (SELECT 1 FORMAT CSV)"), None, "only the outer query");
    assert_eq!(output_format("INSERT INTO t FORMAT CSV"), None, "that's the input's format");
    assert_eq!(output_format("SELECT 'FORMAT JSON'"), None);
    assert_eq!(output_format("SELECT 1 FORMAT CSV SETTINGS max_threads = 1"), Some("CSV".into()));
    assert_eq!(output_format("SELECT 1 FORMAT Pretty;"), Some("Pretty".into()));
}

fn options(port: u16) -> ChOptions {
    let config = ConnectionConfig {
        driver: DriverType::ClickHouse,
        host: "127.0.0.1".into(),
        port: port.into(),
        database: "shop".into(),
        username: "u".into(),
        password: "p".into(),
        ssl_mode: "disable".into(),
        ..Default::default()
    };
    ChOptions::from_config(&config).unwrap()
}

fn ok(body: &str) -> Reply {
    Reply::Send(response("200 OK", body))
}

// getSetting('readonly') at connect: a writable user.
fn writable() -> Reply {
    ok("getSetting('readonly')\nUInt8\n0\n")
}

fn value(n: u8) -> Reply {
    ok(&format!("x\nUInt8\n{n}\n"))
}

fn refused(code: u16, name: &str) -> Reply {
    let body = format!("Code: {code}. DB::Exception: {name} happened. ({name}) (version 26.8.1.1 (official build))\n");
    Reply::Send(response("500 Internal Server Error", &body))
}

async fn run(engine: &Arc<ChEngine>, statements: &[&str]) -> Vec<ScriptEvent> {
    let mut session = Engine::ClickHouse(engine.clone()).session().await.unwrap();
    let (tx, rx) = async_channel::unbounded();
    let statements: Vec<String> = statements.iter().map(|s| s.to_string()).collect();
    let _ = session.run_script(&statements, &tx, &Cancel::new()).await;
    drop(tx);
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }
    events
}

fn values(events: &[ScriptEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            ScriptEvent::Rows { chunk, .. } => chunk.display(0, 0).map(str::to_string),
            _ => None,
        })
        .collect()
}

// BarSQL's notes, as (result, text), with whether each came before its result.
fn notes(events: &[ScriptEvent]) -> Vec<(usize, String, bool)> {
    let mut notes = Vec::new();
    for (ix, event) in events.iter().enumerate() {
        if let ScriptEvent::Messages { result_index, messages, .. } = event {
            let early =
                events[ix..].iter().any(|e| matches!(e, ScriptEvent::Result(r) if r.result_index == *result_index));
            notes.extend(messages.iter().map(|m| (*result_index, m.text.clone(), early)));
        }
    }
    notes
}

// The request line's value of `name`.
fn param<'a>(request: &'a str, name: &str) -> Option<&'a str> {
    let line = request.lines().next()?;
    let query = line.split(' ').nth(1)?.split_once('?')?.1;
    query.split('&').find_map(|pair| pair.strip_prefix(name)?.strip_prefix('='))
}

#[tokio::test]
async fn a_session_starts_in_the_connections_database_and_keeps_its_own_after() {
    let server = fake(vec![writable(), ok(""), value(1), value(2)]).await;
    let engine = ChEngine::connect(options(server.addr.port())).await.unwrap();
    assert_eq!(values(&run(&engine, &["SELECT 1", "SELECT 2"]).await), ["1", "2"]);
    let requests = server.requests();
    let (start, first, second) = (&requests[1], &requests[2], &requests[3]);
    assert!(start.ends_with("USE `shop`"), "{start}");
    assert_eq!(param(start, "database"), Some("shop"));
    assert_eq!(param(start, "session_check"), None, "the session doesn't exist yet");
    let session = param(start, "session_id").expect("a session");
    for request in [first, second] {
        assert_eq!(param(request, "session_id"), Some(session));
        assert_eq!(param(request, "session_check"), Some("1"));
        // A database here would undo the tab's own USE.
        assert_eq!(param(request, "database"), None, "{request}");
    }
    assert!(requests.iter().all(|r| r.contains("X-ClickHouse-User: u") && r.contains("X-ClickHouse-Key: p")));
}

#[tokio::test]
async fn an_ended_session_reruns_the_statement_in_a_new_one() {
    let server = fake(vec![writable(), ok(""), ok(""), refused(372, "SESSION_NOT_FOUND"), ok(""), value(2)]).await;
    let engine = ChEngine::connect(options(server.addr.port())).await.unwrap();
    let events = run(&engine, &["SET max_threads = 1", "SELECT 2"]).await;
    assert_eq!(values(&events), ["2"]);
    let notes = notes(&events);
    assert!(
        matches!(&notes[..], [(1, text, true)] if text.contains("ran in a new one") && text.contains("Settings")),
        "{notes:?}"
    );
    let requests = server.requests();
    let (old, new) = (param(&requests[1], "session_id"), param(&requests[4], "session_id"));
    assert!(requests[4].ends_with("USE `shop`") && old.is_some() && new.is_some() && old != new, "{requests:?}");
}

// Sessions time out after ten idle minutes, so losing one that held nothing goes unsaid.
#[tokio::test]
async fn a_session_that_held_nothing_is_replaced_quietly() {
    let server = fake(vec![writable(), ok(""), value(1), refused(372, "SESSION_NOT_FOUND"), ok(""), value(2)]).await;
    let engine = ChEngine::connect(options(server.addr.port())).await.unwrap();
    let events = run(&engine, &["SELECT 1", "SELECT 2"]).await;
    assert_eq!(values(&events), ["1", "2"]);
    assert!(notes(&events).is_empty());
}

#[tokio::test]
async fn a_busy_session_is_waited_for() {
    let server = fake(vec![writable(), ok(""), refused(373, "SESSION_IS_LOCKED"), value(1)]).await;
    let engine = ChEngine::connect(options(server.addr.port())).await.unwrap();
    let events = run(&engine, &["SELECT 1"]).await;
    assert_eq!(values(&events), ["1"]);
    assert!(notes(&events).is_empty());
    let requests = server.requests();
    assert_eq!(param(&requests[2], "session_id"), param(&requests[3], "session_id"), "the same session");
}

#[tokio::test]
async fn an_error_after_rows_went_out_ends_the_statement() {
    let body = "x\nUInt8\n1\n2\n\r\n__exception__\r\nTAG\r\nCode: 395. DB::Exception: Value passed to 'throwIf' \
                function is non-zero. (FUNCTION_THROW_IF_VALUE_IS_NON_ZERO)\n104 TAG\r\n__exception__\r\n";
    let server = fake(vec![writable(), ok(""), ok(body)]).await;
    let engine = ChEngine::connect(options(server.addr.port())).await.unwrap();
    let events = run(&engine, &["SELECT throwIf(number = 2) FROM numbers(3)"]).await;
    assert_eq!(values(&events), ["1"], "the rows before it still show");
    let error = events
        .iter()
        .find_map(|e| match e {
            ScriptEvent::Result(r) => r.error.clone(),
            _ => None,
        })
        .expect("an error");
    assert_eq!((error.code.as_str(), error.detail.as_str()), ("395", "FUNCTION_THROW_IF_VALUE_IS_NON_ZERO"));
    assert_eq!(error.message, "Value passed to 'throwIf' function is non-zero");
}
