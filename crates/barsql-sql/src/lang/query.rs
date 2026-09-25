use std::collections::HashSet;
use std::sync::Arc;

use super::catalog::{Catalog, OrderedMap, TableBinding};
use super::quoting::{is_alias_stop_word, unquote_ident};
use super::tokens::{Token, TokenKind, is_keyword, is_punct, next_code_token, prev_code_token, tokenize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryTableRef {
    pub schema: String,
    pub table: String,
    pub alias: Option<String>,
    // Offset of the FROM, JOIN, UPDATE or INSERT keyword.
    pub index: usize,
    // Span of the table name, for diagnostics and hover.
    pub name_start: usize,
    pub name_end: usize,
    // False when the name didn't resolve against the schema, so it may be a typo.
    pub known: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedQuery {
    pub query_tables: Vec<QueryTableRef>,
    // Keyed by lowercased table name and alias.
    pub bindings: OrderedMap<TableBinding>,
    // CTE names from a leading WITH, offered like tables in FROM/JOIN.
    pub ctes: Vec<String>,
    // Columns of CTEs and derived tables by lowercased name. Empty when opaque, e.g. SELECT *.
    pub virtual_columns: OrderedMap<Vec<String>>,
}

impl ParsedQuery {
    pub fn is_cte(&self, lower: &str) -> bool {
        self.ctes.iter().any(|c| c.to_lowercase() == lower)
    }
}

struct RawRef {
    name1: usize,
    name2: Option<usize>,
    alias: Option<usize>,
    // Last token the ref consumed.
    last: usize,
}

// `name[.name] [AS] [alias]`. A bare alias can't be a reserved word, so `FROM t JOIN` has no alias.
fn read_table_ref(tokens: &[Token], at: usize, allow_alias: bool) -> Option<RawRef> {
    if !tokens[at].is_ident_like() {
        return None;
    }
    let mut last = at;
    let mut name2 = None;
    let mut j = next_code_token(tokens, Some(last));
    if let Some(dot) = j.filter(|&j| tokens[j].is_punct(".")) {
        let Some(k) = next_code_token(tokens, Some(dot)).filter(|&k| tokens[k].is_ident_like()) else {
            // A dangling `schema.` while typing binds the first part only.
            return Some(RawRef { name1: at, name2: None, alias: None, last });
        };
        name2 = Some(k);
        last = k;
        j = next_code_token(tokens, Some(last));
    }
    let mut alias = None;
    if allow_alias && let Some(j) = j {
        let t = &tokens[j];
        if t.is_keyword("as") {
            if let Some(k) = next_code_token(tokens, Some(j)).filter(|&k| tokens[k].is_ident_like()) {
                alias = Some(k);
                last = k;
            }
        } else if t.is_ident_like() && (t.kind == TokenKind::Quoted || !is_alias_stop_word(&t.lower)) {
            alias = Some(j);
            last = j;
        }
    }
    Some(RawRef { name1: at, name2, alias, last })
}

fn match_paren(tokens: &[Token], open: usize) -> Option<usize> {
    let mut depth = 0;
    for (i, t) in tokens.iter().enumerate().skip(open) {
        if t.kind != TokenKind::Punct {
            continue;
        }
        if t.text == "(" {
            depth += 1;
        } else if t.text == ")" {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
    }
    None
}

// Unaliased expressions and `*` add no column name.
fn extract_projection(tokens: &[Token], start: usize, end: usize) -> Vec<String> {
    let mut cols = Vec::new();
    let mut i = start;
    let mut depth = 0i32;
    while i < end {
        let t = &tokens[i];
        if t.kind == TokenKind::Punct {
            if t.text == "(" {
                depth += 1;
            } else if t.text == ")" {
                depth -= 1;
            }
        } else if depth == 0 && t.is_keyword("select") {
            break;
        }
        i += 1;
    }
    if i >= end {
        return cols;
    }
    let mut at = next_code_token(tokens, Some(i));
    if at.is_some_and(|k| tokens[k].is_keyword("distinct") || tokens[k].is_keyword("all")) {
        at = next_code_token(tokens, at);
    }
    let Some(mut i) = at else { return cols };

    let mut item: Vec<&Token> = Vec::new();
    let flush = |item: &mut Vec<&Token>, cols: &mut Vec<String>| {
        if let Some(last) = item.last().filter(|t| t.is_ident_like()) {
            let prev = item.len().checked_sub(2).map(|p| item[p]);
            let aliased = prev.is_some_and(|p| {
                matches!(p.kind, TokenKind::Ident | TokenKind::Quoted | TokenKind::Number) || p.is_punct(")")
            });
            if (item.len() == 1 || is_punct(prev, ".") || aliased) && !last.is_keyword("as") {
                cols.push(last.ident_text());
            }
        }
        item.clear();
    };

    let mut inner = 0;
    while i < end {
        let t = &tokens[i];
        i += 1;
        if t.kind == TokenKind::Comment {
            continue;
        }
        if t.kind == TokenKind::Punct {
            if t.text == "(" {
                inner += 1;
            } else if t.text == ")" {
                if inner == 0 {
                    break;
                }
                inner -= 1;
            } else if t.text == "," && inner == 0 {
                flush(&mut item, &mut cols);
                continue;
            }
        }
        if inner == 0 && t.is_keyword("from") {
            break;
        }
        item.push(t);
    }
    flush(&mut item, &mut cols);
    cols
}

// Reads a leading WITH only. Bodies are skipped by matching parens, and the main pass finds their refs.
fn collect_ctes(tokens: &[Token], virtual_columns: &mut OrderedMap<Vec<String>>) -> Vec<String> {
    let mut ctes = Vec::new();
    let mut i = next_code_token(tokens, None);
    if !i.is_some_and(|i| tokens[i].is_keyword("with")) {
        return ctes;
    }
    i = next_code_token(tokens, i);
    if i.is_some_and(|i| tokens[i].is_keyword("recursive")) {
        i = next_code_token(tokens, i);
    }
    while let Some(name_ix) = i.filter(|&i| tokens[i].is_ident_like()) {
        let name = tokens[name_ix].ident_text();
        let mut explicit: Option<Vec<String>> = None;
        let mut j = next_code_token(tokens, Some(name_ix));
        if let Some(open) = j.filter(|&j| tokens[j].is_punct("(")) {
            let Some(close) = match_paren(tokens, open) else { break };
            explicit =
                Some(tokens[open + 1..close].iter().filter(|t| t.is_ident_like()).map(Token::ident_text).collect());
            j = next_code_token(tokens, Some(close));
        }
        if !j.is_some_and(|j| tokens[j].is_keyword("as")) {
            break;
        }
        j = next_code_token(tokens, j);
        if j.is_some_and(|j| tokens[j].is_keyword("not")) {
            j = next_code_token(tokens, j);
        }
        if j.is_some_and(|j| tokens[j].is_keyword("materialized")) {
            j = next_code_token(tokens, j);
        }
        let Some(open) = j.filter(|&j| tokens[j].is_punct("(")) else { break };
        // Past `AS (`. The body may still be unterminated while typing.
        ctes.push(name.clone());
        let close = match_paren(tokens, open);
        let body_end = close.unwrap_or(tokens.len());
        let cols = explicit.unwrap_or_else(|| extract_projection(tokens, open + 1, body_end));
        virtual_columns.insert(name.to_lowercase(), cols);
        let Some(close) = close else { break };
        let Some(comma) = next_code_token(tokens, Some(close)).filter(|&c| tokens[c].is_punct(",")) else { break };
        i = next_code_token(tokens, Some(comma));
    }
    ctes
}

pub fn parse_query(sql: &str, catalog: &Catalog) -> Arc<ParsedQuery> {
    catalog.cached_parse(sql, || parse_uncached(sql, catalog))
}

fn parse_uncached(sql: &str, catalog: &Catalog) -> ParsedQuery {
    let tokens = tokenize(sql, Some(&catalog.driver));
    let mut query_tables = Vec::new();
    let mut virtual_columns = OrderedMap::default();

    let push_ref = |query_tables: &mut Vec<QueryTableRef>, kw_offset: usize, raw: &RawRef| {
        let name_tok = &tokens[raw.name2.unwrap_or(raw.name1)];
        let name = name_tok.ident_text();
        let schema_hint = raw.name2.map(|_| tokens[raw.name1].ident_text());
        let binding = catalog.resolve_table_name(&name, schema_hint.as_deref());
        let alias = raw
            .alias
            .map(|a| tokens[a].ident_text())
            .filter(|a| !a.is_empty() && !is_alias_stop_word(&a.to_lowercase()));
        // A lone name matching a schema is a qualifier still being typed, like `FROM public.`, not a typo.
        let known = catalog.lookup_table(&name, schema_hint.as_deref()).is_some()
            || (raw.name2.is_none() && catalog.has_schema_of_tables(&name.to_lowercase()));
        query_tables.push(QueryTableRef {
            schema: binding.schema,
            table: binding.table,
            alias,
            index: kw_offset,
            name_start: name_tok.start,
            name_end: name_tok.end,
            known,
        });
    };

    for i in 0..tokens.len() {
        let t = &tokens[i];
        if t.kind != TokenKind::Ident {
            continue;
        }
        match t.lower.as_str() {
            "from" | "join" => {
                // The FROM of `IS [NOT] DISTINCT FROM` is a comparison operator, not a table source.
                if t.lower == "from" && is_keyword(prev_code_token(&tokens, i).map(|p| &tokens[p]), "distinct") {
                    continue;
                }
                let join = t.lower == "join";
                // FROM takes a comma list, JOIN a single ref.
                let mut at = next_code_token(&tokens, Some(i));
                loop {
                    if let Some(open) = at.filter(|&a| tokens[a].is_punct("(")) {
                        // Derived table `(SELECT ...) alias`. The main loop still walks the body for its
                        // inner refs.
                        let Some(close) = match_paren(&tokens, open) else { break };
                        let mut alias_ix = next_code_token(&tokens, Some(close));
                        if alias_ix.is_some_and(|a| tokens[a].is_keyword("as")) {
                            alias_ix = next_code_token(&tokens, alias_ix);
                        }
                        let Some(alias_ix) = alias_ix.filter(|&a| {
                            let a = &tokens[a];
                            a.kind == TokenKind::Quoted || (a.kind == TokenKind::Ident && !is_alias_stop_word(&a.lower))
                        }) else {
                            break;
                        };
                        virtual_columns.insert(
                            tokens[alias_ix].ident_text().to_lowercase(),
                            extract_projection(&tokens, open + 1, close),
                        );
                        if join {
                            break;
                        }
                        let Some(comma) = next_code_token(&tokens, Some(alias_ix)).filter(|&c| tokens[c].is_punct(","))
                        else {
                            break;
                        };
                        at = next_code_token(&tokens, Some(comma));
                        continue;
                    }
                    let Some(raw) = at.and_then(|a| read_table_ref(&tokens, a, true)) else { break };
                    push_ref(&mut query_tables, t.start, &raw);
                    if join {
                        break;
                    }
                    let Some(comma) = next_code_token(&tokens, Some(raw.last)).filter(|&c| tokens[c].is_punct(","))
                    else {
                        break;
                    };
                    at = next_code_token(&tokens, Some(comma));
                }
            }
            "update" => {
                if let Some(raw) = next_code_token(&tokens, Some(i)).and_then(|a| read_table_ref(&tokens, a, true)) {
                    push_ref(&mut query_tables, t.start, &raw);
                }
            }
            "into" => {
                if let Some(p) = prev_code_token(&tokens, i)
                    .filter(|&p| tokens[p].is_keyword("insert") || tokens[p].is_keyword("replace"))
                    && let Some(raw) = next_code_token(&tokens, Some(i)).and_then(|a| read_table_ref(&tokens, a, false))
                {
                    push_ref(&mut query_tables, tokens[p].start, &raw);
                }
            }
            _ => {}
        }
    }

    let mut deduped: Vec<QueryTableRef> = Vec::new();
    let mut seen = HashSet::new();
    for r in query_tables {
        let key = format!("{}|{}|{}", r.schema, r.table, r.alias.as_deref().unwrap_or("")).to_lowercase();
        if seen.insert(key) {
            deduped.push(r);
        }
    }

    let mut bindings = OrderedMap::default();
    for r in &deduped {
        let binding = TableBinding { schema: r.schema.clone(), table: r.table.clone() };
        bindings.insert(r.table.to_lowercase(), binding.clone());
        if let Some(alias) = &r.alias {
            bindings.insert(alias.to_lowercase(), binding);
        }
    }

    let ctes = collect_ctes(&tokens, &mut virtual_columns);
    ParsedQuery { query_tables: deduped, bindings, ctes, virtual_columns }
}

pub fn resolve_qualifier_to_table(qualifier: &str, parsed: &ParsedQuery, catalog: &Catalog) -> Option<TableBinding> {
    let key = qualifier.to_lowercase();
    // A schema qualifier like `analytics.` should list that schema's tables, not resolve to a same-named
    // alias or table.
    if catalog.has_schema_of_tables(&key) {
        return None;
    }
    if let Some(bound) = parsed.bindings.get(&key) {
        return Some(bound.clone());
    }
    Some(catalog.resolve_table_name(qualifier, None))
}

// `segments` are raw tokens before the dot, either [qualifier] or [schema, table].
pub fn resolve_dot_completion(segments: &[String], parsed: &ParsedQuery, catalog: &Catalog) -> Option<TableBinding> {
    if segments.len() == 2 {
        let (schema, table) = (unquote_ident(&segments[0]), unquote_ident(&segments[1]));
        return Some(catalog.resolve_table_name(&table, Some(&schema)));
    }
    resolve_qualifier_to_table(&unquote_ident(segments.first()?), parsed, catalog)
}
