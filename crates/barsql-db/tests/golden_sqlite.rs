mod common;

use barsql_core::DriverType;
use barsql_db::Engine;
use common::{Report, assert_matches, compare_probes, fixture, run_all, schema_snapshot, sqlite_config, strings};

// A trigger body runs as part of its CREATE TRIGGER instead of being split at its first `;`.
const DEVIATIONS: &[&str] = &["trigger_body_split"];

async fn fresh(dir: &tempfile::TempDir, name: &str) -> Engine {
    Engine::connect(&sqlite_config(&dir.path().join(name))).await.expect("open sqlite")
}

#[tokio::test]
async fn sqlite_values_and_errors_match_the_fixtures() {
    let _serial = common::serial().await;
    let dir = tempfile::tempdir().unwrap();
    let lite = DriverType::Sqlite;
    let mut report = Report::default();

    let values = fixture("values/sqlite.json");
    let engine = fresh(&dir, "values.db").await;
    let mut session = engine.session().await.unwrap();
    run_all(&mut session, &strings(&values["setup"])).await;
    compare_probes(&mut session, &lite, &values["probes"], "values", DEVIATIONS, &mut report).await;
    compare_probes(&mut session, &lite, &values["localProbes"], "local", DEVIATIONS, &mut report).await;

    let errors = fixture("errors/sqlite.json");
    let engine = fresh(&dir, "errors.db").await;
    let mut session = engine.session().await.unwrap();
    run_all(&mut session, &strings(&errors["setup"])).await;
    compare_probes(&mut session, &lite, &errors["probes"], "errors", DEVIATIONS, &mut report).await;
    report.assert_clean("sqlite");
}

#[tokio::test]
async fn sqlite_schema_and_ddl_match_the_fixtures() {
    let _serial = common::serial().await;
    let dir = tempfile::tempdir().unwrap();
    let expected = fixture("schema/sqlite.json");
    let engine = fresh(&dir, "schema.db").await;
    let mut session = engine.session().await.unwrap();
    run_all(&mut session, &strings(&expected["setup"])).await;
    drop(session);
    let actual = schema_snapshot(&engine, expected["schema"].as_str().unwrap(), &expected).await;
    assert_matches(&format!("schema/{}.json", expected["engine"].as_str().unwrap()), &expected, &actual);
}
