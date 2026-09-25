#![allow(clippy::result_large_err)]

mod backup;
mod connections;
mod edits;
mod events;
mod files;
mod import;
mod info;
mod jobs;
mod runs;
mod stores;
mod tabs;
pub mod update;

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use barsql_core::{ConnectionConfig, QueryError};
use barsql_db::Engine;
use barsql_storage::Stores;

pub use backup::{BackupOutcome, BackupRequest};
pub use barsql_io::{CsvOptions, ImportPreview};
pub use barsql_storage::{ConnectionFolder, EditorSession, EditorTab, TableViewRef};
pub use events::{AppEvent, ImportDone, ImportEvent, ImportHandle, ImportProgress, RunEvent, RunHandle, RunResult};
pub use files::{find_sqlite_arg, is_sqlite_file, path_from_file_url, sqlite_file_payload};
pub use import::{CsvImportRequest, ImportResult, SqlImportRequest};
pub use info::{AppInfo, PathDefaults, app_info, path_defaults};
pub use runs::BufferedResult;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

// Kept free of GPUI. Methods run on the tokio runtime and streaming ones return a handle with a channel.
#[derive(Clone)]
pub struct BarApp {
    inner: Arc<Inner>,
}

struct Inner {
    stores: Stores,
    runtime: tokio::runtime::Handle,
    engines: Mutex<HashMap<String, PooledEngine>>,
    connect_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    tabs: tabs::Tabs,
    jobs: jobs::Jobs,
    events: (async_channel::Sender<AppEvent>, async_channel::Receiver<AppEvent>),
    pending_file: Mutex<Option<String>>,
}

#[derive(Clone)]
struct PooledEngine {
    engine: Engine,
    fingerprint: String,
}

impl BarApp {
    pub fn open(data_dir: &Path, runtime: tokio::runtime::Handle) -> std::io::Result<Self> {
        let app = Self {
            inner: Arc::new(Inner {
                stores: Stores::open(data_dir)?,
                runtime,
                engines: Mutex::new(HashMap::new()),
                connect_locks: Mutex::new(HashMap::new()),
                tabs: tabs::Tabs::default(),
                jobs: jobs::Jobs::default(),
                events: async_channel::unbounded(),
                pending_file: Mutex::new(None),
            }),
        };
        app.spawn_idle_sweep();
        Ok(app)
    }

    // Holds only a weak reference, so it ends with the app.
    fn spawn_idle_sweep(&self) {
        let inner = Arc::downgrade(&self.inner);
        self.inner.runtime.spawn(async move {
            let mut tick = tokio::time::interval(tabs::TAB_SWEEP_INTERVAL);
            tick.tick().await;
            loop {
                tick.tick().await;
                let Some(inner) = inner.upgrade() else { return };
                BarApp { inner }.release_idle_tabs(tabs::TAB_IDLE_TIMEOUT);
            }
        });
    }

    pub fn runtime(&self) -> &tokio::runtime::Handle {
        &self.inner.runtime
    }

    pub fn events(&self) -> async_channel::Receiver<AppEvent> {
        self.inner.events.1.clone()
    }

    pub(crate) fn emit(&self, event: AppEvent) {
        let _ = self.inner.events.0.try_send(event);
    }

    pub(crate) fn config(&self, connection_id: &str) -> Result<ConnectionConfig, QueryError> {
        self.inner.stores.connections.get(connection_id).ok_or_else(|| QueryError::message("connection not found"))
    }

    // Rolls back open transactions before the pools close.
    pub async fn shutdown(&self) {
        self.inner.jobs.cancel_all();
        let tab_ids = self.inner.tabs.ids();
        for tab_id in tab_ids {
            self.cleanup_tab(&tab_id).await;
        }
        let engines: Vec<Engine> =
            self.inner.engines.lock().unwrap_or_else(|e| e.into_inner()).drain().map(|(_, p)| p.engine).collect();
        for engine in engines {
            engine.close().await;
        }
    }
}

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}
