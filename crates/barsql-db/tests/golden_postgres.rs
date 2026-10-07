#![cfg(feature = "e2e")]

mod common;

use std::time::Duration;

use barsql_core::{DriverType, FunctionKind};
use barsql_db::{Cancel, Engine};
use common::{Report, assert_matches, compare_probes, fixture, postgres_config, run_all, schema_snapshot, strings};
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

    let values = fixture("values/postgres.json");
    let utc = connect(TimeZone::UTC).await;
    let mut session = utc.session().await.unwrap();
    run_all(&mut session, &strings(&values["setup"])).await;
    compare_probes(&mut session, &pg, &values["probes"], "values", DEVIATIONS, &mut report).await;

    let plus3 = connect(TimeZone::fixed(Offset::from_seconds(3 * 3600).unwrap())).await;
    let mut local = plus3.session().await.unwrap();
    compare_probes(&mut local, &pg, &values["localProbes"], "local", DEVIATIONS, &mut report).await;

    let errors = fixture("errors/postgres.json");
    run_all(&mut session, &strings(&errors["setup"])).await;
    compare_probes(&mut session, &pg, &errors["probes"], "errors", DEVIATIONS, &mut report).await;
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
