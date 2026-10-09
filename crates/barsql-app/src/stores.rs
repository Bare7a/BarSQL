use std::collections::BTreeMap;

use barsql_core::{HistoryEntry, QueryError, SavedQuery};
use barsql_storage::EditorSession;

use crate::BarApp;

fn storage_error(err: std::io::Error) -> QueryError {
    QueryError::message(err.to_string())
}

impl BarApp {
    pub fn query_history(&self, connection_id: &str, limit: usize) -> Vec<HistoryEntry> {
        self.inner.stores.history.list(connection_id, limit)
    }

    pub fn clear_query_history(&self, connection_id: &str) -> Result<(), QueryError> {
        self.inner.stores.history.clear(connection_id).map_err(storage_error)
    }

    pub fn delete_query_history_entry(&self, id: &str) -> bool {
        self.inner.stores.history.delete(id).unwrap_or(false)
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

    pub fn delete_saved_query(&self, id: &str) -> bool {
        self.inner.stores.saved_queries.delete(id).unwrap_or(false)
    }

    pub fn editor_session(&self) -> EditorSession {
        self.inner.stores.session.get()
    }

    pub fn save_editor_session(&self, session: EditorSession) -> Result<(), QueryError> {
        self.inner.stores.session.save(session).map_err(storage_error)
    }

    pub fn settings(&self) -> BTreeMap<String, String> {
        self.inner.stores.settings.all()
    }

    // Keys like "barsql-theme" are opaque here.
    pub fn set_setting(&self, key: &str, value: &str) -> Result<(), QueryError> {
        self.inner.stores.settings.set(key, value).map_err(storage_error)
    }

    pub fn delete_setting(&self, key: &str) -> Result<(), QueryError> {
        self.inner.stores.settings.delete(key).map_err(storage_error)
    }
}
