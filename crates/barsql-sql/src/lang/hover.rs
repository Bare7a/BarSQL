use barsql_core::TableInfo;

use super::catalog::{Catalog, TableBinding};
use super::functions::{CallForm, FunctionDoc};
use super::query::{ParsedQuery, resolve_dot_completion};
use super::quoting::{is_quote_forcing_keyword, unquote_ident};
use super::tokens::{Token, TokenKind, tokenize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnLookup {
    pub bindings: Vec<TableBinding>,
    // Lowercased column name.
    pub name: String,
}

// What the hovered name refers to. Tables and columns need the caller's column cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HoverSubject {
    // A catalog table or view, named directly or through `alias`. The caller loads its columns.
    Table { table: TableInfo, alias: Option<String> },
    // An alias whose table isn't in the catalog.
    Alias { name: String, table: String },
    // A CTE or subquery, with the columns it outputs. Empty when opaque, e.g. SELECT *.
    Derived { name: String, cte: bool, columns: Vec<String> },
    // A column of a CTE or subquery.
    DerivedColumn { name: String, source: String, cte: bool },
    Schema { name: String },
    // A table column. The caller searches these tables' columns in order and takes the first match.
    Column(ColumnLookup),
    Function(Box<FunctionDoc>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HoverQuery {
    // Statement-relative span of the hovered token.
    pub start: usize,
    pub end: usize,
    pub subject: HoverSubject,
}

// Words a `(` follows without making them calls.
const NOT_CALLS: [&str; 9] = ["values", "in", "exists", "over", "using", "as", "filter", "within", "on"];
// Before a name that a `(` follows, these make it something being defined or a table's column list.
const DEFINES: [&str; 9] =
    ["into", "table", "references", "view", "index", "function", "procedure", "trigger", "exists"];

// A call like `lower(` or `pg_catalog.now(`, or a word like CURRENT_DATE that is a call without parentheses.
// Some(None) is a call to something the catalog doesn't know, which nothing else describes either.
fn function_hover(tokens: &[&Token], idx: usize, catalog: &Catalog) -> Option<Option<HoverQuery>> {
    let tok = tokens[idx];
    let qualified = idx >= 2 && tokens[idx - 1].is_punct(".") && tokens[idx - 2].is_ident_like();
    let before = if qualified { idx.checked_sub(3) } else { idx.checked_sub(1) }.map(|i| tokens[i]);
    if before.is_some_and(|t| t.kind == TokenKind::Ident && DEFINES.contains(&t.lower.as_str())) {
        return None;
    }
    let found = |doc: FunctionDoc| {
        Some(Some(HoverQuery { start: tok.start, end: tok.end, subject: HoverSubject::Function(Box::new(doc)) }))
    };
    if tokens.get(idx + 1).is_some_and(|t| t.is_punct("(")) {
        if tok.kind == TokenKind::Ident && NOT_CALLS.contains(&tok.lower.as_str()) {
            return None;
        }
        let schema = qualified.then(|| tokens[idx - 2].ident_text());
        return match catalog.functions.lookup(schema.as_deref(), &tok.ident_text()) {
            Some(doc) => found(doc),
            None => Some(None),
        };
    }
    if qualified || tok.kind != TokenKind::Ident {
        return None;
    }
    catalog.functions.lookup(None, tok.text).filter(|doc| doc.call == CallForm::NoParens).and_then(found)
}

pub fn analyze_hover(stmt_text: &str, offset: usize, parsed: &ParsedQuery, catalog: &Catalog) -> Option<HoverQuery> {
    let all = tokenize(stmt_text, Some(&catalog.driver));
    let tokens: Vec<&Token> = all.iter().filter(|t| t.kind != TokenKind::Comment).collect();
    let idx = tokens.iter().position(|t| offset >= t.start && offset < t.end && t.is_ident_like())?;
    let tok = tokens[idx];
    if let Some(function) = function_hover(&tokens, idx, catalog) {
        return function;
    }
    // A bare keyword isn't an identifier. A quoted token always is.
    if tok.kind == TokenKind::Ident && is_quote_forcing_keyword(&tok.lower) {
        return None;
    }
    let name = tok.ident_text();
    let name_lc = name.to_lowercase();
    let found = |subject| Some(HoverQuery { start: tok.start, end: tok.end, subject });

    // `qual.` or `schema.qual.` before the hovered token.
    let mut segments: Vec<String> = Vec::new();
    let mut k = idx;
    while segments.len() < 2 && k >= 2 && tokens[k - 1].is_punct(".") && tokens[k - 2].is_ident_like() {
        segments.insert(0, tokens[k - 2].text.to_string());
        k -= 2;
    }
    if !segments.is_empty() {
        let qual = unquote_ident(segments.last().map_or("", String::as_str));
        let qual_lc = qual.to_lowercase();
        if let Some(cols) = parsed.virtual_columns.get(&qual_lc) {
            if !cols.iter().any(|c| c.to_lowercase() == name_lc) {
                return None;
            }
            return found(HoverSubject::DerivedColumn { name, source: qual, cte: parsed.is_cte(&qual_lc) });
        }
        let binding = resolve_dot_completion(&segments, parsed, catalog)?;
        return found(HoverSubject::Column(ColumnLookup { bindings: vec![binding], name: name_lc }));
    }

    if let Some(bound) = parsed.bindings.get(&name_lc) {
        let info = catalog.tables.iter().find(|t| t.name == bound.table && t.schema == bound.schema);
        let alias = (bound.table.to_lowercase() != name_lc).then(|| name.clone());
        match (info, alias) {
            (Some(info), alias) => return found(HoverSubject::Table { table: info.clone(), alias }),
            (None, Some(alias)) => return found(HoverSubject::Alias { name: alias, table: bound.table.clone() }),
            (None, None) => {}
        }
    }

    if let Some(cols) = parsed.virtual_columns.get(&name_lc) {
        return found(HoverSubject::Derived { cte: parsed.is_cte(&name_lc), name, columns: cols.clone() });
    }

    if let Some(table) = catalog.tables.iter().find(|t| t.name.to_lowercase() == name_lc) {
        return found(HoverSubject::Table { table: table.clone(), alias: None });
    }

    if let Some(schema) = catalog.schemas.iter().find(|s| s.name.to_lowercase() == name_lc) {
        return found(HoverSubject::Schema { name: schema.name.clone() });
    }

    // Bare column. The caller lazily loads the in-scope tables' columns and searches them.
    let mut candidates: Vec<TableBinding> = Vec::new();
    for b in parsed.bindings.values() {
        if !candidates.iter().any(|c| c.schema == b.schema && c.table == b.table) {
            candidates.push(b.clone());
        }
    }
    if candidates.is_empty() {
        return None;
    }
    found(HoverSubject::Column(ColumnLookup { bindings: candidates, name: name_lc }))
}
