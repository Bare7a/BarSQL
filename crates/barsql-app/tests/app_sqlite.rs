mod support;

use std::time::{Duration, Instant};

use barsql_app::{BackupOutcome, BackupRequest, CsvImportRequest, CsvOptions, RunEvent, SqlImportRequest};
use barsql_core::{DriverType, ObjectKind, Row, RowDelete, RowUpdate, SavedQuery, TableDataRequest, Value};
use barsql_sql::alter;
use support::{Fixture, collect, import_outcome, results, rows};

fn row(pairs: &[(&str, Value)]) -> Row {
    pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
}

fn header(has_header: bool) -> CsvOptions {
    CsvOptions { has_header, ..Default::default() }
}

fn names(values: &[&str]) -> Vec<String> {
    values.iter().map(|s| s.to_string()).collect()
}

#[tokio::test]
async fn connection_crud_and_lifecycle() {
    let f = Fixture::new();
    let id = f.sqlite().await;
    assert!(!id.is_empty());
    assert_eq!(f.app.list_connections().iter().map(|c| c.id.clone()).collect::<Vec<_>>(), [id.as_str()]);
    f.app.connect(&id).await.unwrap();
    assert!(f.app.is_connected(&id));
    let status = f.app.connection_status(&id).await.unwrap();
    assert!(status.connected);
    f.app.disconnect(&id).await;
    assert!(!f.app.is_connected(&id));
    assert!(f.app.delete_connection(&id).await);
    assert!(f.app.list_connections().is_empty());
    assert!(f.app.connect("nope").await.is_err());
}

#[tokio::test]
async fn execute_query_and_history() {
    let f = Fixture::new();
    let id = f.sqlite().await;
    f.exec(&id, "CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)").await;
    f.exec(&id, "INSERT INTO t VALUES (1, 'hello')").await;
    let res = f.app.execute_query(&id, "SELECT * FROM t").await.unwrap();
    assert_eq!((res.row_count, res.rows[0][1].clone()), (1, Value::from("hello")));

    f.app.clear_query_history(&id).unwrap();
    f.app.execute_query(&id, "SELECT 42").await.unwrap();
    let history = f.app.query_history(&id, 10);
    assert_eq!(history.len(), 1);
    assert_eq!((history[0].sql.as_str(), history[0].success), ("SELECT 42", true));
    f.app.clear_query_history(&id).unwrap();
    assert!(f.app.query_history(&id, 10).is_empty());
}

#[tokio::test]
async fn read_only_blocks_writes_but_not_reads() {
    let f = Fixture::new();
    let mut cfg = f.sqlite_config();
    cfg.read_only = true;
    let id = f.app.save_connection(cfg).await.unwrap().id;
    assert!(f.app.execute_query(&id, "CREATE TABLE t (id INTEGER)").await.is_err());
    f.app.execute_query(&id, "SELECT 1").await.unwrap();
    assert!(f.app.execute_query_stream(&id, "tab", "DELETE FROM t").await.is_err());
    let err = f.app.update_row(&id, &RowUpdate { table: "t".into(), ..Default::default() }).await.unwrap_err();
    assert!(err.message.contains("read-only"));
}

#[tokio::test]
async fn saved_queries_round_trip() {
    let f = Fixture::new();
    let saved = f
        .app
        .save_saved_query(SavedQuery {
            name: "get all".into(),
            sql: "SELECT * FROM users".into(),
            ..Default::default()
        })
        .unwrap();
    assert!(!saved.id.is_empty());
    assert_eq!(f.app.list_saved_queries("").len(), 1);
    assert!(f.app.delete_saved_query(&saved.id));
    assert!(f.app.list_saved_queries("").is_empty());
}

#[tokio::test]
async fn row_mutations_return_the_stored_row() {
    let f = Fixture::new();
    let id = f.sqlite().await;
    f.exec(&id, "CREATE TABLE things (id INTEGER PRIMARY KEY, label TEXT)").await;
    let inserted =
        f.app.insert_row(&id, "main", "things", &row(&[("id", 1.into()), ("label", "first".into())])).await.unwrap();
    assert_eq!(inserted.get("id"), Some(&Value::Int(1)));
    assert_eq!(inserted.get("label"), Some(&Value::from("first")));
    f.app
        .update_row(
            &id,
            &RowUpdate {
                schema: "main".into(),
                table: "things".into(),
                primary_key: row(&[("id", 1.into())]),
                changes: row(&[("label", "updated".into())]),
            },
        )
        .await
        .unwrap();
    assert_eq!(f.query(&id, "SELECT label FROM things").await, [["updated"]]);
    let (deleted, err) = f
        .app
        .delete_rows(
            &id,
            &RowDelete { schema: "main".into(), table: "things".into(), primary_keys: vec![row(&[("id", 1.into())])] },
        )
        .await;
    assert_eq!((deleted, err), (1, None));
}

#[tokio::test]
async fn streamed_runs_deliver_every_result_set_and_record_history() {
    let f = Fixture::new();
    let id = f.sqlite().await;
    f.app.clear_query_history(&id).unwrap();
    let handle = f.app.execute_query_stream(&id, "tab-1", "SELECT 1 AS a; SELECT 'x' AS b, 'y' AS c").await.unwrap();
    let events = collect(handle).await;
    let res = results(&events);
    assert_eq!(res.len(), 2);
    assert_eq!(rows(&events, 0), [[Some("1".to_string())]]);
    assert_eq!(rows(&events, 1), [[Some("x".to_string()), Some("y".to_string())]]);
    assert!(matches!(events.last(), Some(RunEvent::Done { result_count: 2, error: None })));
    assert_eq!(f.app.query_history(&id, 10).len(), 2);

    let failing = collect(
        f.app.execute_query_stream(&id, "tab-1", "SELECT 1; SELECT * FROM missing_zzz; SELECT 2").await.unwrap(),
    )
    .await;
    let res = results(&failing);
    assert_eq!(res.len(), 2, "the script stops at the failing statement");
    assert!(res[0].error.is_none() && res[1].error.is_some());
    assert!(matches!(failing.last(), Some(RunEvent::Done { result_count: 2, error: None })));
}

#[tokio::test]
async fn a_tab_keeps_its_session_state_between_runs() {
    let f = Fixture::new();
    let id = f.sqlite().await;
    collect(
        f.app
            .execute_query_stream(&id, "tab-a", "CREATE TEMP TABLE scratch (v INTEGER); INSERT INTO scratch VALUES (7)")
            .await
            .unwrap(),
    )
    .await;
    let events = collect(f.app.execute_query_stream(&id, "tab-a", "SELECT v FROM scratch").await.unwrap()).await;
    assert_eq!(rows(&events, 0), [[Some("7".to_string())]]);
}

#[tokio::test]
async fn table_pages_carry_primary_keys() {
    let f = Fixture::new();
    let id = f.sqlite().await;
    f.exec(&id, "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)").await;
    for i in 0..7 {
        f.exec(&id, &format!("INSERT INTO t (name) VALUES ('n{i}')")).await;
    }
    let req = TableDataRequest {
        schema: "main".into(),
        table: "t".into(),
        limit: 5,
        order_by: "id".into(),
        order_dir: "DESC".into(),
        ..Default::default()
    };
    let events = collect(f.app.query_table_stream(&id, "tab-t", req).await.unwrap()).await;
    let res = results(&events);
    let summary = res[0].summary.as_ref().unwrap();
    assert_eq!(
        (summary.row_count, summary.primary_keys.clone(), summary.table_name.as_str()),
        (5, vec!["id".to_string()], "t")
    );
    assert_eq!(rows(&events, 0)[0][0].as_deref(), Some("7"));

    let bad = TableDataRequest { table: "t".into(), filter: "1=1; DROP TABLE t".into(), ..Default::default() };
    let events = collect(f.app.query_table_stream(&id, "tab-t", bad).await.unwrap()).await;
    assert!(results(&events)[0].error.as_ref().unwrap().message.contains("';' is not allowed"));
}

#[tokio::test]
async fn transactions_commit_roll_back_and_guard() {
    let f = Fixture::new();
    let id = f.sqlite().await;
    f.exec(&id, "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT NOT NULL)").await;
    assert!(f.app.begin_transaction(&id, "").await.is_err());

    f.app.begin_transaction(&id, "tab").await.unwrap();
    assert!(f.app.transaction_status("tab"));
    assert!(f.app.begin_transaction(&id, "tab").await.is_err(), "one transaction per tab");
    collect(f.app.execute_query_stream(&id, "tab", "INSERT INTO t (name) VALUES ('a')").await.unwrap()).await;
    f.app.rollback_transaction("tab").await.unwrap();
    assert!(!f.app.transaction_status("tab"));
    assert_eq!(f.query(&id, "SELECT count(*) FROM t").await, [["0"]]);

    f.app.begin_transaction(&id, "tab").await.unwrap();
    collect(f.app.execute_query_stream(&id, "tab", "INSERT INTO t (name) VALUES ('b')").await.unwrap()).await;
    f.app.commit_transaction("tab").await.unwrap();
    assert_eq!(f.query(&id, "SELECT name FROM t").await, [["b"]]);

    f.app.begin_transaction(&id, "tab").await.unwrap();
    collect(f.app.execute_query_stream(&id, "tab", "INSERT INTO t (name) VALUES ('orphan')").await.unwrap()).await;
    f.app.cleanup_tab("tab").await;
    assert!(!f.app.transaction_status("tab"));
    assert_eq!(f.query(&id, "SELECT count(*) FROM t").await, [["1"]], "closing the tab rolls back");
    assert!(f.app.commit_transaction("never-opened").await.is_err());

    // A typed BEGIN is tracked too, so the toolbar shows the open transaction.
    collect(f.app.execute_query_stream(&id, "typed", "BEGIN; INSERT INTO t (name) VALUES ('c')").await.unwrap()).await;
    assert!(f.app.transaction_status("typed"));
    f.app.rollback_transaction("typed").await.unwrap();
}

#[tokio::test]
async fn cancel_stops_a_running_statement_promptly() {
    let f = Fixture::new();
    let id = f.sqlite().await;
    let slow = "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n) SELECT count(*) FROM n";
    let handle = f.app.execute_query_stream(&id, "tab", slow).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let started = Instant::now();
    assert!(f.app.cancel_query("tab"));
    let events = collect(handle).await;
    assert!(started.elapsed() < Duration::from_secs(5));
    let error = results(&events)[0].error.clone().expect("the statement is interrupted");
    assert!(error.cancelled, "{error:?}");
    assert!(!f.app.cancel_query("tab"), "nothing left to cancel");

    // Cancelling one tab leaves the others alone.
    let other = collect(f.app.execute_query_stream(&id, "tab-2", "SELECT 5").await.unwrap()).await;
    assert_eq!(rows(&other, 0), [[Some("5".to_string())]]);
}

#[tokio::test]
async fn a_new_run_on_the_tab_supersedes_the_old_one() {
    let f = Fixture::new();
    let id = f.sqlite().await;
    let slow = "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n) SELECT count(*) FROM n";
    let first = f.app.execute_query_stream(&id, "tab", slow).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let second = f.app.execute_query_stream(&id, "tab", "SELECT 9").await.unwrap();
    assert!(second.stream_id > first.stream_id);
    let first_events = collect(first).await;
    assert!(results(&first_events)[0].error.as_ref().is_some_and(|e| e.cancelled));
    assert_eq!(rows(&collect(second).await, 0), [[Some("9".to_string())]]);
}

async fn explain_fixture(read_only: bool) -> (Fixture, String) {
    let f = Fixture::new();
    let id = f.sqlite().await;
    for stmt in [
        "CREATE TABLE users (id INTEGER PRIMARY KEY, email TEXT, age INTEGER)",
        "CREATE INDEX users_email ON users (email)",
        "INSERT INTO users (email, age) VALUES ('a@example.com', 30), ('b@example.com', 41)",
    ] {
        f.exec(&id, stmt).await;
    }
    if read_only {
        let mut cfg = f.app.list_connections().remove(0);
        cfg.read_only = true;
        f.app.save_connection(cfg).await.unwrap();
    }
    (f, id)
}

#[tokio::test]
async fn explain_on_sqlite() {
    let (f, id) = explain_fixture(false).await;
    let plan = f.app.explain_query(&id, "", "SELECT * FROM users WHERE email = 'a@example.com'", false).await.unwrap();
    assert_eq!(plan.driver, DriverType::Sqlite);
    assert!(!plan.analyzed);
    assert_eq!(plan.explain_sql, "EXPLAIN QUERY PLAN SELECT * FROM users WHERE email = 'a@example.com'");
    let root = &plan.nodes[0];
    assert_eq!((root.label.as_str(), root.relation.as_str(), root.index.as_str()), ("SEARCH", "users", "users_email"));
    assert!(!plan.raw.is_empty());
    let scan = f.app.explain_query(&id, "tab", "SELECT * FROM users WHERE age > 20", false).await.unwrap();
    assert_eq!((scan.nodes[0].label.as_str(), scan.nodes[0].relation.as_str()), ("SCAN", "users"));

    let err = f.app.explain_query(&id, "", "SELECT * FROM users", true).await.unwrap_err();
    assert!(err.message.contains("EXPLAIN ANALYZE"), "{err:?}");
    assert!(f.app.explain_query(&id, "", "SELECT 1; SELECT 2", false).await.is_err());
    assert!(f.app.explain_query(&id, "", "   ", false).await.is_err());
    assert!(f.app.explain_query("nope", "", "SELECT 1", false).await.is_err());

    f.app.clear_query_history(&id).unwrap();
    f.app.explain_query(&id, "", "SELECT * FROM users", false).await.unwrap();
    assert!(f.app.query_history(&id, 10).is_empty(), "plans are not history");
}

#[tokio::test]
async fn explain_on_a_read_only_connection_blocks_writes() {
    let (f, id) = explain_fixture(true).await;
    assert!(f.app.explain_query(&id, "", "DELETE FROM users", false).await.is_err());
    f.app.explain_query(&id, "", "SELECT * FROM users", false).await.unwrap();
}

async fn import_fixture(file: &str, content: &str) -> (Fixture, String, String) {
    let f = Fixture::new();
    let id = f.sqlite().await;
    let path = f.write(file, content);
    (f, id, path)
}

async fn run_csv(f: &Fixture, id: &str, req: CsvImportRequest) -> barsql_app::ImportResult {
    let (_, done) = import_outcome(f.app.import_csv(id, "imp", req).await.unwrap()).await;
    done.result.unwrap_or_else(|| panic!("import failed: {}", done.error))
}

#[tokio::test]
async fn csv_import_creates_the_table_and_loads_rows() {
    let (f, id, path) = import_fixture("people.csv", "id,name,score\n1,Alice,9.5\n2,Bob,7\n3,Carol,8.25\n").await;
    let result = run_csv(
        &f,
        &id,
        CsvImportRequest {
            path,
            table: "people".into(),
            create_table: true,
            options: header(true),
            mapping: names(&["id", "name", "score"]),
            column_types: names(&["int", "text", "float"]),
            ..Default::default()
        },
    )
    .await;
    assert_eq!((result.inserted, result.skipped), (3, 0));
    assert_eq!(f.query(&id, "SELECT name FROM people ORDER BY id").await, [["Alice"], ["Bob"], ["Carol"]]);
    let cols = f.app.list_columns(&id, "main", "people").await.unwrap();
    let types: Vec<String> = cols.iter().map(|c| c.data_type.to_uppercase()).collect();
    assert_eq!(types, ["INTEGER", "TEXT", "REAL"]);
}

#[tokio::test]
async fn csv_import_mapping_nulls_and_bad_rows() {
    let (f, id, path) = import_fixture("people.csv", "id,name,ignored\n1,Alice,junk\n2,Bob,junk\n").await;
    f.exec(&id, "CREATE TABLE people (id INTEGER NOT NULL, name TEXT)").await;
    let result = run_csv(
        &f,
        &id,
        CsvImportRequest {
            path,
            table: "people".into(),
            options: header(true),
            mapping: names(&["id", "name", ""]),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(result.inserted, 2);
    assert_eq!(f.query(&id, "SELECT name FROM people ORDER BY id").await, [["Alice"], ["Bob"]]);

    let path = f.write("nulls.csv", "id,name\n1,\n2,\\N\n3,Carol\n");
    let options = CsvOptions { has_header: true, null_literal: "\\N".into(), ..Default::default() };
    run_csv(
        &f,
        &id,
        CsvImportRequest {
            path,
            table: "people".into(),
            options,
            mapping: names(&["id", "name"]),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(f.query(&id, "SELECT COUNT(*) FROM people WHERE name IS NULL").await, [["2"]]);

    let path = f.write("bad.csv", "id,name\n1,Alice\n2,Bob\n,Carol\n4,Dave\n");
    let result = run_csv(
        &f,
        &id,
        CsvImportRequest {
            path,
            table: "people".into(),
            options: header(true),
            mapping: names(&["id", "name"]),
            batch_size: 10,
            ..Default::default()
        },
    )
    .await;
    assert_eq!((result.inserted, result.skipped), (3, 1));
    assert!(!result.errors.is_empty());
}

#[tokio::test]
async fn csv_import_stop_on_error_keeps_committed_batches() {
    let (f, id, path) = import_fixture("people.csv", "id,name\n1,Alice\n,Bob\n3,Carol\n").await;
    f.exec(&id, "CREATE TABLE people (id INTEGER NOT NULL, name TEXT)").await;
    let req = CsvImportRequest {
        path,
        table: "people".into(),
        options: header(true),
        mapping: names(&["id", "name"]),
        batch_size: 1,
        stop_on_error: true,
        ..Default::default()
    };
    let (_, done) = import_outcome(f.app.import_csv(&id, "imp", req).await.unwrap()).await;
    assert!(done.result.is_none() && !done.error.is_empty());
    assert_eq!(f.query(&id, "SELECT COUNT(*) FROM people").await, [["1"]]);
}

#[tokio::test]
async fn csv_import_truncates_headerless_and_booleans() {
    let (f, id, path) = import_fixture("people.csv", "id,name\n9,Zoe\n").await;
    f.exec(&id, "CREATE TABLE people (id INTEGER, name TEXT)").await;
    f.exec(&id, "INSERT INTO people (id, name) VALUES (1, 'Old')").await;
    run_csv(
        &f,
        &id,
        CsvImportRequest {
            path,
            table: "people".into(),
            truncate: true,
            options: header(true),
            mapping: names(&["id", "name"]),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(f.query(&id, "SELECT name FROM people").await, [["Zoe"]]);

    let path = f.write("headerless.csv", "1,Alice\n2,Bob\n");
    run_csv(
        &f,
        &id,
        CsvImportRequest {
            path,
            table: "people2".into(),
            create_table: true,
            options: header(false),
            mapping: names(&["col1", "col2"]),
            column_types: names(&["int", "text"]),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(f.query(&id, "SELECT col2 FROM people2 ORDER BY col1").await, [["Alice"], ["Bob"]]);

    let path = f.write("flags.csv", "id,active\n1,true\n2,no\n3,Y\n");
    run_csv(
        &f,
        &id,
        CsvImportRequest {
            path,
            table: "flags".into(),
            create_table: true,
            options: header(true),
            mapping: names(&["id", "active"]),
            column_types: names(&["int", "bool"]),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(f.query(&id, "SELECT COUNT(*) FROM flags WHERE active = 1").await, [["2"]]);
}

#[tokio::test]
async fn csv_import_value_shapes() {
    let (f, id, path) = import_fixture("blanks.csv", "id,a,b\n1,,\"\"\n").await;
    f.exec(&id, "CREATE TABLE t (id INTEGER, a TEXT, b TEXT NOT NULL)").await;
    run_csv(
        &f,
        &id,
        CsvImportRequest {
            path,
            table: "t".into(),
            options: header(true),
            mapping: names(&["id", "a", "b"]),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(f.query(&id, "SELECT a IS NULL, b IS NULL, b FROM t").await, [["1", "0", ""]]);

    f.exec(&id, "CREATE TABLE f (v TEXT)").await;
    let path = f.write("formulas.csv", "v\n'=1+1\n'@handle\n'-not a number\n'plain\nO'Brien\n");
    run_csv(
        &f,
        &id,
        CsvImportRequest {
            path,
            table: "f".into(),
            options: header(true),
            mapping: names(&["v"]),
            ..Default::default()
        },
    )
    .await;
    let got: Vec<String> = f.query(&id, "SELECT v FROM f").await.into_iter().map(|r| r[0].clone()).collect();
    assert_eq!(got, ["=1+1", "@handle", "-not a number", "'plain", "O'Brien"]);

    f.exec(&id, "CREATE TABLE n (v TEXT)").await;
    let path = f.write("nulls.csv", "v\n\"\\N\"\n");
    let options = CsvOptions { has_header: true, null_literal: "\\N".into(), ..Default::default() };
    run_csv(
        &f,
        &id,
        CsvImportRequest { path, table: "n".into(), options, mapping: names(&["v"]), ..Default::default() },
    )
    .await;
    assert_eq!(f.query(&id, "SELECT v IS NULL FROM n").await, [["1"]], "the NULL literal beats quoting");

    f.exec(&id, "CREATE TABLE b (v TEXT)").await;
    let path = f.write("flags.csv", "v\ntrue\nf\n");
    run_csv(
        &f,
        &id,
        CsvImportRequest {
            path,
            table: "b".into(),
            options: header(true),
            mapping: names(&["v"]),
            column_types: names(&["bool"]),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(f.query(&id, "SELECT v FROM b ORDER BY rowid").await, [["true"], ["f"]], "bool-shaped text stays text");
}

#[tokio::test]
async fn csv_import_reports_the_row_target_in_progress() {
    let (f, id, path) = import_fixture("people.csv", "id,name\n1,Alice\n2,Bob\n3,Carol\n").await;
    f.exec(&id, "CREATE TABLE people (id INTEGER, name TEXT)").await;
    let req = CsvImportRequest {
        path,
        table: "people".into(),
        options: header(true),
        mapping: names(&["id", "name"]),
        ..Default::default()
    };
    let (progress, done) = import_outcome(f.app.import_csv(&id, "imp", req).await.unwrap()).await;
    assert_eq!(done.result.unwrap().inserted, 3);
    let last = progress.last().expect("a final progress event");
    assert_eq!((last.total_rows, last.processed, last.inserted), (3, 3, 3));
    assert_eq!(last.bytes_read, last.total_bytes);
}

#[tokio::test]
async fn previews_read_the_file_with_the_connection_dialect() {
    let (f, id, path) = import_fixture("people.csv", "id;name;joined\n1;Alice;2026-01-02\n2;Bob;2026-03-04\n").await;
    let preview = f.app.preview_import_file(&id, &path, &header(true)).unwrap();
    assert_eq!(preview.columns, ["id", "name", "joined"]);
    assert_eq!(preview.delimiter, ";");
    assert_eq!(preview.inferred_types, ["int", "text", "date"]);
    assert_eq!(preview.sql_types, ["INTEGER", "TEXT", "TEXT"]);
    assert_eq!((preview.rows.len(), preview.truncated, preview.total_rows), (2, false, 2));

    let mut big = String::from("id,name\n");
    for i in 1..=300 {
        big.push_str(&format!("{i},n{i}\n"));
    }
    let path = f.write("big.csv", &big);
    let preview = f.app.preview_import_file(&id, &path, &header(true)).unwrap();
    assert_eq!((preview.rows.len(), preview.truncated, preview.total_rows), (100, true, 300));

    let path = f.write("lazy.csv", "a,b\n1,\"x\n2,y\n");
    assert_eq!(f.app.preview_import_file(&id, &path, &header(true)).unwrap().total_rows, 1);
    let path = f.write("header.csv", "a,b\n");
    assert_eq!(f.app.preview_import_file(&id, &path, &header(true)).unwrap().total_rows, 0);
    let path = f.write("noheader.csv", "1,Alice\n2,Bob\n");
    let preview = f.app.preview_import_file(&id, &path, &header(false)).unwrap();
    assert_eq!((preview.columns.clone(), preview.rows[0][1].as_str()), (names(&["col1", "col2"]), "Alice"));
    let path = f.write("empty.csv", "");
    assert!(f.app.preview_import_file(&id, &path, &header(true)).is_err());
}

#[tokio::test]
async fn sql_import_runs_statements_and_reports_failures() {
    let script = "CREATE TABLE t (id INTEGER, name TEXT);\nINSERT INTO t (id, name) VALUES (1, 'Alice');\nINSERT INTO t (id, name) VALUES (2, 'Bob');\n";
    let (f, id, path) = import_fixture("dump.sql", script).await;
    let (_, done) =
        import_outcome(f.app.import_sql(&id, "imp", SqlImportRequest { path, stop_on_error: false }).await.unwrap())
            .await;
    let result = done.result.unwrap();
    assert_eq!((result.statements, result.skipped, result.inserted), (3, 0, 2));

    let path = f.write(
        "partial.sql",
        "CREATE TABLE u (id INTEGER);\nINSERT INTO nonexistent (id) VALUES (1);\nINSERT INTO u (id) VALUES (2);\n",
    );
    let (_, done) =
        import_outcome(f.app.import_sql(&id, "imp", SqlImportRequest { path, stop_on_error: false }).await.unwrap())
            .await;
    let result = done.result.unwrap();
    assert_eq!((result.statements, result.skipped), (2, 1));
    assert!(result.errors[0].contains("statement 2"), "{:?}", result.errors);

    let path = f.write(
        "abort.sql",
        "CREATE TABLE v (id INTEGER);\nINSERT INTO nope (id) VALUES (1);\nINSERT INTO v (id) VALUES (2);\n",
    );
    let (_, done) =
        import_outcome(f.app.import_sql(&id, "imp", SqlImportRequest { path, stop_on_error: true }).await.unwrap())
            .await;
    assert!(done.result.is_none() && done.error.contains("statement 2"));
    assert_eq!(f.query(&id, "SELECT COUNT(*) FROM v").await, [["0"]]);
}

#[tokio::test]
async fn imports_validate_the_request_and_the_connection() {
    let (f, id, path) = import_fixture("people.csv", "id\n1\n").await;
    let base =
        CsvImportRequest { path: path.clone(), table: "t".into(), mapping: names(&["id"]), ..Default::default() };
    assert!(f.app.import_csv(&id, "", base.clone()).await.is_err());
    assert!(f.app.import_csv(&id, "i", CsvImportRequest { path: String::new(), ..base.clone() }).await.is_err());
    assert!(f.app.import_csv(&id, "i", CsvImportRequest { table: String::new(), ..base.clone() }).await.is_err());
    assert!(f.app.import_csv(&id, "i", CsvImportRequest { mapping: vec![], ..base.clone() }).await.is_err());

    let mut cfg = f.app.list_connections().remove(0);
    cfg.read_only = true;
    f.app.save_connection(cfg).await.unwrap();
    assert!(f.app.import_csv(&id, "i", base).await.is_err());
    assert!(f.app.import_sql(&id, "i", SqlImportRequest { path, stop_on_error: false }).await.is_err());
}

#[tokio::test]
async fn text_files_are_written_in_chunks_with_owner_only_permissions() {
    let f = Fixture::new();
    let path = f.path("export.csv");
    f.app.append_text_file(&path, "a,b\n", true).unwrap();
    f.app.append_text_file(&path, "1,2\n", false).unwrap();
    f.app.append_text_file(&path, "3,4", false).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "a,b\n1,2\n3,4");
    std::fs::write(&path, "a much longer previous export").unwrap();
    f.app.append_text_file(&path, "fresh", true).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "fresh");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let fresh = f.path("new.csv");
        f.app.save_text_file(&fresh, "x").unwrap();
        assert_eq!(std::fs::metadata(&fresh).unwrap().permissions().mode() & 0o777, 0o600);
    }
    assert!(f.app.append_text_file("", "x", true).is_err());
}

#[tokio::test]
async fn the_pending_file_is_handed_out_once() {
    let f = Fixture::new();
    let path = f.write("launch.sqlite", "");
    f.app.set_pending_file(&path);
    assert_eq!(f.app.take_pending_file(), Some(barsql_app::sqlite_file_payload(&path)));
    assert_eq!(f.app.take_pending_file(), None);
}

#[tokio::test]
async fn a_trigger_body_runs_as_part_of_its_statement() {
    let f = Fixture::new();
    let id = f.sqlite().await;
    let script = "CREATE TABLE t (a INTEGER); CREATE TABLE log (a INTEGER);\n\
        CREATE TRIGGER t_log AFTER INSERT ON t BEGIN\n  INSERT INTO log VALUES (NEW.a);\n  INSERT INTO log VALUES (NEW.a * 10);\nEND;\n\
        INSERT INTO t VALUES (1)";
    let events = collect(f.app.execute_query_stream(&id, "tab", script).await.unwrap()).await;
    let results = results(&events);
    assert_eq!(results.len(), 4, "{results:?}");
    assert!(results.iter().all(|r| r.error.is_none()), "{results:?}");
    assert_eq!(f.query(&id, "SELECT a FROM log ORDER BY a").await, [["1"], ["10"]]);
}

#[tokio::test]
async fn shutdown_closes_every_engine_and_rolls_back_tab_transactions() {
    let f = Fixture::new();
    let first = f.sqlite().await;
    let mut other = f.sqlite_config();
    other.file_path = f.path("other.db");
    let second = f.app.save_connection(other).await.unwrap().id;
    f.exec(&first, "CREATE TABLE t (a INTEGER)").await;
    f.app.connect(&second).await.unwrap();
    f.app.begin_transaction(&first, "tab").await.unwrap();
    let events = collect(f.app.execute_query_stream(&first, "tab", "INSERT INTO t VALUES (1)").await.unwrap()).await;
    assert!(results(&events).iter().all(|r| r.error.is_none()));

    f.app.shutdown().await;
    assert!(!f.app.is_connected(&first) && !f.app.is_connected(&second));
    assert!(!f.app.transaction_status("tab"));
    assert_eq!(f.query(&first, "SELECT count(*) FROM t").await, [["0"]], "the tab transaction was rolled back");
}

#[tokio::test]
async fn a_script_mixes_plans_and_grids() {
    let f = Fixture::new();
    let id = f.sqlite().await;
    let script = "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT); INSERT INTO t VALUES (1, 'a'), (2, 'b'); \
        SELECT * FROM t; EXPLAIN QUERY PLAN SELECT * FROM t WHERE id = 1; SELECT COUNT(*) FROM t; EXPLAIN SELECT * FROM t";
    let events = collect(f.app.execute_query_stream(&id, "tab", script).await.unwrap()).await;
    let results = results(&events);
    assert_eq!(results.len(), 6, "{results:?}");
    assert!(results.iter().all(|r| r.error.is_none()), "{results:?}");
    let plan = results[3].plan.as_ref().expect("a plan for EXPLAIN QUERY PLAN");
    assert!(!plan.analyzed);
    assert!(plan.nodes[0].label.contains("SEARCH") && plan.nodes[0].relation == "t", "{:?}", plan.nodes);
    assert!(rows(&events, 3).is_empty(), "a plan carries no grid rows");
    for grid in [2, 4, 5] {
        assert!(results[grid].plan.is_none(), "result {grid}");
        assert!(!rows(&events, grid).is_empty(), "result {grid} has rows");
    }
}

#[tokio::test]
async fn rows_stream_in_batches_of_five_thousand() {
    let f = Fixture::new();
    let id = f.sqlite().await;
    let sql = "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 12000) \
        SELECT i AS id, 'row ' || i AS label FROM n";
    let events = collect(f.app.execute_query_stream(&id, "tab", sql).await.unwrap()).await;
    let metas = events.iter().filter(|e| matches!(e, RunEvent::Meta { result_index: 0, .. })).count();
    let chunks: Vec<usize> = events
        .iter()
        .filter_map(|e| match e {
            RunEvent::Rows { result_index: 0, chunk } => Some(chunk.rows()),
            _ => None,
        })
        .collect();
    assert_eq!((metas, chunks), (1, vec![5000, 5000, 2000]));
    assert_eq!(results(&events)[0].summary.as_ref().map(|s| s.row_count), Some(12000));
}

fn backup_request(f: &Fixture, table: &str, structure: bool) -> BackupRequest {
    BackupRequest {
        schema: "main".into(),
        table: table.into(),
        structure,
        data: true,
        path: f.path(&format!("{table}-{structure}.sql")).into(),
    }
}

async fn restore(f: &Fixture, id: &str, path: &std::path::Path) {
    let req = SqlImportRequest { path: path.display().to_string(), stop_on_error: true };
    let (_, done) = import_outcome(f.app.import_sql(id, "restore", req).await.unwrap()).await;
    assert!(done.result.is_some(), "{}", done.error);
}

// Generated columns are left out, and 252 rows take three INSERT statements.
#[tokio::test]
async fn a_table_backup_restores_every_value() {
    let f = Fixture::new();
    let id = f.sqlite().await;
    f.exec(
        &id,
        "CREATE TABLE kept (id INTEGER PRIMARY KEY, name TEXT NOT NULL, score REAL, photo BLOB, note TEXT,
            doubled INTEGER GENERATED ALWAYS AS (id * 2))",
    )
    .await;
    f.exec(&id, "CREATE INDEX kept_name ON kept (name)").await;
    f.exec(
        &id,
        "INSERT INTO kept (id, name, score, photo, note) VALUES
            (1, 'it''s', 0.1, X'00FF10', NULL), (2, 'line' || char(10) || 'break', 1.0000000000000002, X'', 'x')",
    )
    .await;
    f.exec(
        &id,
        "WITH RECURSIVE n(i) AS (SELECT 3 UNION ALL SELECT i + 1 FROM n WHERE i < 252)
            INSERT INTO kept (id, name) SELECT i, 'row ' || i FROM n",
    )
    .await;
    let snapshot = "SELECT id, name, quote(score), quote(photo), note, doubled FROM kept ORDER BY id";
    let before = f.query(&id, snapshot).await;

    let req = backup_request(&f, "kept", true);
    let path = req.path.clone();
    let (tx, rx) = async_channel::unbounded();
    let outcome = f.app.backup_table(&id, "backup", req, tx).await.unwrap();
    assert_eq!(outcome, BackupOutcome { rows: 252, cancelled: false });
    let mut last = None;
    while let Ok(rows) = rx.try_recv() {
        last = Some(rows);
    }
    assert_eq!(last, Some(252), "the last progress is the whole table");
    let script = std::fs::read_to_string(&path).unwrap();
    assert!(script.starts_with("-- BarSQL backup of \"kept\"\n"), "{script}");
    assert!(script.contains("CREATE TABLE kept") && script.contains("CREATE INDEX kept_name"), "{script}");
    assert_eq!(script.matches(r#"INSERT INTO "kept" ("id", "name", "score", "photo", "note") VALUES"#).count(), 3);

    f.exec(&id, "DROP TABLE kept").await;
    restore(&f, &id, &path).await;
    assert_eq!(f.query(&id, snapshot).await, before);
    assert_eq!(f.query(&id, "SELECT name FROM sqlite_master WHERE type = 'index'").await, [["kept_name"]]);
}

#[tokio::test]
async fn a_data_only_backup_refills_a_truncated_table() {
    let f = Fixture::new();
    let id = f.sqlite().await;
    f.exec(&id, "CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)").await;
    f.exec(&id, "INSERT INTO t VALUES (1, 'a'), (2, 'b')").await;
    let req = backup_request(&f, "t", false);
    let path = req.path.clone();
    let (tx, _rx) = async_channel::unbounded();
    f.app.backup_table(&id, "backup", req, tx).await.unwrap();
    assert!(!std::fs::read_to_string(&path).unwrap().contains("CREATE"));

    let truncate = alter::truncate_table(&DriverType::Sqlite, "main", "t", false, false);
    f.app.execute_statement(&id, "truncate", &truncate).await.unwrap();
    assert_eq!(f.app.count_rows(&id, "main", "t").await.unwrap(), 0);
    restore(&f, &id, &path).await;
    assert_eq!(f.app.count_rows(&id, "main", "t").await.unwrap(), 2);
    assert_eq!(f.query(&id, "SELECT v FROM t ORDER BY id").await, [["a"], ["b"]]);
}

#[tokio::test]
async fn a_failed_backup_keeps_the_older_file() {
    let f = Fixture::new();
    let id = f.sqlite().await;
    let req = backup_request(&f, "missing", true);
    let path = req.path.clone();
    std::fs::write(&path, "last week's backup").unwrap();
    let (tx, _rx) = async_channel::unbounded();
    assert!(f.app.backup_table(&id, "backup", req, tx).await.is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "last week's backup");
    let leftovers: Vec<_> = std::fs::read_dir(f.dir.path()).unwrap().flatten().map(|e| e.file_name()).collect();
    assert!(!leftovers.iter().any(|name| name.to_string_lossy().contains("barsql-partial")), "{leftovers:?}");
}

// Foreign keys are on, and a row can point at one restored after it.
#[tokio::test]
async fn a_self_referencing_table_restores_in_any_order() {
    let f = Fixture::new();
    let id = f.sqlite().await;
    f.exec(&id, "CREATE TABLE staff (id INTEGER PRIMARY KEY, manager_id INTEGER REFERENCES staff(id))").await;
    f.exec(
        &id,
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 250)
            INSERT INTO staff SELECT i, CASE WHEN i < 250 THEN 250 END FROM n",
    )
    .await;
    let req = backup_request(&f, "staff", true);
    let path = req.path.clone();
    let (tx, _rx) = async_channel::unbounded();
    f.app.backup_table(&id, "backup", req, tx).await.unwrap();
    f.exec(&id, "DROP TABLE staff").await;
    restore(&f, &id, &path).await;
    assert_eq!(f.app.count_rows(&id, "main", "staff").await.unwrap(), 250);
    assert_eq!(f.query(&id, "PRAGMA foreign_keys").await, [["1"]], "checks are back on");
}

#[tokio::test]
async fn sidebar_statements_run_and_respect_read_only() {
    let f = Fixture::new();
    let id = f.sqlite().await;
    let lite = DriverType::Sqlite;
    f.exec(&id, "CREATE TABLE a (id INTEGER, old_name TEXT, gone TEXT)").await;
    f.exec(&id, "INSERT INTO a VALUES (1, 'x', 'y')").await;
    for sql in [
        alter::rename_column(&lite, "main", "a", "old_name", "new_name"),
        alter::drop_column(&lite, "main", "a", "gone", false),
        alter::rename_relation(&lite, &ObjectKind::Table, "main", "a", "b").unwrap(),
    ] {
        f.app.execute_statement(&id, "change", &sql).await.unwrap_or_else(|e| panic!("{sql}: {e:?}"));
    }
    let columns: Vec<String> =
        f.app.list_columns(&id, "main", "b").await.unwrap().into_iter().map(|c| c.name).collect();
    assert_eq!(columns, ["id", "new_name"]);
    assert_eq!(f.app.count_rows(&id, "main", "b").await.unwrap(), 1);
    let missing = alter::drop_relation(&lite, &ObjectKind::Table, "main", "a", false);
    assert!(f.app.execute_statement(&id, "change", &missing).await.unwrap_err().message.contains("no such table"));
    let history = f.app.query_history(&id, 10);
    assert!(history.iter().any(|h| h.sql.contains(r#"RENAME TO "b""#) && h.success), "{history:?}");
    assert!(history.iter().any(|h| h.sql == missing && !h.success), "{history:?}");

    let mut cfg = f.app.list_connections().remove(0);
    cfg.read_only = true;
    f.app.save_connection(cfg).await.unwrap();
    let drop = alter::drop_relation(&lite, &ObjectKind::Table, "main", "b", false);
    assert!(f.app.execute_statement(&id, "change", &drop).await.unwrap_err().message.contains("read-only"));
    assert_eq!(f.app.count_rows(&id, "main", "b").await.unwrap(), 1, "counting still works");
}
