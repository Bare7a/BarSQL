mod connections;
mod history;
mod json_file;
mod saved_queries;
mod session;
mod settings;

use std::io;
use std::path::Path;

pub use connections::{ConnectionFolder, ConnectionStore, ConnectionsFile, DEFAULT_COLOR, load_connections};
pub use history::{HISTORY_LIMIT, HistoryStore};
pub use json_file::{load_json_file, save_json_file, write_file_atomic};
pub use saved_queries::SavedQueriesStore;
pub use session::{EditorSession, EditorTab, SessionStore, TableViewRef};
pub use settings::SettingsStore;

pub struct Stores {
    pub connections: ConnectionStore,
    pub history: HistoryStore,
    pub saved_queries: SavedQueriesStore,
    pub session: SessionStore,
    pub settings: SettingsStore,
}

impl Stores {
    pub fn open(data_dir: &Path) -> io::Result<Self> {
        Ok(Self {
            connections: ConnectionStore::open(data_dir)?,
            history: HistoryStore::open(data_dir)?,
            saved_queries: SavedQueriesStore::open(data_dir)?,
            session: SessionStore::open(data_dir)?,
            settings: SettingsStore::open(data_dir)?,
        })
    }
}
