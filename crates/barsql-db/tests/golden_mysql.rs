#![cfg(feature = "e2e")]

mod common;

use std::time::{Duration, Instant};

use barsql_core::DriverType;
use barsql_db::{Cancel, Engine};
use common::{Report, assert_matches, compare_probes, fixture, mysql_config, run_all, schema_snapshot, strings};

const DEVIATIONS: &[&str] = &[
    // A CALL with no result set reports rows affected, not an empty result set.
    "call",
];

async fn connect(prefix: &str, port: &str) -> Engine {
    Engine::connect(&mysql_config(prefix, port)).await.unwrap_or_else(|err| {
        panic!("{prefix} is not reachable ({err:?}); bring the stack up with `cargo xtask e2e up`")
    })
}

async fn check_engine(engine_name: &str, prefix: &str, port: &str) {
    let engine = connect(prefix, port).await;
    let mut session = engine.session().await.unwrap();
    let mut report = Report::default();
    let my = DriverType::MySql;
    let values = fixture(&format!("values/{engine_name}.json"));
    run_all(&mut session, &strings(&values["setup"])).await;
    compare_probes(&mut session, &my, &values["probes"], "values", DEVIATIONS, &mut report).await;
    compare_probes(&mut session, &my, &values["localProbes"], "local", DEVIATIONS, &mut report).await;
    let errors = fixture(&format!("errors/{engine_name}.json"));
    run_all(&mut session, &strings(&errors["setup"])).await;
    compare_probes(&mut session, &my, &errors["probes"], "errors", DEVIATIONS, &mut report).await;
    report.assert_clean(engine_name);

    let expected = fixture(&format!("schema/{engine_name}.json"));
    run_all(&mut session, &strings(&expected["setup"])).await;
    let actual = schema_snapshot(&engine, expected["schema"].as_str().unwrap(), &expected).await;
    assert_matches(&format!("schema/{}.json", expected["engine"].as_str().unwrap()), &expected, &actual);
}

#[tokio::test]
async fn mysql_matches_the_fixtures() {
    let _serial = common::serial().await;
    check_engine("mysql", "MYSQL", "33306").await;
}

#[tokio::test]
async fn mariadb_matches_the_fixtures() {
    let _serial = common::serial().await;
    check_engine("mariadb", "MARIADB", "33307").await;
}

#[tokio::test]
async fn kill_query_cancels_without_dropping_the_session() {
    let _serial = common::serial().await;
    let engine = connect("MYSQL", "33306").await;
    let mut session = engine.session().await.unwrap();
    let cancel = Cancel::new();
    let trigger = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        trigger.cancel();
    });
    let started = Instant::now();
    let (tx, rx) = async_channel::unbounded();
    let result = session.run_script(&["SELECT SLEEP(30) AS slept".into()], &tx, &cancel).await;
    drop(tx);
    assert!(started.elapsed() < Duration::from_secs(10), "the kill must interrupt the sleep");
    // SLEEP returns 1 when interrupted. Other statements fail with 1317.
    let interrupted = match result {
        Ok(_) => {
            let mut slept = None;
            while let Ok(event) = rx.try_recv() {
                if let barsql_db::ScriptEvent::Rows { chunk, .. } = event {
                    slept = chunk.display(0, 0).map(str::to_string);
                }
            }
            slept.as_deref() == Some("1")
        }
        Err(err) => err.code == "1317" && err.cancelled,
    };
    assert!(interrupted);
    let after = session.buffered("SELECT 2 AS still_connected", &Cancel::new()).await.unwrap();
    assert_eq!(after.text(0, 0), Some("2"));
}

#[tokio::test]
async fn mariadb_ed25519_login_works() {
    let _serial = common::serial().await;
    let admin = connect("MARIADB", "33307").await;
    let mut session = admin.session().await.unwrap();
    for statement in [
        "INSTALL SONAME 'auth_ed25519'",
        "DROP USER IF EXISTS 'barsql_ed'@'%'",
        "CREATE USER 'barsql_ed'@'%' IDENTIFIED VIA ed25519 USING PASSWORD('ed-secret')",
        "GRANT SELECT ON *.* TO 'barsql_ed'@'%'",
    ] {
        if let Err(err) = session.buffered(statement, &Cancel::new()).await {
            assert_eq!(err.code, "1968", "{statement}: {err:?}");
        }
    }
    let mut cfg = mysql_config("MARIADB", "33307");
    cfg.username = "barsql_ed".into();
    cfg.password = "ed-secret".into();
    let ed = Engine::connect(&cfg).await.expect("ed25519 login");
    let plugin = ed
        .session()
        .await
        .unwrap()
        .buffered("SELECT plugin FROM mysql.user WHERE user = 'barsql_ed'", &Cancel::new())
        .await
        .unwrap();
    assert_eq!(plugin.text(0, 0), Some("ed25519"));
}
