use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use barsql_core::HistoryEntry;
use barsql_core::clock::now_rfc3339;

use crate::{load_json_file, save_json_file};

pub const HISTORY_LIMIT: usize = 500;

// Stored oldest-first so appends are cheap. Listing walks back from the end for newest-first.
pub struct HistoryStore {
    path: PathBuf,
    entries: Mutex<Vec<HistoryEntry>>,
}

impl HistoryStore {
    pub fn open(data_dir: &Path) -> io::Result<Self> {
        let path = data_dir.join("query_history.json");
        let mut entries: Vec<HistoryEntry> = load_json_file(&path)?;
        // A hand-edited file shouldn't stay oversized until the next add.
        trim(&mut entries);
        Ok(Self { path, entries: Mutex::new(entries) })
    }

    pub fn add(&self, mut entry: HistoryEntry) -> io::Result<HistoryEntry> {
        let mut entries = self.lock();
        if entry.id.is_empty() {
            entry.id = uuid::Uuid::new_v4().to_string();
        }
        if entry.executed_at.is_empty() {
            entry.executed_at = now_rfc3339();
        }
        entries.push(entry.clone());
        trim(&mut entries);
        save_json_file(&self.path, &*entries)?;
        Ok(entry)
    }

    pub fn list(&self, connection_id: &str, limit: usize) -> Vec<HistoryEntry> {
        let limit = if limit == 0 { 100 } else { limit };
        self.lock()
            .iter()
            .rev()
            .filter(|e| connection_id.is_empty() || e.connection_id == connection_id)
            .take(limit)
            .cloned()
            .collect()
    }

    pub fn delete(&self, id: &str) -> io::Result<bool> {
        let mut entries = self.lock();
        let before = entries.len();
        entries.retain(|e| e.id != id);
        if entries.len() == before {
            return Ok(false);
        }
        save_json_file(&self.path, &*entries)?;
        Ok(true)
    }

    pub fn clear(&self, connection_id: &str) -> io::Result<()> {
        let mut entries = self.lock();
        if connection_id.is_empty() {
            entries.clear();
        } else {
            entries.retain(|e| e.connection_id != connection_id);
        }
        save_json_file(&self.path, &*entries)
    }

    fn lock(&self) -> MutexGuard<'_, Vec<HistoryEntry>> {
        self.entries.lock().unwrap_or_else(|e| e.into_inner())
    }
}

fn trim(entries: &mut Vec<HistoryEntry>) {
    if entries.len() > HISTORY_LIMIT {
        entries.drain(..entries.len() - HISTORY_LIMIT);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn entry(connection_id: &str, sql: &str) -> HistoryEntry {
        HistoryEntry { connection_id: connection_id.into(), sql: sql.into(), ..Default::default() }
    }

    #[test]
    fn clear_persists_per_connection() {
        let tmp = tempfile::tempdir().unwrap();
        let h = HistoryStore::open(tmp.path()).unwrap();
        h.add(entry("c1", "SELECT 1")).unwrap();
        h.add(entry("c2", "SELECT 2")).unwrap();
        h.clear("c1").unwrap();
        let body = fs::read_to_string(tmp.path().join("query_history.json")).unwrap();
        assert!(!body.contains("\"connectionId\": \"c1\"") && !body.contains("SELECT 1"), "{body}");
        assert!(body.contains("\"connectionId\": \"c2\""), "{body}");
        let reloaded = HistoryStore::open(tmp.path()).unwrap();
        assert!(reloaded.list("c1", 100).is_empty());
        assert_eq!(reloaded.list("c2", 100).len(), 1);
    }

    #[test]
    fn clear_all_writes_an_empty_array() {
        let tmp = tempfile::tempdir().unwrap();
        let h = HistoryStore::open(tmp.path()).unwrap();
        h.add(entry("c1", "SELECT 1")).unwrap();
        h.clear("").unwrap();
        assert_eq!(fs::read_to_string(tmp.path().join("query_history.json")).unwrap().trim(), "[]");
    }

    #[test]
    fn keeps_the_newest_500_and_lists_newest_first() {
        let tmp = tempfile::tempdir().unwrap();
        let h = HistoryStore::open(tmp.path()).unwrap();
        for i in 0..HISTORY_LIMIT + 5 {
            h.add(entry(if i % 2 == 0 { "a" } else { "b" }, &format!("SELECT {i}"))).unwrap();
        }
        let all = h.list("", HISTORY_LIMIT + 50);
        assert_eq!(all.len(), HISTORY_LIMIT);
        assert_eq!(all[0].sql, format!("SELECT {}", HISTORY_LIMIT + 4));
        assert_eq!(all.last().unwrap().sql, "SELECT 5");
        assert_eq!(h.list("a", 0).len(), 100);
        assert!(h.list("b", 3).iter().all(|e| e.connection_id == "b"));
        let first = all[0].clone();
        assert!(!first.id.is_empty() && !first.executed_at.is_empty());
        assert!(h.delete(&first.id).unwrap());
        assert!(!h.delete(&first.id).unwrap());
    }
}
