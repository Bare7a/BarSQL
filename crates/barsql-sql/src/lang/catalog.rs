use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use barsql_core::{ColumnInfo, DriverType, SchemaInfo, TableInfo};

use super::cache::Lru;
use super::query::ParsedQuery;

// Completion, hover and diagnostics reparse on every keystroke, and most statements haven't changed.
const PARSE_CACHE: usize = 256;

// Build once per schema load. Parsed statements are cached on it, so a new snapshot starts fresh.
#[derive(Debug)]
pub struct Catalog {
    pub driver: DriverType,
    pub schemas: Vec<SchemaInfo>,
    pub tables: Vec<TableInfo>,
    by_name: HashMap<String, Vec<usize>>,
    by_schema: HashMap<String, Vec<usize>>,
    schema_names: HashSet<String>,
    parsed: Mutex<Lru<String, Arc<ParsedQuery>>>,
}

impl Clone for Catalog {
    fn clone(&self) -> Self {
        Self::new(self.driver.clone(), self.schemas.clone(), self.tables.clone())
    }
}

impl Default for Catalog {
    fn default() -> Self {
        Self::new(DriverType::default(), Vec::new(), Vec::new())
    }
}

// Keyed by `schema.table`, see `column_cache_key`.
pub type ColumnMap = HashMap<String, Vec<ColumnInfo>>;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TableBinding {
    pub schema: String,
    pub table: String,
}

impl Catalog {
    pub fn new(driver: DriverType, schemas: Vec<SchemaInfo>, tables: Vec<TableInfo>) -> Self {
        let mut catalog = Self {
            driver,
            schemas,
            tables,
            by_name: HashMap::new(),
            by_schema: HashMap::new(),
            schema_names: HashSet::new(),
            parsed: Mutex::new(Lru::new(PARSE_CACHE)),
        };
        for (i, t) in catalog.tables.iter().enumerate() {
            catalog.by_name.entry(t.name.to_lowercase()).or_default().push(i);
            catalog.by_schema.entry(t.schema.clone()).or_default().push(i);
            catalog.schema_names.insert(t.schema.to_lowercase());
        }
        catalog
    }

    pub(crate) fn cached_parse(&self, sql: &str, parse: impl FnOnce() -> ParsedQuery) -> Arc<ParsedQuery> {
        let lock = || self.parsed.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(hit) = lock().get(sql) {
            return hit;
        }
        let parsed = Arc::new(parse());
        lock().insert(sql.to_string(), parsed.clone());
        parsed
    }

    pub fn has_schema_of_tables(&self, lower: &str) -> bool {
        self.schema_names.contains(lower)
    }

    pub(crate) fn lookup_table(&self, name: &str, schema_hint: Option<&str>) -> Option<&TableInfo> {
        let candidates = self.by_name.get(&name.to_lowercase())?;
        let hinted = schema_hint
            .filter(|h| !h.is_empty())
            .map(str::to_lowercase)
            .and_then(|hint| candidates.iter().map(|&i| &self.tables[i]).find(|t| t.schema.to_lowercase() == hint));
        hinted.or_else(|| candidates.first().map(|&i| &self.tables[i]))
    }

    pub(crate) fn resolve_table_name(&self, name: &str, schema_hint: Option<&str>) -> TableBinding {
        let found = self.lookup_table(name, schema_hint);
        let mut schema =
            schema_hint.filter(|h| !h.is_empty()).map(str::to_string).or_else(|| found.map(|t| t.schema.clone()));
        if schema.as_deref().is_none_or(str::is_empty) {
            let first = self.schemas.first().map(|s| s.name.clone());
            schema = Some(if self.driver == DriverType::Postgres {
                self.schemas
                    .iter()
                    .find(|s| s.name == "public")
                    .map(|s| s.name.clone())
                    .or(first)
                    .unwrap_or_else(|| "public".into())
            } else {
                first.unwrap_or_default()
            });
        }
        TableBinding {
            schema: schema.unwrap_or_default(),
            table: found.map_or_else(|| name.to_string(), |t| t.name.clone()),
        }
    }

    // Tries the exact schema name first, then a case-insensitive match.
    pub fn tables_in_schema(&self, schema: &str) -> Vec<&TableInfo> {
        if let Some(ix) = self.by_schema.get(schema) {
            return ix.iter().map(|&i| &self.tables[i]).collect();
        }
        let lower = schema.to_lowercase();
        self.tables.iter().filter(|t| t.schema.to_lowercase() == lower).collect()
    }
}

// Keeps insertion order. Re-inserting a key keeps its original position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderedMap<V> {
    entries: Vec<(String, V)>,
}

impl<V> Default for OrderedMap<V> {
    fn default() -> Self {
        Self { entries: Vec::new() }
    }
}

impl<V> OrderedMap<V> {
    pub fn get(&self, key: &str) -> Option<&V> {
        self.entries.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    pub fn contains_key(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    pub fn insert(&mut self, key: String, value: V) {
        match self.entries.iter_mut().find(|(k, _)| *k == key) {
            Some(entry) => entry.1 = value,
            None => self.entries.push((key, value)),
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &V)> {
        self.entries.iter().map(|(k, v)| (k.as_str(), v))
    }

    pub fn values(&self) -> impl Iterator<Item = &V> {
        self.entries.iter().map(|(_, v)| v)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}
