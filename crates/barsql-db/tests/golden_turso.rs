#![cfg(feature = "e2e")]

mod common;

use barsql_core::DriverType;
use barsql_db::{Cancel, Engine};
use common::{
    Report, assert_matches, compare_probes, fixture, golden_write, run_all, schema_snapshot, snapshot_probes, strings,
    turso_config, write_fixture,
};
use serde_json::json;

const TURSO: DriverType = DriverType::Turso;

async fn connect() -> Engine {
    Engine::connect(&turso_config()).await.expect("sqld is not reachable; bring the stack up with `cargo xtask e2e up`")
}

// One database serves every test, so each starts by dropping what the last one made.
async fn reset(engine: &Engine) {
    let mut session = engine.session().await.unwrap();
    let objects = session
        .buffered(
            "SELECT type, name FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' AND type IN ('view', 'trigger', 'table')
            ORDER BY type = 'table'",
            &Cancel::new(),
        )
        .await
        .unwrap();
    let mut statements = vec!["PRAGMA foreign_keys = OFF".to_string()];
    for row in 0..objects.rows() {
        let (kind, name) = (objects.text(row, 0).unwrap_or_default(), objects.text(row, 1).unwrap_or_default());
        statements.push(format!("DROP {} IF EXISTS \"{}\"", kind.to_uppercase(), name.replace('"', "\"\"")));
    }
    statements.push("PRAGMA foreign_keys = ON".into());
    run_all(&mut session, &statements).await;
}

// Turso runs SQLite, so its fixtures reuse SQLite's setup and probes with what the server answers.
#[tokio::test]
async fn turso_values_and_errors_match_the_fixtures() {
    let _serial = common::serial().await;
    let engine = connect().await;
    let mut report = Report::default();
    for (kind, keys) in [("values", &["probes", "localProbes"][..]), ("errors", &["probes"][..])] {
        let template = fixture(&format!("{kind}/sqlite.json"));
        reset(&engine).await;
        let mut session = engine.session().await.unwrap();
        run_all(&mut session, &strings(&template["setup"])).await;
        let rel = format!("{kind}/turso.json");
        if golden_write() {
            let mut out = json!({ "engine": "turso", "setup": template["setup"] });
            for key in keys {
                out[*key] = snapshot_probes(&mut session, &TURSO, &template[*key]).await;
            }
            write_fixture(&rel, &out);
            continue;
        }
        let expected = fixture(&rel);
        for key in keys {
            compare_probes(&mut session, &TURSO, &expected[*key], kind, &[], &mut report).await;
        }
    }
    report.assert_clean("turso");
}

#[tokio::test]
async fn turso_schema_and_ddl_match_the_fixtures() {
    let _serial = common::serial().await;
    let engine = connect().await;
    reset(&engine).await;
    let template = fixture("schema/sqlite.json");
    let mut session = engine.session().await.unwrap();
    run_all(&mut session, &strings(&template["setup"])).await;
    drop(session);
    let mut actual = schema_snapshot(&engine, "main", &template).await;
    actual["engine"] = json!("turso");
    if golden_write() {
        write_fixture("schema/turso.json", &actual);
        return;
    }
    assert_matches("schema/turso.json", &fixture("schema/turso.json"), &actual);
}
