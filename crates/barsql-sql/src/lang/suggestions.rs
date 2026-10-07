use std::collections::{HashMap, HashSet};

use barsql_core::{ColumnInfo, DriverType, SqlDialect, TableInfo};

use super::catalog::{Catalog, ColumnMap, TableBinding};
use super::context::StatementShape;
use super::labels::{SqlLabels, fill};
use super::query::QueryTableRef;
use super::quoting::{column_cache_key, format_sql_identifier};
use super::statements::trim_spaces;

pub struct CompletionContext<'a> {
    pub catalog: &'a Catalog,
    // Columns loaded so far, keyed by `schema.table`.
    pub columns_by_table: &'a ColumnMap,
    // Fallback columns when nothing narrower applies. The editor passes none.
    pub columns: &'a [ColumnInfo],
    pub labels: &'a SqlLabels,
}

impl CompletionContext<'_> {
    pub fn driver(&self) -> &DriverType {
        &self.catalog.driver
    }

    fn columns_of(&self, schema: &str, table: &str) -> &[ColumnInfo] {
        self.columns_by_table.get(&column_cache_key(schema, table)).map_or(&[], Vec::as_slice)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    Keyword,
    Field,
    Class,
    Module,
    Function,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionItem {
    pub label: String,
    pub kind: ItemKind,
    pub detail: Option<String>,
    // Shown dimmed right after the label, like a function's arguments.
    pub label_detail: Option<String>,
    pub insert_text: String,
    // `insert_text` is a snippet: `$0` marks where the caret goes, and `\$` is a literal dollar sign.
    pub snippet: bool,
    // Falls back to the label. Identifiers that need quotes carry the quoted form so typing the opening
    // quote still matches.
    pub filter_text: Option<String>,
    // Lower sorts first.
    pub sort_text: String,
}

// `where_ok` means the keyword is still offered inside WHERE.
struct KeywordRule {
    kw: &'static str,
    dialects: DialectSet,
    where_ok: bool,
    gate: fn(&StatementShape) -> bool,
}

// The dialects a keyword belongs to. Turso offers SQLite's.
#[derive(Clone, Copy, PartialEq, Eq)]
struct DialectSet(u8);

impl DialectSet {
    const fn of(dialects: &[SqlDialect]) -> Self {
        let mut bits = 0;
        let mut i = 0;
        while i < dialects.len() {
            bits |= Self::bit(dialects[i]);
            i += 1;
        }
        Self(bits)
    }

    const fn bit(dialect: SqlDialect) -> u8 {
        match dialect {
            SqlDialect::Postgres => 1,
            SqlDialect::MySql => 2,
            SqlDialect::Sqlite => 4,
            SqlDialect::TSql => 8,
            SqlDialect::ClickHouse => 16,
        }
    }

    // A dialect-specific keyword needs a known dialect.
    fn allows(self, dialect: Option<SqlDialect>) -> bool {
        self == ANY || dialect.is_some_and(|d| self.0 & Self::bit(d) != 0)
    }
}

const ANY: DialectSet = DialectSet(0);
const PG: DialectSet = DialectSet::of(&[SqlDialect::Postgres]);
const MY: DialectSet = DialectSet::of(&[SqlDialect::MySql]);
const LITE: DialectSet = DialectSet::of(&[SqlDialect::Sqlite]);
const PG_MY: DialectSet = DialectSet::of(&[SqlDialect::Postgres, SqlDialect::MySql]);
const MY_LITE: DialectSet = DialectSet::of(&[SqlDialect::MySql, SqlDialect::Sqlite]);
const PG_LITE: DialectSet = DialectSet::of(&[SqlDialect::Postgres, SqlDialect::Sqlite]);

fn always(_: &StatementShape) -> bool {
    true
}

const fn rule(
    kw: &'static str,
    dialects: DialectSet,
    where_ok: bool,
    gate: fn(&StatementShape) -> bool,
) -> KeywordRule {
    KeywordRule { kw, dialects, where_ok, gate }
}

const KEYWORD_RULES: &[KeywordRule] = &[
    rule("SELECT", ANY, false, |s| s.at_statement_start),
    rule("INSERT INTO", ANY, false, |s| s.at_statement_start),
    rule("UPDATE", ANY, false, |s| s.at_statement_start),
    rule("DELETE", ANY, false, |s| s.at_statement_start),
    rule("CREATE TABLE", ANY, false, |s| s.at_statement_start),
    rule("ALTER TABLE", ANY, false, |s| s.at_statement_start),
    rule("DROP TABLE", ANY, false, |s| s.at_statement_start),
    rule("EXPLAIN", ANY, false, |s| s.at_statement_start),
    rule("TRUNCATE TABLE", PG_MY, false, |s| s.at_statement_start),
    rule("REPLACE INTO", MY_LITE, false, |s| s.at_statement_start),
    rule("VACUUM", PG_LITE, false, |s| s.at_statement_start),
    rule("PRAGMA", LITE, false, |s| s.at_statement_start),
    rule("SHOW TABLES", MY, false, |s| s.at_statement_start),
    rule("SHOW DATABASES", MY, false, |s| s.at_statement_start),
    rule("FROM", ANY, false, |s| !s.has_from),
    rule("WHERE", ANY, false, |s| !s.in_where),
    rule("JOIN", ANY, false, |s| s.joinable),
    rule("LEFT JOIN", ANY, false, |s| s.joinable),
    rule("RIGHT JOIN", ANY, false, |s| s.joinable),
    rule("INNER JOIN", ANY, false, |s| s.joinable),
    rule("CROSS JOIN", ANY, false, |s| s.joinable),
    rule("FULL JOIN", PG_LITE, false, |s| s.joinable),
    rule("ON", ANY, false, |s| s.has_join),
    rule("GROUP BY", ANY, true, |s| s.has_from && !s.group_by_seen),
    rule("ORDER BY", ANY, true, |s| s.has_from && !s.order_by_seen),
    rule("HAVING", ANY, false, |s| s.group_by_seen),
    rule("LIMIT", ANY, true, always),
    rule("OFFSET", ANY, true, always),
    rule("VALUES", ANY, false, |s| s.has_insert),
    rule("SET", ANY, false, |s| s.has_update),
    rule("UNION", ANY, false, always),
    rule("UNION ALL", ANY, false, always),
    rule("AND", ANY, true, always),
    rule("OR", ANY, true, always),
    rule("NOT", ANY, true, always),
    rule("IN", ANY, true, always),
    rule("LIKE", ANY, true, always),
    rule("BETWEEN", ANY, true, always),
    rule("IS NULL", ANY, true, always),
    rule("IS NOT NULL", ANY, true, always),
    rule("EXISTS", ANY, true, always),
    rule("NULL", ANY, true, always),
    rule("AS", ANY, false, always),
    rule("DISTINCT", ANY, false, always),
    rule("CASE", ANY, false, always),
    rule("WHEN", ANY, false, |s| s.has_case),
    rule("THEN", ANY, false, |s| s.has_case),
    rule("ELSE", ANY, false, |s| s.has_case),
    rule("END", ANY, false, |s| s.has_case),
    // Dialect operators only exist or behave sanely on their own engine.
    rule("NOT LIKE", ANY, true, |s| s.in_filter_clause),
    rule("NOT IN", ANY, true, |s| s.in_filter_clause),
    rule("NOT BETWEEN", ANY, true, |s| s.in_filter_clause),
    rule("ILIKE", PG, true, |s| s.in_filter_clause),
    rule("NOT ILIKE", PG, true, |s| s.in_filter_clause),
    rule("SIMILAR TO", PG, true, |s| s.in_filter_clause),
    rule("IS DISTINCT FROM", PG, true, |s| s.in_filter_clause),
    rule("IS NOT DISTINCT FROM", PG, true, |s| s.in_filter_clause),
    rule("REGEXP", MY, true, |s| s.in_filter_clause),
    rule("RLIKE", MY, true, |s| s.in_filter_clause),
    rule("GLOB", LITE, true, |s| s.in_filter_clause),
    rule("MATCH", LITE, true, |s| s.in_filter_clause),
    rule("RETURNING", PG_LITE, true, |s| s.returning_slot),
    rule("ON CONFLICT", PG_LITE, true, |s| s.insert_body),
    rule("ON DUPLICATE KEY UPDATE", MY, true, |s| s.insert_body),
];

pub fn keywords_for_shape(shape: &StatementShape, driver: Option<&DriverType>) -> Vec<&'static str> {
    KEYWORD_RULES
        .iter()
        .filter(|r| r.dialects.allows(driver.and_then(DriverType::dialect)))
        .filter(|r| !shape.in_where || r.where_ok)
        .filter(|r| (r.gate)(shape))
        .map(|r| r.kw)
        .collect()
}

// None = no match, 0 = starts with, 1 = substring. A leading quote on the partial word is ignored.
pub fn match_score(label: &str, lc_prefix: &str) -> Option<u8> {
    let needle = lc_prefix.strip_prefix(['"', '\'', '`']).unwrap_or(lc_prefix);
    if needle.is_empty() {
        return Some(0);
    }
    if label.is_ascii() && needle.is_ascii() {
        // Compare without allocating. The needle is already lowercase, so only label bytes get lowercased.
        let (l, n) = (label.as_bytes(), needle.as_bytes());
        let at = |i: usize| l[i..i + n.len()].iter().zip(n).all(|(a, b)| a.to_ascii_lowercase() == *b);
        if l.len() < n.len() {
            return None;
        }
        return if at(0) { Some(0) } else { (1..=l.len() - n.len()).any(at).then_some(1) };
    }
    let lc = label.to_lowercase();
    if lc.starts_with(needle) {
        Some(0)
    } else if lc.contains(needle) {
        Some(1)
    } else {
        None
    }
}

// Tiers: 0 in-query columns/aliases, 1 all tables/columns, 2 user functions, 3 schemas, 4 keywords,
// 5 built-in functions, 6 other server functions.
pub fn rank(tier: u8, score: u8, label: &str) -> String {
    let mut out = String::with_capacity(label.len() + 3);
    out.push(char::from(b'0' + tier));
    out.push(char::from(b'0' + score));
    out.push('_');
    if label.is_ascii() {
        out.extend(label.bytes().map(|b| char::from(b.to_ascii_lowercase())));
    } else {
        out.push_str(&label.to_lowercase());
    }
    out
}

pub fn column_detail(c: &ColumnInfo, labels: &SqlLabels) -> String {
    if c.is_primary || c.is_foreign {
        let tags = match (c.is_primary, c.is_foreign) {
            (true, true) => format!("{} · {}", labels.pk, labels.fk),
            (true, false) => labels.pk.clone(),
            _ => labels.fk.clone(),
        };
        return format!("{} · {tags}", c.data_type);
    }
    if !c.is_nullable {
        return format!("{} · {}", c.data_type, labels.not_null);
    }
    c.data_type.clone()
}

// Unknown kinds from the catalog pass through as is.
pub fn relation_type_label(kind: &str, labels: &SqlLabels) -> String {
    if kind.is_empty() || kind == "table" {
        return labels.table.clone();
    }
    let raw = trim_spaces(kind);
    match raw.to_lowercase().as_str() {
        "table" | "base table" => labels.table.clone(),
        "view" => labels.view.clone(),
        _ => raw.to_string(),
    }
}

pub fn keyword_item(kw: &str, lc_prefix: &str) -> Option<CompletionItem> {
    let score = match_score(kw, lc_prefix)?;
    Some(CompletionItem {
        label: kw.to_string(),
        kind: ItemKind::Keyword,
        detail: None,
        insert_text: kw.to_string(),
        filter_text: None,
        label_detail: None,
        snippet: false,
        sort_text: rank(4, score, kw),
    })
}

fn push_column_items(
    items: &mut Vec<CompletionItem>,
    cols: &[ColumnInfo],
    tier: u8,
    lc_prefix: &str,
    ctx: &CompletionContext,
    mut seen: Option<&mut HashSet<String>>,
) {
    for c in cols {
        let key = c.name.to_lowercase();
        if seen.as_ref().is_some_and(|s| s.contains(&key)) {
            continue;
        }
        let Some(score) = match_score(&c.name, lc_prefix) else { continue };
        if let Some(seen) = seen.as_deref_mut() {
            seen.insert(key);
        }
        items.push(CompletionItem {
            label: c.name.clone(),
            kind: ItemKind::Field,
            detail: Some(column_detail(c, ctx.labels)),
            insert_text: format_sql_identifier(&c.name, ctx.driver()),
            filter_text: None,
            label_detail: None,
            snippet: false,
            sort_text: rank(tier, score, &c.name),
        });
    }
}

// Names only. CTE and derived-table columns have no known types.
pub fn suggest_virtual_columns(
    names: &[String],
    lc_prefix: &str,
    driver: &DriverType,
    detail: &str,
) -> Vec<CompletionItem> {
    let mut items = Vec::new();
    let mut seen = HashSet::new();
    for name in names {
        let key = name.to_lowercase();
        if seen.contains(&key) {
            continue;
        }
        let Some(score) = match_score(name, lc_prefix) else { continue };
        seen.insert(key);
        items.push(CompletionItem {
            label: name.clone(),
            kind: ItemKind::Field,
            detail: Some(detail.to_string()),
            insert_text: format_sql_identifier(name, driver),
            filter_text: None,
            label_detail: None,
            snippet: false,
            sort_text: rank(0, score, name),
        });
    }
    items
}

pub fn suggest_cte_items(ctes: &[String], lc_prefix: &str, ctx: &CompletionContext) -> Vec<CompletionItem> {
    let mut items = Vec::new();
    let mut seen = HashSet::new();
    for name in ctes {
        let key = name.to_lowercase();
        if seen.contains(&key) {
            continue;
        }
        let Some(score) = match_score(name, lc_prefix) else { continue };
        seen.insert(key);
        let insert = format_sql_identifier(name, ctx.driver());
        items.push(CompletionItem {
            label: name.clone(),
            kind: ItemKind::Class,
            detail: Some(ctx.labels.cte.clone()),
            insert_text: insert.clone(),
            filter_text: Some(insert),
            label_detail: None,
            snippet: false,
            // Query-local, so it ranks above the table list and survives the 100-item cap.
            sort_text: rank(0, score, name),
        });
    }
    items
}

pub fn suggest_tables(ctx: &CompletionContext, lc_prefix: &str, schema_filter: Option<&str>) -> Vec<CompletionItem> {
    let source: Vec<&TableInfo> = match schema_filter {
        Some(schema) => ctx.catalog.tables_in_schema(schema),
        None => ctx.catalog.tables.iter().collect(),
    };
    let mut items = Vec::new();
    for t in source {
        let Some(score) = match_score(&t.name, lc_prefix) else { continue };
        let insert = format_sql_identifier(&t.name, ctx.driver());
        let kind = if schema_filter.is_some() { "table" } else { t.kind.as_str() };
        items.push(CompletionItem {
            label: t.name.clone(),
            kind: ItemKind::Class,
            detail: Some(relation_type_label(kind, ctx.labels)),
            insert_text: insert.clone(),
            filter_text: Some(insert),
            label_detail: None,
            snippet: false,
            sort_text: rank(1, score, &t.name),
        });
    }
    items
}

pub fn suggest_schemas(ctx: &CompletionContext, lc_prefix: &str) -> Vec<CompletionItem> {
    let mut items = Vec::new();
    for s in &ctx.catalog.schemas {
        let Some(score) = match_score(&s.name, lc_prefix) else { continue };
        items.push(CompletionItem {
            label: s.name.clone(),
            kind: ItemKind::Module,
            detail: Some(ctx.labels.schema.clone()),
            insert_text: format_sql_identifier(&s.name, ctx.driver()),
            filter_text: None,
            label_detail: None,
            snippet: false,
            sort_text: rank(3, score, &s.name),
        });
    }
    items
}

pub fn suggest_query_table_refs(
    ctx: &CompletionContext,
    query_tables: &[QueryTableRef],
    lc_prefix: &str,
) -> Vec<CompletionItem> {
    let mut items = Vec::new();
    let mut seen_tables = HashSet::new();
    let mut seen_aliases = HashSet::new();
    for r in query_tables {
        let table_key = r.table.to_lowercase();
        if !seen_tables.contains(&table_key)
            && let Some(score) = match_score(&r.table, lc_prefix)
        {
            seen_tables.insert(table_key);
            let insert = format_sql_identifier(&r.table, ctx.driver());
            let detail = if r.schema.is_empty() {
                ctx.labels.table.clone()
            } else {
                fill(&ctx.labels.schema_table, &[("schema", &r.schema), ("type", &ctx.labels.table)])
            };
            items.push(CompletionItem {
                label: r.table.clone(),
                kind: ItemKind::Class,
                detail: Some(detail),
                insert_text: insert.clone(),
                filter_text: Some(insert),
                label_detail: None,
                snippet: false,
                sort_text: rank(0, score, &r.table),
            });
        }
        if let Some(alias) = &r.alias {
            let alias_key = alias.to_lowercase();
            if !seen_aliases.contains(&alias_key)
                && let Some(score) = match_score(alias, lc_prefix)
            {
                seen_aliases.insert(alias_key);
                let insert = format_sql_identifier(alias, ctx.driver());
                items.push(CompletionItem {
                    label: alias.clone(),
                    kind: ItemKind::Class,
                    detail: Some(fill(&ctx.labels.alias_arrow, &[("table", &r.table)])),
                    insert_text: insert.clone(),
                    filter_text: Some(insert),
                    label_detail: None,
                    snippet: false,
                    sort_text: rank(0, score, alias),
                });
            }
        }
    }
    items
}

// Column names shared by several sources are offered as `alias.col`.
pub fn suggest_columns_in_scope(
    ctx: &CompletionContext,
    query_tables: &[QueryTableRef],
    lc_prefix: &str,
    mut seen: Option<&mut HashSet<String>>,
) -> Vec<CompletionItem> {
    let mut sources: Vec<(&QueryTableRef, &[ColumnInfo])> = Vec::new();
    let mut seen_refs = HashSet::new();
    for r in query_tables {
        let key =
            format!("{}|{}", column_cache_key(&r.schema, &r.table), r.alias.as_deref().unwrap_or("").to_lowercase());
        if seen_refs.insert(key) {
            sources.push((r, ctx.columns_of(&r.schema, &r.table)));
        }
    }

    // Counted per source, so a self-join counts once per alias. A single source can't be ambiguous.
    let name_count: Option<HashMap<String, usize>> = (sources.len() > 1).then(|| {
        let mut counts = HashMap::new();
        for (_, cols) in &sources {
            let unique: HashSet<String> = cols.iter().map(|c| c.name.to_lowercase()).collect();
            for key in unique {
                *counts.entry(key).or_insert(0) += 1;
            }
        }
        counts
    });

    let mut items = Vec::new();
    for (r, cols) in &sources {
        let mut emitted = HashSet::new();
        for c in *cols {
            let key = c.name.to_lowercase();
            if !emitted.insert(key.clone()) {
                continue;
            }
            let Some(score) = match_score(&c.name, lc_prefix) else { continue };
            if name_count.as_ref().is_some_and(|n| n.get(&key).copied().unwrap_or(0) > 1) {
                let qualifier = r.alias.as_deref().unwrap_or(&r.table);
                let label = format!("{qualifier}.{}", c.name);
                if let Some(seen) = seen.as_deref_mut() {
                    seen.insert(key);
                }
                items.push(CompletionItem {
                    sort_text: rank(0, score, &label),
                    label,
                    kind: ItemKind::Field,
                    detail: Some(column_detail(c, ctx.labels)),
                    insert_text: format!(
                        "{}.{}",
                        format_sql_identifier(qualifier, ctx.driver()),
                        format_sql_identifier(&c.name, ctx.driver())
                    ),
                    filter_text: None,
                    label_detail: None,
                    snippet: false,
                });
            } else {
                if seen.as_ref().is_some_and(|s| s.contains(&key)) {
                    continue;
                }
                if let Some(seen) = seen.as_deref_mut() {
                    seen.insert(key);
                }
                items.push(CompletionItem {
                    label: c.name.clone(),
                    kind: ItemKind::Field,
                    detail: Some(column_detail(c, ctx.labels)),
                    insert_text: format_sql_identifier(&c.name, ctx.driver()),
                    filter_text: None,
                    label_detail: None,
                    snippet: false,
                    sort_text: rank(0, score, &c.name),
                });
            }
        }
    }
    items
}

const VALUE_LITERALS: [&str; 4] = ["NULL", "TRUE", "FALSE", "DEFAULT"];

pub fn suggest_value_items(
    ctx: &CompletionContext,
    query_tables: &[QueryTableRef],
    lc_prefix: &str,
) -> Vec<CompletionItem> {
    // The value is often another qualified column, so in-scope tables and aliases come first and bare
    // columns after.
    let mut items = suggest_query_table_refs(ctx, query_tables, lc_prefix);
    for lit in VALUE_LITERALS {
        // SQLite has no DEFAULT expression.
        if lit == "DEFAULT" && ctx.driver().dialect() == Some(SqlDialect::Sqlite) {
            continue;
        }
        items.extend(keyword_item(lit, lc_prefix));
    }
    let mut seen = HashSet::new();
    items.extend(suggest_columns_in_scope(ctx, query_tables, lc_prefix, Some(&mut seen)));
    push_column_items(&mut items, ctx.columns, 1, lc_prefix, ctx, Some(&mut seen));
    items
}

pub fn suggest_columns_for_table(
    ctx: &CompletionContext,
    binding: &TableBinding,
    lc_prefix: &str,
) -> Vec<CompletionItem> {
    let key = column_cache_key(&binding.schema, &binding.table);
    let cols = ctx.columns_by_table.get(&key).map_or(ctx.columns, Vec::as_slice);
    let mut items = Vec::new();
    push_column_items(&mut items, cols, 0, lc_prefix, ctx, None);
    items
}

// For a caret touching a clause keyword. Without the space `email` would insert as `WHEREemail`.
pub fn with_leading_space(items: Vec<CompletionItem>) -> Vec<CompletionItem> {
    items.into_iter().map(|item| CompletionItem { insert_text: format!(" {}", item.insert_text), ..item }).collect()
}
