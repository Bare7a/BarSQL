use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use barsql_core::QueryError;
use barsql_db::{Cancel, Engine, Session};
use futures_util::future::join_all;
use tokio::sync::OwnedMutexGuard;

use crate::{BarApp, lock};

// Each open editor tab gets its own session so SET, temp tables and transactions stay with that tab.
#[derive(Default)]
pub(crate) struct Tabs {
    slots: Mutex<HashMap<String, Slot>>,
    // The connection each tab's session is on, so closing a connection needn't wait on other connections' runs.
    connections: Mutex<HashMap<String, String>>,
    in_transaction: Mutex<HashSet<String>>,
}

// An idle tab gives its connection back. Its next run opens a fresh session.
pub(crate) const TAB_IDLE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
pub(crate) const TAB_SWEEP_INTERVAL: Duration = Duration::from_secs(60);

pub(crate) type Slot = Arc<tokio::sync::Mutex<TabSlot>>;

#[derive(Default)]
pub(crate) struct TabSlot {
    pub(crate) connection_id: String,
    pub(crate) session: Option<Session>,
    last_used: Option<Instant>,
    // Set when the tab closes, for work that took the slot before then.
    closed: bool,
}

pub(crate) type TabGuard = OwnedMutexGuard<TabSlot>;

impl TabSlot {
    // The session as the tab's last run left it, if it's still open on `connection_id`.
    pub(crate) fn live_session(&mut self, connection_id: &str) -> Option<&mut Session> {
        let same = !self.closed && self.connection_id == connection_id;
        self.session.as_mut().filter(|session| same && !session.is_broken())
    }
}

impl Tabs {
    // Work takes its slot before its first await, so a tab closed while the work connects doesn't get a new one.
    pub(crate) fn slot(&self, tab_id: &str) -> Slot {
        lock(&self.slots).entry(tab_id.to_string()).or_default().clone()
    }

    pub(crate) fn existing(&self, tab_id: &str) -> Option<Slot> {
        lock(&self.slots).get(tab_id).cloned()
    }

    fn remove(&self, tab_id: &str) -> Option<Slot> {
        lock(&self.connections).remove(tab_id);
        lock(&self.slots).remove(tab_id)
    }

    pub(crate) fn ids(&self) -> Vec<String> {
        lock(&self.slots).keys().cloned().collect()
    }

    // Closing a connection cancels its work before reading these, so under this lock a session is found or never kept.
    fn bind(&self, tab_id: &str, connection_id: &str, cancel: &Cancel) -> bool {
        let mut connections = lock(&self.connections);
        if cancel.is_cancelled() {
            return false;
        }
        connections.insert(tab_id.to_string(), connection_id.to_string());
        true
    }

    fn unbind(&self, tab_id: &str) {
        lock(&self.connections).remove(tab_id);
    }

    fn on_connection(&self, connection_id: &str) -> Vec<(String, Slot)> {
        let tabs: Vec<String> =
            lock(&self.connections).iter().filter(|(_, on)| *on == connection_id).map(|(tab, _)| tab.clone()).collect();
        let slots = lock(&self.slots);
        tabs.into_iter().filter_map(|tab| slots.get(&tab).cloned().map(|slot| (tab, slot))).collect()
    }

    pub(crate) fn mark(&self, tab_id: &str, in_transaction: bool) {
        let mut tabs = lock(&self.in_transaction);
        if in_transaction {
            tabs.insert(tab_id.to_string());
        } else {
            tabs.remove(tab_id);
        }
    }

    pub(crate) fn in_transaction(&self, tab_id: &str) -> bool {
        lock(&self.in_transaction).contains(tab_id)
    }

    pub(crate) fn any_in_transaction(&self) -> bool {
        !lock(&self.in_transaction).is_empty()
    }
}

impl BarApp {
    // Waits for the tab's running work to finish before handing out its session.
    pub(crate) async fn tab_session(
        &self,
        tab_id: &str,
        slot: Slot,
        connection_id: &str,
        cancel: &Cancel,
    ) -> Result<TabGuard, QueryError> {
        // Before the slot lock, since closing an engine waits on slot locks while holding its own.
        let engine = self.engine_unless_cancelled(connection_id, cancel).await?;
        let mut slot = slot.lock_owned().await;
        if slot.closed || cancel.is_cancelled() {
            return Err(QueryError::cancelled());
        }
        let stale = slot.session.as_ref().is_some_and(|s| slot.connection_id != connection_id || s.is_broken());
        if stale {
            let other_connection = slot.connection_id != connection_id;
            if other_connection && slot.session.as_ref().is_some_and(Session::in_transaction) {
                return Err(QueryError::message("a transaction is open on this tab's other connection"));
            }
            slot.session = None;
            self.inner.tabs.unbind(tab_id);
            self.inner.tabs.mark(tab_id, false);
        }
        if slot.session.is_none() {
            // A lease can wait for a pooled connection, so a cancel ends the wait.
            let session = unless_cancelled(engine.session(), cancel).await?;
            if !self.inner.tabs.bind(tab_id, connection_id, cancel) {
                return Err(QueryError::cancelled());
            }
            slot.session = Some(session);
            slot.connection_id = connection_id.to_string();
        }
        slot.last_used = Some(Instant::now());
        Ok(slot)
    }

    // The connect goes on in its own task, so a reconnect that closes the old engine is never left half done.
    pub(crate) async fn engine_unless_cancelled(
        &self,
        connection_id: &str,
        cancel: &Cancel,
    ) -> Result<Engine, QueryError> {
        let app = self.clone();
        let id = connection_id.to_string();
        let connect = self.inner.runtime.spawn(async move { app.engine(&id).await });
        unless_cancelled(
            async { connect.await.unwrap_or_else(|err| Err(QueryError::message(err.to_string()))) },
            cancel,
        )
        .await
    }

    // Called when the tab's work is done with the slot.
    pub(crate) fn sync_transaction(&self, tab_id: &str, slot: &mut TabSlot) {
        slot.last_used = Some(Instant::now());
        self.inner.tabs.mark(tab_id, slot.session.as_ref().is_some_and(Session::in_transaction));
    }

    // Skips busy tabs and tabs with an open transaction. The idle sweep calls this with TAB_IDLE_TIMEOUT.
    pub fn release_idle_tabs(&self, idle: Duration) {
        let slots: Vec<(String, Slot)> =
            lock(&self.inner.tabs.slots).iter().map(|(tab, slot)| (tab.clone(), slot.clone())).collect();
        for (tab_id, slot) in slots {
            let Ok(mut slot) = slot.try_lock_owned() else { continue };
            let idle_enough = slot.last_used.is_none_or(|at| at.elapsed() >= idle);
            if idle_enough && slot.session.as_ref().is_some_and(|s| !s.in_transaction()) {
                slot.session = None;
                self.inner.tabs.unbind(&tab_id);
            }
        }
    }

    pub(crate) async fn cleanup_tab_inner(&self, tab_id: &str) {
        self.inner.jobs.cancel(tab_id);
        let Some(slot) = self.inner.tabs.remove(tab_id) else { return };
        let mut slot = slot.lock_owned().await;
        slot.closed = true;
        self.end_session(tab_id, &mut slot).await;
    }

    // Returns the tabs whose transaction was rolled back. All at once: on SQLite one tab's Begin can wait for the
    // connection another tab's transaction keeps.
    pub(crate) async fn close_tab_sessions(&self, connection_id: &str) -> Vec<String> {
        let tabs = self.inner.tabs.on_connection(connection_id);
        let closing = tabs.into_iter().map(|(tab_id, slot)| self.close_tab_session(tab_id, slot, connection_id));
        join_all(closing).await.into_iter().flatten().collect()
    }

    async fn close_tab_session(&self, tab_id: String, slot: Slot, connection_id: &str) -> Option<String> {
        let mut slot = slot.lock().await;
        if slot.connection_id != connection_id || slot.session.is_none() {
            return None;
        }
        self.end_session(&tab_id, &mut slot).await.then_some(tab_id)
    }

    // Drops the session, rolling back its open transaction. True when there was one.
    async fn end_session(&self, tab_id: &str, slot: &mut TabSlot) -> bool {
        let mut rolled_back = false;
        if let Some(session) = slot.session.as_mut()
            && session.in_transaction()
        {
            let _ = session.rollback().await;
            rolled_back = true;
        }
        slot.session = None;
        self.inner.tabs.unbind(tab_id);
        self.inner.tabs.mark(tab_id, false);
        rolled_back
    }
}

pub(crate) async fn unless_cancelled<T>(
    work: impl Future<Output = Result<T, QueryError>>,
    cancel: &Cancel,
) -> Result<T, QueryError> {
    tokio::select! {
        result = work => result,
        _ = cancel.cancelled() => Err(QueryError::cancelled()),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use barsql_core::Value;
    use tokio::sync::OwnedMutexGuard;

    use crate::BarApp;
    use crate::connections::tests::setup;

    // Stands in for a connection that is still opening, which SQLite never is for long.
    async fn hold_connect(app: &BarApp, id: &str) -> OwnedMutexGuard<()> {
        app.connect_lock(id).lock_owned().await
    }

    async fn assert_closed(app: &BarApp, id: &str, tab: &str) {
        assert!(app.inner.tabs.existing(tab).is_none(), "the closed tab's slot came back");
        assert!(!app.transaction_status(tab));
        let free = tokio::time::timeout(Duration::from_secs(5), app.execute_query(id, "SELECT 1")).await;
        assert!(free.expect("the closed tab still holds the connection").is_ok());
    }

    #[tokio::test]
    async fn stop_reaches_a_run_that_is_still_connecting() {
        let (app, id, _dir) = setup().await;
        let connecting = hold_connect(&app, &id).await;
        let run = tokio::spawn({
            let (app, id) = (app.clone(), id.clone());
            async move { app.execute_query_stream(&id, "tab", "CREATE TABLE t (a INTEGER)").await.map(|_| ()) }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(app.cancel_query("tab"), "Stop must find the run while it connects");
        let stopped = tokio::time::timeout(Duration::from_secs(5), run).await.expect("Stop must end the wait");
        assert!(stopped.unwrap().is_err_and(|e| e.cancelled));
        drop(connecting);
        let made = app.execute_query(&id, "SELECT count(*) FROM sqlite_master WHERE name = 't'").await.unwrap();
        assert_eq!(made.rows, [[Value::Int(0)]], "the stopped script ran once the connection opened");
    }

    #[tokio::test]
    async fn closing_a_tab_while_its_run_connects_leaves_no_session() {
        let (app, id, _dir) = setup().await;
        let connecting = hold_connect(&app, &id).await;
        let run = tokio::spawn({
            let (app, id) = (app.clone(), id.clone());
            async move { app.execute_query_stream(&id, "tab", "BEGIN; CREATE TABLE t (a INTEGER)").await }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        app.cleanup_tab("tab").await;
        drop(connecting);
        if let Ok(handle) = run.await.unwrap() {
            while handle.events.recv().await.is_ok() {}
        }
        assert_closed(&app, &id, "tab").await;
    }

    #[tokio::test]
    async fn closing_a_tab_while_begin_connects_leaves_no_transaction() {
        let (app, id, _dir) = setup().await;
        let connecting = hold_connect(&app, &id).await;
        let begin = tokio::spawn({
            let (app, id) = (app.clone(), id.clone());
            async move { app.begin_transaction(&id, "tab").await }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        app.cleanup_tab("tab").await;
        drop(connecting);
        assert!(begin.await.unwrap().is_err(), "a closed tab began a transaction");
        assert_closed(&app, &id, "tab").await;
    }
}
