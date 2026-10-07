use std::time::{Duration, Instant};

use barsql_app::AppEvent;
use barsql_core::Value;

use crate::harness::{E2e, Kind, run, run_error, unique_table};
use crate::support::collect;

async fn temp_table(e: &E2e, prefix: &str) -> String {
    let table = unique_table(prefix);
    e.create_temp_table(&e.auto_pk_table(&table), &table).await;
    table
}

fn insert(e: &E2e, table: &str, name: &str) -> String {
    format!("INSERT INTO {} (name) VALUES ('{name}')", e.qualified(table))
}

each_engine!(async fn transaction_commit(e) {
    require!(e, interactive_transactions);
    let table = temp_table(&e, "txn_commit").await;
    let tab = format!("tab-commit-{table}");
    e.app.begin_transaction(&e.id, &tab).await.unwrap();
    assert!(e.app.transaction_status(&tab));
    e.exec_on_tab(&tab, &insert(&e, &table, "t1")).await;
    e.exec_on_tab(&tab, &insert(&e, &table, "t2")).await;
    assert_eq!(e.count(&table).await, 0, "uncommitted rows are visible to another connection");
    e.app.commit_transaction(&tab).await.unwrap();
    assert!(!e.app.transaction_status(&tab));
    assert_eq!(e.count(&table).await, 2);
});

each_engine!(async fn transaction_rollback(e) {
    require!(e, interactive_transactions);
    let table = temp_table(&e, "txn_rollback").await;
    let tab = format!("tab-rollback-{table}");
    e.app.begin_transaction(&e.id, &tab).await.unwrap();
    e.exec_on_tab(&tab, &insert(&e, &table, "gone")).await;
    e.app.rollback_transaction(&tab).await.unwrap();
    assert!(!e.app.transaction_status(&tab));
    assert_eq!(e.count(&table).await, 0);
});

each_engine!(async fn transaction_guards(e) {
    require!(e, interactive_transactions);
    assert!(e.app.begin_transaction(&e.id, "").await.is_err());
    let table = temp_table(&e, "txn_guard").await;
    let tab = format!("tab-guard-{table}");
    e.app.begin_transaction(&e.id, &tab).await.unwrap();
    assert!(e.app.begin_transaction(&e.id, &tab).await.is_err(), "a second BEGIN on the same tab");
    e.exec_on_tab(&tab, &insert(&e, &table, "orphan")).await;
    e.app.cleanup_tab(&tab).await;
    assert!(!e.app.transaction_status(&tab));
    assert_eq!(e.count(&table).await, 0, "closing the tab rolls its transaction back");
    assert!(e.app.commit_transaction(&format!("tab-never-opened-{table}")).await.is_err());
});

each_engine!(async fn transactions_are_concurrent_per_tab(e) {
    require!(e, interactive_transactions);
    let table = temp_table(&e, "txn_multi").await;
    let (tab_a, tab_b) = (format!("tabA-{table}"), format!("tabB-{table}"));
    e.app.begin_transaction(&e.id, &tab_a).await.unwrap();
    e.app.begin_transaction(&e.id, &tab_b).await.unwrap();
    e.exec_on_tab(&tab_a, &insert(&e, &table, "a")).await;
    e.exec_on_tab(&tab_b, &insert(&e, &table, "b")).await;
    e.app.commit_transaction(&tab_a).await.unwrap();
    e.app.rollback_transaction(&tab_b).await.unwrap();
    let res = e.query(&format!("SELECT name FROM {}", e.qualified(&table))).await;
    assert_eq!(res.rows, [[Value::from("a")]], "only the committed row survives");
});

// Postgres turns COMMIT of an aborted transaction into a silent rollback. The app reports it as an error.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn commit_of_an_aborted_transaction_fails() {
    run(Kind::Postgres, |e| async move {
        let table = temp_table(&e, "txn_aborted").await;
        let tab = format!("tab-aborted-{table}");
        e.app.begin_transaction(&e.id, &tab).await.unwrap();
        e.exec_on_tab(&tab, &insert(&e, &table, "lost")).await;
        assert!(run_error(&e.run_on_tab(&tab, "SELECT 1/0").await).is_some());
        assert!(e.app.transaction_status(&tab), "an aborted transaction stays open until it ends");
        let err = e.app.commit_transaction(&tab).await.expect_err("the commit rolled back");
        assert!(err.message.contains("rollback"), "{err:?}");
        assert!(!e.app.transaction_status(&tab));
        assert_eq!(e.count(&table).await, 0);
    })
    .await
}

const SLEEP_SECONDS: u32 = 30;

// Waits for the statement to register before cancelling it.
async fn cancel_sleep(e: &E2e, tab: &str) -> Option<barsql_core::QueryError> {
    let started = Instant::now();
    let handle = e.app.execute_query_stream(&e.id, tab, &e.sleep_sql(SLEEP_SECONDS)).await.unwrap();
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert!(e.app.cancel_query(tab), "no in-flight query was registered on the tab");
    let events = collect(handle).await;
    let elapsed = started.elapsed();
    assert!(elapsed < Duration::from_secs(u64::from(SLEEP_SECONDS) / 2), "the cancel took {elapsed:?}");
    run_error(&events)
}

each_engine!(async fn cancel_query(e) {
    let error = cancel_sleep(&e, "tab-cancel").await.expect("a cancelled query reports an error");
    assert!(error.cancelled, "{error:?}");
    let after = e.run_on_tab("tab-cancel", "SELECT 2 AS still_usable").await;
    assert!(run_error(&after).is_none(), "the tab keeps working after a cancel");
});

each_engine!(async fn cancel_is_scoped_to_the_tab(e) {
    let handle = e.app.execute_query_stream(&e.id, "tab-long", &e.sleep_sql(SLEEP_SECONDS)).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(run_error(&e.run_on_tab("tab-other", "SELECT 1").await).is_none());
    assert!(!e.app.cancel_query("tab-other"), "nothing is running on the other tab");
    assert!(e.app.cancel_query("tab-long"), "the first tab's run must still be in flight");
    assert!(run_error(&collect(handle).await).is_some_and(|err| err.cancelled));
});

each_engine!(async fn cancel_inside_a_transaction_keeps_it(e) {
    require!(e, interactive_transactions);
    let table = temp_table(&e, "txn_cancel").await;
    let tab = format!("tab-cancel-{table}");
    e.app.begin_transaction(&e.id, &tab).await.unwrap();
    e.exec_on_tab(&tab, &insert(&e, &table, "kept")).await;
    let error = cancel_sleep(&e, &tab).await.expect("a cancelled query reports an error");
    assert!(error.cancelled, "{error:?}");
    // tiberius 0.13 can't read the server's acknowledgement of a cancel, so the connection goes, and the server
    // rolls back what it held. The tab must say so rather than claim the transaction.
    if e.is_sqlserver() {
        assert!(!e.app.transaction_status(&tab), "the tab still claims a transaction its connection lost");
        assert_eq!(e.count(&table).await, 0);
        return;
    }
    assert!(e.app.transaction_status(&tab), "the cancel must not end the tab's transaction");
    e.app.commit_transaction(&tab).await.unwrap();
    assert_eq!(e.count(&table).await, 1, "the write before the cancel commits");
});

each_engine!(async fn saving_or_disconnecting_ends_tab_transactions(e) {
    require!(e, interactive_transactions);
    let table = temp_table(&e, "txn_ended").await;
    let events = e.app.events();
    let saved = e.app.list_connections().into_iter().find(|c| c.id == e.id).unwrap();
    let tab = format!("tab-saved-{table}");
    e.app.begin_transaction(&e.id, &tab).await.unwrap();
    e.exec_on_tab(&tab, &insert(&e, &table, "discarded")).await;
    e.app.save_connection(barsql_core::ConnectionConfig { name: format!("{} renamed", saved.name), ..saved }).await.unwrap();
    assert_eq!(events.try_recv(), Ok(AppEvent::TransactionsEnded { tab_ids: vec![tab.clone()] }));
    assert!(!e.app.transaction_status(&tab));

    let tab = format!("tab-disconnected-{table}");
    e.app.begin_transaction(&e.id, &tab).await.unwrap();
    e.exec_on_tab(&tab, &insert(&e, &table, "discarded")).await;
    e.app.disconnect(&e.id).await;
    assert_eq!(events.try_recv(), Ok(AppEvent::TransactionsEnded { tab_ids: vec![tab.clone()] }));
    assert!(!e.app.transaction_status(&tab));
    assert_eq!(e.count(&table).await, 0, "both transactions were rolled back");
    assert!(run_error(&e.run_on_tab(&tab, "SELECT 1").await).is_none(), "the tab reconnects on its next run");
});

each_engine!(async fn idle_tabs_release_their_sessions(e) {
    let (idle, busy) = ("tab-idle", "tab-idle-in-transaction");
    let caps = e.driver().capabilities();
    let (transactions, state) = (caps.interactive_transactions, caps.session_state);
    // sqld refuses temp tables, so a Turso tab holds nothing to lose.
    let (create, read) = match e.kind {
        Kind::SqlServer => ("CREATE TABLE #tmp_idle (a INT)", "SELECT a FROM #tmp_idle"),
        _ => ("CREATE TEMPORARY TABLE tmp_idle (a INT)", "SELECT a FROM tmp_idle"),
    };
    if state {
        e.exec_on_tab(idle, create).await;
    }
    let table = temp_table(&e, "txn_idle").await;
    if transactions {
        e.app.begin_transaction(&e.id, busy).await.unwrap();
        e.exec_on_tab(busy, &insert(&e, &table, "kept")).await;
    }

    e.app.release_idle_tabs(Duration::from_secs(3600));
    if state {
        let kept = run_error(&e.run_on_tab(idle, read).await);
        assert!(kept.is_none(), "a recent tab keeps its session");
    }

    e.app.release_idle_tabs(Duration::ZERO);
    if state {
        let gone = run_error(&e.run_on_tab(idle, read).await);
        assert!(gone.is_some(), "the session state went with it");
    }
    assert!(run_error(&e.run_on_tab(idle, "SELECT 1").await).is_none(), "the tab opens a fresh session");
    if transactions {
        assert!(e.app.transaction_status(busy), "a tab in a transaction keeps its session");
        e.app.commit_transaction(busy).await.unwrap();
        assert_eq!(e.count(&table).await, 1);
    }
});

// The server drops an idle stream within seconds, so a run can't leave a transaction behind.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn turso_rolls_back_what_a_run_leaves_open() {
    run(Kind::Turso, |e| async move {
        let table = temp_table(&e, "txn_open").await;
        assert!(e.app.begin_transaction(&e.id, "tab-turso").await.is_err(), "no transaction button on Turso");
        let events = e.run_on_tab("tab-open", &format!("BEGIN; {}", insert(&e, &table, "lost"))).await;
        let error = run_error(&events).expect("the run reports the rollback");
        assert!(error.message.contains("Rolled back"), "{error:?}");
        assert_eq!(e.count(&table).await, 0);
        e.exec_on_tab("tab-open", &format!("BEGIN; {}; COMMIT", insert(&e, &table, "kept"))).await;
        assert_eq!(e.count(&table).await, 1, "a transaction that ends in the run commits");
    })
    .await
}
