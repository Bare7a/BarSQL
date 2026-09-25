use std::sync::Arc;

use barsql_core::{QueryError, ResultSummary};
use barsql_db::{ColumnMeta, ResultChunk};
use barsql_sql::QueryPlan;
use serde::Serialize;

use crate::import::ImportResult;

// Arrives in order and already filtered to one run.
#[derive(Debug)]
pub enum RunEvent {
    Meta { result_index: usize, columns: Arc<[ColumnMeta]>, schema_name: String, table_name: String },
    Rows { result_index: usize, chunk: Arc<ResultChunk> },
    Result(Box<RunResult>),
    // Ends the run. `error` is a run-level failure, per-statement errors come in Result.
    Done { result_count: usize, error: Option<QueryError> },
}

#[derive(Debug, Clone, Default)]
pub struct RunResult {
    pub result_index: usize,
    pub summary: Option<ResultSummary>,
    pub plan: Option<QueryPlan>,
    pub statement: String,
    pub error: Option<QueryError>,
}

pub struct RunHandle {
    // Increases with each run, so a later run supersedes an earlier one.
    pub stream_id: u64,
    pub events: async_channel::Receiver<RunEvent>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportProgress {
    pub processed: i64,
    pub inserted: i64,
    pub skipped: i64,
    pub bytes_read: i64,
    pub total_bytes: i64,
    // 0 when unknown, in which case bytes_read drives the progress bar.
    pub total_rows: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ImportDone {
    pub result: Option<ImportResult>,
    pub error: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportEvent {
    Progress(ImportProgress),
    Done(ImportDone),
}

pub struct ImportHandle {
    pub events: async_channel::Receiver<ImportEvent>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppEvent {
    // Sent when a connection closes under these tabs and rolls back their transactions.
    TransactionsEnded { tab_ids: Vec<String> },
    OpenSqlite { file_path: String, name: String },
    UpdateAvailable { version: String },
    // Another launch asked for the window.
    Activate,
}
