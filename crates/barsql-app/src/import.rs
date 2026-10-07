use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::path::Path;
use std::time::{Duration, Instant};

use barsql_core::{DriverType, QueryError, SqlDialect, Value};
use barsql_db::{Cancel, ScriptEvent, Session};
use barsql_io::csv_import::open_error;
use barsql_io::{CsvField, CsvOptions, ImportPreview, count_rows, new_reader, preview_file};
use barsql_sql::dml::{
    IMPORT_BOOL, IMPORT_DATE, IMPORT_TEXT, IMPORT_TIMESTAMP, accepts_empty_string, build_batch_insert_with,
    build_import_create_table, build_truncate,
};
use barsql_sql::{Dialect, READ_ONLY_ERROR, split_statement_texts};
use serde::{Deserialize, Serialize};

use crate::BarApp;
use crate::events::{ImportDone, ImportEvent, ImportHandle, ImportProgress};

// Caps the messages only. The skipped count stays exact.
const MAX_REPORTED_ERRORS: usize = 20;
const DEFAULT_BATCH_SIZE: usize = 500;
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CsvImportRequest {
    pub path: String,
    pub schema: String,
    pub table: String,
    pub create_table: bool,
    pub truncate: bool,
    pub options: CsvOptions,
    // Target column per file column, empty to skip it.
    pub mapping: Vec<String>,
    // Parallel to `mapping`. Used for CREATE TABLE and boolean normalization.
    pub column_types: Vec<String>,
    pub batch_size: usize,
    pub stop_on_error: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SqlImportRequest {
    pub path: String,
    pub stop_on_error: bool,
}

// Batches autocommit, so a failed run keeps what it loaded.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportResult {
    pub inserted: i64,
    pub skipped: i64,
    pub statements: i64,
    pub duration_ms: i64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
    pub cancelled: bool,
}

impl ImportResult {
    fn report(&mut self, errors: impl IntoIterator<Item = String>) {
        for error in errors {
            if self.errors.len() >= MAX_REPORTED_ERRORS {
                return;
            }
            self.errors.push(error);
        }
    }
}

struct Emitter {
    events: async_channel::Sender<ImportEvent>,
    total_rows: i64,
    last: Option<Instant>,
}

impl Emitter {
    // Safe to call per row. Calls within PROGRESS_INTERVAL of the last one are dropped.
    fn progress(&mut self, processed: i64, inserted: i64, skipped: i64, bytes_read: i64, total_bytes: i64) {
        if self.last.is_some_and(|at| at.elapsed() < PROGRESS_INTERVAL) {
            return;
        }
        self.progress_now(processed, inserted, skipped, bytes_read, total_bytes);
    }

    fn progress_now(&mut self, processed: i64, inserted: i64, skipped: i64, bytes_read: i64, total_bytes: i64) {
        self.last = Some(Instant::now());
        let _ = self.events.try_send(ImportEvent::Progress(ImportProgress {
            processed,
            inserted,
            skipped,
            bytes_read,
            total_bytes,
            total_rows: self.total_rows,
        }));
    }
}

pub(crate) fn resolve_mapping(mapping: &[String]) -> Result<(Vec<String>, Vec<usize>), QueryError> {
    let mut seen = HashSet::new();
    let mut targets = Vec::new();
    let mut sources = Vec::new();
    for (i, target) in mapping.iter().enumerate() {
        let name = target.trim();
        if name.is_empty() {
            continue;
        }
        if !seen.insert(name.to_lowercase()) {
            return Err(QueryError::message(format!("column {name:?} is mapped more than once")));
        }
        targets.push(name.to_string());
        sources.push(i);
    }
    if targets.is_empty() {
        return Err(QueryError::message("map at least one column"));
    }
    Ok((targets, sources))
}

impl BarApp {
    pub fn preview_import_file(
        &self,
        connection_id: &str,
        path: &str,
        options: &CsvOptions,
    ) -> Result<ImportPreview, QueryError> {
        if path.is_empty() {
            return Err(QueryError::message("no file selected"));
        }
        let cfg = self.config(connection_id)?;
        preview_file(Path::new(path), &cfg.driver, options).map_err(QueryError::message)
    }

    fn require_writable_import(&self, connection_id: &str) -> Result<(), QueryError> {
        if self.config(connection_id)?.read_only { Err(QueryError::message(READ_ONLY_ERROR)) } else { Ok(()) }
    }

    // Returns once the run is registered, then progress and the outcome arrive as events.
    // Only its own id cancels it, not other work on the connection.
    pub async fn import_csv(
        &self,
        connection_id: &str,
        import_id: &str,
        req: CsvImportRequest,
    ) -> Result<ImportHandle, QueryError> {
        if import_id.is_empty() {
            return Err(QueryError::message("import id is required"));
        }
        if req.path.is_empty() {
            return Err(QueryError::message("no file selected"));
        }
        if req.table.is_empty() {
            return Err(QueryError::message("no target table"));
        }
        self.require_writable_import(connection_id)?;
        let (targets, sources) = resolve_mapping(&req.mapping)?;
        let connection = connection_id.to_string();
        Ok(self.run_import(import_id, connection_id, move |app, mut emitter, cancel| async move {
            app.run_csv_import(&connection, &req, &targets, &sources, &mut emitter, &cancel).await
        }))
    }

    pub async fn import_sql(
        &self,
        connection_id: &str,
        import_id: &str,
        req: SqlImportRequest,
    ) -> Result<ImportHandle, QueryError> {
        if import_id.is_empty() {
            return Err(QueryError::message("import id is required"));
        }
        if req.path.is_empty() {
            return Err(QueryError::message("no file selected"));
        }
        self.require_writable_import(connection_id)?;
        let connection = connection_id.to_string();
        Ok(self.run_import(import_id, connection_id, move |app, mut emitter, cancel| async move {
            app.run_sql_import(&connection, &req, &mut emitter, &cancel).await
        }))
    }

    fn run_import<F, Fut>(&self, import_id: &str, connection_id: &str, work: F) -> ImportHandle
    where
        F: FnOnce(BarApp, Emitter, Cancel) -> Fut + Send + 'static,
        Fut: Future<Output = Result<ImportResult, QueryError>> + Send,
    {
        let (job, cancel) = self.inner.jobs.start(import_id, connection_id);
        let (tx, rx) = async_channel::unbounded();
        let app = self.clone();
        let key = import_id.to_string();
        self.inner.runtime.spawn(async move {
            let emitter = Emitter { events: tx.clone(), total_rows: 0, last: None };
            let started = Instant::now();
            let outcome = work(app.clone(), emitter, cancel.clone()).await;
            let done = match outcome {
                Ok(mut result) => {
                    result.duration_ms = started.elapsed().as_millis() as i64;
                    ImportDone { result: Some(result), error: String::new() }
                }
                Err(err) if cancel.is_cancelled() || err.cancelled => ImportDone {
                    result: Some(ImportResult { cancelled: true, ..Default::default() }),
                    error: String::new(),
                },
                Err(err) => ImportDone { result: None, error: err.message },
            };
            app.inner.jobs.end(&key, job);
            let _ = tx.send(ImportEvent::Done(done)).await;
        });
        ImportHandle { events: rx }
    }

    async fn run_csv_import(
        &self,
        connection_id: &str,
        req: &CsvImportRequest,
        targets: &[String],
        sources: &[usize],
        emitter: &mut Emitter,
        cancel: &Cancel,
    ) -> Result<ImportResult, QueryError> {
        let path = Path::new(&req.path);
        let mut file = File::open(path).map_err(|e| QueryError::message(open_error(path, &e)))?;
        let total_bytes = file.metadata().map(|m| m.len()).unwrap_or(0) as i64;
        // Before reading, so bytes_read covers only the import's own pass.
        emitter.total_rows = count_rows(&mut file, &req.options, total_bytes as u64) as i64;
        let mut reader = new_reader(file, &req.options).map_err(QueryError::message)?;

        let engine = self.engine(connection_id).await?;
        let driver = engine.driver();
        let mut session = engine.session().await?;
        let schema = req.schema.as_str();
        let mut batch_size = if req.batch_size == 0 { DEFAULT_BATCH_SIZE } else { req.batch_size };
        if let Some(max_params) = Dialect::for_driver(&driver).max_bind_params {
            batch_size = batch_size.min((max_params / targets.len()).max(1));
        }
        if let Some(max_rows) = driver.capabilities().max_rows_per_insert {
            batch_size = batch_size.min(max_rows);
        }

        // Read the header before any DDL, so an unreadable file fails before a table exists.
        if req.options.has_header {
            reader.read().map_err(|e| QueryError::message(e.to_string()))?;
        }
        if req.create_table {
            let create = build_import_create_table(
                &driver,
                schema,
                &req.table,
                targets,
                &mapped_types(&req.column_types, sources),
            )
            .map_err(QueryError::message)?;
            session
                .buffered(&create, cancel)
                .await
                .map_err(|e| QueryError::message(format!("create table: {}", e.message)))?;
        } else if req.truncate {
            session
                .buffered(&build_truncate(&driver, schema, &req.table), cancel)
                .await
                .map_err(|e| QueryError::message(format!("empty table: {}", e.message)))?;
        }

        let conv = self.value_converter(&engine, schema, req, targets, sources).await;
        let wraps = conv.wraps(targets.len());
        let mut result = ImportResult::default();
        let mut batch: Vec<Vec<Value>> = Vec::with_capacity(batch_size);
        let mut processed = 0i64;
        loop {
            if cancel.is_cancelled() {
                result.cancelled = true;
                break;
            }
            let record = match reader.read() {
                Ok(Some(record)) => conv.build_values(record, sources),
                Ok(None) => break,
                Err(err) => {
                    if req.stop_on_error {
                        return Err(QueryError::message(err.to_string()));
                    }
                    result.skipped += 1;
                    result.report([err.to_string()]);
                    continue;
                }
            };
            processed += 1;
            batch.push(record);
            if batch.len() >= batch_size {
                flush(
                    &mut session,
                    &driver,
                    schema,
                    &req.table,
                    targets,
                    &wraps,
                    &mut batch,
                    req.stop_on_error,
                    cancel,
                    &mut result,
                )
                .await?;
            }
            // Throttled, so a file smaller than one batch still reports progress.
            emitter.progress(processed, result.inserted, result.skipped, reader.bytes_read() as i64, total_bytes);
        }
        if !result.cancelled {
            flush(
                &mut session,
                &driver,
                schema,
                &req.table,
                targets,
                &wraps,
                &mut batch,
                req.stop_on_error,
                cancel,
                &mut result,
            )
            .await?;
        }
        emitter.progress_now(processed, result.inserted, result.skipped, reader.bytes_read() as i64, total_bytes);
        Ok(result)
    }

    // New tables take their types from the request, existing ones from the catalog.
    async fn value_converter(
        &self,
        engine: &barsql_db::Engine,
        schema: &str,
        req: &CsvImportRequest,
        targets: &[String],
        sources: &[usize],
    ) -> ValueConverter {
        let driver = engine.driver();
        let mut conv = ValueConverter {
            null_literal: req.options.null_literal.clone(),
            trim: req.options.trim_space,
            // Postgres booleans need real bools. MySQL, SQLite and SQL Server take 1/0.
            bool_as_bool: match driver.dialect() {
                Some(SqlDialect::Postgres | SqlDialect::ClickHouse) => true,
                Some(SqlDialect::MySql | SqlDialect::Sqlite | SqlDialect::TSql) | None => false,
            },
            ..Default::default()
        };
        // MySQL rejects ISO timestamps with a zone, so dates are rewritten for it. ClickHouse reads a Date only as
        // YYYY-MM-DD.
        let (mysql, clickhouse) = match driver.dialect() {
            Some(SqlDialect::MySql) => (true, false),
            Some(SqlDialect::ClickHouse) => (false, true),
            Some(SqlDialect::Postgres | SqlDialect::Sqlite | SqlDialect::TSql) | None => (false, false),
        };
        let tsql = driver.dialect() == Some(SqlDialect::TSql);
        let requested_bool: HashSet<usize> = sources
            .iter()
            .enumerate()
            .filter(|(_, src)| req.column_types.get(**src).is_some_and(|t| t == IMPORT_BOOL))
            .map(|(i, _)| i)
            .collect();
        if req.create_table {
            conv.bool_cols = requested_bool;
            for (i, src) in sources.iter().enumerate() {
                let t = req.column_types.get(*src).filter(|t| !t.is_empty()).map_or(IMPORT_TEXT, String::as_str);
                if t == IMPORT_TEXT {
                    conv.text_cols.insert(i);
                }
                if mysql && (t == IMPORT_DATE || t == IMPORT_TIMESTAMP) {
                    conv.date_cols.insert(i);
                }
                if clickhouse && t == IMPORT_DATE {
                    conv.day_cols.insert(i);
                }
            }
            return conv;
        }
        let Ok(columns) = engine.list_columns(schema, &req.table).await else {
            // Without the catalog, load everything as text and honour the quoting.
            conv.text_cols = (0..targets.len()).collect();
            return conv;
        };
        let by_name: HashMap<String, String> =
            columns.into_iter().map(|c| (c.name.to_lowercase(), c.data_type)).collect();
        for (i, target) in targets.iter().enumerate() {
            let data_type = by_name.get(&target.to_lowercase());
            if data_type.is_none_or(|t| accepts_empty_string(t)) {
                conv.text_cols.insert(i);
            }
            let upper = data_type.map(|t| barsql_sql::to_upper(t.trim())).unwrap_or_default();
            if upper.contains("BOOL") {
                conv.bool_cols.insert(i);
            } else if requested_bool.contains(&i) && !conv.text_cols.contains(&i) {
                // Numeric targets get 1/0. Text targets keep the value as written.
                conv.bool_int_cols.insert(i);
            }
            if mysql && matches!(upper.as_str(), "DATE" | "DATETIME" | "TIMESTAMP") {
                conv.date_cols.insert(i);
            }
            let base = upper.trim_start_matches("NULLABLE(").trim_end_matches(')');
            if clickhouse && matches!(base, "DATE" | "DATE32") {
                conv.day_cols.insert(i);
            }
            if tsql && matches!(upper.split('(').next(), Some("BINARY" | "VARBINARY" | "IMAGE")) {
                conv.hex_cols.insert(i);
            }
        }
        conv
    }

    async fn run_sql_import(
        &self,
        connection_id: &str,
        req: &SqlImportRequest,
        emitter: &mut Emitter,
        cancel: &Cancel,
    ) -> Result<ImportResult, QueryError> {
        // Read in full because the splitter needs the whole script to track quoting.
        let path = Path::new(&req.path);
        let data = std::fs::read(path).map_err(|e| QueryError::message(open_error(path, &e)))?;
        let engine = self.engine(connection_id).await?;
        let statements = split_statement_texts(&engine.driver(), &String::from_utf8_lossy(&data));
        let total = statements.len() as i64;
        let mut session = engine.session().await?;
        let mut result = ImportResult::default();
        for (i, stmt) in statements.iter().enumerate() {
            if cancel.is_cancelled() {
                result.cancelled = true;
                break;
            }
            match affected_rows(&mut session, stmt, cancel).await {
                Ok(affected) => {
                    result.statements += 1;
                    result.inserted += affected;
                }
                Err(err) if req.stop_on_error => {
                    return Err(QueryError { message: format!("statement {}: {}", i + 1, err.message), ..err });
                }
                Err(err) => {
                    result.skipped += 1;
                    result.report([format!("statement {}: {}", i + 1, err.message)]);
                }
            }
            // No byte progress per statement, so the index drives the bar.
            emitter.progress(i as i64 + 1, result.inserted, result.skipped, i as i64 + 1, total);
        }
        Ok(result)
    }
}

// Discards rows since the import only reports counts.
async fn affected_rows(session: &mut Session, stmt: &str, cancel: &Cancel) -> Result<i64, QueryError> {
    let (tx, rx) = async_channel::bounded(4);
    let run = async {
        let result = session.stream(stmt, &tx, cancel).await;
        drop(tx);
        result
    };
    let drain = async {
        let mut affected = 0;
        while let Ok(event) = rx.recv().await {
            if let ScriptEvent::Result(result) = event {
                affected += result.summary.map_or(0, |s| s.affected_rows);
            }
        }
        affected
    };
    let (result, affected) = tokio::join!(run, drain);
    result.map(|_| affected)
}

// Retries a failed batch row by row so one bad row doesn't sink the rest.
#[allow(clippy::too_many_arguments)]
async fn flush(
    session: &mut Session,
    driver: &DriverType,
    schema: &str,
    table: &str,
    targets: &[String],
    wraps: &[Option<&str>],
    batch: &mut Vec<Vec<Value>>,
    stop_on_error: bool,
    cancel: &Cancel,
    result: &mut ImportResult,
) -> Result<(), QueryError> {
    if batch.is_empty() {
        return Ok(());
    }
    let rows = std::mem::take(batch);
    let (inserted, skipped, errors) = match insert(session, driver, schema, table, targets, wraps, &rows).await {
        Ok(()) => (rows.len() as i64, 0, Vec::new()),
        Err(err) if stop_on_error || cancel.is_cancelled() => (0, rows.len() as i64, vec![err.message]),
        Err(_) => {
            let mut outcome = (0, 0, Vec::new());
            for row in &rows {
                match insert(session, driver, schema, table, targets, wraps, std::slice::from_ref(row)).await {
                    Ok(()) => outcome.0 += 1,
                    Err(err) => {
                        outcome.1 += 1;
                        outcome.2.push(err.message);
                    }
                }
            }
            outcome
        }
    };
    result.inserted += inserted;
    result.skipped += skipped;
    let first = errors.first().cloned();
    result.report(errors);
    if stop_on_error && skipped > 0 {
        return Err(QueryError::message(first.unwrap_or_else(|| "import failed".into())));
    }
    Ok(())
}

async fn insert(
    session: &mut Session,
    driver: &DriverType,
    schema: &str,
    table: &str,
    targets: &[String],
    wraps: &[Option<&str>],
    rows: &[Vec<Value>],
) -> Result<(), QueryError> {
    let (sql, args) =
        build_batch_insert_with(driver, schema, table, targets, rows, wraps).map_err(QueryError::message)?;
    session.execute_params(&sql, &args).await.map(|_| ())
}

fn mapped_types(column_types: &[String], sources: &[usize]) -> Vec<String> {
    sources
        .iter()
        .map(|src| column_types.get(*src).filter(|t| !t.is_empty()).cloned().unwrap_or_else(|| IMPORT_TEXT.into()))
        .collect()
}

#[derive(Default)]
struct ValueConverter {
    // Native boolean columns. bool_int_cols load bool-like values into numeric columns as 1/0.
    bool_cols: HashSet<usize>,
    bool_int_cols: HashSet<usize>,
    // MySQL needs its own datetime literal rather than RFC 3339.
    date_cols: HashSet<usize>,
    // ClickHouse Date columns, which take the day alone.
    day_cols: HashSet<usize>,
    // SQL Server binary columns. Text won't convert to binary there, so these go as 0x hex through CONVERT.
    hex_cols: HashSet<usize>,
    // Columns that accept ''. Elsewhere a quoted empty field is still NULL.
    text_cols: HashSet<usize>,
    null_literal: String,
    trim: bool,
    bool_as_bool: bool,
}

impl ValueConverter {
    // A quoted empty field is '' and a bare one is NULL, matching Postgres COPY csv.
    fn build_values(&self, record: &[CsvField], sources: &[usize]) -> Vec<Value> {
        sources
            .iter()
            .enumerate()
            .map(|(i, &src)| {
                let Some(field) = record.get(src) else { return Value::Null };
                let v = if self.trim { field.value.trim() } else { field.value.as_str() };
                if !self.null_literal.is_empty() && v == self.null_literal {
                    return Value::Null;
                }
                if v.is_empty() {
                    return if field.quoted && self.text_cols.contains(&i) {
                        Value::Text(String::new())
                    } else {
                        Value::Null
                    };
                }
                let v = undefuse_formula(v);
                if self.bool_cols.contains(&i) {
                    normalize_bool(v, self.bool_as_bool)
                } else if self.bool_int_cols.contains(&i) {
                    normalize_bool(v, false)
                } else if self.date_cols.contains(&i) {
                    Value::Text(mysql_datetime(v))
                } else if self.day_cols.contains(&i) {
                    Value::Text(day_part(v).to_string())
                } else if self.hex_cols.contains(&i) {
                    Value::Text(hex_literal(v))
                } else {
                    Value::Text(v.to_string())
                }
            })
            .collect()
    }
}

impl ValueConverter {
    fn wraps(&self, columns: usize) -> Vec<Option<&'static str>> {
        (0..columns).map(|i| self.hex_cols.contains(&i).then_some("CONVERT(varbinary(max), {}, 1)")).collect()
    }
}

// The grid shows bytes as text when they're UTF-8 and as \x hex otherwise, so a value reads back either way.
fn hex_literal(v: &str) -> String {
    match v.strip_prefix("\\x").filter(|hex| hex.len() % 2 == 0 && hex.bytes().all(|b| b.is_ascii_hexdigit())) {
        Some(hex) => format!("0x{}", hex.to_uppercase()),
        None => format!("0x{}", hex::encode_upper(v.as_bytes())),
    }
}

// Undoes the export's formula guard, a ' prefixed to values starting with = + - or @.
fn undefuse_formula(v: &str) -> &str {
    let b = v.as_bytes();
    if b.len() >= 2 && b[0] == b'\'' && matches!(b[1], b'=' | b'+' | b'-' | b'@') { &v[1..] } else { v }
}

// Unrecognised text passes through so the engine reports it instead of us rewriting data.
pub(crate) fn normalize_bool(v: &str, as_bool: bool) -> Value {
    match v.trim().to_lowercase().as_str() {
        "true" | "t" | "yes" | "y" | "1" => {
            if as_bool {
                Value::Bool(true)
            } else {
                Value::Int(1)
            }
        }
        "false" | "f" | "no" | "n" | "0" => {
            if as_bool {
                Value::Bool(false)
            } else {
                Value::Int(0)
            }
        }
        _ => Value::Text(v.to_string()),
    }
}

// The YYYY-MM-DD a timestamp starts with, or the value as written.
fn day_part(v: &str) -> &str {
    let b = v.as_bytes();
    let shaped = b.len() >= 10
        && b[4] == b'-'
        && b[7] == b'-'
        && [0..4, 5..7, 8..10].iter().all(|r| b[r.clone()].iter().all(u8::is_ascii_digit));
    if shaped { &v[..10] } else { v }
}

// Rewrites RFC 3339 as a MySQL literal, keeping the wall time as written and dropping the offset.
fn mysql_datetime(v: &str) -> String {
    let b = v.as_bytes();
    let digits = |r: std::ops::Range<usize>| b.get(r.clone()).is_some_and(|s| s.iter().all(u8::is_ascii_digit));
    let shape = b.len() >= 20
        && digits(0..4)
        && b[4] == b'-'
        && digits(5..7)
        && b[7] == b'-'
        && digits(8..10)
        && b[10] == b'T'
        && digits(11..13)
        && b[13] == b':'
        && digits(14..16)
        && b[16] == b':'
        && digits(17..19);
    if !shape {
        return v.to_string();
    }
    let mut i = 19;
    let mut fraction = "";
    if b[i] == b'.' {
        let start = i + 1;
        i = start;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == start {
            return v.to_string();
        }
        fraction = &v[start..i];
    }
    let zone = &v[i..];
    let zone_ok = zone == "Z"
        || (zone.len() == 6
            && matches!(zone.as_bytes()[0], b'+' | b'-')
            && zone.as_bytes()[3] == b':'
            && zone[1..3].bytes().chain(zone[4..6].bytes()).all(|c| c.is_ascii_digit()));
    let field = |r: std::ops::Range<usize>| v[r].parse::<u32>().unwrap_or(99);
    let valid = (1..=12).contains(&field(5..7))
        && (1..=31).contains(&field(8..10))
        && field(11..13) < 24
        && field(14..16) < 60
        && field(17..19) < 60;
    if !zone_ok || !valid {
        return v.to_string();
    }
    let micros: String = fraction.chars().take(6).collect();
    let micros = micros.trim_end_matches('0');
    let base = format!("{} {}", &v[0..10], &v[11..19]);
    if micros.is_empty() { base } else { format!("{base}.{micros}") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mapping_resolution() {
        let mapping: Vec<String> = ["id", "", "name", "  "].map(String::from).to_vec();
        assert_eq!(resolve_mapping(&mapping).unwrap(), (vec!["id".to_string(), "name".to_string()], vec![0, 2]));
        assert!(resolve_mapping(&["id".to_string(), "ID".to_string()]).is_err());
        assert!(resolve_mapping(&[String::new(), String::new()]).is_err());
    }

    #[test]
    fn booleans_normalize_by_target() {
        for v in ["true", "T", "yes", "Y", "1"] {
            assert_eq!(normalize_bool(v, false), Value::Int(1));
            assert_eq!(normalize_bool(v, true), Value::Bool(true));
        }
        for v in ["false", "F", "no", "N", "0"] {
            assert_eq!(normalize_bool(v, false), Value::Int(0));
            assert_eq!(normalize_bool(v, true), Value::Bool(false));
        }
        assert_eq!(normalize_bool("maybe", true), Value::from("maybe"));
    }

    #[test]
    fn mysql_datetimes_and_formula_guards() {
        assert_eq!(mysql_datetime("2024-02-29T13:45:30Z"), "2024-02-29 13:45:30");
        assert_eq!(mysql_datetime("2024-02-29T13:45:30.1234567+02:00"), "2024-02-29 13:45:30.123456");
        assert_eq!(mysql_datetime("2024-02-29T13:45:30.500Z"), "2024-02-29 13:45:30.5");
        assert_eq!(mysql_datetime("2024-02-29 13:45:30"), "2024-02-29 13:45:30");
        assert_eq!(mysql_datetime("nope"), "nope");
        assert_eq!(day_part("2026-08-09T00:00:00Z"), "2026-08-09");
        assert_eq!(day_part("2026-08-09"), "2026-08-09");
        assert_eq!(day_part("09/08/2026"), "09/08/2026");
        assert_eq!(undefuse_formula("'=1+1"), "=1+1");
        assert_eq!(undefuse_formula("'plain"), "'plain");
        assert_eq!(undefuse_formula("'"), "'");
    }
}
