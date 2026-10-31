mod common;

use barsql_core::{
    ConnectionConfig, DriverType, ExtensionFile, ExtensionsConfig, FunctionKind, ObjectKind, ObjectRef, Row, RowDelete,
    RowUpdate, Value,
};
use barsql_db::sqlite::SqliteConnectOptions;
use barsql_db::{Cancel, Engine, ScriptEvent};
use barsql_sql::structure::Step;

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
async fn an_extension_that_wont_load_fails_the_connection_and_names_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("crsqlite.dylib").display().to_string();
    let mut cfg = config(&dir.path().join("ext.db"), false);
    let files = vec![ExtensionFile { path: missing.clone(), ..Default::default() }];
    cfg.extensions = ExtensionsConfig { enabled: true, files, ..Default::default() };
    let err = Engine::test(&cfg).await.expect_err("the extension is missing");
    assert_eq!(err.message, "can't load extension crsqlite.dylib: file not found");
    let failure = err.extension.expect("names the file");
    assert_eq!((failure.path, failure.reason.as_str()), (missing, "file not found"));
    cfg.extensions.enabled = false;
    Engine::test(&cfg).await.expect("a list that's off isn't loaded");
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
async fn result_columns_name_the_table_columns_they_come_from() {
    let dir = tempfile::tempdir().unwrap();
    let engine = engine(&dir.path().join("origins.db"), false).await;
    exec(&engine, "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)").await;
    let mut session = engine.session().await.unwrap();
    let sql = "SELECT id, name AS n, upper(name) FROM t";
    let res = session.buffered(sql, &Cancel::new()).await.unwrap();
    let sources = session.column_sources(sql, &res.columns, &Cancel::new()).await.unwrap();
    let named: Vec<Option<(String, String, String)>> =
        sources.columns.into_iter().map(|s| s.map(|s| (s.schema, s.table, s.column))).collect();
    let column = |name: &str| Some(("main".to_string(), "t".to_string(), name.to_string()));
    assert_eq!(named, [column("id"), column("name"), None]);
}

#[tokio::test]
async fn each_results_columns_come_with_its_statement() {
    let dir = tempfile::tempdir().unwrap();
    let engine = engine(&dir.path().join("statements.db"), false).await;
    exec(&engine, "CREATE TABLE t (id INTEGER PRIMARY KEY)").await;
    let mut session = engine.session().await.unwrap();
    let (tx, rx) = async_channel::unbounded();
    let script = ["SELECT id FROM t".to_string(), "SELECT 1 AS one".to_string()];
    session.run_script(&script, &tx, &Cancel::new()).await.unwrap();
    drop(tx);
    let named: Vec<(usize, String)> = std::iter::from_fn(|| rx.try_recv().ok())
        .filter_map(|event| match event {
            ScriptEvent::Meta { result_index, statement, .. } => Some((result_index, statement)),
            _ => None,
        })
        .collect();
    assert_eq!(named, [(0, script[0].clone()), (1, script[1].clone())]);
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

// A column declared INTEGER PRIMARY KEY DESC doesn't alias the rowid, though a PRIMARY KEY (id DESC) constraint does.
#[tokio::test]
async fn only_the_rowid_alias_is_an_identity() {
    let dir = tempfile::tempdir().unwrap();
    let engine = engine(&dir.path().join("rowid.db"), false).await;
    let mut session = engine.session().await.unwrap();
    for (table, definition, alias) in [
        ("plain", "(id INTEGER PRIMARY KEY, v TEXT)", true),
        ("no_rowid", "(id INTEGER PRIMARY KEY, v TEXT) WITHOUT ROWID", false),
        ("pair", "(id INTEGER, other INTEGER, v TEXT, PRIMARY KEY (id, other))", false),
        ("column_desc", "(id INTEGER PRIMARY KEY DESC, v TEXT)", false),
        ("constraint_desc", "(id INTEGER, v TEXT, PRIMARY KEY (id DESC))", true),
    ] {
        exec(&engine, &format!("CREATE TABLE {table} {definition}")).await;
        let columns = engine.list_columns("main", table).await.unwrap();
        let identities: Vec<&str> = columns.iter().filter(|c| c.is_identity).map(|c| c.name.as_str()).collect();
        assert_eq!(identities, if alias { vec!["id"] } else { vec![] }, "{table}");
        // SQLite agrees: an INSERT without the key gets one only through the alias.
        let insert = format!("INSERT INTO {table} (v) VALUES ('x') RETURNING id");
        let inserted = session.buffered(&insert, &Cancel::new()).await.map(|res| res.values());
        assert_eq!(matches!(inserted.as_deref(), Ok([row]) if row[0] != Value::Null), alias, "{table}: {inserted:?}");
    }
}

// SQLite has no enum type, so a CHECK that lists the values stands in for one, on the column or the table.
#[tokio::test]
async fn a_check_that_lists_values_gives_the_column_its_values() {
    let dir = tempfile::tempdir().unwrap();
    let engine = engine(&dir.path().join("checks.db"), false).await;
    exec(
        &engine,
        "CREATE TABLE reactions (id INTEGER PRIMARY KEY, kind TEXT NOT NULL CHECK (kind IN ('like', 'love')), \
         stars INT, note TEXT CHECK (length(note) < 100), CONSTRAINT star_list CHECK (stars = 1 OR stars = 2))",
    )
    .await;
    let columns = engine.list_columns("main", "reactions").await.unwrap();
    assert_eq!(common::column_values(&columns), ["id: ", "kind: like, love", "stars: 1, 2", "note: "]);
    exec(&engine, "CREATE VIEW reacted AS SELECT kind FROM reactions").await;
    let columns = engine.list_columns("main", "reacted").await.unwrap();
    assert_eq!(common::column_values(&columns), ["kind: "], "a view has no checks of its own");
}

// Earlier rows hold the values a wrong lookup would find, like a stale last_insert_rowid or a shadowed table's.
#[tokio::test]
async fn an_insert_returns_the_row_it_stored() {
    let dir = tempfile::tempdir().unwrap();
    let engine = engine(&dir.path().join("insert.db"), false).await;
    for sql in [
        "CREATE TABLE plain (id INTEGER PRIMARY KEY, v TEXT)",
        "CREATE TABLE pair (a INTEGER, b INTEGER, v TEXT, PRIMARY KEY (a, b))",
        "CREATE TABLE named (rowid TEXT, v TEXT)",
        "CREATE TABLE no_rowid (id INTEGER PRIMARY KEY, v TEXT) WITHOUT ROWID",
        "CREATE TABLE tagged (k TEXT PRIMARY KEY DEFAULT 'auto', v TEXT, note TEXT) WITHOUT ROWID",
        "CREATE TABLE shaded (id INTEGER PRIMARY KEY, v TEXT)",
        "CREATE TEMP TABLE shaded (id INTEGER PRIMARY KEY, v TEXT) WITHOUT ROWID",
        "CREATE VIEW recent AS SELECT id, v FROM plain",
        "CREATE TRIGGER add_recent INSTEAD OF INSERT ON recent BEGIN INSERT INTO plain (v) VALUES (NEW.v); END",
        "INSERT INTO pair VALUES (2, 9, 'old')",
        "INSERT INTO named VALUES ('2', 'old')",
        "INSERT INTO no_rowid VALUES (7, 'old')",
        "INSERT INTO plain VALUES (7, 'old')",
    ] {
        exec(&engine, sql).await;
    }
    let new = Value::from("new");
    for (table, values, stored) in [
        ("no_rowid", vec![("id", Value::Int(9)), ("v", new.clone())], vec![]),
        ("tagged", vec![("v", new.clone()), ("note", Value::Null)], vec![("k", Value::from("auto"))]),
        ("shaded", vec![("id", Value::Int(3)), ("v", new.clone())], vec![]),
        ("pair", vec![("a", Value::Int(5)), ("b", Value::Int(6)), ("v", new.clone())], vec![]),
        ("named", vec![("rowid", Value::from("r")), ("v", new.clone())], vec![]),
        ("plain", vec![("v", new.clone())], vec![("id", Value::Int(8))]),
        ("recent", vec![("v", Value::from("seen"))], vec![("id", Value::Int(9))]),
    ] {
        let inserted = engine.insert_row("main", table, &row(&values)).await.unwrap();
        assert_eq!(inserted, row(&[values, stored].concat()), "{table}");
    }
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
async fn a_cancel_mid_stream_keeps_the_rows_already_read() {
    let dir = tempfile::tempdir().unwrap();
    let engine = seeded(&dir).await;
    let mut session = engine.session().await.unwrap();
    let (_, events, _) =
        common::cancel_after(&mut session, "SELECT * FROM t, t AS t2", std::time::Duration::from_millis(100)).await;
    let (sent, counted) = common::rows_and_count(&events);
    assert!(sent > 0, "the cancel came before the first row");
    assert_eq!(counted, Some(sent as i64));
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

#[tokio::test]
async fn a_cancel_ends_the_wait_for_the_connection() {
    let dir = tempfile::tempdir().unwrap();
    let engine = seeded(&dir).await;
    let mut holder = engine.session().await.unwrap();
    holder.begin().await.unwrap();
    let mut waiting = engine.session().await.unwrap();
    let cancel = Cancel::new();
    let (tx, rx) = async_channel::unbounded();
    let run = tokio::spawn({
        let cancel = cancel.clone();
        async move { waiting.run_script(&["SELECT count(*) FROM t".into()], &tx, &cancel).await }
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    cancel.cancel();
    let ended = tokio::time::timeout(std::time::Duration::from_secs(5), run).await.expect("the cancel ends the wait");
    assert!(ended.unwrap().is_err_and(|e| e.cancelled));
    let events: Vec<ScriptEvent> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
    assert!(
        matches!(events.as_slice(), [ScriptEvent::Result(r)] if r.error.as_ref().is_some_and(|e| e.cancelled)),
        "{events:?}"
    );
    holder.commit().await.unwrap();
}

// A tab keeps its session between runs, so a plan must not keep the file's one connection with it.
#[tokio::test]
async fn a_plan_gives_the_connection_back() {
    let dir = tempfile::tempdir().unwrap();
    let engine = engine(&dir.path().join("plan.db"), false).await;
    exec(&engine, "CREATE TABLE t (id INTEGER PRIMARY KEY)").await;
    let mut tab = engine.session().await.unwrap();
    let strategy = barsql_sql::ExplainStrategy::Query("EXPLAIN QUERY PLAN SELECT * FROM t".into());
    assert!(!tab.plan_rows(&strategy, &Cancel::new()).await.unwrap().rows.is_empty());
    let next = tokio::time::timeout(std::time::Duration::from_secs(5), exec(&engine, "SELECT 1")).await;
    assert!(next.is_ok(), "the plan kept the connection");
}

#[tokio::test]
async fn a_failed_plan_leaves_the_shared_connection_clean() {
    let dir = tempfile::tempdir().unwrap();
    let engine = engine(&dir.path().join("plan.db"), false).await;
    let steps = [
        "PRAGMA foreign_keys = OFF",
        "BEGIN",
        "CREATE TABLE t2 (a)",
        "INSERT INTO missing VALUES (1)",
        "COMMIT",
        "PRAGMA foreign_keys = ON",
    ]
    .map(Step::sql);
    let mut session = engine.session().await.unwrap();
    assert!(barsql_db::structure::apply(&mut session, &steps, &Cancel::new()).await.is_err());
    drop(session);

    let mut session = engine.session().await.unwrap();
    let keys = session.buffered("PRAGMA foreign_keys", &Cancel::new()).await.unwrap();
    assert_eq!(keys.values(), [[Value::Int(1)]]);
    let created = session.buffered("SELECT name FROM sqlite_master WHERE name = 't2'", &Cancel::new()).await.unwrap();
    assert_eq!(created.rows(), 0, "the CREATE was rolled back");
    session.buffered("BEGIN", &Cancel::new()).await.expect("no transaction left open");
    session.buffered("COMMIT", &Cancel::new()).await.unwrap();
}

#[tokio::test]
async fn a_check_step_with_rows_fails_the_plan() {
    let dir = tempfile::tempdir().unwrap();
    let engine = engine(&dir.path().join("check.db"), false).await;
    let mut session = engine.session().await.unwrap();
    let steps = [Step { sql: "SELECT 1".into(), expect_no_rows: true }];
    assert!(barsql_db::structure::apply(&mut session, &steps, &Cancel::new()).await.is_err());
}
