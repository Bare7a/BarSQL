use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use barsql_core::{
    ColumnInfo, ConnectionConfig, ConnectionStatus, ConstraintInfo, DriverType, IndexInfo, ObjectRef, QueryError,
    RoutineInfo, SchemaBundle, SchemaInfo, SchemaTables, TableInfo, TriggerInfo,
};
use barsql_db::Engine;
use barsql_sql::table_ref;
use barsql_storage::ConnectionFolder;

use crate::events::AppEvent;
use crate::{BarApp, PooledEngine, lock};

// A saturated pool must not wedge the sidebar, which loads these lazily.
const SCHEMA_TIMEOUT: Duration = Duration::from_secs(15);
// Completion and hover call this on cache misses.
const COLUMNS_TIMEOUT: Duration = Duration::from_secs(5);

async fn within<T>(limit: Duration, work: impl Future<Output = Result<T, QueryError>>) -> Result<T, QueryError> {
    tokio::time::timeout(limit, work).await.map_err(|_| QueryError::message("context deadline exceeded"))?
}

impl BarApp {
    pub fn list_connections(&self) -> Vec<ConnectionConfig> {
        self.inner.stores.connections.list()
    }

    pub async fn save_connection(&self, mut cfg: ConnectionConfig) -> Result<ConnectionConfig, QueryError> {
        cfg.normalize();
        cfg.validate().map_err(QueryError::message)?;
        let saved = self.inner.stores.connections.save(cfg).map_err(|e| QueryError::message(e.to_string()))?;
        self.disconnect(&saved.id).await;
        Ok(saved)
    }

    pub async fn delete_connection(&self, id: &str) -> bool {
        self.disconnect(id).await;
        self.inner.stores.connections.delete(id).unwrap_or(false)
    }

    pub fn reorder_connections(&self, ordered_ids: &[String]) -> bool {
        self.inner.stores.connections.reorder(ordered_ids).is_ok()
    }

    pub async fn test_connection(&self, mut cfg: ConnectionConfig) -> Result<(), QueryError> {
        cfg.normalize();
        cfg.validate().map_err(QueryError::message)?;
        Engine::test(&cfg).await
    }

    pub async fn list_databases(&self, cfg: ConnectionConfig) -> Result<Vec<String>, QueryError> {
        Engine::databases(&cfg).await
    }

    pub async fn connect(&self, id: &str) -> Result<(), QueryError> {
        self.engine(id).await.map(|_| ())
    }

    // Reconnects when the connection's settings changed since the engine was opened.
    pub(crate) async fn engine(&self, id: &str) -> Result<Engine, QueryError> {
        let mut cfg = self.config(id)?;
        cfg.normalize();
        cfg.validate().map_err(QueryError::message)?;
        let fingerprint = cfg.fingerprint();
        let flight = self.connect_lock(id);
        let _flight = flight.lock().await;
        let existing = lock(&self.inner.engines).get(id).cloned();
        if let Some(pooled) = existing {
            if pooled.fingerprint == fingerprint {
                return Ok(pooled.engine);
            }
            self.close_engine(id).await;
        }
        let engine = Engine::connect(&cfg).await?;
        lock(&self.inner.engines).insert(id.to_string(), PooledEngine { engine: engine.clone(), fingerprint });
        Ok(engine)
    }

    fn connect_lock(&self, id: &str) -> Arc<tokio::sync::Mutex<()>> {
        lock(&self.inner.connect_locks).entry(id.to_string()).or_default().clone()
    }

    // Cancels running work and rolls back tab transactions before the pool closes.
    pub async fn disconnect(&self, id: &str) {
        let flight = self.connect_lock(id);
        let _flight = flight.lock().await;
        self.close_engine(id).await;
    }

    async fn close_engine(&self, id: &str) {
        self.inner.jobs.cancel_connection(id);
        let ended = self.close_tab_sessions(id).await;
        if !ended.is_empty() {
            self.emit(AppEvent::TransactionsEnded { tab_ids: ended });
        }
        let pooled = lock(&self.inner.engines).remove(id);
        if let Some(pooled) = pooled {
            pooled.engine.close().await;
        }
    }

    pub fn is_connected(&self, id: &str) -> bool {
        lock(&self.inner.engines).contains_key(id)
    }

    pub async fn connection_status(&self, id: &str) -> Result<ConnectionStatus, QueryError> {
        let engine = self.engine(id).await?;
        let mut status = engine.connection_info().await?;
        status.connected = true;
        Ok(status)
    }

    // Postgres also preloads "public" so the browser isn't empty when the default schema is a custom one.
    pub async fn load_schema_data(&self, id: &str) -> Result<SchemaBundle, QueryError> {
        let cfg = self.config(id)?;
        let engine = self.engine(id).await?;
        let mut status = engine.connection_info().await?;
        status.connected = true;
        let schemas = engine.list_schemas().await?;
        let browse = cfg.default_browse_schema();
        let mut preload: HashSet<String> = HashSet::from([browse.clone()]);
        if cfg.driver == DriverType::Postgres && browse != "public" {
            preload.insert("public".into());
        }
        let mut loaded_tables = Vec::new();
        for schema in schemas.iter().filter(|s| preload.contains(&s.name)) {
            let tables = engine.list_tables(&schema.name).await?;
            loaded_tables.push(SchemaTables { schema: schema.name.clone(), tables });
        }
        Ok(SchemaBundle { status, schemas, loaded_tables })
    }

    pub async fn list_schemas(&self, id: &str) -> Result<Vec<SchemaInfo>, QueryError> {
        self.engine(id).await?.list_schemas().await
    }

    pub async fn list_tables(&self, id: &str, schema: &str) -> Result<Vec<TableInfo>, QueryError> {
        self.engine(id).await?.list_tables(schema).await
    }

    pub async fn list_columns(&self, id: &str, schema: &str, table: &str) -> Result<Vec<ColumnInfo>, QueryError> {
        let engine = self.engine(id).await?;
        within(COLUMNS_TIMEOUT, engine.list_columns(schema, table)).await
    }

    pub async fn list_indexes(&self, id: &str, schema: &str, table: &str) -> Result<Vec<IndexInfo>, QueryError> {
        let engine = self.engine(id).await?;
        within(SCHEMA_TIMEOUT, engine.list_indexes(schema, table)).await
    }

    pub async fn list_constraints(
        &self,
        id: &str,
        schema: &str,
        table: &str,
    ) -> Result<Vec<ConstraintInfo>, QueryError> {
        let engine = self.engine(id).await?;
        within(SCHEMA_TIMEOUT, engine.list_constraints(schema, table)).await
    }

    pub async fn list_triggers(&self, id: &str, schema: &str, table: &str) -> Result<Vec<TriggerInfo>, QueryError> {
        let engine = self.engine(id).await?;
        within(SCHEMA_TIMEOUT, engine.list_triggers(schema, table)).await
    }

    pub async fn list_routines(&self, id: &str, schema: &str) -> Result<Vec<RoutineInfo>, QueryError> {
        let engine = self.engine(id).await?;
        within(SCHEMA_TIMEOUT, engine.list_routines(schema)).await
    }

    // Reads the catalog only, so it stays available on read-only connections.
    pub async fn object_ddl(&self, id: &str, object: &ObjectRef) -> Result<String, QueryError> {
        if object.name.is_empty() {
            return Err(QueryError::message("object name is required"));
        }
        let engine = self.engine(id).await?;
        within(SCHEMA_TIMEOUT, engine.object_ddl(object)).await
    }

    // Own session and no deadline, since counting a big table takes a while. A disconnect cancels it.
    pub async fn count_rows(&self, id: &str, schema: &str, table: &str) -> Result<i64, QueryError> {
        let key = format!("count:{id}:{schema}.{table}");
        let (job, cancel) = self.inner.jobs.start(&key, id);
        let result = async {
            let engine = self.engine(id).await?;
            let sql = format!("SELECT COUNT(*) FROM {}", table_ref(&engine.driver(), schema, table));
            let mut session = engine.session().await?;
            let result = session.buffered(&sql, &cancel).await?;
            result.text(0, 0).and_then(|count| count.parse().ok()).ok_or_else(|| QueryError::message("no row count"))
        }
        .await;
        self.inner.jobs.end(&key, job);
        result
    }

    pub fn list_folders(&self) -> Vec<ConnectionFolder> {
        self.inner.stores.connections.folders()
    }

    pub fn save_folder(&self, folder: ConnectionFolder) -> ConnectionFolder {
        self.inner.stores.connections.save_folder(folder.clone()).unwrap_or(folder)
    }

    pub fn delete_folder(&self, id: &str) {
        let _ = self.inner.stores.connections.delete_folder(id);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use barsql_core::{ConnectionConfig, DriverType};
    use barsql_db::Engine;

    use crate::events::RunEvent;
    use crate::{BarApp, PooledEngine, lock};

    fn same(a: &Engine, b: &Engine) -> bool {
        matches!((a, b), (Engine::Sqlite(x), Engine::Sqlite(y)) if Arc::ptr_eq(x, y))
    }

    async fn setup() -> (BarApp, String, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let app = BarApp::open(&dir.path().join("data"), tokio::runtime::Handle::current()).unwrap();
        let cfg = ConnectionConfig {
            name: "cache".into(),
            driver: DriverType::Sqlite,
            file_path: dir.path().join("a.db").display().to_string(),
            ..Default::default()
        };
        let id = app.save_connection(cfg).await.unwrap().id;
        (app, id, dir)
    }

    async fn run_error(app: &BarApp, id: &str, tab: &str, sql: &str) -> Option<String> {
        let handle = app.execute_query_stream(id, tab, sql).await.unwrap();
        let mut error = None;
        while let Ok(event) = handle.events.recv().await {
            match event {
                RunEvent::Result(r) => error = error.or(r.error.map(|e| e.message)),
                RunEvent::Done { error: done, .. } => return error.or(done.map(|e| e.message)),
                _ => {}
            }
        }
        error
    }

    #[tokio::test]
    async fn an_unchanged_config_reuses_the_engine() {
        let (app, id, _dir) = setup().await;
        for _ in 0..3 {
            app.connect(&id).await.unwrap();
        }
        assert!(same(&app.engine(&id).await.unwrap(), &app.engine(&id).await.unwrap()));
        assert!(app.is_connected(&id));
    }

    #[tokio::test]
    async fn a_changed_config_reconnects_and_closes_the_old_sessions() {
        let (app, id, dir) = setup().await;
        let first = app.engine(&id).await.unwrap();
        assert_eq!(run_error(&app, &id, "tab", "CREATE TEMP TABLE t (a INTEGER)").await, None);

        let mut cfg = app.config(&id).unwrap();
        cfg.name = "renamed".into();
        cfg.color = "#123456".into();
        app.inner.stores.connections.save(cfg.clone()).unwrap();
        assert!(same(&first, &app.engine(&id).await.unwrap()), "cosmetic changes keep the engine");

        cfg.file_path = dir.path().join("b.db").display().to_string();
        app.inner.stores.connections.save(cfg).unwrap();
        assert!(!same(&first, &app.engine(&id).await.unwrap()), "a new file reconnects");
        assert!(run_error(&app, &id, "tab", "SELECT * FROM t").await.is_some(), "the old tab session was closed");
    }

    #[tokio::test]
    async fn concurrent_connects_open_one_engine() {
        let (app, id, _dir) = setup().await;
        let tasks: Vec<_> = (0..16)
            .map(|_| {
                let (app, id) = (app.clone(), id.clone());
                tokio::spawn(async move { app.engine(&id).await.unwrap() })
            })
            .collect();
        let mut engines = Vec::new();
        for task in tasks {
            engines.push(task.await.unwrap());
        }
        assert!(engines.windows(2).all(|w| same(&w[0], &w[1])));
        assert_eq!(lock(&app.inner.engines).len(), 1);
    }

    #[tokio::test]
    async fn a_disconnect_during_a_connect_wins() {
        let (app, id, _dir) = setup().await;
        let flight = app.connect_lock(&id).lock_owned().await;
        let disconnect = tokio::spawn({
            let (app, id) = (app.clone(), id.clone());
            async move { app.disconnect(&id).await }
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(!disconnect.is_finished(), "the disconnect waits for the in-flight connect");
        // Stand in for the in-flight connect storing its engine.
        let cfg = app.config(&id).unwrap();
        let engine = Engine::connect(&cfg).await.unwrap();
        lock(&app.inner.engines).insert(id.clone(), PooledEngine { engine, fingerprint: cfg.fingerprint() });
        drop(flight);
        disconnect.await.unwrap();
        assert!(!app.is_connected(&id));
    }
}
