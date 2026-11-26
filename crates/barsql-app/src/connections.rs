use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use barsql_core::scope;
use barsql_core::{
    ColumnInfo, ConnectionConfig, ConnectionStatus, ConstraintInfo, DatabaseInfo, FunctionList, IndexInfo, ObjectRef,
    QueryError, ReferencingKey, RoutineInfo, SchemaBundle, SchemaInfo, SchemaTables, TableInfo, TableSize, TriggerInfo,
};
use barsql_db::Engine;
use barsql_storage::ConnectionFolder;

use crate::events::AppEvent;
use crate::stores::storage_error;
use crate::{BarApp, CHANGE_ENGINE, PooledEngine, for_mcp, lock};

// A saturated pool must not wedge the sidebar, which loads these lazily.
pub(crate) const SCHEMA_TIMEOUT: Duration = Duration::from_secs(15);
// Completion and hover call this on cache misses.
const COLUMNS_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) async fn within<T>(
    limit: Duration,
    work: impl Future<Output = Result<T, QueryError>>,
) -> Result<T, QueryError> {
    tokio::time::timeout(limit, work).await.map_err(|_| QueryError::message("context deadline exceeded"))?
}

impl BarApp {
    pub fn list_connections(&self) -> Vec<ConnectionConfig> {
        let list = self.inner.stores.connections.list();
        if !self.inner.mcp {
            return list;
        }
        list.into_iter().filter(|cfg| cfg.mcp).map(for_mcp).collect()
    }

    // MCP mode: picks up sharing changes from the app, and closes engines the assistant may no longer use.
    pub(crate) fn reload_connections(&self) -> Result<(), QueryError> {
        self.inner.stores.connections.reload().map_err(storage_error)?;
        let keys: Vec<String> = lock(&self.inner.engines).keys().cloned().collect();
        for key in keys {
            let usable = match key.strip_suffix(CHANGE_ENGINE) {
                Some(id) => self.config(id).is_ok_and(|cfg| cfg.mcp_changes),
                None => self.config(&key).is_ok(),
            };
            if !usable {
                let app = self.clone();
                self.inner.runtime.spawn(async move { app.disconnect(&key).await });
            }
        }
        Ok(())
    }

    // Only a change to how it connects closes the connection, so a rename keeps its tabs' transactions.
    pub(crate) async fn save_connection_inner(
        &self,
        mut cfg: ConnectionConfig,
    ) -> Result<ConnectionConfig, QueryError> {
        cfg.normalize();
        cfg.validate().map_err(QueryError::message)?;
        let before = self.inner.stores.connections.get(&cfg.id);
        if before.is_none() && scope::split_id(&cfg.id).1.is_some() {
            return Err(QueryError::message("a database of a connection can't be saved as a connection"));
        }
        let saved = self.inner.stores.connections.save(cfg).map_err(storage_error)?;
        if before.is_some_and(|before| !before.connects_like(&saved)) {
            self.disconnect(&saved.id).await;
        }
        Ok(saved)
    }

    pub(crate) async fn delete_connection_inner(&self, id: &str) -> Result<bool, QueryError> {
        self.disconnect(id).await;
        self.inner.stores.connections.delete(id).map_err(storage_error)
    }

    pub fn reorder_connections(&self, ordered_ids: &[String]) -> Result<(), QueryError> {
        self.inner.stores.connections.reorder(ordered_ids).map_err(storage_error)
    }

    pub(crate) async fn test_connection_inner(&self, mut cfg: ConnectionConfig) -> Result<(), QueryError> {
        cfg.normalize();
        cfg.validate().map_err(QueryError::message)?;
        Engine::test(&cfg).await
    }

    pub(crate) async fn list_databases_inner(&self, cfg: ConnectionConfig) -> Result<Vec<String>, QueryError> {
        Engine::databases(&cfg).await
    }

    // Through the connection's own pool, or through a database every server has once its own is gone.
    pub(crate) async fn database_list_inner(&self, id: &str) -> Result<Vec<DatabaseInfo>, QueryError> {
        let listed = within(SCHEMA_TIMEOUT, async { self.engine(id).await?.database_list().await }).await;
        match listed {
            Err(error) if barsql_db::is_missing_database(&error) => {
                let names = Engine::databases(&self.config(id)?).await?;
                Ok(names.into_iter().map(|name| DatabaseInfo { name, can_connect: true, system: false }).collect())
            }
            listed => listed,
        }
    }

    pub(crate) async fn database_sizes_inner(&self, id: &str) -> Result<HashMap<String, u64>, QueryError> {
        within(SCHEMA_TIMEOUT, async { self.engine(id).await?.database_sizes().await }).await
    }

    pub(crate) async fn connect_inner(&self, id: &str) -> Result<(), QueryError> {
        self.engine(id).await.map(|_| ())
    }

    pub(crate) async fn engine(&self, id: &str) -> Result<Engine, QueryError> {
        let cfg = self.config(id)?;
        let key = cfg.id.clone();
        self.pooled_engine(&key, cfg).await
    }

    // Reconnects when the connection's settings changed since the engine was opened. `key` may be a CHANGE_ENGINE one.
    pub(crate) async fn pooled_engine(&self, key: &str, mut cfg: ConnectionConfig) -> Result<Engine, QueryError> {
        cfg.normalize();
        cfg.validate().map_err(QueryError::message)?;
        let fingerprint = cfg.fingerprint();
        let flight = self.connect_lock(key);
        let _flight = flight.lock().await;
        let existing = lock(&self.inner.engines).get_mut(key).map(|pooled| {
            pooled.last_used = Instant::now();
            pooled.clone()
        });
        if let Some(pooled) = existing {
            if pooled.fingerprint == fingerprint {
                return Ok(pooled.engine);
            }
            self.close_engine(key).await;
        }
        let engine = Engine::connect(&cfg).await?;
        let pooled = PooledEngine { engine: engine.clone(), fingerprint, last_used: Instant::now() };
        lock(&self.inner.engines).insert(key.to_string(), pooled);
        Ok(engine)
    }

    pub(crate) fn connect_lock(&self, id: &str) -> Arc<tokio::sync::Mutex<()>> {
        lock(&self.inner.connect_locks).entry(id.to_string()).or_default().clone()
    }

    // Cancels running work and rolls back tab transactions before the pool closes. A saved connection takes its
    // databases' pools with it.
    pub(crate) async fn disconnect_inner(&self, id: &str) {
        for key in self.pool_keys(id) {
            let flight = self.connect_lock(&key);
            let _flight = flight.lock().await;
            self.close_engine(&key).await;
        }
    }

    fn pool_keys(&self, id: &str) -> Vec<String> {
        if scope::split_id(id).1.is_some() || id.ends_with(CHANGE_ENGINE) {
            return vec![id.to_string()];
        }
        let mut keys: Vec<String> = lock(&self.inner.engines)
            .keys()
            .filter(|key| key.as_str() != id && scope::base_id(key.strip_suffix(CHANGE_ENGINE).unwrap_or(key)) == id)
            .cloned()
            .collect();
        keys.push(id.to_string());
        keys
    }

    // Another database's pool closes once no tab or job has used it for `idle`. The connection's own stays open.
    pub(crate) async fn close_idle_databases(&self, idle: Duration) {
        let candidates: Vec<String> = lock(&self.inner.engines)
            .iter()
            .filter(|(key, pooled)| scope::split_id(key).1.is_some() && pooled.last_used.elapsed() >= idle)
            .map(|(key, _)| key.clone())
            .collect();
        for key in candidates {
            let flight = self.connect_lock(&key);
            let _flight = flight.lock().await;
            let idle_now = lock(&self.inner.engines).get(&key).is_some_and(|pooled| pooled.last_used.elapsed() >= idle);
            if idle_now && !self.inner.tabs.has_connection(&key) && !self.inner.jobs.has_connection(&key) {
                self.close_engine(&key).await;
            }
        }
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

    pub(crate) async fn connection_status_inner(&self, id: &str) -> Result<ConnectionStatus, QueryError> {
        let engine = self.engine(id).await?;
        let mut status = engine.connection_info().await?;
        status.connected = true;
        Ok(status)
    }

    // Also preloads the server's current schema, where unqualified names resolve, and Postgres "public" (SQL Server
    // "dbo") so the browser isn't empty when the default schema is a custom one.
    pub(crate) async fn load_schema_data_inner(&self, id: &str) -> Result<SchemaBundle, QueryError> {
        let cfg = self.config(id)?;
        let engine = self.engine(id).await?;
        let mut status = within(SCHEMA_TIMEOUT, engine.connection_info()).await?;
        status.connected = true;
        let schemas = within(SCHEMA_TIMEOUT, engine.list_schemas()).await?;
        let browse = cfg.default_browse_schema();
        let mut preload: HashSet<String> = HashSet::from([browse.clone(), status.schema.clone()]);
        preload.extend(cfg.driver.capabilities().preload_schemas.iter().map(|s| s.to_string()));
        let mut loaded_tables = Vec::new();
        for schema in schemas.iter().filter(|s| preload.contains(&s.name)) {
            let tables = within(SCHEMA_TIMEOUT, engine.list_tables(&schema.name)).await?;
            loaded_tables.push(SchemaTables { schema: schema.name.clone(), tables });
        }
        Ok(SchemaBundle { status, schemas, loaded_tables })
    }

    // One minimal request's round trip on an open connection. None for a local file.
    pub(crate) async fn round_trip_inner(&self, id: &str) -> Result<Option<Duration>, QueryError> {
        let engine = self.engine(id).await?;
        within(SCHEMA_TIMEOUT, engine.round_trip()).await
    }

    pub(crate) async fn list_schemas_inner(&self, id: &str) -> Result<Vec<SchemaInfo>, QueryError> {
        let engine = self.engine(id).await?;
        within(SCHEMA_TIMEOUT, engine.list_schemas()).await
    }

    pub(crate) async fn list_tables_inner(&self, id: &str, schema: &str) -> Result<Vec<TableInfo>, QueryError> {
        let engine = self.engine(id).await?;
        within(SCHEMA_TIMEOUT, engine.list_tables(schema)).await
    }

    pub(crate) async fn referencing_keys_inner(
        &self,
        id: &str,
        schema: &str,
        table: &str,
    ) -> Result<Vec<ReferencingKey>, QueryError> {
        let engine = self.engine(id).await?;
        within(SCHEMA_TIMEOUT, engine.referencing_keys(schema, table)).await
    }

    pub(crate) async fn table_sizes_inner(
        &self,
        id: &str,
        schema: &str,
    ) -> Result<HashMap<String, TableSize>, QueryError> {
        let engine = self.engine(id).await?;
        within(SCHEMA_TIMEOUT, engine.table_sizes(schema)).await
    }

    pub(crate) async fn list_columns_inner(
        &self,
        id: &str,
        schema: &str,
        table: &str,
    ) -> Result<Vec<ColumnInfo>, QueryError> {
        let engine = self.engine(id).await?;
        within(COLUMNS_TIMEOUT, engine.list_columns(schema, table)).await
    }

    pub(crate) async fn list_indexes_inner(
        &self,
        id: &str,
        schema: &str,
        table: &str,
    ) -> Result<Vec<IndexInfo>, QueryError> {
        let engine = self.engine(id).await?;
        within(SCHEMA_TIMEOUT, engine.list_indexes(schema, table)).await
    }

    pub(crate) async fn list_constraints_inner(
        &self,
        id: &str,
        schema: &str,
        table: &str,
    ) -> Result<Vec<ConstraintInfo>, QueryError> {
        let engine = self.engine(id).await?;
        within(SCHEMA_TIMEOUT, engine.list_constraints(schema, table)).await
    }

    pub(crate) async fn list_triggers_inner(
        &self,
        id: &str,
        schema: &str,
        table: &str,
    ) -> Result<Vec<TriggerInfo>, QueryError> {
        let engine = self.engine(id).await?;
        within(SCHEMA_TIMEOUT, engine.list_triggers(schema, table)).await
    }

    pub(crate) async fn list_routines_inner(&self, id: &str, schema: &str) -> Result<Vec<RoutineInfo>, QueryError> {
        let engine = self.engine(id).await?;
        within(SCHEMA_TIMEOUT, engine.list_routines(schema)).await
    }

    pub(crate) async fn list_functions_inner(&self, id: &str) -> Result<FunctionList, QueryError> {
        let engine = self.engine(id).await?;
        within(SCHEMA_TIMEOUT, engine.list_functions()).await
    }

    // Reads the catalog only, so it stays available on read-only connections.
    pub(crate) async fn object_ddl_inner(&self, id: &str, object: &ObjectRef) -> Result<String, QueryError> {
        if object.name.is_empty() {
            return Err(QueryError::message("object name is required"));
        }
        let engine = self.engine(id).await?;
        within(SCHEMA_TIMEOUT, engine.object_ddl(object)).await
    }

    pub(crate) async fn count_rows_inner(&self, id: &str, schema: &str, table: &str) -> Result<i64, QueryError> {
        self.count_matching(id, schema, table, "").await
    }

    // Own session and no deadline, since counting a big table takes a while. A disconnect cancels it. `filter` is a
    // table view's WHERE condition, checked as its page query checks it.
    pub(crate) async fn count_matching_inner(
        &self,
        id: &str,
        schema: &str,
        table: &str,
        filter: &str,
    ) -> Result<i64, QueryError> {
        let key = format!("count:{id}:{schema}.{table}");
        let (job, cancel) = self.inner.jobs.start(&key, id);
        let result = async {
            let engine = self.engine(id).await?;
            barsql_sql::validate_table_filter(&engine.driver(), filter).map_err(QueryError::message)?;
            let sql = barsql_sql::dml::build_count(&engine.driver(), schema, table, filter);
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

    pub fn save_folder(&self, folder: ConnectionFolder) -> Result<ConnectionFolder, QueryError> {
        self.inner.stores.connections.save_folder(folder).map_err(storage_error)
    }

    pub fn delete_folder(&self, id: &str) -> Result<(), QueryError> {
        self.inner.stores.connections.delete_folder(id).map_err(storage_error)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use barsql_core::{ConnectionConfig, DriverType, Value};
    use barsql_db::Engine;

    use crate::events::{AppEvent, RunEvent};
    use crate::{BarApp, CHANGE_ENGINE, ChangeOutcome, PooledEngine, lock};

    fn same(a: &Engine, b: &Engine) -> bool {
        matches!((a, b), (Engine::Sqlite(x), Engine::Sqlite(y)) if Arc::ptr_eq(x, y))
    }

    pub(crate) async fn setup() -> (BarApp, String, tempfile::TempDir) {
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
    async fn saving_a_new_name_or_folder_keeps_the_connection_and_its_transactions() {
        let (app, id, dir) = setup().await;
        let first = app.engine(&id).await.unwrap();
        app.begin_transaction(&id, "tab").await.unwrap();
        assert_eq!(run_error(&app, &id, "tab", "CREATE TABLE kept (a INTEGER)").await, None);
        let events = app.events();

        let folder = app.save_folder(barsql_storage::ConnectionFolder { id: String::new(), name: "Work".into() });
        let moved =
            ConnectionConfig { name: "renamed".into(), folder_id: folder.unwrap().id, ..app.config(&id).unwrap() };
        app.save_connection(moved).await.unwrap();
        assert!(same(&first, &app.engine(&id).await.unwrap()), "the engine stays");
        assert!(app.transaction_status("tab"), "the transaction stays open");
        assert!(events.try_recv().is_err(), "nothing was rolled back");

        let changed =
            ConnectionConfig { file_path: dir.path().join("b.db").display().to_string(), ..app.config(&id).unwrap() };
        app.save_connection(changed).await.unwrap();
        assert!(!app.transaction_status("tab"), "a new file closes the connection");
        assert_eq!(events.try_recv(), Ok(AppEvent::TransactionsEnded { tab_ids: vec!["tab".into()] }));
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
        lock(&app.inner.engines)
            .insert(id.clone(), PooledEngine { engine, fingerprint: cfg.fingerprint(), last_used: Instant::now() });
        drop(flight);
        disconnect.await.unwrap();
        assert!(!app.is_connected(&id));
    }

    #[tokio::test]
    async fn mcp_mode_reaches_only_shared_connections_reads_only_and_saves_nothing() {
        let (app, private, dir) = setup().await;
        let data = dir.path().join("data");
        let file = dir.path().join("a.db").display().to_string();
        let cfg = ConnectionConfig {
            name: "shared".into(),
            driver: DriverType::Sqlite,
            file_path: file,
            mcp: true,
            ..Default::default()
        };
        let shared = app.save_connection(cfg).await.unwrap().id;
        let mcp = BarApp::open_for_mcp(&data, tokio::runtime::Handle::current()).unwrap();
        let listed: Vec<_> = mcp.list_connections().into_iter().map(|cfg| (cfg.id, cfg.read_only)).collect();
        assert_eq!(listed, [(shared.clone(), true)]);
        assert!(mcp.connect(&private).await.is_err(), "an unshared connection is out of reach");
        assert!(mcp.engine(&shared).await.unwrap().read_only(), "the driver session is read-only too");
        assert_eq!(run_error(&mcp, &shared, "tab", "SELECT 1").await, None);
        assert!(mcp.execute_query_stream(&shared, "tab", "CREATE TABLE t (a INTEGER)").await.is_err());
        assert!(!data.join("query_history.json").exists(), "MCP mode keeps no history");

        app.save_connection(ConnectionConfig { mcp: true, ..app.config(&private).unwrap() }).await.unwrap();
        mcp.reload_connections().unwrap();
        assert_eq!(mcp.list_connections().len(), 2, "a reload picks up a connection shared since");
    }

    #[tokio::test]
    async fn mcp_changes_need_permission_and_a_preview_rolls_back() {
        let (app, unshared, dir) = setup().await;
        let file = dir.path().join("lab.db").display().to_string();
        let lab = ConnectionConfig {
            name: "lab".into(),
            driver: DriverType::Sqlite,
            file_path: file,
            mcp: true,
            mcp_changes: true,
            ..Default::default()
        };
        let lab = app.save_connection(lab).await.unwrap();
        let mcp = BarApp::open_for_mcp(&dir.path().join("data"), tokio::runtime::Handle::current()).unwrap();
        let count = || async { app.execute_query(&lab.id, "SELECT count(*) FROM t").await.unwrap().rows[0][0].clone() };

        let made =
            mcp.apply_change(&lab.id, "job", "CREATE TABLE t (a INTEGER); INSERT INTO t VALUES (1), (2)", false, false);
        assert_eq!(made.await.unwrap(), ChangeOutcome { statements: 2, affected_rows: 2, committed: true });
        let previewed = mcp.apply_change(&lab.id, "job", "DELETE FROM t WHERE a > 0", true, false).await.unwrap();
        assert_eq!((previewed.affected_rows, previewed.committed), (2, false));
        assert_eq!(count().await, Value::Int(2), "the preview rolled back");
        let committing = "DELETE FROM t WHERE a > 0; COMMIT";
        assert!(mcp.check_change(&lab.id, committing, true).is_err(), "its COMMIT would end the preview");
        assert!(mcp.apply_change(&lab.id, "job", committing, true, false).await.is_err());
        assert_eq!(count().await, Value::Int(2));
        assert!(mcp.execute_query_stream(&lab.id, "tab", "DELETE FROM t").await.is_err(), "reads stay read-only");
        assert!(mcp.engine(&lab.id).await.unwrap().read_only());

        assert!(mcp.apply_change(&unshared, "job", "DELETE FROM t", false, false).await.is_err(), "not shared");
        app.save_connection(ConnectionConfig { confirm_changes: true, ..lab.clone() }).await.unwrap();
        mcp.reload_connections().unwrap();
        assert!(mcp.apply_change(&lab.id, "job", "DELETE FROM t", false, false).await.is_err(), "not confirmed");
        assert!(mcp.apply_change(&lab.id, "job", "DELETE FROM t WHERE a = 1", false, true).await.is_ok());
        let change_engine = format!("{}{CHANGE_ENGINE}", lab.id);
        assert!(mcp.is_connected(&change_engine), "changes run on an engine of their own");
        app.save_connection(ConnectionConfig { read_only: true, ..lab.clone() }).await.unwrap();
        mcp.reload_connections().unwrap();
        assert!(mcp.apply_change(&lab.id, "job", "DELETE FROM t", false, true).await.is_err(), "read-only");
        assert!(!mcp.list_connections()[0].mcp_changes, "and listed as unchangeable");
        assert_eq!(count().await, Value::Int(1));
        for _ in 0..200 {
            if !mcp.is_connected(&change_engine) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(!mcp.is_connected(&change_engine), "the reload closed the writable engine");
    }

    #[tokio::test]
    async fn another_database_resolves_from_the_saved_connection() {
        let (app, _, _dir) = setup().await;
        let pg = ConnectionConfig {
            name: "Forum".into(),
            driver: DriverType::Postgres,
            host: "localhost".into(),
            database: "forum".into(),
            username: "u".into(),
            schema: "app".into(),
            show_all_databases: true,
            ..Default::default()
        };
        let id = app.save_connection(pg).await.unwrap().id;
        let sales = app.config(&format!("{id}/sales")).unwrap();
        assert_eq!((sales.id, sales.database, sales.schema), (format!("{id}/sales"), "sales".into(), String::new()));
        assert_eq!(app.config(&format!("{id}/forum")).unwrap().id, id, "its own database is the connection");
        assert!(app.config("missing/sales").is_err());

        let ghost = ConnectionConfig { id: format!("{id}/sales"), ..app.config(&id).unwrap() };
        assert!(app.save_connection(ghost).await.is_err(), "a database isn't saved as a connection");
        assert_eq!(app.list_connections().len(), 2);
    }

    #[tokio::test]
    async fn a_connection_closes_with_its_databases_and_idle_databases_close_alone() {
        let (app, id, _dir) = setup().await;
        let (a, b) = (format!("{id}/a"), format!("{id}/b"));
        for key in [&id, &a, &b] {
            app.connect(key).await.unwrap();
        }
        app.disconnect(&a).await;
        assert!(app.is_connected(&id) && !app.is_connected(&a) && app.is_connected(&b), "a database closes alone");
        app.disconnect(&id).await;
        assert!(!app.is_connected(&id) && !app.is_connected(&b), "the connection takes its databases along");

        for key in [&id, &a, &b] {
            app.connect(key).await.unwrap();
        }
        assert_eq!(run_error(&app, &b, "tab", "SELECT 1").await, None);
        let long_ago = Instant::now().checked_sub(Duration::from_secs(700)).unwrap();
        for pooled in lock(&app.inner.engines).values_mut() {
            pooled.last_used = long_ago;
        }
        app.close_idle_databases(Duration::from_secs(600)).await;
        assert!(app.is_connected(&id), "the connection's own pool stays");
        assert!(!app.is_connected(&a), "an idle database closes");
        assert!(app.is_connected(&b), "a tab still holds this one");
        app.cleanup_tab("tab").await;
        app.close_idle_databases(Duration::from_secs(600)).await;
        assert!(!app.is_connected(&b));
    }

    #[tokio::test]
    async fn mcp_mode_reaches_other_databases_only_where_the_connection_lists_them() {
        let (app, _, dir) = setup().await;
        let shared = ConnectionConfig {
            name: "shared".into(),
            driver: DriverType::Sqlite,
            file_path: dir.path().join("s.db").display().to_string(),
            mcp: true,
            ..Default::default()
        };
        let id = app.save_connection(shared).await.unwrap().id;
        let mcp = BarApp::open_for_mcp(&dir.path().join("data"), tokio::runtime::Handle::current()).unwrap();
        assert!(mcp.config(&id).is_ok());
        assert!(mcp.config(&format!("{id}/other")).is_err(), "SQLite lists no databases");
        assert!(app.config(&format!("{id}/other")).is_ok(), "the app resolves it regardless");
    }
}
