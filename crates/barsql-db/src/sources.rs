use std::collections::{BTreeSet, HashMap};

use barsql_core::schema::primary_keys;
use barsql_core::{ColumnInfo, DriverType, QueryError, Row, RowDelete, RowUpdate, SqlDialect, Value};
use barsql_sql::dml::{build_delete_many, build_update_many};
use barsql_sql::{READ_ONLY_ERROR, quote_literal_in};

use crate::engine::row_not_found;
use crate::{Cancel, ColumnMeta, ColumnOrigin, Engine, Session};

// Parameters per statement, which keeps one under SQL Server's 2,100.
const BATCH_PARAMS: usize = 1000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnSource {
    pub schema: String,
    pub table: String,
    pub column: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResultSources {
    pub columns: Vec<Option<ColumnSource>>,
    // SQL Server's, since USE changes what a schema-qualified name means.
    pub database: String,
}

impl ResultSources {
    // As MySQL and SQLite report them with the rows.
    pub fn named(columns: &[ColumnMeta]) -> Self {
        Self { columns: columns.iter().map(named).collect(), database: String::new() }
    }
}

impl Session {
    // Postgres names its OIDs from the catalog and SQL Server describes the statement again. MySQL and SQLite named
    // them with the rows.
    pub async fn column_sources(
        &mut self,
        statement: &str,
        columns: &[ColumnMeta],
        cancel: &Cancel,
    ) -> Result<ResultSources, QueryError> {
        match self.driver() {
            DriverType::Postgres => postgres_sources(self, columns, cancel).await,
            DriverType::SqlServer => sql_server_sources(self, statement, columns.len(), cancel).await,
            _ => Ok(ResultSources::named(columns)),
        }
    }

    // A transaction keeps SQLite's one connection on its session, so SQLite reads its catalog through this one.
    pub async fn list_columns(
        &mut self,
        engine: &Engine,
        schema: &str,
        table: &str,
    ) -> Result<Vec<ColumnInfo>, QueryError> {
        match self {
            Self::Sqlite(session) => session.list_columns(table).await,
            _ => engine.list_columns(schema, table).await,
        }
    }

    pub async fn current_database(&mut self) -> Result<String, QueryError> {
        if self.driver() != DriverType::SqlServer {
            return Ok(String::new());
        }
        let res = self.buffered("SELECT DB_NAME()", &Cancel::new()).await?;
        Ok(res.chunk.display(0, 0).unwrap_or_default().to_string())
    }

    // Inside the session's transaction when one is open, else in one of its own, so a failure lands nothing. `columns`
    // is read beforehand: a session holding SQLite's one connection would wait on itself for it. Returns the deletes
    // that found no row.
    pub async fn apply_row_edits(
        &mut self,
        delete: &RowDelete,
        updates: &[RowUpdate],
        columns: &[ColumnInfo],
    ) -> Result<i64, QueryError> {
        if self.read_only() {
            return Err(QueryError::message(READ_ONLY_ERROR));
        }
        if self.in_transaction() {
            return self.write_row_edits(delete, updates, columns).await;
        }
        self.begin_edits().await?;
        let result = match self.write_row_edits(delete, updates, columns).await {
            Ok(missing) => self.end_edits(true).await.map(|()| missing),
            Err(error) => Err(error),
        };
        // Also after a COMMIT that a deferred constraint refused, which SQLite and Turso leave open.
        if result.is_err() {
            let _ = self.end_edits(false).await;
        }
        result
    }

    // Turso refuses BEGIN, as its server drops a stream left idle between runs, but an apply's statements follow
    // each other at once.
    async fn begin_edits(&mut self) -> Result<(), QueryError> {
        match self {
            Self::Turso(session) => session.begin_edits().await,
            _ => self.begin().await,
        }
    }

    async fn end_edits(&mut self, commit: bool) -> Result<(), QueryError> {
        match (self, commit) {
            (Self::Turso(session), commit) => session.end_edits(commit).await,
            (session, true) => session.commit().await,
            (session, false) => session.rollback().await,
        }
    }

    async fn write_row_edits(
        &mut self,
        delete: &RowDelete,
        updates: &[RowUpdate],
        columns: &[ColumnInfo],
    ) -> Result<i64, QueryError> {
        let mut missing = 0;
        let keys: Vec<&Row> = delete.primary_keys.iter().collect();
        for keys in keys.chunks(key_batch(0, columns)) {
            let deleted = self.delete_keys(&delete.schema, &delete.table, keys, columns).await?;
            missing += keys.len().saturating_sub(deleted as usize) as i64;
        }
        // Rows given the same changes, like a value pasted down a column, go in one statement.
        for (update, keys) in same_changes(updates) {
            for keys in keys.chunks(key_batch(update.changes.len(), columns)) {
                self.update_keys(&update.schema, update, keys, columns).await?;
            }
        }
        Ok(missing)
    }

    pub(crate) async fn update_row(
        &mut self,
        schema: &str,
        update: &RowUpdate,
        columns: &[ColumnInfo],
    ) -> Result<(), QueryError> {
        self.update_keys(schema, update, &[&update.primary_key], columns).await
    }

    // `update`'s changes to each row `keys` names. A key that matches no row is an error, else the edit would vanish
    // silently.
    async fn update_keys(
        &mut self,
        schema: &str,
        update: &RowUpdate,
        keys: &[&Row],
        columns: &[ColumnInfo],
    ) -> Result<(), QueryError> {
        let pks = primary_keys(columns);
        let (sql, args) =
            build_update_many(&self.driver(), schema, &update.table, &update.changes, keys, &pks, columns)
                .map_err(QueryError::message)?;
        let updated = self.execute_counted(&sql, &args).await? as usize;
        match keys {
            [key] if updated == 0 => Err(row_not_found(key)),
            _ if updated < keys.len() => Err(QueryError::message(format!(
                "{} of {} rows weren't found; they may have been changed or deleted since they were loaded",
                keys.len() - updated,
                keys.len()
            ))),
            _ => Ok(()),
        }
    }

    // Returns the rows it deleted.
    pub(crate) async fn delete_keys(
        &mut self,
        schema: &str,
        table: &str,
        keys: &[&Row],
        columns: &[ColumnInfo],
    ) -> Result<u64, QueryError> {
        let pks = primary_keys(columns);
        let (sql, args) =
            build_delete_many(&self.driver(), schema, table, &pks, keys, columns).map_err(QueryError::message)?;
        self.execute_counted(&sql, &args).await
    }

    // The counts tell a row that's gone. A SET NOCOUNT ON or SET ROWCOUNT the tab ran on SQL Server would hide or cap
    // them, and a SET inside sp_executesql, which runs every statement with parameters, ends with it.
    async fn execute_counted(&mut self, sql: &str, args: &[Value]) -> Result<u64, QueryError> {
        match self.driver() {
            DriverType::SqlServer => {
                self.execute_params(&format!("SET NOCOUNT OFF; SET ROWCOUNT 0; {sql}"), args).await
            }
            _ => self.execute_params(sql, args).await,
        }
    }
}

// Keys per statement that keep it under BATCH_PARAMS, beside `changes` parameters of its own.
pub(crate) fn key_batch(changes: usize, columns: &[ColumnInfo]) -> usize {
    (BATCH_PARAMS.saturating_sub(changes) / primary_keys(columns).len().max(1)).max(1)
}

// Updates grouped by table and changes, in the order first seen. Debug text tells changes apart, floats included.
fn same_changes(updates: &[RowUpdate]) -> Vec<(&RowUpdate, Vec<&Row>)> {
    let mut groups: Vec<(&RowUpdate, Vec<&Row>)> = Vec::new();
    let mut seen: HashMap<String, usize> = HashMap::new();
    for update in updates {
        let changes = format!("{:?}", (&update.schema, &update.table, &update.changes));
        match seen.get(&changes) {
            Some(&ix) => groups[ix].1.push(&update.primary_key),
            None => {
                seen.insert(changes, groups.len());
                groups.push((update, vec![&update.primary_key]));
            }
        }
    }
    groups
}

fn named(column: &ColumnMeta) -> Option<ColumnSource> {
    match &column.origin {
        Some(ColumnOrigin::Named { schema, table, column }) => {
            Some(ColumnSource { schema: schema.clone(), table: table.clone(), column: column.clone() })
        }
        _ => None,
    }
}

// OIDs and column numbers mean the same on any session of the database, whatever its search_path.
async fn postgres_sources(
    session: &mut Session,
    columns: &[ColumnMeta],
    cancel: &Cancel,
) -> Result<ResultSources, QueryError> {
    let oid = |column: &ColumnMeta| match column.origin {
        Some(ColumnOrigin::Oid { table, column }) => Some((table, column)),
        _ => None,
    };
    let tables: BTreeSet<u32> = columns.iter().filter_map(|c| oid(c).map(|(table, _)| table)).collect();
    if tables.is_empty() {
        return Ok(ResultSources { columns: vec![None; columns.len()], database: String::new() });
    }
    let list = tables.iter().map(u32::to_string).collect::<Vec<_>>().join(", ");
    let sql = format!(
        "SELECT a.attrelid::bigint, a.attnum::int, n.nspname, c.relname, a.attname \
         FROM pg_attribute a JOIN pg_class c ON c.oid = a.attrelid JOIN pg_namespace n ON n.oid = c.relnamespace \
         WHERE a.attrelid IN ({list}) AND a.attnum > 0 AND NOT a.attisdropped"
    );
    let res = session.buffered(&sql, cancel).await?;
    let text = |row: usize, col: usize| res.chunk.display(row, col).unwrap_or_default();
    let mut names = HashMap::new();
    for row in 0..res.chunk.rows() {
        let (Ok(table), Ok(column)) = (text(row, 0).parse::<u32>(), text(row, 1).parse::<i16>()) else { continue };
        let source =
            ColumnSource { schema: text(row, 2).into(), table: text(row, 3).into(), column: text(row, 4).into() };
        names.insert((table, column), source);
    }
    let columns = columns.iter().map(|c| oid(c).and_then(|key| names.get(&key).cloned())).collect();
    Ok(ResultSources { columns, database: String::new() })
}

// Mode 2 names a view rather than its tables, so views stay read-only. Another database's columns are left out.
async fn sql_server_sources(
    session: &mut Session,
    statement: &str,
    width: usize,
    cancel: &Cancel,
) -> Result<ResultSources, QueryError> {
    let sql = format!(
        "SELECT source_database, source_schema, source_table, source_column, DB_NAME() \
         FROM sys.dm_exec_describe_first_result_set({}, NULL, 2) WHERE is_hidden = 0 ORDER BY column_ordinal",
        quote_literal_in(SqlDialect::TSql, statement)
    );
    let res = session.buffered(&sql, cancel).await?;
    let text = |row: usize, col: usize| res.chunk.display(row, col);
    let database = text(0, 4).unwrap_or_default().to_string();
    if res.chunk.rows() != width {
        return Ok(ResultSources { columns: vec![None; width], database });
    }
    let columns = (0..width)
        .map(|row| {
            let source = ColumnSource {
                schema: text(row, 1)?.to_string(),
                table: text(row, 2)?.to_string(),
                column: text(row, 3)?.to_string(),
            };
            (text(row, 0)? == database).then_some(source)
        })
        .collect();
    Ok(ResultSources { columns, database })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn update(id: i64, name: &str) -> RowUpdate {
        RowUpdate {
            schema: "public".into(),
            table: "t".into(),
            primary_key: Row::from([("id".to_string(), Value::Int(id))]),
            changes: Row::from([("name".to_string(), Value::from(name))]),
        }
    }

    #[test]
    fn updates_making_the_same_changes_go_together() {
        let updates = [update(1, "x"), update(2, "y"), update(3, "x")];
        let groups: Vec<(&str, Vec<i64>)> = same_changes(&updates)
            .into_iter()
            .map(|(update, keys)| {
                let name = match &update.changes["name"] {
                    Value::Text(name) => name.as_str(),
                    _ => "",
                };
                (name, keys.iter().map(|key| if let Value::Int(id) = key["id"] { id } else { 0 }).collect())
            })
            .collect();
        assert_eq!(groups, [("x", vec![1, 3]), ("y", vec![2])]);
    }
}
