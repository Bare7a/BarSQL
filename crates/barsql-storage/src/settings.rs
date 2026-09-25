use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use crate::{load_json_file, save_json_file};

// Flat key/value preferences. Keys like "barsql-theme" are opaque here.
pub struct SettingsStore {
    path: PathBuf,
    values: Mutex<BTreeMap<String, String>>,
}

impl SettingsStore {
    pub fn open(data_dir: &Path) -> io::Result<Self> {
        let path = data_dir.join("settings.json");
        let values = load_json_file(&path)?;
        Ok(Self { path, values: Mutex::new(values) })
    }

    pub fn all(&self) -> BTreeMap<String, String> {
        self.lock().clone()
    }

    pub fn get(&self, key: &str) -> Option<String> {
        self.lock().get(key).cloned()
    }

    pub fn set(&self, key: &str, value: &str) -> io::Result<()> {
        let mut values = self.lock();
        values.insert(key.to_string(), value.to_string());
        save_json_file(&self.path, &*values)
    }

    pub fn delete(&self, key: &str) -> io::Result<()> {
        let mut values = self.lock();
        if values.remove(key).is_none() {
            return Ok(());
        }
        save_json_file(&self.path, &*values)
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<String, String>> {
        self.values.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn set_persists_and_reloads() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SettingsStore::open(tmp.path()).unwrap();
        store.set("barsql-theme", "light").unwrap();
        store.set("barsql-language", "de").unwrap();
        let body = fs::read_to_string(tmp.path().join("settings.json")).unwrap();
        assert!(body.contains("barsql-theme") && body.contains("light"), "{body}");
        let reloaded = SettingsStore::open(tmp.path()).unwrap().all();
        assert_eq!((reloaded["barsql-theme"].as_str(), reloaded["barsql-language"].as_str()), ("light", "de"));
    }

    #[test]
    fn all_returns_a_copy_and_delete_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SettingsStore::open(tmp.path()).unwrap();
        store.set("k", "v").unwrap();
        let mut copy = store.all();
        copy.insert("k".into(), "mutated".into());
        assert_eq!(store.get("k").as_deref(), Some("v"));
        store.delete("k").unwrap();
        assert!(store.get("k").is_none());
        store.delete("missing").unwrap();
    }

    #[test]
    fn recovers_from_a_corrupt_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.json");
        fs::write(&path, "{not valid json").unwrap();
        assert!(SettingsStore::open(tmp.path()).unwrap().all().is_empty());
        assert!(
            fs::read_dir(tmp.path()).unwrap().any(|e| e
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("settings.json.corrupt-"))
        );
    }
}
