use barsql_core::{ColumnInfo, TableInfo};

use super::catalog::{Catalog, TableBinding};
use super::labels::{SqlLabels, fill};
use super::query::{ParsedQuery, resolve_dot_completion};
use super::quoting::{is_quote_forcing_keyword, unquote_ident};
use super::suggestions::{column_detail, relation_type_label};
use super::tokens::{Token, TokenKind, tokenize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnLookup {
    pub bindings: Vec<TableBinding>,
    // Lowercased column name.
    pub name: String,
}

// Either ready `lines` or a `column_lookup` the caller resolves from its column cache.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HoverQuery {
    // Statement-relative span of the hovered token.
    pub start: usize,
    pub end: usize,
    pub lines: Option<Vec<String>>,
    pub column_lookup: Option<ColumnLookup>,
    // Columns of this table go after `lines`. Loaded lazily.
    pub table_columns: Option<TableBinding>,
}

pub fn column_hover_lines(col: &ColumnInfo, table: &TableBinding, labels: &SqlLabels) -> Vec<String> {
    let target =
        if table.schema.is_empty() { table.table.clone() } else { format!("{}.{}", table.schema, table.table) };
    vec![format!("**{}** · {}", col.name, column_detail(col, labels)), fill(&labels.column_of, &[("target", &target)])]
}

// Markdown table of at most `max` columns plus a "... N more columns" line.
pub fn table_columns_markdown(cols: &[ColumnInfo], max: usize, labels: &SqlLabels) -> Vec<String> {
    if cols.is_empty() {
        return Vec::new();
    }
    let shown = &cols[..cols.len().min(max)];
    let mut table = vec![format!("| {} | {} |", labels.column, labels.type_name), "| --- | --- |".to_string()];
    table.extend(shown.iter().map(|c| format!("| {} | {} |", c.name, column_detail(c, labels))));
    let mut out = vec![table.join("\n")];
    if cols.len() > shown.len() {
        out.push(fill(&labels.more_columns, &[("count", &(cols.len() - shown.len()).to_string())]));
    }
    out
}

fn table_lines(info: &TableInfo, labels: &SqlLabels) -> Vec<String> {
    let name = format!("**{}**", info.name);
    let mut lines =
        vec![fill(&labels.name_kind, &[("name", &name), ("kind", &relation_type_label(&info.kind, labels))])];
    if !info.schema.is_empty() {
        lines.push(fill(&labels.schema_name, &[("name", &info.schema)]));
    }
    lines
}

fn virtual_kind_label(is_cte: bool, labels: &SqlLabels) -> &str {
    if is_cte { &labels.cte } else { &labels.subquery }
}

pub fn analyze_hover(
    stmt_text: &str,
    offset: usize,
    parsed: &ParsedQuery,
    catalog: &Catalog,
    labels: &SqlLabels,
) -> Option<HoverQuery> {
    let all = tokenize(stmt_text, Some(&catalog.driver));
    let tokens: Vec<&Token> = all.iter().filter(|t| t.kind != TokenKind::Comment).collect();
    let idx = tokens.iter().position(|t| offset >= t.start && offset < t.end && t.is_ident_like())?;
    let tok = tokens[idx];
    // A bare keyword isn't an identifier. A quoted token always is.
    if tok.kind == TokenKind::Ident && is_quote_forcing_keyword(&tok.lower) {
        return None;
    }
    let name = tok.ident_text();
    let name_lc = name.to_lowercase();
    let span = HoverQuery { start: tok.start, end: tok.end, ..Default::default() };
    let with_lines = |lines: Vec<String>, table_columns: Option<TableBinding>| {
        Some(HoverQuery { lines: Some(lines), table_columns, ..span.clone() })
    };

    // `qual.` or `schema.qual.` before the hovered token.
    let mut segments: Vec<String> = Vec::new();
    let mut k = idx;
    while segments.len() < 2 && k >= 2 && tokens[k - 1].is_punct(".") && tokens[k - 2].is_ident_like() {
        segments.insert(0, tokens[k - 2].text.to_string());
        k -= 2;
    }
    if !segments.is_empty() {
        let qual_lc = unquote_ident(segments.last().map_or("", String::as_str)).to_lowercase();
        if let Some(cols) = parsed.virtual_columns.get(&qual_lc) {
            if !cols.iter().any(|c| c.to_lowercase() == name_lc) {
                return None;
            }
            let target = format!("{} {qual_lc}", virtual_kind_label(parsed.is_cte(&qual_lc), labels));
            return with_lines(vec![format!("**{name}**"), fill(&labels.column_of, &[("target", &target)])], None);
        }
        let binding = resolve_dot_completion(&segments, parsed, catalog)?;
        return Some(HoverQuery {
            column_lookup: Some(ColumnLookup { bindings: vec![binding], name: name_lc }),
            ..span
        });
    }

    if let Some(bound) = parsed.bindings.get(&name_lc) {
        let info = catalog.tables.iter().find(|t| t.name == bound.table && t.schema == bound.schema);
        if bound.table.to_lowercase() != name_lc {
            let kind = fill(&labels.alias_for, &[("table", &bound.table)]);
            let mut lines = vec![fill(&labels.name_kind, &[("name", &format!("**{name}**")), ("kind", &kind)])];
            if let Some(info) = info {
                lines.extend(table_lines(info, labels).into_iter().skip(1));
            }
            return with_lines(lines, info.map(|_| bound.clone()));
        }
        if let Some(info) = info {
            return with_lines(table_lines(info, labels), Some(bound.clone()));
        }
    }

    if let Some(cols) = parsed.virtual_columns.get(&name_lc) {
        let kind = virtual_kind_label(parsed.is_cte(&name_lc), labels);
        let mut lines = vec![fill(&labels.name_kind, &[("name", &format!("**{name}**")), ("kind", kind)])];
        if !cols.is_empty() {
            let mut shown = cols.iter().take(8).cloned().collect::<Vec<_>>().join(", ");
            if cols.len() > 8 {
                shown.push_str(", …");
            }
            lines.push(fill(&labels.columns_list, &[("cols", &shown)]));
        }
        return with_lines(lines, None);
    }

    if let Some(table) = catalog.tables.iter().find(|t| t.name.to_lowercase() == name_lc) {
        let binding = TableBinding { schema: table.schema.clone(), table: table.name.clone() };
        return with_lines(table_lines(table, labels), Some(binding));
    }

    if let Some(schema) = catalog.schemas.iter().find(|s| s.name.to_lowercase() == name_lc) {
        let line = fill(&labels.name_kind, &[("name", &format!("**{}**", schema.name)), ("kind", &labels.schema)]);
        return with_lines(vec![line], None);
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
    Some(HoverQuery { column_lookup: Some(ColumnLookup { bindings: candidates, name: name_lc }), ..span })
}
