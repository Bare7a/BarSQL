use std::sync::Arc;

use barsql_core::schema::primary_keys;
use barsql_core::{
    ColumnInfo, ConnectionStatus, ConstraintInfo, FunctionList, IndexInfo, ObjectRef, QueryError, RoutineInfo,
    SchemaInfo, TableInfo, TriggerInfo,
};

use super::SqliteEngine;
use crate::dump::DumpColumn;
use crate::lite::{LiteSource, catalog};

impl SqliteEngine {
    // The catalog's queries on the file's connection, off the async threads.
    async fn catalog<T: Send + 'static>(
        self: &Arc<Self>,
        work: impl FnOnce(&mut dyn LiteSource) -> Result<T, QueryError> + Send + 'static,
    ) -> Result<T, QueryError> {
        self.with_conn(move |conn| work(&mut super::Rusqlite(conn))).await
    }

    pub async fn connection_info(self: &Arc<Self>) -> Result<ConnectionStatus, QueryError> {
        Ok(ConnectionStatus { connected: true, database: "main".into(), schema: "main".into(), ..Default::default() })
    }

    pub async fn list_schemas(self: &Arc<Self>) -> Result<Vec<SchemaInfo>, QueryError> {
        Ok(vec![SchemaInfo { name: "main".into() }])
    }

    pub(crate) async fn dump_columns(self: &Arc<Self>, table: &str) -> Result<Vec<DumpColumn>, QueryError> {
        let table = table.to_string();
        self.catalog(move |src| catalog::dump_columns(src, &table)).await
    }

    pub async fn list_tables(self: &Arc<Self>, _schema: &str) -> Result<Vec<TableInfo>, QueryError> {
        self.catalog(catalog::tables).await
    }

    pub async fn list_columns(self: &Arc<Self>, _schema: &str, table: &str) -> Result<Vec<ColumnInfo>, QueryError> {
        let table = table.to_string();
        self.catalog(move |src| catalog::columns(src, &table)).await
    }

    pub async fn list_indexes(self: &Arc<Self>, _schema: &str, table: &str) -> Result<Vec<IndexInfo>, QueryError> {
        let table = table.to_string();
        self.catalog(move |src| catalog::indexes(src, &table)).await
    }

    pub async fn list_constraints(
        self: &Arc<Self>,
        _schema: &str,
        table: &str,
    ) -> Result<Vec<ConstraintInfo>, QueryError> {
        let table = table.to_string();
        self.catalog(move |src| catalog::constraints(src, &table)).await
    }

    pub async fn list_triggers(self: &Arc<Self>, _schema: &str, table: &str) -> Result<Vec<TriggerInfo>, QueryError> {
        let table = table.to_string();
        self.catalog(move |src| catalog::triggers(src, &table)).await
    }

    // SQLite has no routines.
    pub async fn list_routines(self: &Arc<Self>, _schema: &str) -> Result<Vec<RoutineInfo>, QueryError> {
        Ok(Vec::new())
    }

    pub async fn list_functions(self: &Arc<Self>) -> Result<FunctionList, QueryError> {
        self.catalog(catalog::functions).await
    }

    pub async fn object_ddl(self: &Arc<Self>, object: &ObjectRef) -> Result<String, QueryError> {
        let object = object.clone();
        self.catalog(move |src| catalog::object_ddl(src, &object)).await
    }

    pub async fn primary_keys(
        self: &Arc<Self>,
        schema: &str,
        table: &str,
    ) -> Result<(Vec<ColumnInfo>, Vec<String>), QueryError> {
        let cols = self.list_columns(schema, table).await?;
        let pks = primary_keys(&cols);
        Ok((cols, pks))
    }
}
