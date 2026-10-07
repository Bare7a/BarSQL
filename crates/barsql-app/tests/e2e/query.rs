use barsql_app::RunEvent;
use barsql_core::{TableDataRequest, Value};
use barsql_io::ExportFormat;

use crate::harness::{E2e, Kind, meta_columns, run, unique_table};
use crate::support::{results, rows, text};

each_engine!(async fn query_lifecycle(e) {
    let table = unique_table("widgets");
    e.create_temp_table(&e.auto_pk_table(&table), &table).await;
    let t = e.qualified(&table);
    let inserted = e.query(&format!("INSERT INTO {t} (name) VALUES ('alpha')")).await;
    assert_eq!(inserted.affected_rows, 1);
    e.exec(&format!("INSERT INTO {t} (name) VALUES ('beta')")).await;

    let selected = e.query(&format!("SELECT id, name FROM {t} ORDER BY id")).await;
    assert_eq!(selected.row_count, 2, "{:?}", selected.rows);
    assert_eq!(selected.columns, ["id", "name"]);
    assert!(selected.column_types.len() == 2 && !selected.column_types[0].is_empty(), "{:?}", selected.column_types);
    assert_eq!([&selected.rows[0][1], &selected.rows[1][1]], [&Value::from("alpha"), &Value::from("beta")]);

    // ClickHouse changes rows through mutations, which this doesn't cover.
    if e.is_clickhouse() {
        return;
    }
    let updated = e.query(&format!("UPDATE {t} SET name = 'ALPHA' WHERE name = 'alpha'")).await;
    assert_eq!(updated.affected_rows, 1);
    let deleted = e.query(&format!("DELETE FROM {t} WHERE name = 'beta'")).await;
    assert_eq!(deleted.affected_rows, 1);
    assert_eq!(e.query(&format!("SELECT name FROM {t}")).await.rows, [[Value::from("ALPHA")]]);
});

// An empty result still carries its columns, so the grid renders the header.
each_engine!(async fn empty_result_set(e) {
    let table = unique_table("empty");
    e.create_temp_table(&e.auto_pk_table(&table), &table).await;
    let res = e.query(&format!("SELECT id, name FROM {} WHERE 1 = 0", e.qualified(&table))).await;
    assert_eq!((res.row_count, res.rows.len(), res.columns.len()), (0, 0, 2));
});

each_engine!(async fn error_surfacing(e) {
    assert!(e.try_query("SELECT * FROM definitely_not_a_real_table_xyz").await.is_err());
    assert!(e.try_query("SELCT 1").await.is_err());
});

each_engine!(async fn value_normalization(e) {
    // 2^53 + 1, the first integer a double cannot hold.
    let res = e.query("SELECT 'hi' AS t, 9007199254740993 AS big, NULL AS n").await;
    assert_eq!(res.rows[0], [Value::from("hi"), Value::from("9007199254740993"), Value::Null]);

    // SQLite only reads text as a time in a column declared as one.
    let lite = unique_table("values");
    let sql = match e.kind {
        Kind::Postgres => {
            r#"SELECT '2021-06-07 08:09:10'::timestamp AS ts, decode('deadbeef','hex') AS b, '{"k": 1}'::jsonb AS j"#
                .to_string()
        }
        Kind::MySql | Kind::MariaDb => "SELECT CAST('2021-06-07 08:09:10' AS DATETIME) AS ts, UNHEX('deadbeef') AS b".into(),
        Kind::ClickHouse => "SELECT toDateTime('2021-06-07 08:09:10', 'UTC') AS ts, unhex('deadbeef') AS b".into(),
        Kind::SqlServer => "SELECT CAST('2021-06-07 08:09:10' AS datetime2) AS ts, 0xDEADBEEF AS b".into(),
        Kind::Turso => {
            e.create_temp_table(&format!("CREATE TABLE {lite} (ts DATETIME, b BLOB)"), &lite).await;
            e.exec(&format!("INSERT INTO {lite} VALUES ('2021-06-07 08:09:10', X'deadbeef')")).await;
            format!("SELECT ts, b FROM {lite}")
        }
    };
    let row = e.query(&sql).await.rows.remove(0);
    assert!(text(&row[0]).starts_with("2021-06-07T08:09:10"), "timestamps are RFC 3339: {row:?}");
    assert_eq!(row[1], Value::from(r"\xdeadbeef"), "binary is hex-encoded");
    if e.is_postgres() {
        assert!(text(&row[2]).contains("\"k\""), "jsonb comes back as JSON text: {row:?}");
    }
});

// RETURNING is Postgres-only. MySQL is covered by insert_row's re-select.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn returning_clause() {
    run(Kind::Postgres, |e| async move {
        let table = unique_table("ret");
        e.create_temp_table(&e.auto_pk_table(&table), &table).await;
        let res =
            e.query(&format!("INSERT INTO {} (name) VALUES ('made') RETURNING id, name", e.qualified(&table))).await;
        assert_eq!(res.row_count, 1, "{:?}", res.rows);
        assert_eq!(res.rows[0][1], Value::from("made"));
    })
    .await
}

// SQLite types only declared columns, so the series goes into a table first.
async fn series_sql(e: &E2e, rows: u32) -> String {
    if e.is_postgres() {
        return format!("SELECT g AS id, 'r' || g AS name FROM generate_series(1, {rows}) AS g");
    }
    if e.is_lite() {
        let table = unique_table("series");
        e.create_temp_table(&format!("CREATE TABLE {table} (id INTEGER, name TEXT)"), &table).await;
        e.exec(&format!(
            "INSERT INTO {table} WITH RECURSIVE g(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM g WHERE n < {rows}) \
             SELECT n, 'r' || n FROM g"
        ))
        .await;
        return format!("SELECT id, name FROM {table} ORDER BY id");
    }
    let digits = (0..10).map(|d| format!("SELECT {d} AS d")).collect::<Vec<_>>().join(" UNION ALL ");
    let (mut terms, mut joins, mut scale, mut n) = (Vec::new(), Vec::new(), 1, 0);
    while scale < rows {
        terms.push(format!("{scale} * t{n}.d"));
        joins.push(format!("({digits}) AS t{n}"));
        (scale, n) = (scale * 10, n + 1);
    }
    format!(
        "SELECT n AS id, CONCAT('r', n) AS name FROM (SELECT {} + 1 AS n FROM {}) AS s WHERE n <= {rows} ORDER BY n",
        terms.join(" + "),
        joins.join(" CROSS JOIN ")
    )
}

each_engine!(async fn stream_batching(e) {
    let events = e.run_on_tab("tab-stream", &series_sql(&e, 12_000).await).await;
    let meta = events.iter().position(|ev| matches!(ev, RunEvent::Meta { .. })).expect("column metadata");
    let first_rows = events.iter().position(|ev| matches!(ev, RunEvent::Rows { .. })).expect("rows");
    assert!(meta < first_rows, "column metadata must arrive before the rows");
    let batches = events.iter().filter(|ev| matches!(ev, RunEvent::Rows { .. })).count();
    assert!(batches >= 2, "expected several batches, got {batches}");
    assert_eq!(rows(&events, 0).len(), 12_000);
    assert_eq!(results(&events)[0].summary.as_ref().map(|s| s.row_count), Some(12_000));
    let RunEvent::Meta { columns, .. } = &events[meta] else { unreachable!() };
    assert_eq!(columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["id", "name"]);
    assert!(columns.iter().all(|c| !c.type_name.is_empty()), "column types must be populated: {columns:?}");
});

// Primary keys in the summary are what make the grid editable.
each_engine!(async fn query_table_stream(e) {
    let table = unique_table("tstream");
    e.create_temp_table(&e.auto_pk_table(&table), &table).await;
    for i in 0..7 {
        e.exec(&format!("INSERT INTO {} (name) VALUES ('n{i}')", e.qualified(&table))).await;
    }
    let req = TableDataRequest {
        schema: e.schema(),
        table: table.clone(),
        limit: 100,
        order_by: "id".into(),
        order_dir: "ASC".into(),
        ..Default::default()
    };
    let page = e.table_page(req).await.unwrap();
    assert_eq!((page.summary.row_count, page.rows.len()), (7, 7));
    let keys: &[&str] = if e.driver().capabilities().row_editing { &["id"] } else { &[] };
    assert_eq!(page.summary.primary_keys, keys);
});

each_engine!(async fn multi_statement_script(e) {
    let events = e.run_on_tab("tab-script", "SELECT 1 AS a; SELECT 'x' AS b, 'y' AS c").await;
    let results = results(&events);
    assert_eq!(results.len(), 2, "one result set per statement");
    assert!(results.iter().all(|r| r.error.is_none()), "{results:?}");
    assert_eq!(meta_columns(&events, 0), ["a"]);
    assert_eq!(meta_columns(&events, 1), ["b", "c"]);
});

each_engine!(async fn script_stops_on_error(e) {
    let events = e.run_on_tab("tab-stop", "SELECT 1; SELECT * FROM missing_table_zzz; SELECT 2").await;
    let results = results(&events);
    assert_eq!(results.len(), 2, "the statement after the failure must not run");
    assert!(results[0].error.is_none(), "{:?}", results[0].error);
    assert!(results[1].error.is_some(), "the failing statement reports its error");
});

each_engine!(async fn query_history(e) {
    e.app.clear_query_history(&e.id).unwrap();
    e.exec("SELECT 1").await;
    assert!(e.try_query("SELECT * FROM nope_history_table").await.is_err());
    let entries = e.app.query_history(&e.id, 10);
    assert!(entries.len() >= 2, "{entries:?}");
    assert!(entries.iter().any(|h| h.sql == "SELECT 1" && h.success), "{entries:?}");
    assert!(
        entries.iter().any(|h| h.sql.contains("nope_history_table") && !h.success && !h.error.is_empty()),
        "a failed query is recorded as unsuccessful with its error: {entries:?}"
    );
    e.app.clear_query_history(&e.id).unwrap();
    assert!(e.app.query_history(&e.id, 10).is_empty());
});

each_engine!(async fn export(e) {
    let table = unique_table("export");
    e.create_temp_table(&e.auto_pk_table(&table), &table).await;
    for name in ["alice", "bob"] {
        e.exec(&format!("INSERT INTO {} (name) VALUES ('{name}')", e.qualified(&table))).await;
    }
    let result = e.stream(&format!("SELECT id, name FROM {} ORDER BY id", e.qualified(&table))).await;
    let cases: [(&str, &[&str]); 4] = [
        ("csv", &["id,name", "alice", "bob"]),
        ("json", &[r#""name": "alice""#, r#""name": "bob""#]),
        ("markdown", &["| id | name |", "| --- | --- |", "alice"]),
        ("sql", &["INSERT INTO", "'alice'", "'bob'"]),
    ];
    for (format, wants) in cases {
        let out = result.export(ExportFormat::parse(format).unwrap());
        for want in wants {
            assert!(out.contains(want), "{format} export is missing {want:?}:\n{out}");
        }
    }
    assert!(ExportFormat::parse("not-a-format").is_none());
});
