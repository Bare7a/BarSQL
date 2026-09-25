use std::collections::HashSet;

use super::catalog::Catalog;
use super::labels::{SqlLabels, fill};
use super::query::parse_query;
use super::statements::parse_statements;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlDiagnostic {
    // Offsets into the whole buffer.
    pub start: usize,
    pub end: usize,
    pub message: String,
}

// Catches table typos before a server round trip. CTE names count as known.
pub fn collect_schema_diagnostics(sql: &str, catalog: &Catalog, labels: &SqlLabels) -> Vec<SqlDiagnostic> {
    let mut out = Vec::new();
    for stmt in parse_statements(sql, Some(&catalog.driver)) {
        let parsed = parse_query(&sql[stmt.start..stmt.end], catalog);
        let local: HashSet<String> = parsed.ctes.iter().map(|c| c.to_lowercase()).collect();
        for r in &parsed.query_tables {
            if r.known || local.contains(&r.table.to_lowercase()) || r.table.trim().is_empty() {
                continue;
            }
            out.push(SqlDiagnostic {
                start: stmt.start + r.name_start,
                end: stmt.start + r.name_end,
                message: fill(&labels.unknown_table, &[("table", &r.table)]),
            });
        }
    }
    out
}
