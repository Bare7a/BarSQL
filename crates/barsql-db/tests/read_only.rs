#![cfg(feature = "e2e")]

mod common;

use barsql_core::ConnectionConfig;
use barsql_db::{Cancel, Engine, Session};
use common::{mysql_config, postgres_config, run_error};

const PURGE: &str = "SELECT barsql_read_only_purge()";

async fn rows(session: &mut Session) -> usize {
    session.buffered("SELECT id FROM barsql_read_only_items", &Cancel::new()).await.unwrap().rows()
}

// The SQL check lets every script through, so only the server can stop the purge.
async fn the_server_refuses(cfg: ConnectionConfig, setup: &[&str], scripts: &[&[&str]]) {
    let writable = Engine::connect(&cfg).await.unwrap();
    let mut session = writable.session().await.unwrap();
    let fill = "INSERT INTO barsql_read_only_items VALUES (1)";
    for sql in setup.iter().chain(&["DELETE FROM barsql_read_only_items", fill, PURGE]) {
        session.buffered(sql, &Cancel::new()).await.unwrap_or_else(|err| panic!("{sql}: {}", err.message));
    }
    assert_eq!(rows(&mut session).await, 0, "the purge deletes");
    session.buffered(fill, &Cancel::new()).await.unwrap();
    let read_only = Engine::connect(&ConnectionConfig { read_only: true, ..cfg }).await.unwrap();
    for script in scripts {
        let statements: Vec<String> = script.iter().map(|sql| sql.to_string()).collect();
        // Twice: the second session comes back from the pool.
        for _ in 0..2 {
            let mut session = read_only.session().await.unwrap();
            assert!(run_error(&mut session, &statements).await.is_some(), "{script:?} ran");
        }
        assert_eq!(rows(&mut session).await, 1, "{script:?} deleted the row");
    }
    // MySQL's close waits for every session to come back.
    drop(session);
    read_only.close().await;
    writable.close().await;
}

#[tokio::test]
async fn postgres_read_only_sessions_refuse_hidden_writes() {
    let setup = [
        "CREATE TABLE IF NOT EXISTS barsql_read_only_items (id int)",
        "CREATE OR REPLACE FUNCTION barsql_read_only_purge() RETURNS void LANGUAGE sql \
         AS 'DELETE FROM barsql_read_only_items'",
    ];
    // A plain SELECT that turns the read-only default off.
    let unset = "SELECT set_config('default_transaction_read_only', 'off', false)";
    the_server_refuses(postgres_config(), &setup, &[&[PURGE], &[unset, PURGE], &[unset, "COMMIT", PURGE]]).await;
}

// A function can switch the session to read-write. MariaDB applies that at the next statement, MySQL after a COMMIT.
#[tokio::test]
async fn mysql_and_mariadb_read_only_sessions_refuse_hidden_writes() {
    let setup = [
        "CREATE TABLE IF NOT EXISTS barsql_read_only_items (id int)",
        "DROP FUNCTION IF EXISTS barsql_read_only_purge",
        "CREATE FUNCTION barsql_read_only_purge() RETURNS int DETERMINISTIC MODIFIES SQL DATA \
         BEGIN DELETE FROM barsql_read_only_items; RETURN 1; END",
        "DROP FUNCTION IF EXISTS barsql_read_only_flip",
        "CREATE FUNCTION barsql_read_only_flip() RETURNS int DETERMINISTIC NO SQL \
         BEGIN SET SESSION TRANSACTION READ WRITE; RETURN 1; END",
        "DROP FUNCTION IF EXISTS barsql_read_only_unset",
        "CREATE FUNCTION barsql_read_only_unset() RETURNS int DETERMINISTIC NO SQL \
         BEGIN SET @@session.transaction_read_only = 0; RETURN 1; END",
    ];
    let (flip, unset) = ("SELECT barsql_read_only_flip()", "SELECT barsql_read_only_unset()");
    let scripts: [&[&str]; 5] =
        [&[PURGE], &[flip, PURGE], &[flip, "COMMIT", PURGE], &[unset, PURGE], &[unset, "COMMIT", PURGE]];
    for (prefix, port) in [("MYSQL", "33306"), ("MARIADB", "33307")] {
        the_server_refuses(mysql_config(prefix, port), &setup, &scripts).await;
    }
}

// No reset may run between a statement and its SHOW WARNINGS.
#[tokio::test]
async fn mysql_and_mariadb_read_only_sessions_keep_warnings_for_show_warnings() {
    for (prefix, port) in [("MYSQL", "33306"), ("MARIADB", "33307")] {
        let read_only =
            Engine::connect(&ConnectionConfig { read_only: true, ..mysql_config(prefix, port) }).await.unwrap();
        let mut session = read_only.session().await.unwrap();
        session.buffered("SELECT 1/0", &Cancel::new()).await.unwrap();
        let warnings = session.buffered("SHOW WARNINGS", &Cancel::new()).await.unwrap();
        assert_eq!(warnings.rows(), 1, "{prefix}");
        drop(session);
        read_only.close().await;
    }
}
