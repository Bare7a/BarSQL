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
