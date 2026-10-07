use barsql_core::{
    ConnectionConfig, DriverType, FunctionKind, ObjectKind, ObjectRef, Row, RowDelete, RowUpdate, Value,
};
use barsql_db::sqlite::SqliteConnectOptions;
use barsql_db::{Cancel, Engine, ScriptEvent};

fn config(path: &std::path::Path, read_only: bool) -> ConnectionConfig {
    ConnectionConfig {
        driver: DriverType::Sqlite,
        file_path: path.display().to_string(),
        read_only,
        ..Default::default()
    }
}

async fn engine(path: &std::path::Path, read_only: bool) -> Engine {
    Engine::connect(&config(path, read_only)).await.expect("open sqlite")
}

async fn exec(engine: &Engine, sql: &str) {
    engine.session().await.unwrap().buffered(sql, &Cancel::new()).await.unwrap_or_else(|e| panic!("{sql}: {e:?}"));
}

fn row(pairs: &[(&str, Value)]) -> Row {
    pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
}

#[tokio::test]
async fn a_read_only_engine_rejects_writes_below_the_app_gate() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ro.db");
    let writable = engine(&path, false).await;
    exec(&writable, "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)").await;
    exec(&writable, "INSERT INTO t VALUES (1, 'a')").await;
    writable.close().await;

    let ro = engine(&path, true).await;
    let mut session = ro.session().await.unwrap();
    assert!(session.buffered("SELECT * FROM t", &Cancel::new()).await.is_ok());
    for write in [
        "UPDATE t SET name = 'b' WHERE id = 1",
        "DELETE FROM t WHERE id = 1",
        "INSERT INTO t (id, name) VALUES (2, 'c')",
        "DROP TABLE t",
        "CREATE TABLE u (id INTEGER)",
        "UPDATE t SET name = 'b' WHERE id = 1 RETURNING *",
        "DELETE FROM t RETURNING *",
        "WITH d AS (SELECT id FROM t) DELETE FROM t WHERE id IN (SELECT id FROM d)",
    ] {
        assert!(session.buffered(write, &Cancel::new()).await.is_err(), "{write}");
    }
    let update = RowUpdate {
        schema: "main".into(),
        table: "t".into(),
        primary_key: row(&[("id", Value::Int(1))]),
        changes: row(&[("name", Value::from("x"))]),
    };
    assert!(ro.update_row(&update).await.is_err());
    let delete =
        RowDelete { schema: "main".into(), table: "t".into(), primary_keys: vec![row(&[("id", Value::Int(1))])] };
    assert!(ro.delete_rows(&delete).await.1.is_some());
    assert!(ro.insert_row("main", "t", &row(&[("name", Value::from("c"))])).await.is_err());
    let left = session.buffered("SELECT name FROM t", &Cancel::new()).await.unwrap();
    assert_eq!((left.rows(), left.text(0, 0)), (1, Some("a")));
}

#[tokio::test]
async fn read_only_sets_query_only_and_paths_drop_their_query() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("q.db");
    for (read_only, want) in [(false, "0"), (true, "1")] {
        let engine = engine(&path, read_only).await;
        let res = engine.session().await.unwrap().buffered("PRAGMA query_only", &Cancel::new()).await.unwrap();
        assert_eq!(res.text(0, 0), Some(want), "read_only={read_only}");
    }
    let mut cfg = config(std::path::Path::new("/tmp/db.sqlite"), false);
    cfg.file_path.push_str("?mode=ro&_pragma=journal_mode(MEMORY)");
    assert_eq!(SqliteConnectOptions::from_config(&cfg).path, std::path::PathBuf::from("/tmp/db.sqlite"));
}

#[tokio::test]
async fn connecting_needs_a_file_path() {
    let err = Engine::connect(&ConnectionConfig { driver: DriverType::Sqlite, ..Default::default() }).await.err();
    assert!(err.is_some_and(|e| e.message.contains("file path")));
}

#[tokio::test]
async fn ping_and_schema_info() {
    let dir = tempfile::tempdir().unwrap();
    let engine = engine(&dir.path().join("info.db"), false).await;
    engine.ping().await.unwrap();
    let schemas: Vec<String> = engine.list_schemas().await.unwrap().into_iter().map(|s| s.name).collect();
    assert_eq!(schemas, ["main"]);
    let info = engine.connection_info().await.unwrap();
    assert_eq!((info.database.as_str(), info.schema.as_str()), ("main", "main"));
}

// The bundled build lists its built-ins. It leaves out the math functions, so completion won't offer them.
#[tokio::test]
async fn functions_come_from_the_function_list() {
    let dir = tempfile::tempdir().unwrap();
    let engine = engine(&dir.path().join("functions.db"), false).await;
    let list = engine.list_functions().await.unwrap();
    let find = |name: &str| list.functions.iter().find(|f| f.name == name);
    for name in ["abs", "json_extract", "iif", "unixepoch", "group_concat", "bm25", "snippet"] {
        assert!(find(name).is_some_and(|f| f.builtin), "{name} missing from {:?}", list.functions.len());
    }
    assert_eq!(find("group_concat").map(|f| f.kind), Some(FunctionKind::Aggregate));
    assert_eq!(find("abs").map(|f| f.kind), Some(FunctionKind::Scalar));
    assert!(list.functions.windows(2).all(|w| w[0].name < w[1].name), "one entry per name, sorted");
    assert!(find("json_each").is_none(), "table-valued functions are modules");
    assert!(find("->").is_none(), "operators aren't callable by name");
}

#[tokio::test]
async fn a_composite_foreign_key_is_one_constraint() {
    let dir = tempfile::tempdir().unwrap();
    let engine = engine(&dir.path().join("fk.db"), false).await;
    exec(&engine, "CREATE TABLE parent (a INTEGER, b INTEGER, PRIMARY KEY (a, b))").await;
    exec(&engine, "CREATE TABLE child (x INTEGER, y INTEGER, FOREIGN KEY (x, y) REFERENCES parent (a, b))").await;
    let constraints = engine.list_constraints("main", "child").await.unwrap();
    let fks: Vec<_> = constraints.iter().filter(|c| c.kind == "FOREIGN KEY").collect();
    assert_eq!(fks.len(), 1, "{constraints:?}");
    assert_eq!(
        (fks[0].columns.as_slice(), fks[0].ref_columns.as_slice()),
        (&["x".to_string(), "y".to_string()][..], &["a".to_string(), "b".to_string()][..])
    );
    assert_eq!(fks[0].ref_table, "parent");
}

#[tokio::test]
async fn ddl_for_routines_and_missing_objects_fails() {
    let dir = tempfile::tempdir().unwrap();
    let engine = engine(&dir.path().join("ddl.db"), false).await;
    let object = |kind: ObjectKind, name: &str| ObjectRef {
        kind,
        schema: "main".into(),
        name: name.into(),
        ..Default::default()
    };
    assert!(engine.object_ddl(&object(ObjectKind::Function, "whatever")).await.is_err());
    assert!(engine.object_ddl(&object(ObjectKind::Table, "nope")).await.is_err());
}

async fn seeded(dir: &tempfile::TempDir) -> Engine {
    let engine = engine(&dir.path().join("cancel.db"), false).await;
    exec(
        &engine,
        "CREATE TABLE t AS WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 3000) \
         SELECT i AS id, 'row ' || i AS name FROM n",
    )
    .await;
    engine
}

fn cancelled() -> Cancel {
    let cancel = Cancel::new();
    cancel.cancel();
    cancel
}

#[tokio::test]
async fn a_cancelled_script_runs_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let engine = seeded(&dir).await;
    let mut session = engine.session().await.unwrap();
    let (tx, rx) = async_channel::unbounded();
    let err =
        session.run_script(&["INSERT INTO t VALUES (99, 'x')".into()], &tx, &cancelled()).await.expect_err("cancelled");
    drop(tx);
    assert!(err.cancelled, "{err:?}");
    let results: Vec<ScriptEvent> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
    assert!(
        matches!(results.as_slice(), [ScriptEvent::Result(r)] if r.error.as_ref().is_some_and(|e| e.cancelled)),
        "{results:?}"
    );
    let count = session.buffered("SELECT count(*) FROM t", &Cancel::new()).await.unwrap();
    assert_eq!(count.text(0, 0), Some("3000"));
}

// A cancel that lands before the statement starts must not be lost to sqlite3_interrupt.
#[tokio::test]
async fn a_cancelled_stream_or_buffered_read_stops() {
    let dir = tempfile::tempdir().unwrap();
    let engine = seeded(&dir).await;
    let mut session = engine.session().await.unwrap();
    let (tx, _rx) = async_channel::unbounded();
    let streamed = session.stream("SELECT * FROM t", &tx, &cancelled()).await;
    assert!(streamed.as_ref().is_err_and(|e| e.cancelled), "{streamed:?}");
    let buffered = session.buffered("SELECT * FROM t, t AS t2 LIMIT 5000000", &cancelled()).await;
    assert!(buffered.as_ref().is_err_and(|e| e.cancelled), "{:?}", buffered.map(|b| b.rows()));
    let after = session.buffered("SELECT count(*) FROM t", &Cancel::new()).await.unwrap();
    assert_eq!(after.text(0, 0), Some("3000"), "the session keeps working");
}

#[tokio::test]
async fn a_long_statement_stops_when_cancelled_mid_run() {
    let dir = tempfile::tempdir().unwrap();
    let engine = seeded(&dir).await;
    let mut session = engine.session().await.unwrap();
    let cancel = Cancel::new();
    let trigger = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        trigger.cancel();
    });
    let started = std::time::Instant::now();
    let res = session.buffered("SELECT count(*) FROM t, t AS t2, t AS t3", &cancel).await;
    assert!(res.as_ref().is_err_and(|e| e.cancelled), "{:?}", res.map(|b| b.rows()));
    assert!(started.elapsed() < std::time::Duration::from_secs(5), "{:?}", started.elapsed());
}

#[tokio::test]
async fn read_only_sessions_refuse_writes_themselves() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("guard.db");
    exec(&engine(&path, false).await, "CREATE TABLE t (a INTEGER)").await;
    let ro = engine(&path, true).await;
    let mut session = ro.session().await.unwrap();
    let (tx, rx) = async_channel::unbounded();
    let scripts = ["SELECT 1".to_string(), "DELETE FROM t".to_string()];
    let err = session.run_script(&scripts, &tx, &Cancel::new()).await.expect_err("refused");
    assert_eq!(err.message, barsql_sql::READ_ONLY_ERROR);
    let events: Vec<ScriptEvent> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
    assert!(matches!(events.as_slice(), [ScriptEvent::Result(r)] if r.statement == "DELETE FROM t"), "{events:?}");
    let streamed = session.stream("DELETE FROM t", &tx, &Cancel::new()).await;
    assert_eq!(streamed.map_err(|e| e.message), Err(barsql_sql::READ_ONLY_ERROR.to_string()));
    assert!(session.run_script(&["SELECT count(*) FROM t".into()], &tx, &Cancel::new()).await.is_ok());
}
