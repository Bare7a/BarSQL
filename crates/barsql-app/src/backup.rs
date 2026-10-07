use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use barsql_core::QueryError;
use barsql_db::{Cancel, Cell, ScriptEvent};
use barsql_sql::ddl::terminate_statement;
use barsql_sql::table_ref;

use crate::BarApp;

// Cap each INSERT so a restore stays fast and within the engines' statement limits.
const STATEMENT_ROWS: usize = 100;
const STATEMENT_BYTES: usize = 256 << 10;
// Flush to disk at this size. Progress updates once per flush.
const WRITE_BYTES: usize = 256 << 10;

#[derive(Debug, Clone)]
pub struct BackupRequest {
    pub schema: String,
    pub table: String,
    pub structure: bool,
    pub data: bool,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackupOutcome {
    pub rows: usize,
    // When set, the file holds only the first `rows` rows.
    pub cancelled: bool,
}

struct Output {
    file: Option<File>,
    text: String,
}

impl Output {
    async fn create(path: &Path) -> Result<Self, QueryError> {
        let path = path.to_path_buf();
        let file =
            tokio::task::spawn_blocking(move || File::create(path)).await.map_err(join_error)?.map_err(io_error)?;
        Ok(Self { file: Some(file), text: String::new() })
    }

    async fn write(&mut self) -> Result<(), QueryError> {
        let (Some(mut file), text) = (self.file.take(), std::mem::take(&mut self.text)) else { return Ok(()) };
        let file = tokio::task::spawn_blocking(move || file.write_all(text.as_bytes()).map(|()| file))
            .await
            .map_err(join_error)?
            .map_err(io_error)?;
        self.file = Some(file);
        Ok(())
    }
}

fn io_error(error: std::io::Error) -> QueryError {
    QueryError::message(error.to_string())
}

fn join_error(error: tokio::task::JoinError) -> QueryError {
    QueryError::message(error.to_string())
}

struct Inserts<'a> {
    head: &'a str,
    rows: usize,
    bytes: usize,
    // SQL Server's GO, after each statement.
    separator: Option<&'a str>,
}

impl Inserts<'_> {
    fn push(&mut self, cells: impl Iterator<Item = String>, out: &mut String) {
        let start = self.open(out);
        out.push('(');
        for (ix, cell) in cells.enumerate() {
            if ix > 0 {
                out.push_str(", ");
            }
            out.push_str(&cell);
        }
        out.push(')');
        self.row_done(start, out);
    }

    // A row the server already wrote as a tuple.
    fn push_tuple(&mut self, tuple: &str, out: &mut String) {
        let start = self.open(out);
        out.push_str(tuple);
        self.row_done(start, out);
    }

    // Starts a row, and the statement before its first row. Returns where the row's text starts.
    fn open(&mut self, out: &mut String) -> usize {
        out.push_str(if self.rows == 0 { self.head } else { "," });
        out.push('\n');
        out.len()
    }

    fn row_done(&mut self, start: usize, out: &mut String) {
        self.rows += 1;
        self.bytes += out.len() - start;
        if self.rows >= STATEMENT_ROWS || self.bytes >= STATEMENT_BYTES {
            self.close(out);
        }
    }

    fn close(&mut self, out: &mut String) {
        if self.rows > 0 {
            out.push_str(";\n");
            push_separator(out, self.separator);
            self.rows = 0;
            self.bytes = 0;
        }
    }
}

// Values arrive as server-written literals. NULL means MySQL gave up on one over max_allowed_packet.
fn literal(cell: Cell<'_>) -> Option<String> {
    match cell {
        Cell::Null => None,
        Cell::Bool(true) => Some("TRUE".into()),
        Cell::Bool(false) => Some("FALSE".into()),
        Cell::Text(text) | Cell::Number(text) => Some(text.to_string()),
    }
}

fn too_large() -> QueryError {
    QueryError::message(
        "the server returned no value for a row; MySQL and MariaDB do this for a value longer than max_allowed_packet",
    )
}

// Next to the target so one rename swaps it in. A failed or stopped backup leaves the old file alone.
fn partial_path(path: &Path) -> PathBuf {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    path.with_file_name(format!(".{name}.barsql-partial"))
}

impl BarApp {
    // Rows go out as INSERTs of literals the server quoted itself, so every type round-trips exactly.
    // `progress` gets the running row count.
    pub async fn backup_table(
        &self,
        connection_id: &str,
        job: &str,
        req: BackupRequest,
        progress: async_channel::Sender<usize>,
    ) -> Result<BackupOutcome, QueryError> {
        let (job_id, cancel) = self.inner.jobs.start(job, connection_id);
        let result = self.write_backup(connection_id, &req, &cancel, &progress).await;
        self.inner.jobs.end(job, job_id);
        result
    }

    async fn write_backup(
        &self,
        connection_id: &str,
        req: &BackupRequest,
        cancel: &Cancel,
        progress: &async_channel::Sender<usize>,
    ) -> Result<BackupOutcome, QueryError> {
        let engine = self.engine(connection_id).await?;
        let query = engine.dump_query(&req.schema, &req.table).await?;
        let (ddl, after_rows) = match req.structure {
            true => engine.backup_ddl(&req.schema, &req.table).await?,
            false => (String::new(), Vec::new()),
        };
        let partial = partial_path(&req.path);
        let mut out = Output::create(&partial).await?;
        let written = async {
            // A line break in the name would end the SQL comment.
            let name = table_ref(&engine.driver(), &req.schema, &req.table).replace(['\r', '\n'], " ");
            let now = jiff::Zoned::now().strftime("%Y-%m-%d %H:%M:%S %:z");
            out.text.push_str(&format!("-- BarSQL backup of {name}\n-- {now}\n\n"));
            let separator = query.batch_separator;
            push_statements(&mut out.text, &query.prologue, separator);
            if req.structure {
                out.text.push_str(&terminate_statement(&ddl));
                out.text.push('\n');
                push_separator(&mut out.text, separator);
                out.text.push('\n');
            }
            let mut written = 0;
            if req.data {
                push_statements(&mut out.text, &query.before_rows, separator);
                written = self.write_rows(&engine, &query, &mut out, cancel, progress).await?;
                push_statements(&mut out.text, &query.sequences, separator);
            }
            push_statements(&mut out.text, &after_rows, separator);
            push_statements(&mut out.text, &query.epilogue, separator);
            out.write().await?;
            Ok::<usize, QueryError>(written)
        }
        .await;
        drop(out);
        let finished = written.as_ref().is_ok() && !cancel.is_cancelled();
        let placed = match finished {
            true => tokio::fs::rename(&partial, &req.path).await.map_err(io_error),
            false => Ok(()),
        };
        if !finished || placed.is_err() {
            let _ = tokio::fs::remove_file(&partial).await;
        }
        placed?;
        let rows = written?;
        progress.try_send(rows).ok();
        Ok(BackupOutcome { rows, cancelled: cancel.is_cancelled() })
    }

    async fn write_rows(
        &self,
        engine: &barsql_db::Engine,
        query: &barsql_db::DumpQuery,
        out: &mut Output,
        cancel: &Cancel,
        progress: &async_channel::Sender<usize>,
    ) -> Result<usize, QueryError> {
        let mut session = engine.pooled_session().await?;
        for statement in &query.setup {
            session.buffered(statement, cancel).await?;
        }
        let (tx, rx) = async_channel::bounded(16);
        let run = async {
            let result = session.stream(&query.select, &tx, cancel).await;
            drop(tx);
            result
        };
        let write = async {
            let result = async {
                let mut inserts = Inserts { head: &query.insert, rows: 0, bytes: 0, separator: query.batch_separator };
                let mut rows = 0;
                while let Ok(event) = rx.recv().await {
                    let ScriptEvent::Rows { chunk, .. } = event else { continue };
                    for row in 0..chunk.rows() {
                        if query.whole_row {
                            let cell = literal(chunk.cell(row, 0)).ok_or_else(too_large)?;
                            inserts.push_tuple(&query.tuple(&cell), &mut out.text);
                        } else {
                            let cells: Option<Vec<String>> =
                                (0..chunk.columns()).map(|col| literal(chunk.cell(row, col))).collect();
                            inserts.push(cells.ok_or_else(too_large)?.into_iter(), &mut out.text);
                        }
                        rows += 1;
                    }
                    if out.text.len() >= WRITE_BYTES {
                        out.write().await?;
                        progress.try_send(rows).ok();
                    }
                }
                inserts.close(&mut out.text);
                Ok::<usize, QueryError>(rows)
            }
            .await;
            // Otherwise the stream blocks forever on the full channel.
            if result.is_err() {
                rx.close();
                cancel.cancel();
            }
            result
        };
        let (streamed, rows) = tokio::join!(run, write);
        let rows = rows?;
        match streamed {
            Err(error) if !cancel.is_cancelled() => Err(error),
            _ => Ok(rows),
        }
    }
}

fn push_statements(out: &mut String, statements: &[String], separator: Option<&str>) {
    for statement in statements {
        out.push_str(&terminate_statement(statement));
        out.push('\n');
        push_separator(out, separator);
    }
    if !statements.is_empty() {
        out.push('\n');
    }
}

fn push_separator(out: &mut String, separator: Option<&str>) {
    if let Some(separator) = separator {
        out.push_str(separator);
        out.push('\n');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_gather_into_statements_of_a_hundred() {
        let mut inserts = Inserts { head: "INSERT INTO t (a) VALUES", rows: 0, bytes: 0, separator: None };
        let mut out = String::new();
        for n in 0..101 {
            inserts.push(std::iter::once(n.to_string()), &mut out);
        }
        inserts.close(&mut out);
        let statements: Vec<&str> = out.split_inclusive(";\n").collect();
        assert_eq!(statements.len(), 2);
        assert!(statements[0].starts_with("INSERT INTO t (a) VALUES\n(0),\n(1),"), "{}", statements[0]);
        assert!(statements[0].ends_with("(99);\n"));
        assert_eq!(statements[1], "INSERT INTO t (a) VALUES\n(100);\n");
    }

    #[test]
    fn a_long_row_closes_its_statement_early() {
        let mut inserts = Inserts { head: "INSERT INTO t (a) VALUES", rows: 0, bytes: 0, separator: None };
        let mut out = String::new();
        inserts.push(std::iter::once(format!("'{}'", "x".repeat(STATEMENT_BYTES))), &mut out);
        inserts.push(std::iter::once("'y'".to_string()), &mut out);
        inserts.close(&mut out);
        assert_eq!(out.matches("INSERT INTO").count(), 2);
        assert!(out.ends_with("INSERT INTO t (a) VALUES\n('y');\n"));
    }

    #[test]
    fn the_servers_literals_pass_through_and_a_null_is_refused() {
        assert_eq!(literal(Cell::Null), None);
        assert_eq!(literal(Cell::Text("'it''s'")).as_deref(), Some("'it''s'"));
        assert_eq!(literal(Cell::Number("42")).as_deref(), Some("42"));
        assert_eq!(literal(Cell::Bool(true)).as_deref(), Some("TRUE"));
    }

    #[test]
    fn a_backup_is_written_beside_its_file_first() {
        assert_eq!(partial_path(Path::new("/b/users.sql")), Path::new("/b/.users.sql.barsql-partial"));
    }
}
