use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use barsql_core::{ColumnInfo, ConnectionStatus, DriverType, ReferencingKey, SchemaInfo, TableInfo, TableSize};
use barsql_sql::lang::quoting::column_cache_key;
use barsql_sql::lang::{Catalog, FunctionCatalog};
use barsql_sql::schema_diff::Tables;
use futures_util::FutureExt as _;
use futures_util::future::Shared;
use gpui_kit::{App, AsyncApp, Global, Task};

use crate::state;

pub type Columns = Arc<Vec<ColumnInfo>>;
pub type ColumnsLoad = Shared<Task<Result<Columns, String>>>;
pub type References = Arc<Vec<Reference>>;

// Another table's foreign key to a table. `columns` pairs each referencing column with the one it points at.
#[derive(Debug, Clone, PartialEq)]
pub struct Reference {
    pub schema: String,
    pub table: String,
    pub columns: Vec<(String, String)>,
}

// Tagged with an id while it runs, as `pending` is, so a load an invalidation overtook can't settle.
enum Load<T> {
    Running(u64),
    Done(T),
}

impl<T> Load<T> {
    fn done(&self) -> Option<&T> {
        match self {
            Load::Done(value) => Some(value),
            Load::Running(_) => None,
        }
    }
}

// Shared by the sidebar tree and the editors. Tables load per schema once something opens, names or searches it,
// columns on first use, and failed loads aren't cached, so the next use retries.
#[derive(Default)]
pub struct Schemas {
    connections: HashMap<String, ConnectionSchema>,
}

impl Global for Schemas {}

#[derive(Default)]
pub struct ConnectionSchema {
    driver: DriverType,
    // What the server said about itself when the schema loaded: database, user, version.
    status: Option<ConnectionStatus>,
    latency_ms: Option<u64>,
    schemas: Option<Vec<SchemaInfo>>,
    tables: HashMap<String, Vec<TableInfo>>,
    tables_loading: HashSet<String>,
    // The editor asks for these again only after a reload. The tree's own loads still retry.
    tables_failed: HashSet<String>,
    // A failed load is kept empty, so the tree doesn't ask again on every frame.
    sizes: HashMap<String, Load<HashMap<String, TableSize>>>,
    // A failed load is kept as None until the row viewer comes back to its table.
    references: HashMap<String, Load<Option<References>>>,
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
    pub fn status(&self) -> Option<&ConnectionStatus> {
        self.status.as_ref()
    }

    pub fn latency_ms(&self) -> Option<u64> {
        self.latency_ms
    }

    pub fn schemas(&self) -> Option<&[SchemaInfo]> {
        self.schemas.as_deref()
    }

    pub fn tables(&self, schema: &str) -> Option<&[TableInfo]> {
        self.tables.get(schema).map(Vec::as_slice)
    }

    pub fn tables_loading(&self, schema: &str) -> bool {
        self.tables_loading.contains(schema)
    }

    pub fn size(&self, schema: &str, table: &str) -> Option<TableSize> {
        self.sizes.get(schema)?.done()?.get(table).copied()
    }

    pub fn sizes_loaded(&self, schema: &str) -> bool {
        self.sizes.get(schema).is_some_and(|load| load.done().is_some())
    }

    pub fn sizes_requested(&self, schema: &str) -> bool {
        self.sizes.contains_key(schema)
    }

    pub fn references(&self, schema: &str, table: &str) -> Option<&References> {
        self.references.get(&column_cache_key(schema, table))?.done()?.as_ref()
    }

    pub fn references_requested(&self, schema: &str, table: &str) -> bool {
        self.references.contains_key(&column_cache_key(schema, table))
    }

    #[cfg(test)]
    pub fn references_loading(&self, schema: &str, table: &str) -> bool {
        matches!(self.references.get(&column_cache_key(schema, table)), Some(Load::Running(_)))
    }

    fn next_load(&mut self) -> u64 {
        self.next_load += 1;
        self.next_load
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

// The column types the loaded tables use, so a new column can pick from them.
pub fn column_types(cx: &App, connection_id: &str) -> Vec<String> {
    let Some(loaded) = get(cx, connection_id) else { return Vec::new() };
    let mut types: Vec<String> =
        loaded.columns.values().flat_map(|columns| columns.iter().map(|c| c.data_type.clone())).collect();
    types.sort();
    types.dedup();
    types
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
                    entry.status = Some(bundle.status);
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
            cx.update(|cx| {
                load_functions(&id, generation, cx);
                load_latency(&id, generation, cx);
            });
        }
    })
    .detach();
}

// Once per schema until a reload, which also refreshes the estimates.
pub fn load_sizes(connection_id: &str, schema: &str, cx: &mut App) {
    if get(cx, connection_id).is_some_and(|entry| entry.sizes_requested(schema)) {
        return;
    }
    let entry = entry(cx, connection_id);
    let load_id = entry.next_load();
    entry.sizes.insert(schema.to_string(), Load::Running(load_id));
    let generation = entry.generation;
    let bar = state::bar(cx);
    let (id, name) = (connection_id.to_string(), schema.to_string());
    let load = state::spawn(cx, async move { bar.table_sizes(&id, &name).await });
    let (id, name) = (connection_id.to_string(), schema.to_string());
    cx.spawn(async move |cx| {
        let sizes = load.await.and_then(Result::ok).unwrap_or_default();
        apply(cx, &id, generation, |entry| settle(&mut entry.sizes, name, load_id, sizes));
    })
    .detach();
}

// Truncate leaves the old figures stale.
pub fn forget_sizes(connection_id: &str, schema: &str, cx: &mut App) {
    entry(cx, connection_id).sizes.remove(schema);
}

// One catalog query per table until its schema changes, so moving between its rows sends none.
pub fn load_references(connection_id: &str, schema: &str, table: &str, cx: &mut App) {
    if get(cx, connection_id).is_some_and(|entry| entry.references_requested(schema, table)) {
        return;
    }
    let key = column_cache_key(schema, table);
    let entry = entry(cx, connection_id);
    let load_id = entry.next_load();
    entry.references.insert(key.clone(), Load::Running(load_id));
    let generation = entry.generation;
    let bar = state::bar(cx);
    let (id, schema, table) = (connection_id.to_string(), schema.to_string(), table.to_string());
    let load = state::spawn(cx, async move { bar.referencing_keys(&id, &schema, &table).await });
    let id = connection_id.to_string();
    cx.spawn(async move |cx| {
        let references = load.await.and_then(Result::ok).map(|keys| Arc::new(group_keys(&keys)));
        apply(cx, &id, generation, |entry| settle(&mut entry.references, key, load_id, references));
    })
    .detach();
}

pub fn retry_failed_references(connection_id: &str, schema: &str, table: &str, cx: &mut App) {
    let key = column_cache_key(schema, table);
    if get(cx, connection_id).is_some_and(|entry| matches!(entry.references.get(&key), Some(Load::Done(None)))) {
        entry(cx, connection_id).references.remove(&key);
    }
}

// Relies on each key's rows arriving together.
fn group_keys(keys: &[ReferencingKey]) -> Vec<Reference> {
    keys.chunk_by(|a, b| a.key == b.key && a.schema == b.schema && a.table == b.table)
        .map(|rows| Reference {
            schema: rows[0].schema.clone(),
            table: rows[0].table.clone(),
            columns: rows.iter().map(|key| (key.column.clone(), key.referenced_column.clone())).collect(),
        })
        .collect()
}

// Unless an invalidation dropped or replaced the load.
fn settle<T>(loads: &mut HashMap<String, Load<T>>, key: String, load_id: u64, value: T) {
    if matches!(loads.get(&key), Some(Load::Running(running)) if *running == load_id) {
        loads.insert(key, Load::Done(value));
    }
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

// After the schemas too, so the tree doesn't wait on it. Asking connects, so not after a disconnect.
fn load_latency(connection_id: &str, generation: u64, cx: &mut App) {
    if get(cx, connection_id).is_none_or(|entry| entry.generation != generation) {
        return;
    }
    let bar = state::bar(cx);
    let id = connection_id.to_string();
    let load = state::spawn(cx, async move { bar.round_trip(&id).await });
    let id = connection_id.to_string();
    cx.spawn(async move |cx| {
        if let Some(Ok(Some(elapsed))) = load.await {
            apply(cx, &id, generation, |entry| entry.latency_ms = Some(elapsed.as_millis() as u64));
        }
    })
    .detach();
}

// A schema's tables, the first `limit` of them with their columns, and how many it has. The table list loads
// through `load_tables` if it hasn't yet, so this polls for it.
pub fn load_schema_columns(
    connection_id: &str,
    schema: &str,
    limit: usize,
    cx: &mut App,
) -> Task<Result<(usize, Tables), String>> {
    load_tables(connection_id, schema, cx);
    let (id, name) = (connection_id.to_string(), schema.to_string());
    cx.spawn(async move |cx| {
        let tables = loop {
            let state = cx.update(|cx| {
                let entry = get(cx, &id)?;
                Some(match entry.tables(&name) {
                    Some(tables) => Ok(tables.to_vec()),
                    None if entry.tables_loading(&name) => Err(None),
                    None => Err(Some(entry.error().unwrap_or("the tables didn't load").to_string())),
                })
            });
            match state {
                Some(Ok(tables)) => break tables,
                Some(Err(Some(error))) => return Err(error),
                _ => cx.background_executor().timer(std::time::Duration::from_millis(30)).await,
            }
        };
        let total = tables.len();
        let names: Vec<String> = tables.into_iter().take(limit).map(|table| table.name).collect();
        let loads: Vec<ColumnsLoad> =
            cx.update(|cx| names.iter().map(|table| columns(&id, &name, table, cx)).collect());
        let columns = futures_util::future::join_all(loads).await;
        let tables = names
            .into_iter()
            .zip(columns)
            .map(|(table, columns)| (table, columns.map(|c| c.to_vec()).unwrap_or_default()))
            .collect();
        Ok((total, tables))
    })
}

// Checked before `entry`, since borrowing the global mutably wakes all its observers.
pub fn load_tables(connection_id: &str, schema: &str, cx: &mut App) {
    if get(cx, connection_id).is_some_and(|e| e.tables.contains_key(schema) || e.tables_loading.contains(schema)) {
        return;
    }
    let entry = entry(cx, connection_id);
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
                    entry.tables_failed.remove(&name);
                    entry.tables.insert(name, tables);
                    entry.rebuild_catalog();
                }
                Some(Err(error)) => {
                    entry.tables_failed.insert(name);
                    entry.error = Some(error.message);
                }
                None => {}
            }
        });
    })
    .detach();
}

// Loads the tables of the listed schemas `names` name in any case, like `sales` in `sales.orders`, and returns the
// ones still loading.
pub fn load_named_schemas(connection_id: &str, names: &[String], cx: &mut App) -> Vec<String> {
    let Some(entry) = get(cx, connection_id) else { return Vec::new() };
    let named: Vec<String> = entry
        .schemas()
        .unwrap_or_default()
        .iter()
        .filter(|s| names.iter().any(|name| s.name.eq_ignore_ascii_case(name)))
        .filter(|s| entry.tables(&s.name).is_none() && !entry.tables_failed.contains(&s.name))
        .map(|s| s.name.clone())
        .collect();
    for name in &named {
        load_tables(connection_id, name, cx);
    }
    named
}

// Done once none of `schemas` is loading, whether their tables came in or not.
pub fn tables_loaded(connection_id: &str, schemas: Vec<String>, cx: &mut App) -> Task<()> {
    let id = connection_id.to_string();
    cx.spawn(async move |cx| {
        let loading = |cx: &App| get(cx, &id).is_some_and(|entry| schemas.iter().any(|s| entry.tables_loading(s)));
        while cx.update(|cx| loading(cx)) {
            cx.background_executor().timer(std::time::Duration::from_millis(30)).await;
        }
    })
}

// After a rename or drop, which can change any table's references. Also forgets the schema's columns and sizes.
pub fn reload_tables(connection_id: &str, schema: &str, cx: &mut App) {
    let entry = entry(cx, connection_id);
    for table in entry.tables.remove(schema).unwrap_or_default() {
        let key = column_cache_key(schema, &table.name);
        entry.columns.remove(&key);
        entry.pending.remove(&key);
    }
    entry.sizes.remove(schema);
    entry.references.clear();
    load_tables(connection_id, schema, cx);
}

// Dropping the handle is fine, the pending map keeps the new load alive.
pub fn reload_columns(connection_id: &str, schema: &str, table: &str, cx: &mut App) {
    let key = column_cache_key(schema, table);
    let entry = entry(cx, connection_id);
    entry.columns.remove(&key);
    entry.pending.remove(&key);
    entry.references.clear();
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
    let load_id = entry.next_load();
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
    use std::cell::Cell;
    use std::rc::Rc;
    use std::sync::{Arc, Barrier};

    use barsql_core::{DriverType, ReferencingKey, SchemaInfo};
    use gpui_kit::TestAppContext;

    use super::{Columns, Reference, group_keys};
    use crate::test_support::Env;

    #[test]
    fn a_key_over_several_columns_is_one_reference() {
        let key = |table: &str, name: &str, column: &str, referenced: &str| ReferencingKey {
            schema: "main".into(),
            table: table.into(),
            key: name.into(),
            column: column.into(),
            referenced_column: referenced.into(),
        };
        let keys = vec![
            key("lines", "fk_order", "order_id", "id"),
            key("lines", "fk_order", "order_rev", "rev"),
            key("lines", "fk_buyer", "buyer_id", "user_id"),
            key("notes", "fk_order", "order_id", "id"),
        ];
        let reference = |table: &str, pairs: &[(&str, &str)]| Reference {
            schema: "main".into(),
            table: table.into(),
            columns: pairs.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect(),
        };
        assert_eq!(
            group_keys(&keys),
            [
                reference("lines", &[("order_id", "id"), ("order_rev", "rev")]),
                reference("lines", &[("buyer_id", "user_id")]),
                reference("notes", &[("order_id", "id")]),
            ]
        );
    }

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

    // SQLite lists only main, so another schema is listed by hand. Its load lists main's tables.
    #[gpui_kit::test]
    fn a_schema_the_editor_names_loads_its_tables_once(cx: &mut TestAppContext) {
        let env = Env::new(cx);
        let id = env.connection.id.clone();
        cx.update(|cx| super::ensure_loaded(&id, DriverType::Sqlite, cx));
        let loaded = |cx: &mut TestAppContext, schema: &str| {
            cx.update(|cx| super::get(cx, &id).is_some_and(|entry| entry.tables(schema).is_some()))
        };
        for _ in 0..1000 {
            cx.run_until_parked();
            if loaded(cx, "main") {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        cx.update(|cx| super::entry(cx, &id).schemas.as_mut().unwrap().push(SchemaInfo { name: "Other".into() }));
        let named = |cx: &mut TestAppContext, names: &[&str]| {
            let names: Vec<String> = names.iter().map(|name| name.to_string()).collect();
            cx.update(|cx| super::load_named_schemas(&id, &names, cx))
        };
        assert_eq!(named(cx, &["o", "main", "other", "OTHER"]), ["Other"], "listed, not loaded yet, any case, once");
        let woken = Rc::new(Cell::new(0));
        let counter = woken.clone();
        let _watch = cx.update(|cx| cx.observe_global::<super::Schemas>(move |_| counter.set(counter.get() + 1)));
        assert_eq!(named(cx, &["other", "main"]), ["Other"], "still loading");
        assert_eq!(woken.get(), 0, "asking again changed nothing, so no observer woke");
        for _ in 0..1000 {
            cx.run_until_parked();
            if loaded(cx, "Other") {
                assert!(named(cx, &["other"]).is_empty(), "already in");
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("the named schema never loaded");
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
