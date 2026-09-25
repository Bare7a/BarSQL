use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use barsql_core::QueryError;
use barsql_db::Session;
use tokio::sync::OwnedMutexGuard;

use crate::{BarApp, lock};

// Each open editor tab gets its own session so SET, temp tables and transactions stay with that tab.
#[derive(Default)]
pub(crate) struct Tabs {
    slots: Mutex<HashMap<String, Arc<tokio::sync::Mutex<TabSlot>>>>,
    in_transaction: Mutex<HashSet<String>>,
}

// An idle tab gives its connection back. Its next run opens a fresh session.
pub(crate) const TAB_IDLE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
pub(crate) const TAB_SWEEP_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Default)]
pub(crate) struct TabSlot {
    pub(crate) connection_id: String,
    pub(crate) session: Option<Session>,
    last_used: Option<Instant>,
}

pub(crate) type TabGuard = OwnedMutexGuard<TabSlot>;

impl Tabs {
    fn slot(&self, tab_id: &str) -> Arc<tokio::sync::Mutex<TabSlot>> {
        lock(&self.slots).entry(tab_id.to_string()).or_default().clone()
    }

    pub(crate) fn existing(&self, tab_id: &str) -> Option<Arc<tokio::sync::Mutex<TabSlot>>> {
        lock(&self.slots).get(tab_id).cloned()
    }

    fn remove(&self, tab_id: &str) -> Option<Arc<tokio::sync::Mutex<TabSlot>>> {
        lock(&self.slots).remove(tab_id)
    }

    pub(crate) fn ids(&self) -> Vec<String> {
        lock(&self.slots).keys().cloned().collect()
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
}

impl BarApp {
    // Waits for the tab's running work to finish before handing out its session.
    pub(crate) async fn tab_session(&self, tab_id: &str, connection_id: &str) -> Result<TabGuard, QueryError> {
        // Before the slot lock, since closing an engine waits on slot locks while holding its own.
        let engine = self.engine(connection_id).await?;
        let mut slot = self.inner.tabs.slot(tab_id).lock_owned().await;
        let stale = slot.session.as_ref().is_some_and(|s| slot.connection_id != connection_id || s.is_broken());
        if stale {
            let other_connection = slot.connection_id != connection_id;
            if other_connection && slot.session.as_ref().is_some_and(Session::in_transaction) {
                return Err(QueryError::message("a transaction is open on this tab's other connection"));
            }
            slot.session = None;
            self.inner.tabs.mark(tab_id, false);
        }
        if slot.session.is_none() {
            slot.session = Some(engine.session().await?);
            slot.connection_id = connection_id.to_string();
        }
        slot.last_used = Some(Instant::now());
        Ok(slot)
    }

    // Called when the tab's work is done with the slot.
    pub(crate) fn sync_transaction(&self, tab_id: &str, slot: &mut TabSlot) {
        slot.last_used = Some(Instant::now());
        self.inner.tabs.mark(tab_id, slot.session.as_ref().is_some_and(Session::in_transaction));
    }

    // Skips busy tabs and tabs with an open transaction. The idle sweep calls this with TAB_IDLE_TIMEOUT.
    pub fn release_idle_tabs(&self, idle: Duration) {
        let slots: Vec<Arc<tokio::sync::Mutex<TabSlot>>> = lock(&self.inner.tabs.slots).values().cloned().collect();
        for slot in slots {
            let Ok(mut slot) = slot.try_lock_owned() else { continue };
            let idle_enough = slot.last_used.is_none_or(|at| at.elapsed() >= idle);
            if idle_enough && slot.session.as_ref().is_some_and(|s| !s.in_transaction()) {
                slot.session = None;
            }
        }
    }

    pub async fn cleanup_tab(&self, tab_id: &str) {
        self.inner.jobs.cancel(tab_id);
        let Some(slot) = self.inner.tabs.remove(tab_id) else { return };
        let mut slot = slot.lock_owned().await;
        if let Some(session) = slot.session.as_mut()
            && session.in_transaction()
        {
            let _ = session.rollback().await;
        }
        slot.session = None;
        self.inner.tabs.mark(tab_id, false);
    }

    // Returns the tabs whose transaction was rolled back.
    pub(crate) async fn close_tab_sessions(&self, connection_id: &str) -> Vec<String> {
        let slots: Vec<(String, Arc<tokio::sync::Mutex<TabSlot>>)> =
            lock(&self.inner.tabs.slots).iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        let mut ended = Vec::new();
        for (tab_id, slot) in slots {
            let mut slot = slot.lock().await;
            if slot.connection_id != connection_id || slot.session.is_none() {
                continue;
            }
            if let Some(session) = slot.session.as_mut()
                && session.in_transaction()
            {
                let _ = session.rollback().await;
                ended.push(tab_id.clone());
            }
            slot.session = None;
            self.inner.tabs.mark(&tab_id, false);
        }
        ended
    }
}
