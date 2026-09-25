use std::sync::Arc;
use std::time::Instant;

use barsql_core::{DriverType, QueryError, ResultSummary, Value};
use barsql_sql::{PlanRows, detect_plan_request, first_keyword, parse_plan, strip_leading_comments};

use crate::event::{BATCH_ROWS, ScriptEvent, Sink, StatementResult, emit};
use crate::{Cancel, ColumnMeta, ResultChunk};

#[derive(Debug, Default)]
pub struct StatementRun {
    pub sets: usize,
    pub error: Option<QueryError>,
}

pub(crate) trait StatementRunner {
    fn driver(&self) -> DriverType;

    // Emits Meta, Rows and Result per result set, numbered from `first_index`.
    async fn stream_statement(
        &mut self,
        stmt: &str,
        first_index: usize,
        batch_rows: usize,
        sink: &Sink,
        cancel: &Cancel,
    ) -> StatementRun;
}

// Held in memory, so only for plans, catalog reads and small helper queries.
#[derive(Debug, Default)]
pub struct Buffered {
    pub columns: Arc<[ColumnMeta]>,
    pub chunk: ResultChunk,
    pub summary: Option<ResultSummary>,
}

impl Buffered {
    pub fn rows(&self) -> usize {
        self.chunk.rows()
    }

    pub fn values(&self) -> Vec<Vec<Value>> {
        (0..self.chunk.rows())
            .map(|r| (0..self.chunk.columns()).map(|c| self.chunk.cell(r, c).to_value()).collect())
            .collect()
    }

    pub fn text(&self, row: usize, col: usize) -> Option<&str> {
        if row >= self.chunk.rows() || col >= self.chunk.columns() {
            return None;
        }
        self.chunk.display(row, col)
    }

    pub fn column(&self, name: &str) -> Option<usize> {
        self.columns.iter().position(|c| c.name.eq_ignore_ascii_case(name))
    }

    pub fn plan_rows(&self) -> PlanRows {
        PlanRows { columns: self.columns.iter().map(|c| c.name.clone()).collect(), rows: self.values() }
    }
}

pub(crate) async fn buffered<R: StatementRunner>(
    runner: &mut R,
    sql: &str,
    cancel: &Cancel,
) -> Result<Buffered, QueryError> {
    let (tx, rx) = async_channel::unbounded();
    let run = runner.stream_statement(sql, 0, usize::MAX, &tx, cancel).await;
    drop(tx);
    let mut out = Buffered::default();
    let mut sets = 0;
    while let Ok(event) = rx.try_recv() {
        match event {
            ScriptEvent::Meta { columns, .. } if sets == 0 => out.columns = columns,
            ScriptEvent::Rows { chunk, .. } if sets == 0 => out.chunk = Arc::try_unwrap(chunk).unwrap_or_default(),
            ScriptEvent::Result(result) => {
                if sets == 0 {
                    out.summary = result.summary;
                }
                sets += 1;
            }
            _ => {}
        }
    }
    match run.error {
        Some(err) => Err(err),
        None => Ok(out),
    }
}

// Stops at the first failing statement. Result indexes keep counting across statements.
pub(crate) async fn run_script<R: StatementRunner>(
    runner: &mut R,
    statements: &[String],
    sink: &Sink,
    cancel: &Cancel,
) -> Result<usize, QueryError> {
    let driver = runner.driver();
    let mut next_index = 0;
    for stmt in statements {
        if cancel.is_cancelled() {
            let error = QueryError::cancelled();
            emit(
                sink,
                ScriptEvent::Result(Box::new(StatementResult {
                    result_index: next_index,
                    statement: stmt.clone(),
                    error: Some(error.clone()),
                    ..Default::default()
                })),
            )
            .await;
            return Err(error);
        }
        if let Some(request) = detect_plan_request(&driver, stmt) {
            let error = run_plan(runner, &driver, stmt, &request.sql, request.analyze, next_index, sink, cancel).await;
            next_index += 1;
            if let Some(error) = error {
                return Err(error);
            }
            continue;
        }
        let run = runner.stream_statement(stmt, next_index, BATCH_ROWS, sink, cancel).await;
        next_index += run.sets;
        if let Some(error) = run.error {
            return Err(error);
        }
    }
    Ok(next_index)
}

// Sends the normalized plan instead of rows. Output the parsers don't recognize falls back to the
// engine's own rows.
#[allow(clippy::too_many_arguments)]
async fn run_plan<R: StatementRunner>(
    runner: &mut R,
    driver: &DriverType,
    stmt: &str,
    explain_sql: &str,
    analyze: bool,
    index: usize,
    sink: &Sink,
    cancel: &Cancel,
) -> Option<QueryError> {
    let started = Instant::now();
    let res = match buffered(runner, explain_sql, cancel).await {
        Ok(res) => res,
        Err(error) => {
            emit(
                sink,
                ScriptEvent::Result(Box::new(StatementResult {
                    result_index: index,
                    statement: stmt.into(),
                    error: Some(error.clone()),
                    ..Default::default()
                })),
            )
            .await;
            return Some(error);
        }
    };
    match parse_plan(driver, stmt, explain_sql, analyze, &res.plan_rows()) {
        Ok(mut plan) => {
            plan.duration_ms = started.elapsed().as_millis() as i64;
            emit(
                sink,
                ScriptEvent::Result(Box::new(StatementResult {
                    result_index: index,
                    statement: stmt.into(),
                    plan: Some(plan),
                    ..Default::default()
                })),
            )
            .await;
        }
        Err(_) => {
            emit(sink, ScriptEvent::Meta { result_index: index, columns: res.columns.clone() }).await;
            let summary = res.summary.clone();
            if res.rows() > 0 {
                emit(sink, ScriptEvent::Rows { result_index: index, chunk: Arc::new(res.chunk) }).await;
            }
            emit(
                sink,
                ScriptEvent::Result(Box::new(StatementResult {
                    result_index: index,
                    statement: stmt.into(),
                    summary,
                    ..Default::default()
                })),
            )
            .await;
        }
    }
    None
}

pub(crate) fn summary_for_rows(columns: &[ColumnMeta], rows: usize, started: Instant) -> ResultSummary {
    ResultSummary {
        columns: columns.iter().map(|c| c.name.clone()).collect(),
        column_types: columns.iter().map(|c| c.type_name.clone()).collect(),
        row_count: rows as i64,
        duration_ms: started.elapsed().as_millis() as i64,
        ..Default::default()
    }
}

pub(crate) fn summary_for_exec(affected: u64, started: Instant) -> ResultSummary {
    ResultSummary {
        affected_rows: affected as i64,
        duration_ms: started.elapsed().as_millis() as i64,
        message: format!("{affected} row(s) affected"),
        ..Default::default()
    }
}

// After any of these the session re-checks its transaction state.
pub(crate) fn touches_transaction(stmt: &str) -> bool {
    matches!(
        first_keyword(strip_leading_comments(stmt)).as_str(),
        "BEGIN"
            | "START"
            | "COMMIT"
            | "END"
            | "ROLLBACK"
            | "ABORT"
            | "PREPARE"
            | "SAVEPOINT"
            | "RELEASE"
            | "SET"
            | "CALL"
            | "DO"
    )
}
