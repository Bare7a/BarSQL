use std::time::Instant;

use barsql_core::clock::now_rfc3339;
use barsql_core::{ConnectionConfig, DriverType, HistoryEntry, QueryError, SqlDialect, TableDataRequest, Value};
use barsql_db::{Cancel, ScriptEvent, Session};
use barsql_sql::plan::{NOTE_ROLLED_BACK, NOTE_TAB_TRANSACTION};
use barsql_sql::{
    ExplainStrategy, QueryPlan, ServerVersion, assert_read_only, build_explain, controls_transactions, first_keyword,
    is_read_only, parse_plan, single_statement, split_statement_texts, strip_leading_comments,
};
use serde::Serialize;

use crate::events::{RunEvent, RunHandle, RunResult};
use crate::stores::history_entry;
use crate::tabs::Slot;
use crate::{BarApp, CHANGE_ENGINE};

const RUN_CHANNEL: usize = 16;
const NO_TRANSACTIONS: &str =
    "leave out BEGIN, COMMIT, ROLLBACK and transaction settings: BarSQL runs each call in its own session";
// Statements MySQL commits at once, with what came before them. A CALL may run any of them.
const COMMITS_AT_ONCE: &[&str] = &[
    "ALTER",
    "ANALYZE",
    "CACHE",
    "CALL",
    "CHECK",
    "CREATE",
    "DROP",
    "FLUSH",
    "GRANT",
    "INSTALL",
    "LOAD",
    "LOCK",
    "OPTIMIZE",
    "RENAME",
    "REPAIR",
    "RESET",
    "REVOKE",
    "TRUNCATE",
    "UNINSTALL",
    "UNLOCK",
];

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeOutcome {
    pub statements: usize,
    pub affected_rows: i64,
    pub committed: bool,
}

impl BarApp {
    pub(crate) fn guard_execute(&self, connection_id: &str, sql: &str) -> Result<DriverType, QueryError> {
        let cfg = self.config(connection_id)?;
        if cfg.read_only {
            assert_read_only(&cfg.driver, sql).map_err(QueryError::message)?;
        }
        // An assistant's BEGIN or COMMIT could only get around the read-only session.
        if self.inner.mcp && controls_transactions(&cfg.driver, sql) {
            return Err(QueryError::message(NO_TRANSACTIONS));
        }
        Ok(cfg.driver)
    }

    // Runs on the tab's own session and ends with Done. A new run cancels the tab's previous one.
    pub(crate) async fn execute_query_stream_inner(
        &self,
        connection_id: &str,
        tab_id: &str,
        sql: &str,
    ) -> Result<RunHandle, QueryError> {
        if tab_id.is_empty() {
            return Err(QueryError::message("tab id is required"));
        }
        let driver = self.guard_execute(connection_id, sql)?;
        // Split with the connection's dialect so the boundaries match the editor's run glyphs.
        let statements = split_statement_texts(&driver, sql);
        // Both before the connect, so Stop and closing the tab reach a run that is still connecting.
        let slot = self.inner.tabs.slot(tab_id);
        let (stream_id, cancel) = self.inner.jobs.start(tab_id, connection_id);
        if let Err(error) = self.engine_unless_cancelled(connection_id, &cancel).await {
            self.inner.jobs.end(tab_id, stream_id);
            return Err(error);
        }
        let (tx, rx) = async_channel::bounded(RUN_CHANNEL);
        let app = self.clone();
        let (tab, connection) = (tab_id.to_string(), connection_id.to_string());
        self.inner.runtime.spawn(async move {
            let done = match app.run_batch(&tab, slot, &connection, &statements, &tx, &cancel).await {
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
        slot: Slot,
        connection_id: &str,
        statements: &[String],
        events: &async_channel::Sender<RunEvent>,
        cancel: &Cancel,
    ) -> Result<usize, QueryError> {
        let mut slot = self.tab_session(tab_id, slot, connection_id, cancel).await?;
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
            let mut reported = false;
            while let Ok(event) = inner_rx.recv().await {
                let event = match event {
                    ScriptEvent::Meta { result_index, columns, statement } => RunEvent::Meta {
                        result_index,
                        columns,
                        statement,
                        schema_name: String::new(),
                        table_name: String::new(),
                    },
                    ScriptEvent::Rows { result_index, chunk } => RunEvent::Rows { result_index, chunk },
                    ScriptEvent::Messages { result_index, messages, dropped } => {
                        RunEvent::Messages { result_index, messages, dropped }
                    }
                    ScriptEvent::Result(result) => {
                        result_count = result.result_index + 1;
                        reported |= result.error.is_some();
                        let duration = result.summary.as_ref().map_or(0, |s| s.duration_ms);
                        let entry = history_entry(connection_id, &result.statement, duration, result.error.as_ref());
                        history.push(HistoryEntry { executed_at: now_rfc3339(), ..entry });
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
            (result_count, history, reported)
        };
        let (ran, (mut result_count, history, reported)) = tokio::join!(run, forward);
        self.sync_transaction(tab_id, &mut slot);
        drop(slot);
        // A failure no result carried, like a panicked worker, gets its own result: Done's error would hide the others.
        if let Err(error) = ran
            && !reported
        {
            let result = RunResult { result_index: result_count, error: Some(error), ..Default::default() };
            result_count += 1;
            let _ = events.send(RunEvent::Result(Box::new(result))).await;
        }
        // After the script, so storage writes stay off the connection.
        self.record_history_all(history);
        Ok(result_count)
    }

    pub fn cancel_query(&self, key: &str) -> bool {
        self.inner.jobs.cancel(key)
    }

    // Always one result set, with the primary keys attached for editing.
    pub(crate) async fn query_table_stream_inner(
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
            // So Stop works while the connection is still opening.
            let done = match app.engine_unless_cancelled(&connection, &cancel).await {
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
                    ScriptEvent::Meta { result_index, columns, statement } => RunEvent::Meta {
                        result_index,
                        columns,
                        statement,
                        schema_name: req.schema.clone(),
                        table_name: req.table.clone(),
                    },
                    ScriptEvent::Rows { result_index, chunk } => RunEvent::Rows { result_index, chunk },
                    // Pages are the app's own SQL.
                    ScriptEvent::Messages { .. } => continue,
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

    pub(crate) async fn begin_transaction_inner(&self, connection_id: &str, tab_id: &str) -> Result<(), QueryError> {
        if tab_id.is_empty() {
            return Err(QueryError::message("tab id is required"));
        }
        if self.inner.tabs.in_transaction(tab_id) {
            return Err(QueryError::message("transaction already active on this tab"));
        }
        self.guard_execute(connection_id, "BEGIN")?;
        let mut slot = self.tab_session(tab_id, self.inner.tabs.slot(tab_id), connection_id, &Cancel::new()).await?;
        let session = slot.session.as_mut().expect("tab session");
        if session.in_transaction() {
            self.sync_transaction(tab_id, &mut slot);
            return Err(QueryError::message("transaction already active on this tab"));
        }
        let result = session.begin().await;
        self.sync_transaction(tab_id, &mut slot);
        result
    }

    pub(crate) async fn commit_transaction_inner(&self, tab_id: &str) -> Result<(), QueryError> {
        self.finish_transaction(tab_id, true).await
    }

    pub(crate) async fn rollback_transaction_inner(&self, tab_id: &str) -> Result<(), QueryError> {
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

    // Whether quitting would roll back a transaction. Reads no tab's slot, so it answers at once while runs go on.
    pub fn any_transaction_open(&self) -> bool {
        self.inner.tabs.any_in_transaction()
    }

    // A measured plan of a write runs in a transaction that is always rolled back. A tab with an open
    // transaction explains inside it instead.
    pub(crate) async fn explain_query_inner(
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
            let mut slot = self.tab_session(tab_id, self.inner.tabs.slot(tab_id), connection_id, &cancel).await?;
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
    pub(crate) async fn execute_statement_inner(
        &self,
        connection_id: &str,
        job: &str,
        sql: &str,
    ) -> Result<(), QueryError> {
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

    // Lets the AI app check a change before asking the user about it.
    pub(crate) fn check_change(&self, connection_id: &str, sql: &str, preview: bool) -> Result<(), QueryError> {
        self.plan_change(connection_id, sql, preview).map(|_| ())
    }

    // MCP mode: runs on a writable engine of its own, in one transaction where the database has them.
    pub(crate) async fn apply_change(
        &self,
        connection_id: &str,
        job: &str,
        sql: &str,
        preview: bool,
        confirmed: bool,
    ) -> Result<ChangeOutcome, QueryError> {
        let (cfg, statements) = self.plan_change(connection_id, sql, preview)?;
        if cfg.confirm_changes && !confirmed {
            return Err(QueryError::message("this connection asks before every change"));
        }
        let transactional = cfg.driver.capabilities().interactive_transactions;
        let (job_id, cancel) = self.inner.jobs.start(job, connection_id);
        let result = async {
            let key = format!("{connection_id}{CHANGE_ENGINE}");
            let mut session = self.pooled_engine(&key, cfg.clone()).await?.session().await?;
            if transactional {
                session.begin().await?;
            }
            let mut affected_rows = 0;
            for (ix, stmt) in statements.iter().enumerate() {
                match session.buffered(stmt, &cancel).await {
                    Ok(done) => affected_rows += done.summary.map_or(0, |summary| summary.affected_rows.max(0)),
                    Err(error) => {
                        if transactional {
                            let _ = session.rollback().await;
                        }
                        return Err(left_applied(error, &cfg.driver, &statements[..ix], Some(stmt)));
                    }
                }
            }
            match (transactional, preview) {
                (true, true) => session.rollback().await?,
                (true, false) => {
                    session.commit().await.map_err(|error| left_applied(error, &cfg.driver, &statements, None))?
                }
                _ => {}
            }
            Ok(ChangeOutcome { statements: statements.len(), affected_rows, committed: !preview })
        }
        .await;
        self.inner.jobs.end(job, job_id);
        result
    }

    fn plan_change(
        &self,
        connection_id: &str,
        sql: &str,
        preview: bool,
    ) -> Result<(ConnectionConfig, Vec<String>), QueryError> {
        let mut cfg = self.config(connection_id)?;
        if !self.inner.mcp || !cfg.mcp_changes {
            return Err(QueryError::message(
                "this connection doesn't let AI assistants change data; the user can allow it in BarSQL",
            ));
        }
        // Safe: for_mcp() clears mcp_changes on read-only connections.
        cfg.read_only = false;
        // A COMMIT in it would end the transaction early, preview or not.
        if controls_transactions(&cfg.driver, sql) {
            return Err(QueryError::message(NO_TRANSACTIONS));
        }
        let statements = split_statement_texts(&cfg.driver, sql);
        // ATTACH could reach any file on this machine.
        if cfg.driver == DriverType::Sqlite
            && statements
                .iter()
                .any(|stmt| matches!(first_keyword(strip_leading_comments(stmt)).as_str(), "ATTACH" | "DETACH"))
        {
            return Err(QueryError::message("ATTACH and DETACH are refused: a change stays in this database"));
        }
        if preview && !cfg.driver.capabilities().interactive_transactions {
            return Err(QueryError::message("this database can't roll back, so a change can't be previewed"));
        }
        if preview && statements.iter().any(|stmt| commits_at_once(&cfg.driver, stmt)) {
            return Err(QueryError::message(
                "this database commits schema changes at once, so they can't be previewed",
            ));
        }
        Ok((cfg, statements))
    }

    // Test support. Runs on a fresh session and returns the last result set.
    pub(crate) async fn execute_query_inner(
        &self,
        connection_id: &str,
        sql: &str,
    ) -> Result<BufferedResult, QueryError> {
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

fn commits_at_once(driver: &DriverType, stmt: &str) -> bool {
    !driver.capabilities().transactional_ddl
        && COMMITS_AT_ONCE.contains(&first_keyword(strip_leading_comments(stmt)).as_str())
}

// Notes what the rollback couldn't undo; `failing` is None for a failed COMMIT. A failing schema statement may or may
// not have committed what came before it.
fn left_applied(mut error: QueryError, driver: &DriverType, done: &[String], failing: Option<&String>) -> QueryError {
    let committed = done.iter().rposition(|stmt| commits_at_once(driver, stmt)).map_or(0, |last| last + 1);
    let failing_commits = failing.is_some_and(|stmt| commits_at_once(driver, stmt));
    let (kept, sure) = match (driver.capabilities().interactive_transactions, failing_commits) {
        (false, _) => (done.len(), true),
        (true, true) => (done.len(), committed == done.len()),
        (true, false) => (committed, true),
    };
    let stay = match (sure, kept) {
        (false, _) => "may stay",
        (true, 1) => "stays",
        (true, _) => "stay",
    };
    match kept {
        0 => {}
        1 => error.message += &format!("; the first statement {stay} applied"),
        n => error.message += &format!("; the first {n} statements {stay} applied"),
    }
    error
}

async fn explain_in(
    session: &mut Session,
    driver: &DriverType,
    stmt: &str,
    analyze: bool,
    cancel: &Cancel,
) -> Result<QueryPlan, QueryError> {
    // First, so a plan the server can't give opens no transaction.
    let strategy = explain_strategy(session, driver, stmt, analyze, cancel).await?;
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
    let plan = plan_of(session, driver, stmt, &strategy, analyze, cancel).await;
    if isolate {
        let _ = session.rollback().await;
    }
    let mut plan = plan?;
    if let Some(note) = note {
        plan.add_note(note);
    }
    Ok(plan)
}

async fn explain_strategy(
    session: &mut Session,
    driver: &DriverType,
    stmt: &str,
    analyze: bool,
    cancel: &Cancel,
) -> Result<ExplainStrategy, QueryError> {
    // MariaDB and MySQL ask for a measured plan differently.
    let version = if driver.dialect() == Some(SqlDialect::MySql) && analyze {
        let res = session.buffered("SELECT VERSION()", cancel).await?;
        ServerVersion::parse(res.text(0, 0).unwrap_or_default())
    } else {
        ServerVersion::default()
    };
    build_explain(driver, version, stmt, analyze).map_err(QueryError::message)
}

async fn plan_of(
    session: &mut Session,
    driver: &DriverType,
    stmt: &str,
    strategy: &ExplainStrategy,
    analyze: bool,
    cancel: &Cancel,
) -> Result<QueryPlan, QueryError> {
    let started = Instant::now();
    let rows = session.plan_rows(strategy, cancel).await?;
    let mut plan = parse_plan(driver, stmt, &strategy.display_sql(), analyze, &rows).map_err(QueryError::message)?;
    plan.duration_ms = started.elapsed().as_millis() as i64;
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use barsql_core::{DriverType, QueryError};

    use super::left_applied;

    fn left(driver: DriverType, statements: &[&str]) -> String {
        let statements: Vec<String> = statements.iter().map(|stmt| stmt.to_string()).collect();
        let (failing, done) = statements.split_last().unwrap();
        left_applied(QueryError::message("failed"), &driver, done, Some(failing)).message
    }

    #[test]
    fn a_failed_change_says_what_its_rollback_left_applied() {
        let ddl_then_bad = ["CREATE TABLE a (id int)", "INSERT INTO a VALUES (1)", "INSERT INTO missing VALUES (1)"];
        assert_eq!(left(DriverType::Postgres, &ddl_then_bad), "failed");
        assert_eq!(left(DriverType::MySql, &ddl_then_bad), "failed; the first statement stays applied");
        let no_commit = ["SET @x = 1", "INSERT INTO a VALUES (@x)", "SELECT 1", "INSERT INTO missing VALUES (1)"];
        assert_eq!(left(DriverType::MySql, &no_commit), "failed");
        let failing_ddl = ["INSERT INTO a VALUES (1)", "ALTER TABLE missing ADD c int"];
        assert_eq!(left(DriverType::MySql, &failing_ddl), "failed; the first statement may stay applied");
        let ddl_only = ["CREATE TABLE a (id int)", "CREATE TABLE b (id int)", "CREATE TABLE a (id int)"];
        assert_eq!(left(DriverType::MySql, &ddl_only), "failed; the first 2 statements stay applied");
        let no_transactions =
            ["INSERT INTO a VALUES (1)", "INSERT INTO a VALUES (2)", "INSERT INTO missing VALUES (1)"];
        assert_eq!(left(DriverType::Turso, &no_transactions), "failed; the first 2 statements stay applied");
        let done = ["CREATE TABLE a (id int)".to_string(), "INSERT INTO a VALUES (1)".to_string()];
        let commit = left_applied(QueryError::message("failed"), &DriverType::MySql, &done, None);
        assert_eq!(commit.message, "failed; the first statement stays applied");
    }
}
