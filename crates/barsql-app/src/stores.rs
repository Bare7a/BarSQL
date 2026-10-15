use std::collections::BTreeMap;
use std::sync::atomic::Ordering;

use barsql_core::{HistoryEntry, QueryError, SavedQuery};
use barsql_storage::EditorSession;

use crate::BarApp;
use crate::events::{AppEvent, Unsaved};

pub(crate) fn storage_error(err: std::io::Error) -> QueryError {
    QueryError::message(err.to_string())
}

impl BarApp {
    pub fn query_history(&self, connection_id: &str, limit: usize) -> Vec<HistoryEntry> {
        self.inner.stores.history.list(connection_id, limit)
    }

    pub(crate) fn record_history(&self, connection_id: &str, sql: &str, duration_ms: i64, error: Option<&QueryError>) {
        let added = self.inner.stores.history.add(HistoryEntry {
            connection_id: connection_id.to_string(),
            sql: sql.to_string(),
            duration_ms,
            success: error.is_none(),
            error: error.map(|e| e.message.clone()).unwrap_or_default(),
            ..Default::default()
        });
        let _ = self.report(Unsaved::History, added);
    }

    // Sends NotSaved for the first failure of a run, so a long script on a full disk sends one event.
    fn report<T>(&self, what: Unsaved, result: std::io::Result<T>) -> Result<T, QueryError> {
        let failing = self.inner.unsaved[what as usize].swap(result.is_err(), Ordering::Relaxed);
        if let Err(err) = &result
            && !failing
        {
            self.emit(AppEvent::NotSaved { what, error: err.to_string() });
        }
        result.map_err(storage_error)
    }

    pub fn clear_query_history(&self, connection_id: &str) -> Result<(), QueryError> {
        self.inner.stores.history.clear(connection_id).map_err(storage_error)
    }

    pub fn delete_query_history_entry(&self, id: &str) -> Result<bool, QueryError> {
        self.inner.stores.history.delete(id).map_err(storage_error)
    }

    pub fn restore_query_history_entry(&self, entry: HistoryEntry) -> Result<(), QueryError> {
        self.inner.stores.history.restore(entry).map_err(storage_error)
    }

    pub fn list_saved_queries(&self, connection_id: &str) -> Vec<SavedQuery> {
        self.inner.stores.saved_queries.list(connection_id)
    }

    pub fn save_saved_query(&self, query: SavedQuery) -> Result<SavedQuery, QueryError> {
        self.inner.stores.saved_queries.save(query).map_err(storage_error)
    }

    pub fn delete_saved_query(&self, id: &str) -> Result<bool, QueryError> {
        self.inner.stores.saved_queries.delete(id).map_err(storage_error)
    }

    pub fn editor_session(&self) -> EditorSession {
        self.inner.stores.session.get()
    }

    pub fn save_editor_session(&self, session: EditorSession) -> Result<(), QueryError> {
        self.report(Unsaved::Session, self.inner.stores.session.save(session))
    }

    pub fn settings(&self) -> BTreeMap<String, String> {
        self.inner.stores.settings.all()
    }

    // Keys like "barsql-theme" are opaque here.
    pub fn set_setting(&self, key: &str, value: &str) -> Result<(), QueryError> {
        self.report(Unsaved::Settings, self.inner.stores.settings.set(key, value))
    }

    pub fn delete_setting(&self, key: &str) -> Result<(), QueryError> {
        self.inner.stores.settings.delete(key).map_err(storage_error)
    }
}
