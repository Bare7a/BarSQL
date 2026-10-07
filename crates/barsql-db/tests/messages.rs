#![cfg(feature = "e2e")]

mod common;

use std::time::{Duration, Instant};

use barsql_core::{ConnectionConfig, MessageLevel, ServerMessage};
use barsql_db::{Cancel, Engine, MAX_MESSAGES, ScriptEvent, Session};
use common::{mysql_config, postgres_config};

async fn session(config: &ConnectionConfig) -> Session {
    let engine = Engine::connect(config).await.expect("the server is not reachable; run `cargo xtask e2e up`");
    engine.session().await.unwrap()
}

// Each event of a run with when it arrived.
async fn run(session: &mut Session, statements: &[&str]) -> Vec<(ScriptEvent, Instant)> {
    let (tx, rx) = async_channel::unbounded();
    let statements: Vec<String> = statements.iter().map(|s| s.to_string()).collect();
    let run = async move {
        let _ = session.run_script(&statements, &tx, &Cancel::new()).await;
    };
    let collect = async {
        let mut events = Vec::new();
        while let Ok(event) = rx.recv().await {
            events.push((event, Instant::now()));
        }
        events
    };
    tokio::join!(run, collect).1
}

// Messages as (result, message), and the dropped count.
fn messages(events: &[(ScriptEvent, Instant)]) -> (Vec<(usize, ServerMessage)>, usize) {
    let (mut kept, mut dropped) = (Vec::new(), 0);
    for (event, _) in events {
        if let ScriptEvent::Messages { result_index, messages, dropped: more } = event {
            kept.extend(messages.iter().map(|m| (*result_index, m.clone())));
            dropped += more;
        }
    }
    (kept, dropped)
}

fn position(events: &[(ScriptEvent, Instant)], wanted: impl Fn(&ScriptEvent) -> bool) -> usize {
    events.iter().position(|(event, _)| wanted(event)).expect("the event")
}

#[tokio::test]
async fn postgres_notices_keep_their_level_detail_and_hint_and_come_before_the_result() {
    let mut pg = session(&postgres_config()).await;
    let events = run(
        &mut pg,
        &["DO $$ BEGIN RAISE NOTICE 'hi'; RAISE WARNING 'careful' USING DETAIL = 'why', HINT = 'fix'; \
           RAISE INFO 'fyi'; END $$"],
    )
    .await;
    let (kept, dropped) = messages(&events);
    let warning = ServerMessage {
        level: MessageLevel::Warning,
        code: "01000".into(),
        text: "careful".into(),
        detail: "why".into(),
        hint: "fix".into(),
    };
    let (notice, info) =
        (ServerMessage::new(MessageLevel::Notice, "hi"), ServerMessage::new(MessageLevel::Info, "fyi"));
    assert_eq!(kept, [(0, notice), (0, warning), (0, info)]);
    assert_eq!(dropped, 0);
    let said = position(&events, |e| matches!(e, ScriptEvent::Messages { .. }));
    assert!(said < position(&events, |e| matches!(e, ScriptEvent::Result(_))));
}

#[tokio::test]
async fn postgres_notices_show_while_the_statement_still_runs() {
    let mut pg = session(&postgres_config()).await;
    let events = run(&mut pg, &["DO $$ BEGIN RAISE NOTICE 'early'; PERFORM pg_sleep(1); END $$"]).await;
    let said = events.iter().find(|(e, _)| matches!(e, ScriptEvent::Messages { .. })).expect("the notice").1;
    let done = events.iter().find(|(e, _)| matches!(e, ScriptEvent::Result(_))).unwrap().1;
    assert!(done - said > Duration::from_millis(500), "{:?}", done - said);
}

#[tokio::test]
async fn postgres_keeps_the_first_notices_of_a_statement_and_counts_the_rest() {
    let mut pg = session(&postgres_config()).await;
    let events = run(
        &mut pg,
        &[
            "DO $$ BEGIN FOR i IN 1..2000 LOOP RAISE NOTICE 'n%', i; END LOOP; END $$",
            "DO $$ BEGIN RAISE NOTICE 'next'; END $$",
        ],
    )
    .await;
    let (kept, dropped) = messages(&events);
    let first: Vec<_> = kept.iter().filter(|(ix, _)| *ix == 0).collect();
    assert_eq!((first.len(), dropped), (MAX_MESSAGES, 2000 - MAX_MESSAGES));
    assert_eq!(first[0].1.text, "n1");
    assert_eq!(kept.last().map(|(ix, m)| (*ix, m.text.as_str())), Some((1, "next")), "a new count per statement");
}

#[tokio::test]
async fn postgres_says_nothing_for_the_apps_own_queries() {
    let mut pg = session(&postgres_config()).await;
    pg.buffered("DO $$ BEGIN RAISE NOTICE 'quiet'; END $$", &Cancel::new()).await.unwrap();
    let (tx, rx) = async_channel::unbounded();
    pg.stream("DO $$ BEGIN RAISE NOTICE 'paged'; END $$", &tx, &Cancel::new()).await.unwrap();
    drop(tx);
    while let Ok(event) = rx.try_recv() {
        assert!(!matches!(event, ScriptEvent::Messages { .. }), "{event:?}");
    }
    let events = run(&mut pg, &["SELECT 1"]).await;
    assert!(messages(&events).0.is_empty(), "nothing left over for the next run");
}

#[tokio::test]
async fn mysql_and_mariadb_list_warnings_and_notes_after_the_statement() {
    for config in [mysql_config("MYSQL", "33306"), mysql_config("MARIADB", "33307")] {
        let mut my = session(&config).await;
        let events =
            run(&mut my, &["SELECT CAST('abc' AS SIGNED) AS v", "DROP TABLE IF EXISTS barsql_no_such_table"]).await;
        let (kept, dropped) = messages(&events);
        let summary: Vec<_> = kept.iter().map(|(ix, m)| (*ix, m.level, m.code.as_str())).collect();
        assert_eq!(summary, [(0, MessageLevel::Warning, "1292"), (1, MessageLevel::Notice, "1051")], "{kept:?}");
        assert!(kept[0].1.text.contains("'abc'"), "{kept:?}");
        assert_eq!(dropped, 0);

        my.buffered("SELECT CAST('abc' AS SIGNED)", &Cancel::new()).await.unwrap();
        assert!(messages(&run(&mut my, &["SELECT 1"]).await).0.is_empty(), "the app's own query stays quiet");
    }
}
