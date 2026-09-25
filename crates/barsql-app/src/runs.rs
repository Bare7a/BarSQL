use std::time::Instant;

use barsql_core::{DriverType, HistoryEntry, QueryError, TableDataRequest, Value};
use barsql_db::{Cancel, ScriptEvent, Session};
use barsql_sql::plan::{NOTE_ROLLED_BACK, NOTE_TAB_TRANSACTION};
use barsql_sql::{
    QueryPlan, ServerVersion, assert_read_only, build_explain_sql, is_read_only, parse_plan, single_statement,
    split_statement_texts,
};

use crate::BarApp;
use crate::events::{RunEvent, RunHandle, RunResult};

const RUN_CHANNEL: usize = 16;

// Only used by the tests.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BufferedResult {
    pub columns: Vec<String>,
    pub column_types: Vec<String>,
    pub rows: Vec<Vec<Value>>,
    pub row_count: i64,
    pub affected_rows: i64,
    pub message: String,
    pub duration_ms: i64,
}

impl BarApp {
    fn guard_execute(&self, connection_id: &str, sql: &str) -> Result<DriverType, QueryError> {
        let cfg = self.config(connection_id)?;
        if cfg.read_only {
            assert_read_only(&cfg.driver, sql).map_err(QueryError::message)?;
        }
        Ok(cfg.driver)
    }

    pub(crate) fn record_history(&self, connection_id: &str, sql: &str, duration_ms: i64, error: Option<&QueryError>) {
        let _ = self.inner.stores.history.add(HistoryEntry {
            connection_id: connection_id.to_string(),
            sql: sql.to_string(),
            duration_ms,
            success: error.is_none(),
            error: error.map(|e| e.message.clone()).unwrap_or_default(),
            ..Default::default()
        });
    }

    // Runs on the tab's own session and ends with Done. A new run cancels the tab's previous one.
    pub async fn execute_query_stream(
        &self,
        connection_id: &str,
        tab_id: &str,
        sql: &str,
    ) -> Result<RunHandle, QueryError> {
        if tab_id.is_empty() {
            return Err(QueryError::message("tab id is required"));
        }
        self.guard_execute(connection_id, sql)?;
        // Split with the connection's dialect so the boundaries match the editor's run glyphs.
        let engine = self.engine(connection_id).await?;
        let statements = split_statement_texts(&engine.driver(), sql);
        let (stream_id, cancel) = self.inner.jobs.start(tab_id, connection_id);
        let (tx, rx) = async_channel::bounded(RUN_CHANNEL);
        let app = self.clone();
        let (tab, connection) = (tab_id.to_string(), connection_id.to_string());
        self.inner.runtime.spawn(async move {
            let done = match app.run_batch(&tab, &connection, &statements, &tx, &cancel).await {
                Ok(result_count) => RunEvent::Done { result_count, error: None },
                Err(error) => RunEvent::Done { result_count: 0, error: Some(error) },
            };
            // Ended first, so a cancel after Done finds nothing running.
            app.inner.jobs.end(&tab, stream_id);
            let _ = tx.send(done).await;
        });
        Ok(RunHandle { stream_id, events: rx })
    }

    async fn run_batch(
        &self,
        tab_id: &str,
        connection_id: &str,
        statements: &[String],
        events: &async_channel::Sender<RunEvent>,
        cancel: &Cancel,
    ) -> Result<usize, QueryError> {
        let mut slot = self.tab_session(tab_id, connection_id).await?;
        let session = slot.session.as_mut().expect("tab session");
        let (inner_tx, inner_rx) = async_channel::bounded(RUN_CHANNEL);
        let run = async move {
            let result = session.run_script(statements, &inner_tx, cancel).await;
            drop(inner_tx);
            result
        };
        let forward = async {
            let mut history = Vec::new();
            let mut result_count = 0;
            while let Ok(event) = inner_rx.recv().await {
                let event = match event {
                    ScriptEvent::Meta { result_index, columns } => {
                        RunEvent::Meta { result_index, columns, schema_name: String::new(), table_name: String::new() }
                    }
                    ScriptEvent::Rows { result_index, chunk } => RunEvent::Rows { result_index, chunk },
                    ScriptEvent::Result(result) => {
                        result_count = result.result_index + 1;
                        let duration = result.summary.as_ref().map_or(0, |s| s.duration_ms);
                        history.push((result.statement.clone(), duration, result.error.clone()));
                        RunEvent::Result(Box::new(RunResult {
                            result_index: result.result_index,
                            summary: result.summary,
                            plan: result.plan,
                            statement: result.statement,
                            error: result.error,
                        }))
                    }
                };
                // A dropped handle cancels the run rather than letting it finish.
                if events.send(event).await.is_err() {
                    cancel.cancel();
                }
            }
            (result_count, history)
        };
        let (_, (result_count, history)) = tokio::join!(run, forward);
        self.sync_transaction(tab_id, &mut slot);
        drop(slot);
        // After the script, so storage writes stay off the connection.
        for (statement, duration, error) in history {
            self.record_history(connection_id, &statement, duration, error.as_ref());
        }
        Ok(result_count)
    }

    pub fn cancel_query(&self, key: &str) -> bool {
        self.inner.jobs.cancel(key)
    }

    // Always one result set, with the primary keys attached for editing.
    pub async fn query_table_stream(
        &self,
        connection_id: &str,
        tab_id: &str,
        mut req: TableDataRequest,
    ) -> Result<RunHandle, QueryError> {
        if tab_id.is_empty() {
            return Err(QueryError::message("tab id is required"));
        }
        if req.limit <= 0 {
            req.limit = 100;
        }
        let (stream_id, cancel) = self.inner.jobs.start(tab_id, connection_id);
        let (tx, rx) = async_channel::bounded(RUN_CHANNEL);
        let app = self.clone();
        let (tab, connection) = (tab_id.to_string(), connection_id.to_string());
        self.inner.runtime.spawn(async move {
            let done = match app.engine(&connection).await {
                Err(error) => RunEvent::Done { result_count: 0, error: Some(error) },
                Ok(engine) => {
                    if let Err(error) = app.stream_table_page(&engine, &req, &tx, &cancel).await {
                        let result = RunResult { error: Some(error), ..Default::default() };
                        let _ = tx.send(RunEvent::Result(Box::new(result))).await;
                    }
                    RunEvent::Done { result_count: 1, error: None }
                }
            };
            app.inner.jobs.end(&tab, stream_id);
            let _ = tx.send(done).await;
        });
        Ok(RunHandle { stream_id, events: rx })
    }

    async fn stream_table_page(
        &self,
        engine: &barsql_db::Engine,
        req: &TableDataRequest,
        events: &async_channel::Sender<RunEvent>,
        cancel: &Cancel,
    ) -> Result<(), QueryError> {
        let query = engine.table_query(req).await?;
        let mut session = engine.pooled_session().await?;
        let (inner_tx, inner_rx) = async_channel::bounded(RUN_CHANNEL);
        let run = async {
            let result = session.stream(&query.sql, &inner_tx, cancel).await;
            drop(inner_tx);
            result
        };
        let forward = async {
            while let Ok(event) = inner_rx.recv().await {
                let event = match event {
                    ScriptEvent::Meta { result_index, columns } => RunEvent::Meta {
                        result_index,
                        columns,
                        schema_name: req.schema.clone(),
                        table_name: req.table.clone(),
                    },
                    ScriptEvent::Rows { result_index, chunk } => RunEvent::Rows { result_index, chunk },
                    ScriptEvent::Result(result) => {
                        let mut summary = result.summary.unwrap_or_default();
                        summary.primary_keys = query.primary_keys.clone();
                        summary.table_name = req.table.clone();
                        summary.schema_name = query.schema.clone();
                        RunEvent::Result(Box::new(RunResult {
                            result_index: 0,
                            summary: Some(summary),
                            error: result.error,
                            ..Default::default()
                        }))
                    }
                };
                if events.send(event).await.is_err() {
                    cancel.cancel();
                }
            }
        };
        // Errors already went out on the Result event.
        let _ = tokio::join!(run, forward);
        Ok(())
    }

    pub async fn begin_transaction(&self, connection_id: &str, tab_id: &str) -> Result<(), QueryError> {
        if tab_id.is_empty() {
            return Err(QueryError::message("tab id is required"));
        }
        if self.inner.tabs.in_transaction(tab_id) {
            return Err(QueryError::message("transaction already active on this tab"));
        }
        self.guard_execute(connection_id, "BEGIN")?;
        let mut slot = self.tab_session(tab_id, connection_id).await?;
        let session = slot.session.as_mut().expect("tab session");
        if session.in_transaction() {
            self.sync_transaction(tab_id, &mut slot);
            return Err(QueryError::message("transaction already active on this tab"));
        }
        let result = session.begin().await;
        self.sync_transaction(tab_id, &mut slot);
        result
    }

    pub async fn commit_transaction(&self, tab_id: &str) -> Result<(), QueryError> {
        self.finish_transaction(tab_id, true).await
    }

    pub async fn rollback_transaction(&self, tab_id: &str) -> Result<(), QueryError> {
        self.finish_transaction(tab_id, false).await
    }

    async fn finish_transaction(&self, tab_id: &str, commit: bool) -> Result<(), QueryError> {
        let missing = || QueryError::message("no active transaction on this tab");
        let slot = self.inner.tabs.existing(tab_id).ok_or_else(missing)?;
        let mut slot = slot.lock_owned().await;
        let Some(session) = slot.session.as_mut().filter(|s| s.in_transaction()) else {
            return Err(missing());
        };
        let result = if commit { session.commit().await } else { session.rollback().await };
        self.sync_transaction(tab_id, &mut slot);
        result
    }

    pub fn transaction_status(&self, tab_id: &str) -> bool {
        self.inner.tabs.in_transaction(tab_id)
    }

    // A measured plan of a write runs in a transaction that is always rolled back. A tab with an open
    // transaction explains inside it instead.
    pub async fn explain_query(
        &self,
        connection_id: &str,
        tab_id: &str,
        sql: &str,
        analyze: bool,
    ) -> Result<QueryPlan, QueryError> {
        let cfg = self.config(connection_id)?;
        let stmt = single_statement(&cfg.driver, sql).map_err(QueryError::message)?;
        if cfg.read_only {
            assert_read_only(&cfg.driver, &stmt).map_err(QueryError::message)?;
        }
        let key = if tab_id.is_empty() { format!("explain:{connection_id}") } else { tab_id.to_string() };
        let (job, cancel) = self.inner.jobs.start(&key, connection_id);
        let result = async {
            if tab_id.is_empty() {
                let mut session = self.engine(connection_id).await?.session().await?;
                return explain_in(&mut session, &cfg.driver, &stmt, analyze, &cancel).await;
            }
            let mut slot = self.tab_session(tab_id, connection_id).await?;
            let session = slot.session.as_mut().expect("tab session");
            let result = explain_in(session, &cfg.driver, &stmt, analyze, &cancel).await;
            self.sync_transaction(tab_id, &mut slot);
            result
        }
        .await;
        self.inner.jobs.end(&key, job);
        result
    }

    // Sidebar-built SQL such as a rename, drop or truncate. Runs on a session of its own.
    pub async fn execute_statement(&self, connection_id: &str, job: &str, sql: &str) -> Result<(), QueryError> {
        let driver = self.guard_execute(connection_id, sql)?;
        let (job_id, cancel) = self.inner.jobs.start(job, connection_id);
        let started = Instant::now();
        let result = async {
            let mut session = self.engine(connection_id).await?.session().await?;
            for stmt in split_statement_texts(&driver, sql) {
                session.buffered(&stmt, &cancel).await?;
            }
            Ok(())
        }
        .await;
        self.inner.jobs.end(job, job_id);
        self.record_history(connection_id, sql, started.elapsed().as_millis() as i64, result.as_ref().err());
        result
    }

    // Test support. Runs on a fresh session and returns the last result set.
    pub async fn execute_query(&self, connection_id: &str, sql: &str) -> Result<BufferedResult, QueryError> {
        let driver = self.guard_execute(connection_id, sql)?;
        let started = Instant::now();
        let result = async {
            let mut session = self.engine(connection_id).await?.session().await?;
            let mut last = BufferedResult::default();
            for stmt in split_statement_texts(&driver, sql) {
                let res = session.buffered(&stmt, &Cancel::new()).await?;
                let summary = res.summary.clone().unwrap_or_default();
                last = BufferedResult {
                    columns: res.columns.iter().map(|c| c.name.clone()).collect(),
                    column_types: res.columns.iter().map(|c| c.type_name.clone()).collect(),
                    rows: res.values(),
                    row_count: summary.row_count,
                    affected_rows: summary.affected_rows,
                    message: summary.message,
                    duration_ms: started.elapsed().as_millis() as i64,
                };
            }
            Ok(last)
        }
        .await;
        let duration = result.as_ref().map_or(0, |r| r.duration_ms);
        self.record_history(connection_id, sql, duration, result.as_ref().err());
        result
    }
}

async fn explain_in(
    session: &mut Session,
    driver: &DriverType,
    stmt: &str,
    analyze: bool,
    cancel: &Cancel,
) -> Result<QueryPlan, QueryError> {
    let note = if session.in_transaction() {
        Some(NOTE_TAB_TRANSACTION)
    } else if analyze && !is_read_only(driver, stmt) {
        Some(NOTE_ROLLED_BACK)
    } else {
        None
    };
    let isolate = note == Some(NOTE_ROLLED_BACK);
    if isolate {
        session.begin().await?;
    }
    let plan = plan_of(session, driver, stmt, analyze, cancel).await;
    if isolate {
        let _ = session.rollback().await;
    }
    let mut plan = plan?;
    if let Some(note) = note {
        plan.add_note(note);
    }
    Ok(plan)
}

async fn plan_of(
    session: &mut Session,
    driver: &DriverType,
    stmt: &str,
    analyze: bool,
    cancel: &Cancel,
) -> Result<QueryPlan, QueryError> {
    // MariaDB and MySQL ask for a measured plan differently.
    let version = if *driver == DriverType::MySql && analyze {
        let res = session.buffered("SELECT VERSION()", cancel).await?;
        ServerVersion::parse(res.text(0, 0).unwrap_or_default())
    } else {
        ServerVersion::default()
    };
    let explain_sql = build_explain_sql(driver, version, stmt, analyze).map_err(QueryError::message)?;
    let started = Instant::now();
    let res = session.buffered(&explain_sql, cancel).await?;
    let mut plan = parse_plan(driver, stmt, &explain_sql, analyze, &res.plan_rows()).map_err(QueryError::message)?;
    plan.duration_ms = started.elapsed().as_millis() as i64;
    Ok(plan)
}
