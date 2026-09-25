use std::sync::Arc;

use barsql_core::{QueryError, ResultSummary};
use barsql_sql::QueryPlan;

use crate::{ColumnMeta, ResultChunk};

pub const BATCH_ROWS: usize = 5000;

// Every result set ends with a Result. A plan statement sends just one Result, carrying the plan and no rows.
#[derive(Debug)]
pub enum ScriptEvent {
    Meta { result_index: usize, columns: Arc<[ColumnMeta]> },
    Rows { result_index: usize, chunk: Arc<ResultChunk> },
    Result(Box<StatementResult>),
}

#[derive(Debug, Clone, Default)]
pub struct StatementResult {
    pub result_index: usize,
    pub statement: String,
    pub summary: Option<ResultSummary>,
    pub plan: Option<QueryPlan>,
    pub error: Option<QueryError>,
}

pub type Sink = async_channel::Sender<ScriptEvent>;

pub(crate) async fn emit(sink: &Sink, event: ScriptEvent) {
    let _ = sink.send(event).await;
}
