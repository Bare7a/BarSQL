#![cfg(feature = "e2e")]

mod common;

use std::time::{Duration, Instant};

use barsql_core::DriverType;
use barsql_db::{Cancel, Engine, QueryError, Session};
use common::{
    Report, assert_matches, check_probes, fixture, golden_write, mysql_config, run_all, schema_snapshot, strings,
    write_fixture,
};

const DEVIATIONS: &[&str] = &[
    // A CALL with no result set reports rows affected, not an empty result set.
    "call",
];

const SERVERS: [(&str, &str); 2] = [("MYSQL", "33306"), ("MARIADB", "33307")];

async fn connect(prefix: &str, port: &str) -> Engine {
    Engine::connect(&mysql_config(prefix, port)).await.unwrap_or_else(|err| {
        panic!("{prefix} is not reachable ({err:?}); bring the stack up with `cargo xtask e2e up`")
    })
}

async fn text(session: &mut Session, sql: &str) -> String {
    let res = session.buffered(sql, &Cancel::new()).await.unwrap_or_else(|err| panic!("{sql}: {err:?}"));
    res.text(0, 0).unwrap_or_default().to_string()
}

async fn rows(session: &mut Session, sql: &str) -> Vec<Vec<String>> {
    let res = session.buffered(sql, &Cancel::new()).await.unwrap_or_else(|err| panic!("{sql}: {err:?}"));
    (0..res.rows())
        .map(|r| (0..res.columns.len()).map(|c| res.text(r, c).unwrap_or_default().into()).collect())
        .collect()
}

// Runs one statement as a script, as a tab does, and returns its error.
async fn script_error(session: &mut Session, sql: &str) -> Option<QueryError> {
    common::run_error(session, &[sql.into()]).await
}

// A session on a connection the pool has reset once, opened while the server's own zone was `zone`. The server's
// zone is put back before this returns.
async fn session_in_server_zone(prefix: &str, port: &str, zone: &str) -> (Engine, Session) {
    let admin = connect(prefix, port).await;
    let mut server = admin.session().await.unwrap();
    let before = text(&mut server, "SELECT @@global.time_zone").await;
    text(&mut server, &format!("SET GLOBAL time_zone = '{zone}'")).await;
    let opened = async {
        let engine = Engine::connect(&mysql_config(prefix, port)).await?;
        drop(engine.session().await?);
        tokio::time::sleep(Duration::from_millis(200)).await;
        let session = engine.session().await?;
        Ok::<_, QueryError>((engine, session))
    }
    .await;
    text(&mut server, &format!("SET GLOBAL time_zone = '{before}'")).await;
    opened.unwrap_or_else(|err| panic!("{prefix}: {err:?}"))
}

async fn check_engine(engine_name: &str, prefix: &str, port: &str) {
    let engine = connect(prefix, port).await;
    let mut session = engine.session().await.unwrap();
    let mut report = Report::default();
    let my = DriverType::MySql;
    let mut values = fixture(&format!("values/{engine_name}.json"));
    run_all(&mut session, &strings(&values["setup"])).await;
    check_probes(&mut session, &my, &mut values, "probes", "values", DEVIATIONS, &mut report).await;
    // TIMESTAMPs come back in the session's zone, which stays UTC whatever the server's and the OS's are.
    let (_local_engine, mut local) = session_in_server_zone(prefix, port, "+03:00").await;
    check_probes(&mut local, &my, &mut values, "localProbes", "local", DEVIATIONS, &mut report).await;
    let mut errors = fixture(&format!("errors/{engine_name}.json"));
    run_all(&mut session, &strings(&errors["setup"])).await;
    check_probes(&mut session, &my, &mut errors, "probes", "errors", DEVIATIONS, &mut report).await;
    if golden_write() {
        write_fixture(&format!("values/{engine_name}.json"), &values);
        write_fixture(&format!("errors/{engine_name}.json"), &errors);
    }
    report.assert_clean(engine_name);

    let expected = fixture(&format!("schema/{engine_name}.json"));
    run_all(&mut session, &strings(&expected["setup"])).await;
    let actual = schema_snapshot(&engine, expected["schema"].as_str().unwrap(), &expected).await;
    assert_matches(&format!("schema/{}.json", expected["engine"].as_str().unwrap()), &expected, &actual);
}

// An enum gives its labels. Otherwise a check that lists a column's values gives them, read from the DDL since
// CHECK_CONSTRAINTS doesn't say which table a check is on.
#[tokio::test]
async fn an_enum_or_a_check_list_gives_a_columns_values() {
    let _serial = common::serial().await;
    for (prefix, port) in SERVERS {
        let engine = connect(prefix, port).await;
        let mut session = engine.session().await.unwrap();
        let schema = text(&mut session, "SELECT DATABASE()").await;
        let setup = [
            "DROP TABLE IF EXISTS enum_probe",
            "CREATE TABLE enum_probe (mood ENUM('sad', 'it''s', 'happy'), kind VARCHAR(10) CHECK (kind IN ('like', 'love')),
                stars INT, note TEXT, CONSTRAINT star_list CHECK (stars = 1 OR stars = 2),
                CONSTRAINT short_note CHECK (CHAR_LENGTH(note) < 100))",
        ];
        run_all(&mut session, &setup.map(String::from)).await;
        let columns = engine.list_columns(&schema, "enum_probe").await.unwrap();
        assert_eq!(
            common::column_values(&columns),
            ["mood: sad, it's, happy", "kind: like, love", "stars: 1, 2", "note: "],
            "{prefix}"
        );
        // A check written on its column alone still counts as the table's.
        let setup = [
            "DROP TABLE IF EXISTS enum_probe_one",
            "CREATE TABLE enum_probe_one (kind TEXT CHECK (kind IN ('a', 'b')))",
        ];
        run_all(&mut session, &setup.map(String::from)).await;
        let columns = engine.list_columns(&schema, "enum_probe_one").await.unwrap();
        assert_eq!(common::column_values(&columns), ["kind: a, b"], "{prefix}");
        run_all(&mut session, &["DROP TABLE enum_probe".into(), "DROP TABLE enum_probe_one".into()]).await;
    }
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
    assert!(!session.is_broken());
    let after = session.buffered("SELECT 2 AS still_connected", &Cancel::new()).await.unwrap();
    assert_eq!(after.text(0, 0), Some("2"));
}

#[tokio::test]
async fn a_connection_the_server_closed_breaks_the_session() {
    let _serial = common::serial().await;
    for (prefix, port) in SERVERS {
        let engine = connect(prefix, port).await;
        let mut session = engine.session().await.unwrap();
        session.begin().await.unwrap();
        // Idle past this, the server closes the connection: MySQL with an out-of-turn notice, MariaDB without one.
        text(&mut session, "SET SESSION wait_timeout = 1").await;
        tokio::time::sleep(Duration::from_secs(3)).await;
        let error = session.buffered("SELECT 1", &Cancel::new()).await.expect_err("the server closed it");
        assert!(session.is_broken(), "{prefix}: {error:?}");
        assert!(!session.in_transaction(), "{prefix}: the server rolled it back");
        let mut fresh = engine.session().await.unwrap();
        assert_eq!(text(&mut fresh, "SELECT 2").await, "2");
    }
}

#[tokio::test]
async fn a_refused_commit_reports_what_the_server_kept() {
    let _serial = common::serial().await;
    for (prefix, port) in SERVERS {
        let engine = connect(prefix, port).await;
        let mut session = engine.session().await.unwrap();
        // COMMIT and ROLLBACK can't end an XA transaction.
        text(&mut session, "XA START 'barsql_refused'").await;
        assert!(session.commit().await.is_err());
        assert!(session.in_transaction(), "{prefix}: the XA transaction is still open");
        assert!(session.rollback().await.is_err());
        assert!(session.in_transaction(), "{prefix}");
        run_all(&mut session, &["XA END 'barsql_refused'".into(), "XA ROLLBACK 'barsql_refused'".into()]).await;
        session.rollback().await.unwrap();
        assert!(!session.in_transaction(), "{prefix}");
    }

    let engine = connect("MYSQL", "33306").await;
    let mut admin = engine.session().await.unwrap();
    text(&mut admin, "DROP TABLE IF EXISTS barsql_test.refused_commit").await;
    text(&mut admin, "CREATE TABLE barsql_test.refused_commit (id INT)").await;
    let mut session = engine.session().await.unwrap();
    session.begin().await.unwrap();
    text(&mut session, "INSERT INTO barsql_test.refused_commit VALUES (1)").await;
    text(&mut admin, "SET GLOBAL super_read_only = ON").await;
    let refused = session.commit().await;
    text(&mut admin, "SET GLOBAL super_read_only = OFF").await;
    text(&mut admin, "SET GLOBAL read_only = OFF").await;
    assert_eq!(refused.map_err(|err| err.code), Err("1290".into()));
    assert!(!session.in_transaction(), "the server rolled it back");
    assert_eq!(text(&mut admin, "SELECT COUNT(*) FROM barsql_test.refused_commit").await, "0");
    text(&mut admin, "DROP TABLE barsql_test.refused_commit").await;
}

#[tokio::test]
async fn scripts_report_the_transaction_the_server_has_open() {
    let _serial = common::serial().await;
    for (prefix, port) in SERVERS {
        let engine = connect(prefix, port).await;
        let mut admin = engine.session().await.unwrap();
        text(&mut admin, "DROP TABLE IF EXISTS barsql_test.script_transactions").await;
        text(&mut admin, "CREATE TABLE barsql_test.script_transactions (id INT PRIMARY KEY) ENGINE=InnoDB").await;
        let insert = |id: i32| format!("INSERT INTO barsql_test.script_transactions VALUES ({id})");
        let mut session = engine.session().await.unwrap();

        run_all(&mut session, &["SET autocommit = 0".into(), insert(1)]).await;
        assert!(session.in_transaction(), "{prefix}: a write with autocommit off opens one");
        run_all(&mut session, &["ROLLBACK".into(), "SET autocommit = 1".into()]).await;
        assert!(!session.in_transaction(), "{prefix}");

        run_all(&mut session, &["BEGIN".into(), insert(2), "COMMIT AND CHAIN".into()]).await;
        assert!(session.in_transaction(), "{prefix}: COMMIT AND CHAIN opens the next one");
        let duplicate = script_error(&mut session, &insert(2)).await;
        assert_eq!(duplicate.map(|err| err.code), Some("1062".into()), "{prefix}");
        assert!(session.in_transaction(), "{prefix}: a failed statement leaves it open");
        run_all(&mut session, &["ROLLBACK".into()]).await;
        assert!(!session.in_transaction(), "{prefix}");

        run_all(&mut session, &["XA START 'barsql_script'".into(), insert(3)]).await;
        assert!(session.in_transaction(), "{prefix}: XA START opens one");
        run_all(&mut session, &["XA END 'barsql_script'".into(), "XA ROLLBACK 'barsql_script'".into()]).await;
        assert!(!session.in_transaction(), "{prefix}: XA ROLLBACK ends it");
        text(&mut admin, "DROP TABLE barsql_test.script_transactions").await;
    }
}

#[tokio::test]
async fn a_deadlock_ends_the_victims_transaction() {
    let _serial = common::serial().await;
    for (prefix, port) in SERVERS {
        let engine = connect(prefix, port).await;
        let mut admin = engine.session().await.unwrap();
        text(&mut admin, "DROP TABLE IF EXISTS barsql_test.deadlock").await;
        text(&mut admin, "CREATE TABLE barsql_test.deadlock (id INT PRIMARY KEY, n INT) ENGINE=InnoDB").await;
        text(&mut admin, "INSERT INTO barsql_test.deadlock VALUES (1, 0), (2, 0)").await;
        let update = |id: i32| format!("UPDATE barsql_test.deadlock SET n = n + 1 WHERE id = {id}");
        let (mut a, mut b) = (engine.session().await.unwrap(), engine.session().await.unwrap());
        run_all(&mut a, &["BEGIN".into(), update(1)]).await;
        run_all(&mut b, &["BEGIN".into(), update(2)]).await;
        let (a_waits, b_closes_the_cycle) = (update(2), update(1));
        let (a_error, b_error) = tokio::join!(script_error(&mut a, &a_waits), async {
            tokio::time::sleep(Duration::from_millis(500)).await;
            script_error(&mut b, &b_closes_the_cycle).await
        });
        let (victim, survivor, error) = match (a_error, b_error) {
            (Some(error), None) => (&mut a, &mut b, error),
            (None, Some(error)) => (&mut b, &mut a, error),
            errors => panic!("{prefix}: one of them must be the victim: {errors:?}"),
        };
        assert_eq!(error.code, "1213", "{prefix}: {error:?}");
        assert!(!victim.in_transaction(), "{prefix}: the server rolled the victim's back");
        assert!(survivor.in_transaction(), "{prefix}");
        survivor.rollback().await.unwrap();
        text(&mut admin, "DROP TABLE barsql_test.deadlock").await;
    }
}

#[tokio::test]
async fn timestamps_show_the_sessions_time_zone() {
    let _serial = common::serial().await;
    for (prefix, port) in SERVERS {
        let engine = connect(prefix, port).await;
        let mut session = engine.session().await.unwrap();
        let setup = [
            "CREATE TEMPORARY TABLE barsql_test.zone_probe (ts TIMESTAMP(3) NULL, dt DATETIME(3) NULL)".into(),
            "INSERT INTO barsql_test.zone_probe VALUES ('2024-02-29 13:45:30.123', '2024-02-29 13:45:30.123')".into(),
            "INSERT INTO barsql_test.zone_probe VALUES ('2024-07-01 12:00:00', '2024-07-01 12:00:00')".into(),
        ];
        run_all(&mut session, &setup).await;
        let sql = "SELECT ts, dt FROM barsql_test.zone_probe ORDER BY ts";
        let utc = [["2024-02-29T13:45:30.123Z", "2024-02-29T13:45:30.123Z"], ["2024-07-01T12:00:00Z"; 2]];
        assert_eq!(rows(&mut session, sql).await, utc, "{prefix}");

        text(&mut session, "SET time_zone = '+05:00'").await;
        let plus5 = [
            ["2024-02-29T18:45:30.123+05:00", "2024-02-29T13:45:30.123Z"],
            ["2024-07-01T17:00:00+05:00", "2024-07-01T12:00:00Z"],
        ];
        assert_eq!(rows(&mut session, sql).await, plus5, "{prefix}");

        text(&mut session, "SET time_zone = 'Europe/Sofia'").await;
        let sofia = [
            ["2024-02-29T15:45:30.123+02:00", "2024-02-29T13:45:30.123Z"],
            ["2024-07-01T15:00:00+03:00", "2024-07-01T12:00:00Z"],
        ];
        assert_eq!(rows(&mut session, sql).await, sofia, "{prefix}");

        let prepared = ["PREPARE set_zone FROM 'SET time_zone = ''-03:30'''".into(), "EXECUTE set_zone".into()];
        run_all(&mut session, &prepared).await;
        let minus330 = [
            ["2024-02-29T10:15:30.123-03:30", "2024-02-29T13:45:30.123Z"],
            ["2024-07-01T08:30:00-03:30", "2024-07-01T12:00:00Z"],
        ];
        assert_eq!(rows(&mut session, sql).await, minus330, "{prefix}");
    }
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
