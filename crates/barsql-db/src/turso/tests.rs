use std::sync::Arc;

use super::{Endpoint, TursoEngine, TursoOptions};
use crate::event::ScriptEvent;
use crate::http::fake::{Reply, fake, response};
use crate::tls::TlsMode;
use crate::{Cancel, Engine, Session};

fn options(port: u16) -> TursoOptions {
    TursoOptions {
        endpoint: Endpoint { tls: false, host: "127.0.0.1".into(), port, path: String::new() },
        token: "secret".into(),
        read_only: false,
        tls: TlsMode::VerifyFull,
        ssh: None,
    }
}

fn ok(body: &str) -> Reply {
    Reply::Send(response("200 OK", body))
}

const SELECT_ONE: &str = r#"{"baton":null,"base_url":null,"results":[{"type":"ok","response":{"type":"execute","result":{"cols":[{"name":"1"}],"rows":[[{"type":"integer","value":"1"}]],"affected_row_count":0}}},{"type":"ok","response":{"type":"close"}}]}"#;

fn cursor(baton: &str, base_url: Option<String>, value: i64) -> Reply {
    let base = base_url.map_or("null".to_string(), |u| format!("\"{u}\""));
    ok(&format!(
        "{{\"baton\":\"{baton}\",\"base_url\":{base}}}\n\
         {{\"type\":\"step_begin\",\"step\":0,\"cols\":[{{\"name\":\"x\",\"decltype\":\"INTEGER\"}}]}}\n\
         {{\"type\":\"row\",\"row\":[{{\"type\":\"integer\",\"value\":\"{value}\"}}]}}\n\
         {{\"type\":\"step_end\",\"affected_row_count\":0}}\n"
    ))
}

const AUTOCOMMIT: &str = r#"{"baton":"b9","base_url":null,"results":[{"type":"ok","response":{"type":"get_autocommit","is_autocommit":true}}]}"#;

async fn run(engine: &Arc<TursoEngine>, statements: &[&str]) -> Vec<ScriptEvent> {
    let mut session = Engine::Turso(engine.clone()).session().await.unwrap();
    let events = run_on(&mut session, statements).await;
    // Dropping it would close the stream, one request more than the script answers.
    std::mem::forget(session);
    events
}

async fn run_on(session: &mut Session, statements: &[&str]) -> Vec<ScriptEvent> {
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

// BarSQL's notes, as (result, text).
fn notes(events: &[ScriptEvent]) -> Vec<(usize, String)> {
    events
        .iter()
        .filter_map(|e| match e {
            ScriptEvent::Messages { result_index, messages, .. } => Some((*result_index, messages[0].text.clone())),
            _ => None,
        })
        .collect()
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

#[tokio::test]
async fn an_older_server_runs_statements_through_v2_pipelines() {
    let rows = r#"{"baton":"b1","base_url":null,"results":[{"type":"ok","response":{"type":"execute","result":{"cols":[{"name":"x","decltype":"INTEGER"}],"rows":[[{"type":"integer","value":"42"}]],"affected_row_count":0}}}]}"#;
    let server = fake(vec![Reply::Send(response("404 Not Found", "")), ok(""), ok(SELECT_ONE), ok(rows)]).await;
    let engine = TursoEngine::connect(options(server.addr.port())).await.unwrap();
    assert_eq!(values(&run(&engine, &["SELECT x FROM t"]).await), ["42"]);
    let requests = server.requests();
    assert!(requests[0].starts_with("GET /v3 ") && requests[1].starts_with("GET /v2 "), "{requests:?}");
    assert!(requests[3].starts_with("POST /v2/pipeline ") && requests[3].contains("SELECT x FROM t"), "{requests:?}");
    assert!(requests.iter().all(|r| r.contains("Authorization: Bearer secret")), "{requests:?}");
}

fn dropped_stream() -> Vec<Reply> {
    vec![
        ok(""),
        ok(SELECT_ONE),
        cursor("b1", None, 7),
        Reply::Send(response("400 Bad Request", "Received an invalid baton")),
        cursor("b2", None, 8),
        ok(AUTOCOMMIT),
    ]
}

#[tokio::test]
async fn a_dropped_stream_runs_the_statement_once_more_on_a_new_one() {
    let server = fake(dropped_stream()).await;
    let engine = TursoEngine::connect(options(server.addr.port())).await.unwrap();
    let events = run(&engine, &["PRAGMA foreign_keys = ON", "SELECT 8"]).await;
    assert_eq!(values(&events), ["7", "8"]);
    let notes = notes(&events);
    assert!(
        matches!(&notes[..], [(1, text)] if text.contains("ran on a new one") && text.contains("PRAGMAs")),
        "{notes:?}"
    );
    let requests = server.requests();
    assert!(requests[3].contains(r#""baton":"b1""#) && requests[4].contains(r#""baton":null"#), "{requests:?}");
}

// Streams expire after a few idle seconds, so losing one that held nothing goes unsaid.
#[tokio::test]
async fn a_stream_that_held_nothing_is_replaced_quietly() {
    let server = fake(dropped_stream()).await;
    let engine = TursoEngine::connect(options(server.addr.port())).await.unwrap();
    let events = run(&engine, &["SELECT 7", "SELECT 8"]).await;
    assert_eq!(values(&events), ["7", "8"]);
    assert!(notes(&events).is_empty());
}

#[tokio::test]
async fn a_refused_statement_ends_the_stream() {
    let refused = r#"{"error":"Internal Error: `SQL string could not be parsed: syntax error around L1:6: `SELEC``"}"#;
    let server = fake(vec![
        ok(""),
        ok(SELECT_ONE),
        cursor("b1", None, 1),
        ok(AUTOCOMMIT),
        Reply::Send(response("400 Bad Request", refused)),
        cursor("b2", None, 8),
        ok(AUTOCOMMIT),
    ])
    .await;
    let engine = TursoEngine::connect(options(server.addr.port())).await.unwrap();
    let mut session = Engine::Turso(engine.clone()).session().await.unwrap();
    run_on(&mut session, &["PRAGMA foreign_keys = ON"]).await;
    let failed = run_on(&mut session, &["SELEC 1"]).await;
    let notes = notes(&failed);
    assert!(matches!(&notes[..], [(0, text)] if text.contains("after the error")), "{notes:?}");
    assert_eq!(values(&run_on(&mut session, &["SELECT 8"]).await), ["8"]);
    std::mem::forget(session);
    let requests = server.requests();
    assert!(requests[5].contains(r#""baton":null"#), "a new stream at once: {requests:?}");
}

#[tokio::test]
async fn the_stream_follows_the_base_url_the_server_names() {
    let other = fake(vec![ok(AUTOCOMMIT)]).await;
    let moved = format!("http://127.0.0.1:{}", other.addr.port());
    let server = fake(vec![ok(""), ok(SELECT_ONE), cursor("b1", Some(moved), 5)]).await;
    let engine = TursoEngine::connect(options(server.addr.port())).await.unwrap();
    assert_eq!(values(&run(&engine, &["SELECT 5"]).await), ["5"]);
    let followed = other.requests();
    assert_eq!(followed.len(), 1, "{followed:?}");
    assert!(followed[0].starts_with("POST /v3/pipeline ") && followed[0].contains(r#""baton":"b1""#), "{followed:?}");
}

#[tokio::test]
async fn a_refused_token_says_so() {
    let server = fake(vec![Reply::Send(response("401 Unauthorized", ""))]).await;
    let err = TursoEngine::connect(options(server.addr.port())).await.err().unwrap();
    assert_eq!(err.message, "the server refused the auth token");
}
