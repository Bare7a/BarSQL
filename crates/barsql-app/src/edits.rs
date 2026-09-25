use barsql_core::{QueryError, Row, RowDelete, RowUpdate};
use barsql_sql::READ_ONLY_ERROR;

use crate::BarApp;

impl BarApp {
    fn require_writable(&self, connection_id: &str) -> Result<(), QueryError> {
        if self.config(connection_id)?.read_only { Err(QueryError::message(READ_ONLY_ERROR)) } else { Ok(()) }
    }

    pub async fn update_row(&self, connection_id: &str, update: &RowUpdate) -> Result<(), QueryError> {
        self.require_writable(connection_id)?;
        self.engine(connection_id).await?.update_row(update).await
    }

    // Deletes key by key. On failure the count still covers the rows already deleted.
    pub async fn delete_rows(&self, connection_id: &str, delete: &RowDelete) -> (i64, Option<QueryError>) {
        if let Err(err) = self.require_writable(connection_id) {
            return (0, Some(err));
        }
        match self.engine(connection_id).await {
            Ok(engine) => engine.delete_rows(delete).await,
            Err(err) => (0, Some(err)),
        }
    }

    pub async fn insert_row(
        &self,
        connection_id: &str,
        schema: &str,
        table: &str,
        values: &Row,
    ) -> Result<Row, QueryError> {
        self.require_writable(connection_id)?;
        self.engine(connection_id).await?.insert_row(schema, table, values).await
    }
}
