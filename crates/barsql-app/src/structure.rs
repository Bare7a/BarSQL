use std::time::Instant;

use barsql_core::{QueryError, TableStructure};
use barsql_sql::structure::ChangePlan;

use crate::BarApp;
use crate::connections::{SCHEMA_TIMEOUT, within};

impl BarApp {
    pub(crate) async fn table_structure_inner(
        &self,
        id: &str,
        schema: &str,
        table: &str,
    ) -> Result<TableStructure, QueryError> {
        let engine = self.engine(id).await?;
        within(SCHEMA_TIMEOUT, engine.table_structure(schema, table)).await
    }

    // Like execute_statement: a session of its own, cancellable as `job`, and one history entry for the script.
    pub(crate) async fn apply_structure_inner(
        &self,
        connection_id: &str,
        job: &str,
        plan: &ChangePlan,
    ) -> Result<(), QueryError> {
        let driver = self.config(connection_id)?.driver;
        let script = plan.script(&driver);
        self.guard_execute(connection_id, &script)?;
        let (job_id, cancel) = self.inner.jobs.start(job, connection_id);
        let started = Instant::now();
        let result = async {
            let mut session = self.engine(connection_id).await?.session().await?;
            barsql_db::structure::apply(&mut session, &plan.steps, &cancel).await
        }
        .await;
        self.inner.jobs.end(job, job_id);
        self.record_history(connection_id, &script, started.elapsed().as_millis() as i64, result.as_ref().err());
        result
    }
}
