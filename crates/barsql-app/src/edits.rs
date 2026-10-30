use std::time::Duration;

use barsql_core::{ColumnInfo, DriverType, QueryError, Row, RowDelete, RowUpdate};
use barsql_db::{Cancel, ColumnMeta, ColumnSource, Engine, ResultSources, Session};
use barsql_sql::READ_ONLY_ERROR;
use barsql_sql::select_source::{SelectItem, SelectSource, select_source};

use crate::BarApp;
use crate::tabs::{Slot, TabGuard, unless_cancelled};

// A lookup stuck on a lock gives up, timed from when it has a session.
const LOOKUP_LIMIT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResultTable {
    pub schema: String,
    pub table: String,
    // None for an expression or another table's value.
    pub columns: Vec<Option<String>>,
    // SQL Server's, checked again before edits, since USE moves the session.
    pub database: String,
    pub catalog: Vec<ColumnInfo>,
    // The connection's as the lookup ran, which an apply must still match.
    pub fingerprint: String,
}

#[derive(Debug, Clone, Default)]
pub struct ResultEdits {
    pub delete: RowDelete,
    pub updates: Vec<RowUpdate>,
    pub columns: Vec<ColumnInfo>,
    pub database: String,
    pub fingerprint: String,
}

#[derive(Debug)]
pub struct ApplyError {
    pub error: QueryError,
    // False when refused before any edit ran, which leaves the tab's transaction as it was.
    pub ran: bool,
}

// Where a lookup's statement ran.
struct Origin<'a> {
    connection_id: &'a str,
    tab_id: &'a str,
    // Taken before the first await: a closed tab has none, and making one would open a session nothing closes.
    slot: Option<Slot>,
    // Ends a wait for the tab once its result is gone, so a stale lookup doesn't hold up the next run.
    cancel: &'a Cancel,
}

impl BarApp {
    fn require_writable(&self, connection_id: &str) -> Result<(), QueryError> {
        if self.config(connection_id)?.read_only { Err(QueryError::message(READ_ONLY_ERROR)) } else { Ok(()) }
    }

    pub(crate) async fn update_row_inner(&self, connection_id: &str, update: &RowUpdate) -> Result<(), QueryError> {
        self.update_rows(connection_id, std::slice::from_ref(update)).await
    }

    pub(crate) async fn update_rows_inner(&self, connection_id: &str, updates: &[RowUpdate]) -> Result<(), QueryError> {
        self.require_writable(connection_id)?;
        self.engine(connection_id).await?.update_rows(updates).await
    }

    // Deletes in batches of keys. On failure the count still covers the batches already deleted.
    pub(crate) async fn delete_rows_inner(&self, connection_id: &str, delete: &RowDelete) -> (i64, Option<QueryError>) {
        if let Err(err) = self.require_writable(connection_id) {
            return (0, Some(err));
        }
        match self.engine(connection_id).await {
            Ok(engine) => engine.delete_rows(delete).await,
            Err(err) => (0, Some(err)),
        }
    }

    pub(crate) async fn insert_row_inner(
        &self,
        connection_id: &str,
        schema: &str,
        table: &str,
        values: &Row,
    ) -> Result<Row, QueryError> {
        self.require_writable(connection_id)?;
        self.engine(connection_id).await?.insert_row(schema, table, values).await
    }

    // Off the tab's session where it can be: a failure inside the tab's transaction would abort it, and a wait would
    // hold up the tab's next run.
    pub(crate) async fn result_table_inner(
        &self,
        connection_id: &str,
        tab_id: &str,
        statement: &str,
        columns: &[ColumnMeta],
        cancel: &Cancel,
    ) -> Result<Option<ResultTable>, QueryError> {
        let config = self.config(connection_id)?;
        let Some(select) =
            select_source(&config.driver, statement).filter(|_| config.driver.capabilities().row_editing)
        else {
            return Ok(None);
        };
        let tab = Origin { connection_id, tab_id, slot: self.inner.tabs.existing(tab_id), cancel };
        let engine = self.engine_unless_cancelled(connection_id, cancel).await?;
        let table = self.trace(&engine, &tab, &select, statement, columns).await?;
        Ok(table.map(|table| ResultTable { fingerprint: config.fingerprint(), ..table }))
    }

    async fn trace(
        &self,
        engine: &Engine,
        tab: &Origin<'_>,
        select: &SelectSource,
        statement: &str,
        columns: &[ColumnMeta],
    ) -> Result<Option<ResultTable>, QueryError> {
        let sources = match engine.driver() {
            // OIDs read the same on any session.
            DriverType::Postgres => {
                let mut session = unless_cancelled(engine.pooled_session(), tab.cancel).await?;
                self.sources(&mut session, statement, columns, tab.cancel).await?
            }
            DriverType::SqlServer => self.describe(engine, tab, statement, columns).await?,
            _ => ResultSources::named(columns),
        };
        let (schema, table) = match traced_table(select, &sources.columns, sources.database.clone()) {
            Some(traced) => (traced.schema.clone(), Some(traced)),
            // Turso says nothing about where a column comes from, so the select list does.
            None if engine.driver() == DriverType::Turso => {
                (select.schema.clone().unwrap_or_else(|| engine.default_schema()), None)
            }
            None => return Ok(None),
        };
        let name = table.as_ref().map_or(select.table.as_str(), |table| table.table.as_str());
        let catalog = self.catalog(engine, tab, &schema, name).await?;
        Ok(match table {
            Some(table) => Some(ResultTable { catalog, ..table }),
            None => listed_table(select, schema, catalog, columns),
        })
    }

    // On the session the statement ran on, for its USE and temp tables. One the idle sweep closed, or that broke, took
    // them along, and a new one would trace the result in the default database, so the lookup gives up. Inside a
    // transaction a pooled session describes it, since a cancel there would roll the transaction back under XACT_ABORT,
    // and that session must be in the tab's database.
    async fn describe(
        &self,
        engine: &Engine,
        tab: &Origin<'_>,
        statement: &str,
        columns: &[ColumnMeta],
    ) -> Result<ResultSources, QueryError> {
        let gone = || QueryError::message("the session the result came from has closed; run the query again");
        let slot = tab.slot.clone().ok_or_else(gone)?;
        let mut slot = unless_cancelled(async { Ok(slot.lock_owned().await) }, tab.cancel).await?;
        let session = slot.live_session(tab.connection_id).ok_or_else(gone)?;
        if !session.in_transaction() {
            let sources = self.sources(session, statement, columns, tab.cancel).await;
            self.sync_transaction(tab.tab_id, &mut slot);
            return sources;
        }
        let database = session.current_database().await?;
        drop(slot);
        let mut pooled = unless_cancelled(engine.pooled_session(), tab.cancel).await?;
        let sources = self.sources(&mut pooled, statement, columns, tab.cancel).await?;
        if sources.database != database {
            return Err(QueryError::message(
                "the tab's transaction is in another database than a new session starts in",
            ));
        }
        Ok(sources)
    }

    // The run holding the slot may go on long after its result is gone, so the wait ends with the lookup's cancel.
    async fn held_slot(&self, tab: &Origin<'_>, slot: Slot) -> Result<TabGuard, QueryError> {
        unless_cancelled(self.tab_session(tab.tab_id, slot, tab.connection_id, tab.cancel), tab.cancel).await
    }

    async fn sources(
        &self,
        session: &mut Session,
        statement: &str,
        columns: &[ColumnMeta],
        cancel: &Cancel,
    ) -> Result<ResultSources, QueryError> {
        let limit = cancel.clone();
        let deadline = self.inner.runtime.spawn(async move {
            tokio::time::sleep(LOOKUP_LIMIT).await;
            limit.cancel();
        });
        let sources = session.column_sources(statement, columns, cancel).await;
        deadline.abort();
        sources
    }

    // A transaction keeps SQLite's one connection on the tab's session, so the catalog comes through it then.
    async fn catalog(
        &self,
        engine: &Engine,
        tab: &Origin<'_>,
        schema: &str,
        table: &str,
    ) -> Result<Vec<ColumnInfo>, QueryError> {
        let held = engine.driver() == DriverType::Sqlite && self.inner.tabs.in_transaction(tab.tab_id);
        let Some(slot) = tab.slot.clone().filter(|_| held) else { return engine.list_columns(schema, table).await };
        let mut slot = self.held_slot(tab, slot).await?;
        let session = slot.session.as_mut().expect("tab session");
        let catalog = session.list_columns(engine, schema, table).await;
        self.sync_transaction(tab.tab_id, &mut slot);
        catalog
    }

    // Inside the tab's transaction when one is open, else in one of its own. Returns the deletes that found no row.
    pub(crate) async fn apply_result_edits_inner(
        &self,
        connection_id: &str,
        tab_id: &str,
        edits: &ResultEdits,
    ) -> Result<i64, ApplyError> {
        let refused = |error| ApplyError { error, ran: false };
        self.require_writable(connection_id).map_err(refused)?;
        let mut slot = self
            .tab_session(tab_id, self.inner.tabs.slot(tab_id), connection_id, &Cancel::new())
            .await
            .map_err(refused)?;
        // An edited connection may point elsewhere now, under the same name and driver or not.
        if self.config(connection_id).map_err(refused)?.fingerprint() != edits.fingerprint {
            return Err(refused(QueryError::message("the connection changed since the query ran; run it again")));
        }
        let session = slot.session.as_mut().expect("tab session");
        let result = apply_in(session, edits).await;
        self.sync_transaction(tab_id, &mut slot);
        result
    }
}

async fn apply_in(session: &mut Session, edits: &ResultEdits) -> Result<i64, ApplyError> {
    let refused = |error| ApplyError { error, ran: false };
    if session.current_database().await.map_err(refused)? != edits.database {
        return Err(refused(QueryError::message(
            "the tab's session is in another database than the result came from; run the query again",
        )));
    }
    let applied = session.apply_row_edits(&edits.delete, &edits.updates, &edits.columns).await;
    applied.map_err(|error| ApplyError { error, ran: true })
}

// Columns traced elsewhere come from a subquery in the select list.
fn traced_table(select: &SelectSource, sources: &[Option<ColumnSource>], database: String) -> Option<ResultTable> {
    let ours = |source: &ColumnSource| {
        source.table.eq_ignore_ascii_case(&select.table)
            && select.schema.as_ref().is_none_or(|schema| schema.eq_ignore_ascii_case(&source.schema))
    };
    let first = sources.iter().flatten().find(|source| ours(source))?;
    let same = |source: &ColumnSource| source.schema == first.schema && source.table == first.table;
    let columns = sources.iter().map(|source| source.as_ref().filter(|s| same(s)).map(|s| s.column.clone())).collect();
    Some(ResultTable {
        schema: first.schema.clone(),
        table: first.table.clone(),
        columns,
        database,
        ..Default::default()
    })
}

// A mismatch with the result's own columns means the list was misread, so nothing is editable.
fn listed_table(
    select: &SelectSource,
    schema: String,
    catalog: Vec<ColumnInfo>,
    columns: &[ColumnMeta],
) -> Option<ResultTable> {
    let known = |name: &str| catalog.iter().find(|c| c.name.eq_ignore_ascii_case(name)).map(|c| c.name.clone());
    let mut expanded: Vec<(Option<String>, Option<String>)> = Vec::new();
    for item in &select.items {
        match item {
            SelectItem::All => expanded.extend(catalog.iter().map(|c| (Some(c.name.clone()), Some(c.name.clone())))),
            SelectItem::Column { name, alias } => {
                let source = Some(known(name)?);
                expanded.push((source, Some(alias.clone().unwrap_or_else(|| name.clone()))));
            }
            SelectItem::Other => expanded.push((None, None)),
        }
    }
    let matches = expanded.len() == columns.len()
        && expanded
            .iter()
            .zip(columns)
            .all(|((_, shown), meta)| shown.as_ref().is_none_or(|shown| shown.eq_ignore_ascii_case(&meta.name)));
    let columns: Vec<Option<String>> = expanded.into_iter().map(|(source, _)| source).collect();
    (matches && columns.iter().any(Option::is_some)).then(|| ResultTable {
        schema,
        table: select.table.clone(),
        columns,
        catalog,
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use barsql_db::ColumnOrigin;

    use super::*;
    use crate::connections::tests::setup;

    fn source(table: &str, column: &str) -> Option<ColumnSource> {
        Some(ColumnSource { schema: "public".into(), table: table.into(), column: column.into() })
    }

    fn select(sql: &str) -> SelectSource {
        select_source(&DriverType::Postgres, sql).expect("one table")
    }

    #[test]
    fn traced_columns_keep_their_table_names_and_others_stay_read_only() {
        let traced = traced_table(
            &select("SELECT id, name AS n, upper(name), (SELECT max(x) FROM o) FROM Users"),
            &[source("users", "id"), source("users", "name"), None, source("o", "x")],
            String::new(),
        )
        .expect("traced");
        assert_eq!((traced.schema.as_str(), traced.table.as_str()), ("public", "users"));
        assert_eq!(traced.columns, [Some("id".into()), Some("name".into()), None, None]);
        assert!(
            traced_table(&select("SELECT * FROM users"), &[source("other", "id")], String::new()).is_none(),
            "a source that isn't the FROM's table"
        );
        assert!(traced_table(&select("SELECT * FROM app.users"), &[source("users", "id")], String::new()).is_none());
        assert!(traced_table(&select("SELECT 1 FROM users"), &[None], String::new()).is_none());
    }

    #[test]
    fn a_select_list_maps_columns_against_the_catalog() {
        let catalog: Vec<ColumnInfo> = ["id", "name", "email"]
            .map(|name| ColumnInfo { name: name.into(), is_primary: name == "id", ..Default::default() })
            .to_vec();
        let meta = |names: &[&str]| -> Vec<ColumnMeta> {
            names.iter().map(|n| ColumnMeta { name: n.to_string(), ..Default::default() }).collect()
        };
        let listed = |sql: &str, names: &[&str]| {
            listed_table(&select(sql), "main".into(), catalog.clone(), &meta(names)).map(|table| table.columns)
        };
        let all = vec![Some("id".to_string()), Some("name".into()), Some("email".into())];
        assert_eq!(listed("SELECT * FROM t", &["id", "name", "email"]), Some(all));
        assert_eq!(
            listed("SELECT ID, name AS n, length(email) FROM t", &["ID", "n", "length(email)"]),
            Some(vec![Some("id".into()), Some("name".into()), None])
        );
        assert_eq!(listed("SELECT nope FROM t", &["nope"]), None, "not a column of the table");
        assert_eq!(listed("SELECT * FROM t", &["id", "name"]), None, "the catalog changed since");
        assert_eq!(listed("SELECT 1 FROM t", &["1"]), None, "nothing to edit");
    }

    // A run can hold the tab long after the UI dropped the result, so a cancelled lookup leaves the queue at once.
    #[tokio::test]
    async fn a_cancelled_lookup_stops_waiting_for_the_tab() {
        let (app, id, _dir) = setup().await;
        app.execute_query(&id, "CREATE TABLE t (id INTEGER PRIMARY KEY)").await.unwrap();
        // In a transaction, SQLite's catalog comes through the tab's session.
        app.begin_transaction(&id, "tab").await.unwrap();
        let run = app.inner.tabs.slot("tab").lock_owned().await;
        let origin = ColumnOrigin::Named { schema: "main".into(), table: "t".into(), column: "id".into() };
        let columns = [ColumnMeta { name: "id".into(), type_name: "INTEGER".into(), origin: Some(origin) }];
        let cancel = Cancel::new();
        let lookup = tokio::spawn({
            let (app, id, cancel) = (app.clone(), id.clone(), cancel.clone());
            async move { app.result_table(&id, "tab", "SELECT id FROM t", &columns, &cancel).await }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel.cancel();
        let looked = tokio::time::timeout(Duration::from_secs(5), lookup).await.expect("the cancel ends the wait");
        assert!(looked.unwrap().is_err_and(|error| error.cancelled));
        drop(run);
        app.rollback_transaction("tab").await.unwrap();
    }
}
