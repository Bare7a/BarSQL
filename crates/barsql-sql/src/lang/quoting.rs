use barsql_core::{DriverType, SqlDialect};

use crate::quote::quote_ident;

pub const ALIAS_STOP_WORDS: [&str; 26] = [
    "on", "and", "or", "where", "join", "inner", "left", "right", "full", "cross", "natural", "as", "set", "order",
    "group", "by", "having", "limit", "offset", "union", "into", "values", "select", "from", "using", "lateral",
];

// Keywords that are plausible identifier names too. Completion must quote them, since an unquoted
// column named `order` is a syntax error.
const QUOTE_FORCING_EXTRA: [&str; 52] = [
    "table",
    "column",
    "user",
    "index",
    "view",
    "key",
    "primary",
    "foreign",
    "references",
    "constraint",
    "default",
    "check",
    "unique",
    "create",
    "drop",
    "alter",
    "insert",
    "update",
    "delete",
    "truncate",
    "grant",
    "revoke",
    "case",
    "when",
    "then",
    "else",
    "end",
    "null",
    "true",
    "false",
    "not",
    "is",
    "in",
    "like",
    "ilike",
    "similar",
    "regexp",
    "rlike",
    "glob",
    "match",
    "between",
    "exists",
    "distinct",
    "all",
    "any",
    "asc",
    "desc",
    "with",
    "recursive",
    "returning",
    "cast",
    "collate",
];

pub fn is_alias_stop_word(lower: &str) -> bool {
    ALIAS_STOP_WORDS.contains(&lower)
}

pub fn is_quote_forcing_keyword(lower: &str) -> bool {
    is_alias_stop_word(lower) || QUOTE_FORCING_EXTRA.contains(&lower)
}

pub fn column_cache_key(schema: &str, table: &str) -> String {
    format!("{schema}.{table}")
}

pub fn unquote_ident(raw: &str) -> String {
    let s = super::statements::trim_spaces(raw);
    let quoted = |q: char| s.len() >= 2 && s.starts_with(q) && s.ends_with(q);
    if quoted('"') {
        return s[1..s.len() - 1].replace("\"\"", "\"");
    }
    if quoted('\'') {
        return s[1..s.len() - 1].replace("''", "'");
    }
    if quoted('`') {
        return s[1..s.len() - 1].to_string();
    }
    s.to_string()
}

pub fn identifier_needs_quote(name: &str, driver: &DriverType) -> bool {
    if name.is_empty() {
        return false;
    }
    // A non-ASCII name needs quotes anyway, so only ASCII names are checked against the keywords.
    let keyword = || ALIAS_STOP_WORDS.iter().chain(&QUOTE_FORCING_EXTRA).any(|k| k.eq_ignore_ascii_case(name));
    if name.as_bytes()[0].is_ascii_digit() || (name.len() <= 10 && name.is_ascii() && keyword()) {
        return true;
    }
    match driver.dialect() {
        // Postgres folds unquoted names to lowercase.
        Some(SqlDialect::Postgres) => !name.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_'),
        Some(SqlDialect::MySql) => !name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'$'),
        Some(SqlDialect::Sqlite | SqlDialect::TSql | SqlDialect::ClickHouse) | None => {
            !name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
        }
    }
}

pub fn format_sql_identifier(name: &str, driver: &DriverType) -> String {
    if identifier_needs_quote(name, driver) { quote_ident(driver, name) } else { name.to_string() }
}

// Leaves out an empty schema and SQLite's default `main`.
pub fn build_qualified_table(driver: &DriverType, schema: &str, table: &str) -> String {
    if schema.is_empty() || (schema == "main" && driver.dialect() == Some(SqlDialect::Sqlite)) {
        return quote_ident(driver, table);
    }
    format!("{}.{}", quote_ident(driver, schema), quote_ident(driver, table))
}
