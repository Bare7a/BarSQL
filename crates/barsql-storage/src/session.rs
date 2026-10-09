use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use serde::{Deserialize, Serialize};

use crate::{load_json_file, save_json_file};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TableViewRef {
    #[serde(default)]
    pub schema: String,
    #[serde(default)]
    pub table: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub filter: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub order_by: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub order_dir: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hidden_columns: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditorTab {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub connection_id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub sql: String,
    #[serde(default)]
    pub color: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub saved_query_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub saved_sql_baseline: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub table_view: Option<TableViewRef>,
    // Pinned tabs sit first and stay open through Close Others, To the Right and All.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pinned: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditorSession {
    #[serde(default, deserialize_with = "null_as_default")]
    pub tabs: Vec<EditorTab>,
    #[serde(default)]
    pub active_tab: String,
}

fn null_as_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

pub struct SessionStore {
    path: PathBuf,
    session: Mutex<EditorSession>,
}

impl SessionStore {
    pub fn open(data_dir: &Path) -> io::Result<Self> {
        let path = data_dir.join("editor_session.json");
        let session = load_json_file(&path)?;
        Ok(Self { path, session: Mutex::new(session) })
    }

    pub fn get(&self) -> EditorSession {
        self.lock().clone()
    }

    pub fn save(&self, session: EditorSession) -> io::Result<()> {
        let mut current = self.lock();
        *current = session;
        save_json_file(&self.path, &*current)
    }

    fn lock(&self) -> MutexGuard<'_, EditorSession> {
        self.session.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn save_persists_tabs_and_table_views() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SessionStore::open(tmp.path()).unwrap();
        let session = EditorSession {
            tabs: vec![
                EditorTab {
                    id: "t1".into(),
                    connection_id: "c1".into(),
                    title: "Query 1".into(),
                    sql: "SELECT 1".into(),
                    color: "#3b82f6".into(),
                    pinned: true,
                    ..Default::default()
                },
                EditorTab {
                    id: "tv1".into(),
                    connection_id: "c1".into(),
                    title: "users".into(),
                    table_view: Some(TableViewRef {
                        schema: "public".into(),
                        table: "users".into(),
                        filter: "age > 30".into(),
                        order_by: "name".into(),
                        order_dir: "DESC".into(),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            ],
            active_tab: "t1".into(),
        };
        store.save(session.clone()).unwrap();
        let body = fs::read_to_string(tmp.path().join("editor_session.json")).unwrap();
        assert!(body.contains("Query 1") && body.contains("\"orderBy\": \"name\""), "{body}");
        assert_eq!(SessionStore::open(tmp.path()).unwrap().get(), session);
    }

    #[test]
    fn empty_tabs_are_written_as_an_array() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SessionStore::open(tmp.path()).unwrap();
        store.save(EditorSession::default()).unwrap();
        let body = fs::read_to_string(tmp.path().join("editor_session.json")).unwrap();
        assert!(body.contains("\"tabs\": []"), "{body}");
        fs::write(tmp.path().join("editor_session.json"), r#"{"tabs":null,"activeTab":"x"}"#).unwrap();
        let loaded = SessionStore::open(tmp.path()).unwrap().get();
        assert_eq!((loaded.tabs.len(), loaded.active_tab.as_str()), (0, "x"));
    }
}
