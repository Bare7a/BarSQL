#![allow(dead_code)]

use std::path::Path;
use std::time::Duration;

use barsql_app::{BarApp, ImportDone, ImportEvent, ImportHandle, ImportProgress, RunEvent, RunHandle};
use barsql_core::{ConnectionConfig, DriverType, Value};
use tempfile::TempDir;

pub struct Fixture {
    pub app: BarApp,
    pub dir: TempDir,
}

impl Fixture {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let app = BarApp::open(&dir.path().join("data"), tokio::runtime::Handle::current()).unwrap();
        Self { app, dir }
    }

    pub fn path(&self, name: &str) -> String {
        self.dir.path().join(name).display().to_string()
    }

    pub fn sqlite_config(&self) -> ConnectionConfig {
        ConnectionConfig {
            name: "test".into(),
            driver: DriverType::Sqlite,
            file_path: self.path("test.db"),
            ..Default::default()
        }
    }

    pub async fn sqlite(&self) -> String {
        self.app.save_connection(self.sqlite_config()).await.unwrap().id
    }

    pub async fn exec(&self, connection_id: &str, sql: &str) {
        if let Err(err) = self.app.execute_query(connection_id, sql).await {
            panic!("exec {sql:?}: {err:?}");
        }
    }

    // Rows as text, NULL as "NULL".
    pub async fn query(&self, connection_id: &str, sql: &str) -> Vec<Vec<String>> {
        let res = self.app.execute_query(connection_id, sql).await.unwrap_or_else(|e| panic!("query {sql:?}: {e:?}"));
        res.rows.iter().map(|row| row.iter().map(text).collect()).collect()
    }

    pub fn write(&self, name: &str, content: &str) -> String {
        let path = self.path(name);
        std::fs::write(&path, content).unwrap();
        path
    }
}

pub fn text(v: &Value) -> String {
    match v {
        Value::Null => "NULL".into(),
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Text(s) => s.clone(),
    }
}

pub async fn collect(handle: RunHandle) -> Vec<RunEvent> {
    let mut events = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(30), handle.events.recv())
            .await
            .expect("the run must finish")
            .expect("the run must end with Done");
        let done = matches!(event, RunEvent::Done { .. });
        events.push(event);
        if done {
            return events;
        }
    }
}

pub async fn import_outcome(handle: ImportHandle) -> (Vec<ImportProgress>, ImportDone) {
    let mut progress = Vec::new();
    loop {
        match tokio::time::timeout(Duration::from_secs(30), handle.events.recv())
            .await
            .expect("import must finish")
            .expect("done")
        {
            ImportEvent::Progress(p) => progress.push(p),
            ImportEvent::Done(done) => return (progress, done),
        }
    }
}

pub fn results(events: &[RunEvent]) -> Vec<&barsql_app::RunResult> {
    events
        .iter()
        .filter_map(|e| match e {
            RunEvent::Result(r) => Some(r.as_ref()),
            _ => None,
        })
        .collect()
}

pub fn rows(events: &[RunEvent], index: usize) -> Vec<Vec<Option<String>>> {
    let mut out = Vec::new();
    for event in events {
        if let RunEvent::Rows { result_index, chunk } = event
            && *result_index == index
        {
            for r in 0..chunk.rows() {
                out.push((0..chunk.columns()).map(|c| chunk.display(r, c).map(str::to_string)).collect());
            }
        }
    }
    out
}

pub fn file_exists(path: &str) -> bool {
    Path::new(path).exists()
}
