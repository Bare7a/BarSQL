use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use barsql_core::{ColumnInfo, DriverType, SchemaInfo, TableInfo};
use barsql_sql::lang::quoting::column_cache_key;
use barsql_sql::lang::{Catalog, FunctionCatalog};
use futures_util::FutureExt as _;
use futures_util::future::Shared;
use gpui_kit::{App, AsyncApp, Global, Task};

use crate::state;

pub type Columns = Arc<Vec<ColumnInfo>>;
pub type ColumnsLoad = Shared<Task<Result<Columns, String>>>;

// Shared by the sidebar tree and the editors. Tables load per schema, columns on first use, and failed loads
// aren't cached, so the next use retries.
#[derive(Default)]
pub struct Schemas {
    connections: HashMap<String, ConnectionSchema>,
}

impl Global for Schemas {}

#[derive(Default)]
pub struct ConnectionSchema {
    driver: DriverType,
    schemas: Option<Vec<SchemaInfo>>,
    tables: HashMap<String, Vec<TableInfo>>,
    tables_loading: HashSet<String>,
    catalog: Option<Arc<Catalog>>,
    // The server's functions merged with the built-ins. Until they load, the catalog offers the built-ins alone.
    functions: Option<Arc<FunctionCatalog>>,
    loading: bool,
    error: Option<String>,
    columns: HashMap<String, Columns>,
    // Tagged with a load id, so a load that a reload replaced can't settle the new one.
    pending: HashMap<String, (u64, ColumnsLoad)>,
    next_load: u64,
    // Bumped by a reload, so loads started before it are dropped.
    generation: u64,
}

impl ConnectionSchema {
    pub fn schemas(&self) -> Option<&[SchemaInfo]> {
        self.schemas.as_deref()
    }

    pub fn tables(&self, schema: &str) -> Option<&[TableInfo]> {
        self.tables.get(schema).map(Vec::as_slice)
    }

    pub fn tables_loading(&self, schema: &str) -> bool {
        self.tables_loading.contains(schema)
    }

    pub fn loading(&self) -> bool {
        self.loading
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    // Listed schemas first in their order, then any others sorted by name.
    pub fn loaded_tables(&self) -> Vec<(&str, &[TableInfo])> {
        let listed = self.schemas.as_deref().unwrap_or_default();
        let mut others: Vec<&String> =
            self.tables.keys().filter(|name| !listed.iter().any(|s| &s.name == *name)).collect();
        others.sort();
        listed
            .iter()
            .map(|s| &s.name)
            .chain(others)
            .filter_map(|name| self.tables.get(name).map(|tables| (name.as_str(), tables.as_slice())))
            .collect()
    }

    pub fn columns(&self, schema: &str, table: &str) -> Option<&Columns> {
        self.columns.get(&column_cache_key(schema, table))
    }

    pub fn columns_loading(&self, schema: &str, table: &str) -> bool {
        self.pending.contains_key(&column_cache_key(schema, table))
    }

    fn rebuild_catalog(&mut self) {
        let Some(schemas) = &self.schemas else { return };
        let mut tables: Vec<TableInfo> = Vec::new();
        for schema in schemas {
            tables.extend(self.tables.get(&schema.name).into_iter().flatten().cloned());
        }
        for (name, list) in &self.tables {
            if !schemas.iter().any(|s| &s.name == name) {
                tables.extend(list.iter().cloned());
            }
        }
        let mut catalog = Catalog::new(self.driver.clone(), schemas.clone(), tables);
        if let Some(functions) = &self.functions {
            catalog = catalog.with_functions(functions.clone());
        }
        self.catalog = Some(Arc::new(catalog));
    }
}

pub fn init(cx: &mut App) {
    cx.set_global(Schemas::default());
}

pub fn get<'a>(cx: &'a App, connection_id: &str) -> Option<&'a ConnectionSchema> {
    cx.global::<Schemas>().connections.get(connection_id)
}

// Unknown relations count as tables. Used for a table view's tab icon.
pub fn is_view(connection_id: &str, schema: &str, table: &str, cx: &App) -> bool {
    get(cx, connection_id)
        .and_then(|entry| entry.tables.get(schema))
        .and_then(|tables| tables.iter().find(|t| t.name == table))
        .is_some_and(|t| is_view_kind(&t.kind))
}

pub fn is_view_kind(kind: &str) -> bool {
    kind == "view" || kind == "materialized view"
}

pub fn catalog(cx: &App, connection_id: &str) -> Option<Arc<Catalog>> {
    get(cx, connection_id)?.catalog.clone()
}

fn entry<'a>(cx: &'a mut App, connection_id: &str) -> &'a mut ConnectionSchema {
    cx.global_mut::<Schemas>().connections.entry(connection_id.to_string()).or_default()
}

// Skipped if the schema was reloaded or forgotten since the load started.
fn apply(cx: &mut AsyncApp, connection_id: &str, generation: u64, f: impl FnOnce(&mut ConnectionSchema)) {
    cx.update_global::<Schemas, _>(|schemas, _| {
        if let Some(entry) = schemas.connections.get_mut(connection_id).filter(|e| e.generation == generation) {
            f(entry);
        }
    });
}

pub fn ensure_loaded(connection_id: &str, driver: DriverType, cx: &mut App) {
    let entry = entry(cx, connection_id);
    if entry.schemas.is_some() || entry.loading {
        return;
    }
    load(connection_id, driver, cx);
}

// Drops everything, the editors' cached columns included, and loads again.
pub fn reload(connection_id: &str, driver: DriverType, cx: &mut App) {
    forget(connection_id, cx);
    load(connection_id, driver, cx);
}

// Used on disconnect. The next use loads everything again.
pub fn forget(connection_id: &str, cx: &mut App) {
    let entry = entry(cx, connection_id);
    *entry = ConnectionSchema { generation: entry.generation + 1, ..Default::default() };
}

fn load(connection_id: &str, driver: DriverType, cx: &mut App) {
    let entry = entry(cx, connection_id);
    entry.loading = true;
    entry.error = None;
    entry.driver = driver;
    let generation = entry.generation;
    let bar = state::bar(cx);
    let id = connection_id.to_string();
    let load = state::spawn(cx, async move { bar.load_schema_data(&id).await });
    let id = connection_id.to_string();
    cx.spawn(async move |cx| {
        let bundle = load.await;
        let loaded = matches!(bundle, Some(Ok(_)));
        apply(cx, &id, generation, |entry| {
            entry.loading = false;
            match bundle {
                Some(Ok(bundle)) => {
                    entry.schemas = Some(bundle.schemas);
                    for block in bundle.loaded_tables {
                        entry.tables.insert(block.schema, block.tables);
                    }
                    entry.rebuild_catalog();
                }
                Some(Err(error)) => entry.error = Some(error.message),
                None => {}
            }
        });
        if loaded {
            cx.update(|cx| load_functions(&id, generation, cx));
        }
    })
    .detach();
}

// After the schemas, so the tree isn't waiting on it. A failure keeps the built-ins, and the tree doesn't show it.
fn load_functions(connection_id: &str, generation: u64, cx: &mut App) {
    let Some(driver) = get(cx, connection_id).filter(|e| e.generation == generation).map(|e| e.driver.clone()) else {
        return;
    };
    let bar = state::bar(cx);
    let id = connection_id.to_string();
    let load = state::spawn(cx, async move {
        let list = bar.list_functions(&id).await?;
        Ok::<_, barsql_core::QueryError>(Arc::new(FunctionCatalog::with_server(&driver, list)))
    });
    let id = connection_id.to_string();
    cx.spawn(async move |cx| {
        if let Some(Ok(functions)) = load.await {
            apply(cx, &id, generation, |entry| {
                entry.functions = Some(functions);
                entry.rebuild_catalog();
            });
        }
    })
    .detach();
}

pub fn load_tables(connection_id: &str, schema: &str, cx: &mut App) {
    let entry = entry(cx, connection_id);
    if entry.tables.contains_key(schema) || entry.tables_loading.contains(schema) {
        return;
    }
    entry.tables_loading.insert(schema.to_string());
    let generation = entry.generation;
    let bar = state::bar(cx);
    let (id, name) = (connection_id.to_string(), schema.to_string());
    let load = state::spawn(cx, async move { bar.list_tables(&id, &name).await });
    let (id, name) = (connection_id.to_string(), schema.to_string());
    cx.spawn(async move |cx| {
        let tables = load.await;
        apply(cx, &id, generation, |entry| {
            entry.tables_loading.remove(&name);
            match tables {
                Some(Ok(tables)) => {
                    entry.tables.insert(name, tables);
                    entry.rebuild_catalog();
                }
                Some(Err(error)) => entry.error = Some(error.message),
                None => {}
            }
        });
    })
    .detach();
}

// After a rename or drop. Also forgets the schema's cached columns.
pub fn reload_tables(connection_id: &str, schema: &str, cx: &mut App) {
    let entry = entry(cx, connection_id);
    for table in entry.tables.remove(schema).unwrap_or_default() {
        let key = column_cache_key(schema, &table.name);
        entry.columns.remove(&key);
        entry.pending.remove(&key);
    }
    load_tables(connection_id, schema, cx);
}

// Dropping the handle is fine, the pending map keeps the new load alive.
pub fn reload_columns(connection_id: &str, schema: &str, table: &str, cx: &mut App) {
    let key = column_cache_key(schema, table);
    let entry = entry(cx, connection_id);
    entry.columns.remove(&key);
    entry.pending.remove(&key);
    drop(columns(connection_id, schema, table, cx));
}

// Concurrent callers share one load. Editors ignore failures and the tree shows them.
pub fn columns(connection_id: &str, schema: &str, table: &str, cx: &mut App) -> ColumnsLoad {
    let key = column_cache_key(schema, table);
    let bar = state::bar(cx);
    let entry = entry(cx, connection_id);
    if let Some(columns) = entry.columns.get(&key) {
        return Task::ready(Ok(columns.clone())).shared();
    }
    if let Some((_, pending)) = entry.pending.get(&key) {
        return pending.clone();
    }
    let generation = entry.generation;
    let load_id = entry.next_load;
    entry.next_load += 1;
    let (id, schema, table) = (connection_id.to_string(), schema.to_string(), table.to_string());
    let load = state::spawn(cx, async move { bar.list_columns(&id, &schema, &table).await });
    let (id, cache_key) = (connection_id.to_string(), key.clone());
    let task = cx
        .spawn(async move |cx| {
            let columns = match load.await {
                Some(Ok(columns)) => Ok(Arc::new(columns)),
                Some(Err(error)) => Err(error.message),
                None => Err(String::new()),
            };
            apply(cx, &id, generation, |entry| {
                if entry.pending.get(&cache_key).is_some_and(|(pending, _)| *pending == load_id) {
                    entry.pending.remove(&cache_key);
                    if let Ok(columns) = &columns {
                        entry.columns.insert(cache_key, columns.clone());
                    }
                }
            });
            columns
        })
        .shared();
    self::entry(cx, connection_id).pending.insert(key, (load_id, task.clone()));
    task
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};

    use barsql_core::DriverType;
    use gpui_kit::TestAppContext;

    use super::Columns;
    use crate::test_support::Env;

    // The load is a GPUI task fed by tokio, so the test executor is driven while it runs.
    fn column_names(env: &Env, cx: &mut TestAppContext) -> Vec<String> {
        let id = env.connection.id.clone();
        let load = cx.update(|cx| super::columns(&id, "main", "things", cx));
        for _ in 0..1000 {
            cx.run_until_parked();
            if let Some(columns) = futures_util::FutureExt::now_or_never(load.clone()) {
                return columns.unwrap().iter().map(|c| c.name.clone()).collect();
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("the columns never loaded");
    }

    // SQLite lists no columns for a missing table, hence the empty result.
    #[gpui_kit::test]
    fn concurrent_requests_share_a_load_and_empty_results_wait_for_a_refresh(cx: &mut TestAppContext) {
        let env = Env::new(cx);
        let id = env.connection.id.clone();
        cx.update(|cx| super::ensure_loaded(&id, DriverType::Sqlite, cx));
        let (first, second) =
            cx.update(|cx| (super::columns(&id, "main", "later", cx), super::columns(&id, "main", "later", cx)));
        assert!(cx.update(|cx| super::get(cx, &id).is_some_and(|entry| entry.columns_loading("main", "later"))));
        for _ in 0..1000 {
            cx.run_until_parked();
            if futures_util::FutureExt::now_or_never(second.clone()).is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let first = futures_util::FutureExt::now_or_never(first).expect("the shared load finished");
        assert!(first.unwrap().is_empty(), "no such table yet");
        env.runtime.block_on(env.bar.execute_query(&id, "CREATE TABLE later (x INTEGER)")).unwrap();
        let cached = cx.update(|cx| super::columns(&id, "main", "later", cx));
        assert!(futures_util::FutureExt::now_or_never(cached).unwrap().unwrap().is_empty(), "still the cached result");
        cx.update(|cx| super::reload(&id, DriverType::Sqlite, cx));
        let load = cx.update(|cx| super::columns(&id, "main", "later", cx));
        for _ in 0..1000 {
            cx.run_until_parked();
            if let Some(result) = futures_util::FutureExt::now_or_never(load.clone()) {
                assert_eq!(result.unwrap().iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["x"]);
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("the retried load never finished");
    }

    #[gpui_kit::test]
    fn a_reload_forgets_cached_columns(cx: &mut TestAppContext) {
        let env = Env::new(cx);
        cx.update(|cx| super::ensure_loaded(&env.connection.id, DriverType::Sqlite, cx));
        assert_eq!(column_names(&env, cx), ["id", "name"]);
        env.runtime
            .block_on(env.bar.execute_query(&env.connection.id, "ALTER TABLE things ADD COLUMN extra TEXT"))
            .unwrap();
        assert_eq!(column_names(&env, cx), ["id", "name"], "served from the cache");
        cx.update(|cx| super::reload(&env.connection.id, DriverType::Sqlite, cx));
        assert_eq!(column_names(&env, cx), ["id", "name", "extra"]);
    }

    // Both tokio workers are held while the reload's load is queued, so the older load finishes first.
    #[gpui_kit::test]
    fn a_load_replaced_by_a_reload_leaves_the_new_columns_cached(cx: &mut TestAppContext) {
        let env = Env::new(cx);
        let id = env.connection.id.clone();
        let metrics = env.runtime.metrics();
        let idle = metrics.num_alive_tasks();
        let old = cx.update(|cx| super::columns(&id, "main", "things", cx));
        for _ in 0..5000 {
            if metrics.num_alive_tasks() <= idle {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        env.runtime.block_on(env.bar.execute_query(&id, "ALTER TABLE things ADD COLUMN extra TEXT")).unwrap();

        let workers = metrics.num_workers();
        let (held, release) = (Arc::new(Barrier::new(workers + 1)), Arc::new(Barrier::new(workers + 1)));
        for _ in 0..workers {
            let (held, release) = (held.clone(), release.clone());
            env.runtime.spawn(async move {
                held.wait();
                release.wait();
            });
        }
        held.wait();
        cx.update(|cx| super::reload_columns(&id, "main", "things", cx));
        let new = cx.update(|cx| super::columns(&id, "main", "things", cx));
        cx.run_until_parked();
        release.wait();
        for _ in 0..1000 {
            cx.run_until_parked();
            if futures_util::FutureExt::now_or_never(new.clone()).is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }

        let names = |columns: &Columns| columns.iter().map(|c| c.name.clone()).collect::<Vec<_>>();
        let old = futures_util::FutureExt::now_or_never(old).expect("the old load finished").unwrap();
        assert_eq!(names(&old), ["id", "name"]);
        let cached = cx.update(|cx| super::get(cx, &id).and_then(|entry| entry.columns("main", "things").cloned()));
        assert_eq!(names(&cached.expect("the new columns are cached")), ["id", "name", "extra"]);
    }
}
