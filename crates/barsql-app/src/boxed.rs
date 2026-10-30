// BarApp's public async methods. Each crate that awaits a future compiles its own copy of the async code below it, so
// these box them: the engines' async code compiles once, here, for the GUI and the MCP server alike.

use barsql_core::{
    ColumnInfo, ConnectionConfig, ConnectionStatus, ConstraintInfo, FunctionList, IndexInfo, ObjectRef, QueryError,
    RoutineInfo, Row, RowDelete, RowUpdate, SchemaBundle, SchemaInfo, TableDataRequest, TableInfo, TableStructure,
    TriggerInfo,
};
use barsql_db::{Cancel, ColumnMeta};
use barsql_sql::QueryPlan;
use barsql_sql::structure::ChangePlan;
use futures_util::future::BoxFuture;

use crate::events::{ImportHandle, RunHandle};
use crate::{
    ApplyError, BackupOutcome, BackupRequest, BarApp, BufferedResult, CsvImportRequest, ResultEdits, ResultTable,
    SqlImportRequest,
};

impl BarApp {
    pub fn backup_table<'a>(
        &'a self,
        connection_id: &'a str,
        job: &'a str,
        req: BackupRequest,
        progress: async_channel::Sender<usize>,
    ) -> BoxFuture<'a, Result<BackupOutcome, QueryError>> {
        Box::pin(self.backup_table_inner(connection_id, job, req, progress))
    }

    pub fn save_connection(&self, cfg: ConnectionConfig) -> BoxFuture<'_, Result<ConnectionConfig, QueryError>> {
        Box::pin(self.save_connection_inner(cfg))
    }

    pub fn delete_connection<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<bool, QueryError>> {
        Box::pin(self.delete_connection_inner(id))
    }

    pub fn test_connection(&self, cfg: ConnectionConfig) -> BoxFuture<'_, Result<(), QueryError>> {
        Box::pin(self.test_connection_inner(cfg))
    }

    pub fn list_databases(&self, cfg: ConnectionConfig) -> BoxFuture<'_, Result<Vec<String>, QueryError>> {
        Box::pin(self.list_databases_inner(cfg))
    }

    pub fn connect<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<(), QueryError>> {
        Box::pin(self.connect_inner(id))
    }

    pub fn disconnect<'a>(&'a self, id: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(self.disconnect_inner(id))
    }

    pub fn connection_status<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<ConnectionStatus, QueryError>> {
        Box::pin(self.connection_status_inner(id))
    }

    pub fn load_schema_data<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<SchemaBundle, QueryError>> {
        Box::pin(self.load_schema_data_inner(id))
    }

    pub fn list_schemas<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Vec<SchemaInfo>, QueryError>> {
        Box::pin(self.list_schemas_inner(id))
    }

    pub fn list_tables<'a>(
        &'a self,
        id: &'a str,
        schema: &'a str,
    ) -> BoxFuture<'a, Result<Vec<TableInfo>, QueryError>> {
        Box::pin(self.list_tables_inner(id, schema))
    }

    pub fn list_columns<'a>(
        &'a self,
        id: &'a str,
        schema: &'a str,
        table: &'a str,
    ) -> BoxFuture<'a, Result<Vec<ColumnInfo>, QueryError>> {
        Box::pin(self.list_columns_inner(id, schema, table))
    }

    pub fn list_indexes<'a>(
        &'a self,
        id: &'a str,
        schema: &'a str,
        table: &'a str,
    ) -> BoxFuture<'a, Result<Vec<IndexInfo>, QueryError>> {
        Box::pin(self.list_indexes_inner(id, schema, table))
    }

    pub fn list_constraints<'a>(
        &'a self,
        id: &'a str,
        schema: &'a str,
        table: &'a str,
    ) -> BoxFuture<'a, Result<Vec<ConstraintInfo>, QueryError>> {
        Box::pin(self.list_constraints_inner(id, schema, table))
    }

    pub fn list_triggers<'a>(
        &'a self,
        id: &'a str,
        schema: &'a str,
        table: &'a str,
    ) -> BoxFuture<'a, Result<Vec<TriggerInfo>, QueryError>> {
        Box::pin(self.list_triggers_inner(id, schema, table))
    }

    pub fn list_routines<'a>(
        &'a self,
        id: &'a str,
        schema: &'a str,
    ) -> BoxFuture<'a, Result<Vec<RoutineInfo>, QueryError>> {
        Box::pin(self.list_routines_inner(id, schema))
    }

    pub fn list_functions<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<FunctionList, QueryError>> {
        Box::pin(self.list_functions_inner(id))
    }

    pub fn object_ddl<'a>(&'a self, id: &'a str, object: &'a ObjectRef) -> BoxFuture<'a, Result<String, QueryError>> {
        Box::pin(self.object_ddl_inner(id, object))
    }

    pub fn count_rows<'a>(
        &'a self,
        id: &'a str,
        schema: &'a str,
        table: &'a str,
    ) -> BoxFuture<'a, Result<i64, QueryError>> {
        Box::pin(self.count_rows_inner(id, schema, table))
    }

    pub fn count_matching<'a>(
        &'a self,
        id: &'a str,
        schema: &'a str,
        table: &'a str,
        filter: &'a str,
    ) -> BoxFuture<'a, Result<i64, QueryError>> {
        Box::pin(self.count_matching_inner(id, schema, table, filter))
    }

    pub fn update_row<'a>(
        &'a self,
        connection_id: &'a str,
        update: &'a RowUpdate,
    ) -> BoxFuture<'a, Result<(), QueryError>> {
        Box::pin(self.update_row_inner(connection_id, update))
    }

    pub fn update_rows<'a>(
        &'a self,
        connection_id: &'a str,
        updates: &'a [RowUpdate],
    ) -> BoxFuture<'a, Result<(), QueryError>> {
        Box::pin(self.update_rows_inner(connection_id, updates))
    }

    pub fn delete_rows<'a>(
        &'a self,
        connection_id: &'a str,
        delete: &'a RowDelete,
    ) -> BoxFuture<'a, (i64, Option<QueryError>)> {
        Box::pin(self.delete_rows_inner(connection_id, delete))
    }

    pub fn insert_row<'a>(
        &'a self,
        connection_id: &'a str,
        schema: &'a str,
        table: &'a str,
        values: &'a Row,
    ) -> BoxFuture<'a, Result<Row, QueryError>> {
        Box::pin(self.insert_row_inner(connection_id, schema, table, values))
    }

    pub fn result_table<'a>(
        &'a self,
        connection_id: &'a str,
        tab_id: &'a str,
        statement: &'a str,
        columns: &'a [ColumnMeta],
        cancel: &'a Cancel,
    ) -> BoxFuture<'a, Result<Option<ResultTable>, QueryError>> {
        Box::pin(self.result_table_inner(connection_id, tab_id, statement, columns, cancel))
    }

    pub fn apply_result_edits<'a>(
        &'a self,
        connection_id: &'a str,
        tab_id: &'a str,
        edits: &'a ResultEdits,
    ) -> BoxFuture<'a, Result<i64, ApplyError>> {
        Box::pin(self.apply_result_edits_inner(connection_id, tab_id, edits))
    }

    pub fn import_csv<'a>(
        &'a self,
        connection_id: &'a str,
        import_id: &'a str,
        req: CsvImportRequest,
    ) -> BoxFuture<'a, Result<ImportHandle, QueryError>> {
        Box::pin(self.import_csv_inner(connection_id, import_id, req))
    }

    pub fn import_sql<'a>(
        &'a self,
        connection_id: &'a str,
        import_id: &'a str,
        req: SqlImportRequest,
    ) -> BoxFuture<'a, Result<ImportHandle, QueryError>> {
        Box::pin(self.import_sql_inner(connection_id, import_id, req))
    }

    pub fn shutdown(&self) -> BoxFuture<'_, ()> {
        Box::pin(self.shutdown_inner())
    }

    pub fn execute_query_stream<'a>(
        &'a self,
        connection_id: &'a str,
        tab_id: &'a str,
        sql: &'a str,
    ) -> BoxFuture<'a, Result<RunHandle, QueryError>> {
        Box::pin(self.execute_query_stream_inner(connection_id, tab_id, sql))
    }

    pub fn query_table_stream<'a>(
        &'a self,
        connection_id: &'a str,
        tab_id: &'a str,
        req: TableDataRequest,
    ) -> BoxFuture<'a, Result<RunHandle, QueryError>> {
        Box::pin(self.query_table_stream_inner(connection_id, tab_id, req))
    }

    pub fn begin_transaction<'a>(
        &'a self,
        connection_id: &'a str,
        tab_id: &'a str,
    ) -> BoxFuture<'a, Result<(), QueryError>> {
        Box::pin(self.begin_transaction_inner(connection_id, tab_id))
    }

    pub fn commit_transaction<'a>(&'a self, tab_id: &'a str) -> BoxFuture<'a, Result<(), QueryError>> {
        Box::pin(self.commit_transaction_inner(tab_id))
    }

    pub fn rollback_transaction<'a>(&'a self, tab_id: &'a str) -> BoxFuture<'a, Result<(), QueryError>> {
        Box::pin(self.rollback_transaction_inner(tab_id))
    }

    pub fn explain_query<'a>(
        &'a self,
        connection_id: &'a str,
        tab_id: &'a str,
        sql: &'a str,
        analyze: bool,
    ) -> BoxFuture<'a, Result<QueryPlan, QueryError>> {
        Box::pin(self.explain_query_inner(connection_id, tab_id, sql, analyze))
    }

    pub fn execute_statement<'a>(
        &'a self,
        connection_id: &'a str,
        job: &'a str,
        sql: &'a str,
    ) -> BoxFuture<'a, Result<(), QueryError>> {
        Box::pin(self.execute_statement_inner(connection_id, job, sql))
    }

    pub fn execute_query<'a>(
        &'a self,
        connection_id: &'a str,
        sql: &'a str,
    ) -> BoxFuture<'a, Result<BufferedResult, QueryError>> {
        Box::pin(self.execute_query_inner(connection_id, sql))
    }

    pub fn table_structure<'a>(
        &'a self,
        id: &'a str,
        schema: &'a str,
        table: &'a str,
    ) -> BoxFuture<'a, Result<TableStructure, QueryError>> {
        Box::pin(self.table_structure_inner(id, schema, table))
    }

    pub fn apply_structure<'a>(
        &'a self,
        connection_id: &'a str,
        job: &'a str,
        plan: &'a ChangePlan,
    ) -> BoxFuture<'a, Result<(), QueryError>> {
        Box::pin(self.apply_structure_inner(connection_id, job, plan))
    }

    pub fn cleanup_tab<'a>(&'a self, tab_id: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(self.cleanup_tab_inner(tab_id))
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    #[test]
    fn every_public_async_method_is_boxed() {
        fn check(dir: &Path) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    check(&path);
                } else if path.extension().is_some_and(|ext| ext == "rs") {
                    let source = std::fs::read_to_string(&path).unwrap();
                    assert!(!source.contains(concat!("pub ", "async fn")), "{}: box it in boxed.rs", path.display());
                }
            }
        }
        check(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"));
    }
}
