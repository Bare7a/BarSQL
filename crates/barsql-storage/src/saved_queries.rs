use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use barsql_core::SavedQuery;
use barsql_core::clock::now_rfc3339;

use crate::connections::upsert;
use crate::{load_json_file, save_json_file};

pub struct SavedQueriesStore {
    path: PathBuf,
    queries: Mutex<Vec<SavedQuery>>,
}

impl SavedQueriesStore {
    pub fn open(data_dir: &Path) -> io::Result<Self> {
        let path = data_dir.join("saved_queries.json");
        let queries = load_json_file(&path)?;
        Ok(Self { path, queries: Mutex::new(queries) })
    }

    // Queries without a connection belong to every connection.
    pub fn list(&self, connection_id: &str) -> Vec<SavedQuery> {
        let mut out: Vec<SavedQuery> = self
            .lock()
            .iter()
            .filter(|q| connection_id.is_empty() || q.connection_id.is_empty() || q.connection_id == connection_id)
            .cloned()
            .collect();
        out.sort_by_cached_key(|q| q.name.to_lowercase());
        out
    }

    pub fn save(&self, mut query: SavedQuery) -> io::Result<SavedQuery> {
        let mut queries = self.lock();
        let now = now_rfc3339();
        if query.id.is_empty() {
            query.id = uuid::Uuid::new_v4().to_string();
            query.created_at = now.clone();
        }
        if query.created_at.is_empty() {
            query.created_at = now.clone();
        }
        query.updated_at = now;
        upsert(&mut queries, query.clone(), |q| &q.id);
        save_json_file(&self.path, &*queries)?;
        Ok(query)
    }

    pub fn delete(&self, id: &str) -> io::Result<bool> {
        let mut queries = self.lock();
        let before = queries.len();
        queries.retain(|q| q.id != id);
        if queries.len() == before {
            return Ok(false);
        }
        save_json_file(&self.path, &*queries)?;
        Ok(true)
    }

    fn lock(&self) -> MutexGuard<'_, Vec<SavedQuery>> {
        self.queries.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn save_and_delete_persist() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("saved_queries.json");
        let store = SavedQueriesStore::open(tmp.path()).unwrap();
        let saved = store
            .save(SavedQuery {
                name: "My query".into(),
                connection_id: "c1".into(),
                sql: "SELECT 1".into(),
                ..Default::default()
            })
            .unwrap();
        assert!(!saved.id.is_empty() && !saved.created_at.is_empty() && saved.updated_at == saved.created_at);
        assert!(fs::read_to_string(&path).unwrap().contains("My query"));
        assert!(store.delete(&saved.id).unwrap());
        assert!(!fs::read_to_string(&path).unwrap().contains("My query"));
        assert!(SavedQueriesStore::open(tmp.path()).unwrap().list("").is_empty());
    }

    #[test]
    fn empty_file_is_an_array_and_missing_delete_is_false() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SavedQueriesStore::open(tmp.path()).unwrap();
        assert!(!store.delete("missing").unwrap());
        fs::write(tmp.path().join("saved_queries.json"), "[]").unwrap();
        assert!(SavedQueriesStore::open(tmp.path()).unwrap().list("").is_empty());
    }

    #[test]
    fn lists_by_scope_sorted_by_name() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SavedQueriesStore::open(tmp.path()).unwrap();
        for (name, conn) in [("beta", "c1"), ("Alpha", ""), ("gamma", "c2")] {
            store.save(SavedQuery { name: name.into(), connection_id: conn.into(), ..Default::default() }).unwrap();
        }
        let names = |list: Vec<SavedQuery>| list.into_iter().map(|q| q.name).collect::<Vec<_>>();
        assert_eq!(names(store.list("c1")), ["Alpha", "beta"]);
        assert_eq!(names(store.list("")), ["Alpha", "beta", "gamma"]);
    }
}
