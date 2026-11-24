#![cfg(feature = "e2e")]

mod common;

use std::time::Duration;

use barsql_core::{DriverType, FunctionKind};
use barsql_db::{Cancel, Engine};
use common::{
    Report, assert_matches, check_probes, column_values, fixture, golden_write, postgres_config, run_all,
    schema_snapshot, strings, write_fixture,
};
use jiff::tz::{Offset, TimeZone};

// Result sets come from the protocol, so SHOW returns its row. A CALL without OUT parameters reports rows
// affected instead of an empty result set.
const DEVIATIONS: &[&str] = &["show", "call"];

async fn connect(zone: TimeZone) -> Engine {
    Engine::connect_in_zone(&postgres_config(), zone)
        .await
        .expect("Postgres is not reachable; bring the stack up with `cargo xtask e2e up`")
}

#[tokio::test]
async fn postgres_reports_its_server_version() {
    let info = connect(TimeZone::UTC).await.connection_info().await.unwrap();
    assert!(info.version.starts_with(|c: char| c.is_ascii_digit()), "{}", info.version);
}

// reltuples counts after ANALYZE, and the size takes in the indexes too. fresh_probe is too small for autovacuum to
// analyze, and its size waits for no lock. A REINDEX locks just the indexes, and leaves an empty table's reltuples
// at -1.
#[tokio::test]
async fn postgres_table_sizes_come_from_the_catalog() {
    let _serial = common::serial().await;
    let engine = connect(TimeZone::UTC).await;
    let mut session = engine.session().await.unwrap();
    let setup = [
        "DROP TABLE IF EXISTS sized_probe, fresh_probe, empty_probe",
        "CREATE TABLE sized_probe (id int PRIMARY KEY)",
        "INSERT INTO sized_probe SELECT generate_series(1, 100)",
        "ANALYZE sized_probe",
        "CREATE TABLE fresh_probe (id int PRIMARY KEY)",
        "INSERT INTO fresh_probe VALUES (1), (2), (3)",
        "CREATE TABLE empty_probe (id int PRIMARY KEY)",
    ];
    run_all(&mut session, &setup.map(String::from)).await;
    let sizes = engine.table_sizes("public").await.unwrap();
    let mut locker = engine.session().await.unwrap();
    run_all(&mut locker, &["BEGIN".into(), "LOCK TABLE fresh_probe IN ACCESS EXCLUSIVE MODE".into()]).await;
    let locked = tokio::time::timeout(Duration::from_secs(10), engine.table_sizes("public")).await;
    run_all(&mut locker, &["ROLLBACK".into()]).await;
    run_all(&mut locker, &["BEGIN".into(), "REINDEX TABLE empty_probe".into()]).await;
    let reindexing = tokio::time::timeout(Duration::from_secs(10), engine.table_sizes("public")).await;
    run_all(&mut locker, &["ROLLBACK".into()]).await;
    run_all(&mut session, &["DROP TABLE sized_probe, fresh_probe, empty_probe".to_string()]).await;
    let sized = sizes["sized_probe"];
    assert_eq!(sized.rows, Some(100));
    assert!(sized.bytes.is_some_and(|bytes| bytes > 8192), "{sized:?}");
    let fresh = sizes["fresh_probe"];
    assert!(fresh.rows.is_some() && fresh.bytes.is_some_and(|bytes| bytes > 0), "{fresh:?}");
    let locked = locked.expect("no wait behind the lock").unwrap()["fresh_probe"];
    assert!(locked.rows.is_some() && locked.bytes.is_none(), "{locked:?}");
    let reindexing = reindexing.expect("no wait behind the REINDEX").unwrap();
    let (empty, fresh) = (reindexing["empty_probe"], reindexing["fresh_probe"]);
    assert!(empty.rows.is_some() && empty.bytes.is_none() && fresh.bytes.is_some(), "{reindexing:?}");
}

// Postgres copies ref_parts' key to each of its partitions, and it's still listed once.
#[tokio::test]
async fn postgres_keys_pointing_at_a_table_come_from_the_catalog() {
    let _serial = common::serial().await;
    let engine = connect(TimeZone::UTC).await;
    let mut session = engine.session().await.unwrap();
    run_all(&mut session, &["DROP TABLE IF EXISTS ref_lines, ref_parts, ref_orders, ref_users".into()]).await;
    run_all(&mut session, &common::REF_TABLES.map(String::from)).await;
    let parts = [
        "CREATE TABLE ref_parts (order_id int, order_rev int,
            FOREIGN KEY (order_id, order_rev) REFERENCES ref_orders (id, rev)) PARTITION BY LIST (order_rev)",
        "CREATE TABLE ref_parts_1 PARTITION OF ref_parts FOR VALUES IN (1)",
        "CREATE TABLE ref_parts_2 PARTITION OF ref_parts FOR VALUES IN (2)",
    ];
    run_all(&mut session, &parts.map(String::from)).await;
    let keys = engine.referencing_keys("public", "ref_orders").await.unwrap();
    let columns = engine.list_columns("public", "ref_lines").await.unwrap();
    run_all(&mut session, &["DROP TABLE ref_lines, ref_parts, ref_orders, ref_users".to_string()]).await;
    common::assert_ref_keys("postgres", &keys, &columns, "public", &["ref_lines", "ref_parts"]);
}

#[tokio::test]
async fn postgres_values_and_errors_match_the_fixtures() {
    let _serial = common::serial().await;
    let mut report = Report::default();
    let pg = DriverType::Postgres;

    let mut values = fixture("values/postgres.json");
    let utc = connect(TimeZone::UTC).await;
    let mut session = utc.session().await.unwrap();
    run_all(&mut session, &strings(&values["setup"])).await;
    check_probes(&mut session, &pg, &mut values, "probes", "values", DEVIATIONS, &mut report).await;

    let plus3 = connect(TimeZone::fixed(Offset::from_seconds(3 * 3600).unwrap())).await;
    let mut local = plus3.session().await.unwrap();
    check_probes(&mut local, &pg, &mut values, "localProbes", "local", DEVIATIONS, &mut report).await;

    let mut errors = fixture("errors/postgres.json");
    run_all(&mut session, &strings(&errors["setup"])).await;
    check_probes(&mut session, &pg, &mut errors, "probes", "errors", DEVIATIONS, &mut report).await;
    if golden_write() {
        write_fixture("values/postgres.json", &values);
        write_fixture("errors/postgres.json", &errors);
    }
    report.assert_clean("postgres");
}

#[tokio::test]
async fn postgres_schema_and_ddl_match_the_fixtures() {
    let _serial = common::serial().await;
    let expected = fixture("schema/postgres.json");
    let engine = connect(TimeZone::UTC).await;
    let mut session = engine.session().await.unwrap();
    run_all(&mut session, &strings(&expected["setup"])).await;
    let actual = schema_snapshot(&engine, expected["schema"].as_str().unwrap(), &expected).await;
    assert_matches(&format!("schema/{}.json", expected["engine"].as_str().unwrap()), &expected, &actual);
}

// An enum gives its labels in their order. Otherwise a check on just that column can list the values.
#[tokio::test]
async fn an_enum_or_a_check_list_gives_a_columns_values() {
    let _serial = common::serial().await;
    let engine = connect(TimeZone::UTC).await;
    let mut session = engine.session().await.unwrap();
    let setup = [
        "DROP TABLE IF EXISTS public.enum_probe",
        "DROP TYPE IF EXISTS public.enum_probe_mood",
        "CREATE TYPE public.enum_probe_mood AS ENUM ('sad', 'happy')",
        "ALTER TYPE public.enum_probe_mood ADD VALUE 'meh' BEFORE 'happy'",
        "CREATE TABLE public.enum_probe (mood public.enum_probe_mood, kind varchar(10) CHECK (kind IN ('like', 'love')),
            stars int, note text CHECK (length(note) < 100), CONSTRAINT star_list CHECK (stars IN (1, 2)),
            CONSTRAINT pair CHECK (stars < 3 OR kind = 'love'))",
    ];
    run_all(&mut session, &setup.map(String::from)).await;
    let columns = engine.list_columns("public", "enum_probe").await.unwrap();
    assert_eq!(column_values(&columns), ["mood: sad, meh, happy", "kind: like, love", "stars: 1, 2", "note: "]);
    run_all(&mut session, &["DROP TABLE public.enum_probe".into(), "DROP TYPE public.enum_probe_mood".into()]).await;
}

// Built-ins come with every overload. Functions that only back operators, casts, types, aggregates and index
// methods aren't callable as written, so they stay out.
#[tokio::test]
async fn postgres_lists_callable_functions() {
    let _serial = common::serial().await;
    let engine = connect(TimeZone::UTC).await;
    let started = std::time::Instant::now();
    let list = engine.list_functions().await.unwrap();
    let elapsed = started.elapsed();
    let find = |name: &str| list.functions.iter().find(|f| f.schema == "pg_catalog" && f.name == name);
    let kind = |name: &str| find(name).map(|f| f.kind);
    assert_eq!(kind("jsonb_build_object"), Some(FunctionKind::Scalar));
    assert_eq!(kind("count"), Some(FunctionKind::Aggregate));
    assert_eq!(kind("row_number"), Some(FunctionKind::Window));
    assert_eq!(kind("rank"), Some(FunctionKind::Window), "a window function and a hypothetical-set aggregate");
    assert_eq!(kind("generate_series"), Some(FunctionKind::Table));
    let series = find("generate_series").unwrap();
    assert!(series.builtin && !series.qualified_only && series.signatures.len() > 2, "{series:?}");
    for hidden in ["int4pl", "array_in", "float8_accum", "btint4cmp", "eqsel", "plpgsql_call_handler", "textin"] {
        assert!(find(hidden).is_none(), "{hidden} isn't callable as written");
    }
    let names: std::collections::HashSet<_> = list.functions.iter().map(|f| (&f.schema, &f.name)).collect();
    assert_eq!(names.len(), list.functions.len(), "overloads fold into one entry");
    assert!(elapsed < Duration::from_secs(2), "{} functions took {elapsed:?}", list.functions.len());
}

// Only the pg_ prefix is reserved for system schemas, so one like pg-boss's is the user's.
#[tokio::test]
async fn schemas_named_like_pg_but_not_pg_underscore_are_listed() {
    let engine = connect(TimeZone::UTC).await;
    let mut session = engine.session().await.unwrap();
    run_all(&mut session, &["DROP SCHEMA IF EXISTS pgprobe".into(), "CREATE SCHEMA pgprobe".into()]).await;
    let listed: Vec<String> = engine.list_schemas().await.unwrap().into_iter().map(|s| s.name).collect();
    run_all(&mut session, &["DROP SCHEMA pgprobe".into()]).await;
    assert!(listed.iter().any(|name| name == "pgprobe"), "{listed:?}");
    assert!(!listed.iter().any(|name| name.starts_with("pg_") || name == "information_schema"), "{listed:?}");
}

#[tokio::test]
async fn a_cancel_keeps_the_rows_already_fetched() {
    let _serial = common::serial().await;
    let engine = connect(TimeZone::UTC).await;
    let mut session = engine.session().await.unwrap();
    let slow = "SELECT i, pg_sleep(0.001) AS slept FROM generate_series(1, 100000) AS i";
    let (error, events, _) = common::cancel_after(&mut session, slow, Duration::from_millis(500)).await;
    assert_eq!(error.code, "57014");
    let (sent, counted) = common::rows_and_count(&events);
    assert!(sent > 0, "the cancel came before the first row");
    assert_eq!(counted, Some(sent as i64));
}

#[tokio::test]
async fn cancel_keeps_the_session_and_the_tab_transaction() {
    let _serial = common::serial().await;
    let engine = connect(TimeZone::UTC).await;
    let mut session = engine.session().await.unwrap();
    session.begin().await.unwrap();
    run_all(
        &mut session,
        &["CREATE TEMP TABLE cancel_probe (id int)".into(), "INSERT INTO cancel_probe VALUES (1)".into()],
    )
    .await;

    let cancel = Cancel::new();
    let trigger = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        trigger.cancel();
    });
    let (tx, rx) = async_channel::unbounded();
    let error = session.run_script(&["SELECT pg_sleep(30)".into()], &tx, &cancel).await.expect_err("cancelled");
    drop(tx);
    assert_eq!(error.code, "57014");
    assert!(error.cancelled);
    assert!(rx.try_recv().is_ok());

    assert!(session.in_transaction(), "the cancel must not end the transaction");
    let kept = session.buffered("SELECT id FROM cancel_probe", &Cancel::new()).await.unwrap();
    assert_eq!(kept.text(0, 0), Some("1"));
    session.rollback().await.unwrap();
    assert!(!session.in_transaction());
}

#[tokio::test]
async fn postgres_structure_and_edit_match_the_fixture() {
    let _serial = common::serial().await;
    let engine = Engine::connect(&postgres_config()).await.expect("postgres is not reachable");
    common::structure_fixture_test(&engine, "structure/postgres.json").await;
}
