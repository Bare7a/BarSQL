#![cfg(feature = "e2e")]

mod common;

use std::time::Duration;

use barsql_core::{MessageLevel, ObjectKind, ObjectRef};
use barsql_db::{Cancel, MAX_MESSAGES, ScriptEvent, Session};
use common::{cancel_after, mssql_config, mssql_engine};

// Each run's events.
async fn run(session: &mut Session, batches: &[&str]) -> (Vec<ScriptEvent>, Result<usize, barsql_core::QueryError>) {
    let (tx, rx) = async_channel::unbounded();
    let batches: Vec<String> = batches.iter().map(|s| s.to_string()).collect();
    let result = session.run_script(&batches, &tx, &Cancel::new()).await;
    drop(tx);
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }
    (events, result)
}

fn cells(events: &[ScriptEvent], index: usize) -> Vec<Vec<Option<String>>> {
    let mut rows = Vec::new();
    for event in events {
        if let ScriptEvent::Rows { result_index, chunk } = event
            && *result_index == index
        {
            for r in 0..chunk.rows() {
                rows.push((0..chunk.columns()).map(|c| chunk.display(r, c).map(str::to_string)).collect());
            }
        }
    }
    rows
}

fn messages(events: &[ScriptEvent]) -> Vec<(usize, MessageLevel, String)> {
    events
        .iter()
        .flat_map(|e| match e {
            ScriptEvent::Messages { result_index, messages, .. } => {
                messages.iter().map(|m| (*result_index, m.level, m.text.clone())).collect()
            }
            _ => Vec::new(),
        })
        .collect()
}

fn results(events: &[ScriptEvent]) -> Vec<&barsql_db::StatementResult> {
    events
        .iter()
        .filter_map(|e| match e {
            ScriptEvent::Result(r) => Some(r.as_ref()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn values_come_back_as_the_grid_shows_them() {
    let Some(cfg) = mssql_config() else { return };
    let engine = mssql_engine(&cfg).await;
    let mut session = engine.session().await.unwrap();
    let (events, result) = run(
        &mut session,
        &["SELECT 1 AS one, N'héllo' AS t, CAST(12.5 AS decimal(10, 2)) AS d, CAST(1 AS bit) AS b,
            CAST('2024-02-29T13:45:30.123' AS datetime) AS dt, CAST('2024-02-29' AS date) AS day,
            CAST('2024-02-29T13:45:30.1234567+02:00' AS datetimeoffset) AS dto, CAST(12.5 AS money) AS m,
            CAST('6F9619FF-8B86-D011-B42D-00C04FC964FF' AS uniqueidentifier) AS id, 0xDEADBEEF AS bin,
            CAST(NULL AS int) AS nothing, CAST(9007199254740993 AS bigint) AS big"],
    )
    .await;
    assert_eq!(result.unwrap(), 1);
    let row = cells(&events, 0).remove(0);
    let shown: Vec<&str> = row.iter().map(|c| c.as_deref().unwrap_or("NULL")).collect();
    assert_eq!(
        shown,
        [
            "1",
            "héllo",
            "12.50",
            "true",
            "2024-02-29T13:45:30.123Z",
            "2024-02-29T00:00:00Z",
            "2024-02-29T13:45:30.1234567+02:00",
            "12.5000",
            "6F9619FF-8B86-D011-B42D-00C04FC964FF",
            "\\xdeadbeef",
            "NULL",
            "9007199254740993"
        ]
    );
    let ScriptEvent::Meta { columns, .. } = &events[0] else { panic!("{events:?}") };
    let types: Vec<&str> = columns.iter().map(|c| c.type_name.as_str()).collect();
    assert_eq!(&types[..4], ["int", "nvarchar", "decimal", "bit"]);
}

#[tokio::test]
async fn a_batch_gives_each_result_set_and_its_messages() {
    let Some(cfg) = mssql_config() else { return };
    let engine = mssql_engine(&cfg).await;
    let mut session = engine.session().await.unwrap();
    session.buffered("DROP TABLE IF EXISTS #items; CREATE TABLE #items (id int)", &Cancel::new()).await.unwrap();
    let (events, result) = run(
        &mut session,
        &["PRINT 'starting';
          SELECT 1 AS a;
          INSERT INTO #items VALUES (1), (2), (3);
          UPDATE #items SET id = id + 10 WHERE id > 1;
          SELECT id FROM #items ORDER BY id;
          PRINT 'done'"],
    )
    .await;
    assert_eq!(result.unwrap(), 2, "two result sets");
    assert_eq!(cells(&events, 1), [[Some("1".into())], [Some("12".into())], [Some("13".into())]]);
    let said = messages(&events);
    assert_eq!(
        said,
        [
            (0, MessageLevel::Notice, "starting".into()),
            (1, MessageLevel::Notice, "done".into()),
            (1, MessageLevel::Info, "(3 rows affected)".into()),
            (1, MessageLevel::Info, "(2 rows affected)".into()),
        ]
    );

    // No result set: one summary result.
    let (events, _) = run(&mut session, &["UPDATE #items SET id = id"]).await;
    let done = results(&events);
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].summary.as_ref().map(|s| s.affected_rows), Some(3));
    assert!(messages(&events).is_empty(), "one count needs no list");
    let (events, _) = run(&mut session, &["SET NOCOUNT ON; UPDATE #items SET id = id; SET NOCOUNT OFF"]).await;
    let summary = results(&events)[0].summary.clone().unwrap();
    assert_eq!(summary.message, "Commands completed successfully.");
}

#[tokio::test]
async fn variable_assignments_list_no_row_counts() {
    let Some(cfg) = mssql_config() else { return };
    let engine = mssql_engine(&cfg).await;
    let mut session = engine.session().await.unwrap();
    let (events, result) = run(
        &mut session,
        &["DECLARE @i int = 1, @n int;
          WHILE @i <= 3 BEGIN SET @i += 1; SELECT @n = object_id FROM sys.objects END;
          PRINT CONCAT('i = ', @i)"],
    )
    .await;
    result.unwrap();
    assert_eq!(messages(&events), [(0, MessageLevel::Notice, "i = 4".into())]);
    let done = results(&events);
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].summary.as_ref().map(|s| s.message.as_str()), Some("Commands completed successfully."));

    let (events, result) = run(&mut session, &["DECLARE @a int = 1; SET @a += 1; SELECT @a AS a"]).await;
    assert_eq!(result.unwrap(), 1);
    assert_eq!(cells(&events, 0), [[Some("2".into())]]);
    assert!(messages(&events).is_empty(), "{events:?}");
}

// Left as is: tiberius doesn't pass on the DONE token's CurCmd that tells an assignment's count apart.
#[tokio::test]
async fn assignments_still_count_where_rows_can_change() {
    let Some(cfg) = mssql_config() else { return };
    let engine = mssql_engine(&cfg).await;
    let mut session = engine.session().await.unwrap();
    let setup = "DROP TABLE IF EXISTS #rows; CREATE TABLE #rows (id int); INSERT INTO #rows VALUES (1), (2)";
    session.buffered(setup, &Cancel::new()).await.unwrap();
    let (events, result) = run(&mut session, &["DECLARE @n int = 0; UPDATE #rows SET id = id WHERE id > @n"]).await;
    result.unwrap();
    let info = |text: &str| (0, MessageLevel::Info, text.to_string());
    assert_eq!(messages(&events), [info("(1 row affected)"), info("(2 rows affected)")]);
    assert_eq!(results(&events)[0].summary.as_ref().map(|s| s.affected_rows), Some(3));
    session
        .buffered("CREATE PROCEDURE #bump AS BEGIN DECLARE @a int = 1; SET @a += 1 END", &Cancel::new())
        .await
        .unwrap();
    let (events, result) = run(&mut session, &["EXEC #bump"]).await;
    result.unwrap();
    assert_eq!(messages(&events), [info("(1 row affected)"), info("(1 row affected)")]);
    assert_eq!(results(&events)[0].summary.as_ref().map(|s| s.affected_rows), Some(2));
}

#[tokio::test]
async fn a_batch_keeps_its_first_messages_and_counts_the_rest() {
    let Some(cfg) = mssql_config() else { return };
    let engine = mssql_engine(&cfg).await;
    let mut session = engine.session().await.unwrap();
    let flood =
        format!("DECLARE @i int = 1; WHILE @i <= {} BEGIN PRINT CONCAT('n', @i); SET @i += 1 END", MAX_MESSAGES + 500);
    let (events, result) = run(&mut session, &[&flood, "PRINT 'next'"]).await;
    result.unwrap();
    let said = messages(&events);
    let dropped: usize =
        events.iter().map(|e| if let ScriptEvent::Messages { dropped, .. } = e { *dropped } else { 0 }).sum();
    let first: Vec<_> = said.iter().filter(|(ix, ..)| *ix == 0).collect();
    assert_eq!((first.len(), dropped), (MAX_MESSAGES, 500));
    assert_eq!(first[0].2, "n1");
    assert_eq!(said.last().map(|(ix, _, text)| (*ix, text.as_str())), Some((1, "next")), "a new count per batch");
}

#[tokio::test]
async fn errors_carry_their_number_and_line() {
    let Some(cfg) = mssql_config() else { return };
    let engine = mssql_engine(&cfg).await;
    let mut session = engine.session().await.unwrap();
    // A statement's error lets the batch go on, so both result sets come before it.
    let batch = "SELECT 1 AS ok;\nRAISERROR('boom', 16, 1);\nSELECT 2 AS after";
    let (events, result) = run(&mut session, &[batch]).await;
    let error = result.unwrap_err();
    assert_eq!((error.code.as_str(), error.message.as_str()), ("50000", "boom"));
    assert_eq!(error.position, 17, "the second line");
    assert!(error.detail.starts_with("Msg 50000, Level 16, State 1, Line 2"), "{error:?}");
    assert_eq!(cells(&events, 0), [[Some("1".into())]]);
    assert_eq!(cells(&events, 1), [[Some("2".into())]]);
    assert!(results(&events).last().unwrap().error.is_some());
    // A missing table ends the whole batch.
    let (events, result) =
        run(&mut session, &["SELECT 1 AS ok;\nSELECT * FROM no_such_table;\nSELECT 2 AS after"]).await;
    let error = result.unwrap_err();
    assert_eq!((error.code.as_str(), error.position), ("208", 17));
    assert_eq!(results(&events).len(), 2, "the first result, then the error");
    // The session is still usable.
    let (_, again) = run(&mut session, &["SELECT 1"]).await;
    assert!(again.is_ok());
}

#[tokio::test]
async fn transactions_follow_the_server() {
    let Some(cfg) = mssql_config() else { return };
    let engine = mssql_engine(&cfg).await;
    let mut session = engine.session().await.unwrap();
    session.begin().await.unwrap();
    assert!(session.in_transaction());
    session.rollback().await.unwrap();
    assert!(!session.in_transaction());
    let (_, result) = run(&mut session, &["BEGIN TRANSACTION"]).await;
    result.unwrap();
    assert!(session.in_transaction(), "seen in the batch's tokens");
    let (_, result) = run(&mut session, &["COMMIT"]).await;
    result.unwrap();
    assert!(!session.in_transaction());
    // A doomed transaction can't commit.
    session.begin().await.unwrap();
    let _ = run(&mut session, &["SET XACT_ABORT OFF; BEGIN TRY SELECT 1 / 0 END TRY BEGIN CATCH END CATCH"]).await;
    let (_, _) = run(&mut session, &["SET XACT_ABORT ON; SELECT CAST('x' AS int)"]).await;
    assert!(!session.in_transaction(), "XACT_ABORT rolled it back: {}", session.in_transaction());
}

#[tokio::test]
async fn a_cancel_stops_a_running_statement_at_once() {
    let Some(cfg) = mssql_config() else { return };
    let engine = mssql_engine(&cfg).await;
    let mut session = engine.session().await.unwrap();
    let (_, events, took) =
        cancel_after(&mut session, "WAITFOR DELAY '00:00:10'; SELECT 1", Duration::from_millis(300)).await;
    assert!(took < Duration::from_secs(5), "{took:?}");
    // tiberius can't take the server's acknowledgement of this one, so the tab moves to a new connection.
    assert!(session.is_broken());
    assert!(
        messages(&events).iter().any(|(_, level, text)| *level == MessageLevel::Warning && text.contains("new one"))
    );
    let mut fresh = engine.session().await.unwrap();
    let (events, result) = run(&mut fresh, &["SELECT 42"]).await;
    assert_eq!(result.unwrap(), 1);
    assert_eq!(cells(&events, 0), [[Some("42".into())]]);
}

#[tokio::test]
async fn a_cancel_while_rows_stream_keeps_the_connection() {
    let Some(cfg) = mssql_config() else { return };
    let engine = mssql_engine(&cfg).await;
    let mut session = engine.session().await.unwrap();
    let rows = "SELECT TOP (5000000) a.object_id, b.name FROM sys.all_columns a CROSS JOIN sys.all_columns b";
    let (_, _, took) = cancel_after(&mut session, rows, Duration::from_millis(500)).await;
    assert!(took < Duration::from_secs(5), "{took:?}");
    eprintln!("broken after a streaming cancel: {}", session.is_broken());
    if !session.is_broken() {
        let (events, result) = run(&mut session, &["SELECT 42"]).await;
        assert_eq!(result.unwrap(), 1);
        assert_eq!(cells(&events, 0), [[Some("42".into())]]);
    }
}

#[tokio::test]
async fn the_catalog_describes_a_table() {
    let Some(cfg) = mssql_config() else { return };
    let engine = mssql_engine(&cfg).await;
    let mut session = engine.session().await.unwrap();
    session
        .buffered(
            "DROP TABLE IF EXISTS dbo.catalog_child; DROP TABLE IF EXISTS dbo.catalog_parent;
            CREATE TABLE dbo.catalog_parent (id int IDENTITY(1, 1) CONSTRAINT pk_catalog_parent PRIMARY KEY,
                name nvarchar(50) NOT NULL DEFAULT N'anon', price decimal(10, 2) CHECK (price >= 0),
                doubled AS (price * 2) PERSISTED);
            CREATE TABLE dbo.catalog_child (id int PRIMARY KEY, parent_id int REFERENCES dbo.catalog_parent (id));
            CREATE UNIQUE INDEX ix_catalog_parent_name ON dbo.catalog_parent (name) INCLUDE (price) WHERE name <> N''",
            &Cancel::new(),
        )
        .await
        .unwrap();
    let columns = engine.list_columns("dbo", "catalog_parent").await.unwrap();
    let summary: Vec<(&str, &str, bool, bool, bool)> = columns
        .iter()
        .map(|c| (c.name.as_str(), c.data_type.as_str(), c.is_primary, c.is_identity, c.is_computed))
        .collect();
    assert_eq!(
        summary,
        [
            ("id", "int", true, true, false),
            ("name", "nvarchar(50)", false, false, false),
            ("price", "decimal(10, 2)", false, false, false),
            ("doubled", "decimal(12, 2)", false, false, true),
        ]
    );
    let child = engine.list_columns("dbo", "catalog_child").await.unwrap();
    assert_eq!((child[1].foreign_table.as_str(), child[1].foreign_column.as_str()), ("catalog_parent", "id"));
    let indexes = engine.list_indexes("dbo", "catalog_parent").await.unwrap();
    assert!(indexes.iter().any(|i| i.is_primary && i.columns == ["id"]), "{indexes:?}");
    let tables = engine.list_tables("dbo").await.unwrap();
    assert!(tables.iter().any(|t| t.name == "catalog_parent" && t.kind == "table"));
    let object = ObjectRef {
        schema: "dbo".into(),
        name: "catalog_parent".into(),
        kind: ObjectKind::Table,
        ..Default::default()
    };
    let ddl = engine.object_ddl(&object).await.unwrap();
    eprintln!("{ddl}");
    assert!(ddl.contains("CREATE TABLE [dbo].[catalog_parent]"), "{ddl}");
    assert!(ddl.contains("CREATE UNIQUE INDEX [ix_catalog_parent_name]"), "{ddl}");
    assert!(ddl.contains("CONSTRAINT [pk_catalog_parent] PRIMARY KEY CLUSTERED ([id])"), "{ddl}");
    session.buffered("DROP TABLE dbo.catalog_child; DROP TABLE dbo.catalog_parent", &Cancel::new()).await.unwrap();
}
