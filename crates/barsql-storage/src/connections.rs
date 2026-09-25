use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use barsql_core::ConnectionConfig;
use serde::{Deserialize, Serialize};

use crate::{load_json_file, save_json_file};

pub const DEFAULT_COLOR: &str = "#3b82f6";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ConnectionsFile {
    #[serde(default)]
    pub connections: Vec<ConnectionConfig>,
    #[serde(default)]
    pub folders: Vec<ConnectionFolder>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionFolder {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
}

pub fn load_connections(data_dir: &Path) -> io::Result<ConnectionsFile> {
    load_json_file(&data_dir.join("connections.json"))
}

pub struct ConnectionStore {
    path: PathBuf,
    state: Mutex<ConnectionsFile>,
}

impl ConnectionStore {
    pub fn open(data_dir: &Path) -> io::Result<Self> {
        fs::create_dir_all(data_dir)?;
        let path = data_dir.join("connections.json");
        let state = load_json_file(&path)?;
        Ok(Self { path, state: Mutex::new(state) })
    }

    pub fn list(&self) -> Vec<ConnectionConfig> {
        self.lock().connections.clone()
    }

    pub fn get(&self, id: &str) -> Option<ConnectionConfig> {
        self.lock().connections.iter().find(|c| c.id == id).cloned()
    }

    pub fn save(&self, mut cfg: ConnectionConfig) -> io::Result<ConnectionConfig> {
        let mut state = self.lock();
        if cfg.id.is_empty() {
            cfg.id = uuid::Uuid::new_v4().to_string();
        }
        if cfg.color.is_empty() {
            cfg.color = DEFAULT_COLOR.into();
        }
        upsert(&mut state.connections, cfg.clone(), |c| &c.id);
        save_json_file(&self.path, &*state)?;
        Ok(cfg)
    }

    pub fn delete(&self, id: &str) -> io::Result<bool> {
        let mut state = self.lock();
        let before = state.connections.len();
        state.connections.retain(|c| c.id != id);
        if state.connections.len() == before {
            return Ok(false);
        }
        save_json_file(&self.path, &*state)?;
        Ok(true)
    }

    // Skips unknown ids. Connections missing from `ordered_ids` go last, in their current order.
    pub fn reorder(&self, ordered_ids: &[String]) -> io::Result<()> {
        let mut state = self.lock();
        if state.connections.is_empty() {
            return Ok(());
        }
        let by_id: HashMap<&str, &ConnectionConfig> = state.connections.iter().map(|c| (c.id.as_str(), c)).collect();
        let mut seen = HashSet::new();
        let mut reordered: Vec<ConnectionConfig> = Vec::with_capacity(state.connections.len());
        for id in ordered_ids {
            if let Some(c) = by_id.get(id.as_str()) {
                reordered.push((*c).clone());
                seen.insert(id.as_str());
            }
        }
        reordered.extend(state.connections.iter().filter(|c| !seen.contains(c.id.as_str())).cloned());
        state.connections = reordered;
        save_json_file(&self.path, &*state)
    }

    pub fn folders(&self) -> Vec<ConnectionFolder> {
        self.lock().folders.clone()
    }

    pub fn save_folder(&self, mut folder: ConnectionFolder) -> io::Result<ConnectionFolder> {
        let mut state = self.lock();
        if folder.id.is_empty() {
            folder.id = uuid::Uuid::new_v4().to_string();
        }
        upsert(&mut state.folders, folder.clone(), |f| &f.id);
        save_json_file(&self.path, &*state)?;
        Ok(folder)
    }

    pub fn delete_folder(&self, id: &str) -> io::Result<()> {
        let mut state = self.lock();
        state.folders.retain(|f| f.id != id);
        for c in state.connections.iter_mut().filter(|c| c.folder_id == id) {
            c.folder_id.clear();
        }
        save_json_file(&self.path, &*state)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ConnectionsFile> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

pub(crate) fn upsert<T>(items: &mut Vec<T>, item: T, id_of: impl Fn(&T) -> &String) {
    match items.iter().position(|existing| id_of(existing) == id_of(&item)) {
        Some(ix) => items[ix] = item,
        None => items.push(item),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::save_json_file;
    use barsql_core::DriverType;
    use serde_json::Value;

    const SAVED: &str = r##"{
  "connections": [
    {
      "id": "8f7c",
      "name": "Local PG",
      "driver": "postgres",
      "color": "#3b82f6",
      "folderId": "f1",
      "host": "127.0.0.1",
      "port": 5432,
      "database": "app",
      "username": "me",
      "password": "secret",
      "sslMode": "require",
      "readOnly": true,
      "ssh": {
        "enabled": true,
        "host": "bastion",
        "port": 22,
        "username": "ops",
        "auth": "key",
        "keyPath": "/home/me/.ssh/id_ed25519"
      },
      "futureField": {"kept": true}
    },
    {
      "id": "a1",
      "name": "Notes",
      "driver": "sqlite",
      "color": "",
      "filePath": "/tmp/notes.db",
      "ssh": {}
    }
  ],
  "folders": [
    {
      "id": "f1",
      "name": "Work"
    }
  ]
}"##;

    #[test]
    fn reads_saved_connections_and_keeps_unknown_fields() {
        let file: ConnectionsFile = serde_json::from_str(SAVED).unwrap();
        assert_eq!(file.connections.len(), 2);
        let pg = &file.connections[0];
        assert_eq!((pg.driver.clone(), pg.port, pg.read_only), (DriverType::Postgres, 5432, true));
        assert!(pg.ssh.enabled);
        assert_eq!(pg.ssh.key_path, "/home/me/.ssh/id_ed25519");
        assert_eq!(pg.extra.get("futureField"), Some(&serde_json::json!({"kept": true})));
        let round_trip: Value = serde_json::to_value(&file).unwrap();
        let original: Value = serde_json::from_str(SAVED).unwrap();
        assert_eq!(round_trip, original);
    }

    #[test]
    fn missing_file_is_empty_and_corrupt_file_is_backed_up() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        assert_eq!(load_connections(dir).unwrap(), ConnectionsFile::default());
        fs::write(dir.join("connections.json"), "{not json").unwrap();
        assert_eq!(load_connections(dir).unwrap(), ConnectionsFile::default());
        let names: Vec<String> =
            fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        assert!(!names.contains(&"connections.json".to_string()));
        assert!(names.iter().any(|n| n.starts_with("connections.json.corrupt-")));
    }

    #[test]
    fn saves_atomically_with_owner_only_permissions() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("connections.json");
        let file: ConnectionsFile = serde_json::from_str(SAVED).unwrap();
        save_json_file(&path, &file).unwrap();
        assert_eq!(load_connections(tmp.path()).unwrap(), file);
        assert_eq!(fs::read_dir(tmp.path()).unwrap().count(), 1);
    }

    fn named(id: &str) -> ConnectionConfig {
        ConnectionConfig { id: id.into(), name: id.to_uppercase(), ..Default::default() }
    }

    #[test]
    fn reorder_persists() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ConnectionStore::open(tmp.path()).unwrap();
        for id in ["a", "b", "c"] {
            store.save(named(id)).unwrap();
        }
        store.reorder(&["c".into(), "a".into(), "b".into()]).unwrap();
        let ids = |list: Vec<ConnectionConfig>| list.into_iter().map(|c| c.id).collect::<Vec<_>>();
        assert_eq!(ids(store.list()), ["c", "a", "b"]);
        assert_eq!(ids(ConnectionStore::open(tmp.path()).unwrap().list()), ["c", "a", "b"]);
        store.reorder(&["zzz".into(), "b".into()]).unwrap();
        assert_eq!(ids(store.list()), ["b", "c", "a"]);
    }

    #[test]
    fn save_assigns_ids_and_colours_and_delete_persists() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ConnectionStore::open(tmp.path()).unwrap();
        let fresh = store.save(ConnectionConfig { name: "New".into(), ..Default::default() }).unwrap();
        assert!(!fresh.id.is_empty());
        assert_eq!(fresh.color, DEFAULT_COLOR);
        let saved = store
            .save(ConnectionConfig {
                id: "c1".into(),
                name: "One".into(),
                driver: DriverType::Sqlite,
                file_path: "/tmp/x.db".into(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(saved.id, "c1");
        assert!(store.delete("c1").unwrap());
        assert!(!store.delete("c1").unwrap());
        let raw = fs::read_to_string(tmp.path().join("connections.json")).unwrap();
        assert!(!raw.contains("\"c1\"") && !raw.contains("\"One\""), "{raw}");
        assert_eq!(ConnectionStore::open(tmp.path()).unwrap().list().len(), 1);
    }

    #[test]
    fn deleting_a_folder_unfiles_its_connections() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ConnectionStore::open(tmp.path()).unwrap();
        let folder = store.save_folder(ConnectionFolder { name: "Work".into(), ..Default::default() }).unwrap();
        store.save(ConnectionConfig { id: "c".into(), folder_id: folder.id.clone(), ..Default::default() }).unwrap();
        store.delete_folder(&folder.id).unwrap();
        assert!(store.folders().is_empty());
        assert_eq!(store.get("c").unwrap().folder_id, "");
    }
}
