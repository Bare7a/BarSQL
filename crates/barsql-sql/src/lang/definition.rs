use std::ops::Range;

use barsql_core::DriverType;

use crate::lang::catalog::Catalog;
use crate::lang::hover::{ColumnLookup, HoverSubject, analyze_hover};
use crate::lang::query::ParsedQuery;
use crate::lang::tokens::{Token, TokenKind, tokenize};

// Where Go to Definition takes the name at an offset, as DataGrip's does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Definition {
    // Statement-relative span of the name's declaration: an alias after its table, a CTE's name, a subquery's alias.
    Declared(Range<usize>),
    Table { schema: String, name: String },
    // A column of the first of these tables that has it.
    Column(ColumnLookup),
}

// The span of the name at `offset` and where it leads. A table's own name opens the table, and its alias jumps to
// where the query declares it.
pub fn analyze_definition(
    stmt: &str,
    offset: usize,
    parsed: &ParsedQuery,
    catalog: &Catalog,
    driver: Option<&DriverType>,
) -> Option<(Range<usize>, Definition)> {
    let hover = analyze_hover(stmt, offset, parsed, catalog)?;
    let span = hover.start..hover.end;
    let tokens = tokenize(stmt, driver);
    let word = tokens.iter().find(|t| t.start == span.start).map(|t| t.ident_text().to_lowercase())?;
    let definition = match hover.subject {
        HoverSubject::Table { alias: Some(alias), table }
            if alias.to_lowercase() == word && table.name.to_lowercase() != word =>
        {
            Definition::Declared(alias_declaration(&tokens, parsed, &word)?)
        }
        HoverSubject::Table { table, .. } => Definition::Table { schema: table.schema, name: table.name },
        HoverSubject::Alias { name, .. } => {
            Definition::Declared(alias_declaration(&tokens, parsed, &name.to_lowercase())?)
        }
        HoverSubject::Derived { name, cte, .. } | HoverSubject::DerivedColumn { source: name, cte, .. } => {
            Definition::Declared(derived_declaration(&tokens, &name.to_lowercase(), cte)?)
        }
        HoverSubject::Column(lookup) => Definition::Column(lookup),
        HoverSubject::Schema { .. } | HoverSubject::Function(_) => return None,
    };
    Some((span, definition))
}

fn named(token: &Token, lower: &str) -> bool {
    token.is_ident_like() && token.ident_text().to_lowercase() == lower
}

// The alias token after its table's name.
fn alias_declaration(tokens: &[Token], parsed: &ParsedQuery, lower: &str) -> Option<Range<usize>> {
    let table = parsed.query_tables.iter().find(|t| t.alias.as_deref().is_some_and(|a| a.to_lowercase() == lower))?;
    tokens.iter().find(|t| t.start >= table.name_end && named(t, lower)).map(|t| t.start..t.end)
}

// A CTE's name, before AS or its column list. A subquery's alias, after its closing parenthesis and an optional AS.
fn derived_declaration(tokens: &[Token], lower: &str, cte: bool) -> Option<Range<usize>> {
    let code: Vec<&Token> = tokens.iter().filter(|t| t.kind != TokenKind::Comment).collect();
    let at = |i: Option<usize>| i.and_then(|i| code.get(i));
    let declares = |i: usize| match cte {
        true => at(Some(i + 1)).is_some_and(|t| t.is_keyword("as") || t.is_punct("(")),
        false => {
            let before = at(i.checked_sub(1));
            before.is_some_and(|t| t.is_punct(")"))
                || (before.is_some_and(|t| t.is_keyword("as")) && at(i.checked_sub(2)).is_some_and(|t| t.is_punct(")")))
        }
    };
    (0..code.len()).find(|&i| named(code[i], lower) && declares(i)).map(|i| code[i].start..code[i].end)
}

#[cfg(test)]
mod tests {
    use barsql_core::{SchemaInfo, TableInfo};

    use super::*;
    use crate::lang::catalog::TableBinding;
    use crate::lang::query::parse_query;

    fn catalog() -> Catalog {
        let table = |name: &str| TableInfo { schema: "public".into(), name: name.into(), kind: "table".into() };
        Catalog::new(
            DriverType::Postgres,
            vec![SchemaInfo { name: "public".into() }],
            vec![table("users"), table("orders")],
        )
    }

    // The target of `needle`'s `occurrence`th match, with a declaration shown as the text it spans.
    fn target(sql: &str, needle: &str, occurrence: usize) -> Option<String> {
        let offset = sql.match_indices(needle).nth(occurrence - 1).expect("needle").0;
        let catalog = catalog();
        let (_, definition) =
            analyze_definition(sql, offset, &parse_query(sql, &catalog), &catalog, Some(&DriverType::Postgres))?;
        Some(match definition {
            Definition::Declared(range) => format!("declared {} at {}", &sql[range.clone()], range.start),
            Definition::Table { schema, name } => format!("table {schema}.{name}"),
            Definition::Column(lookup) => {
                let tables: Vec<String> = lookup.bindings.iter().map(|b: &TableBinding| b.table.clone()).collect();
                format!("column {} of {}", lookup.name, tables.join(","))
            }
        })
    }

    #[test]
    fn a_table_opens_and_its_alias_jumps_to_the_declaration() {
        let sql = "SELECT u.id FROM users u JOIN orders AS o ON o.user_id = u.id";
        assert_eq!(target(sql, "users", 1).as_deref(), Some("table public.users"));
        assert_eq!(target(sql, "u.", 1).as_deref(), Some("declared u at 23"));
        assert_eq!(target(sql, "o.", 1).as_deref(), Some("declared o at 40"));
    }

    #[test]
    fn a_cte_or_subquery_jumps_to_where_it_is_defined() {
        let sql = "WITH recent AS (SELECT * FROM orders) SELECT * FROM recent";
        assert_eq!(target(sql, "recent", 2).as_deref(), Some("declared recent at 5"));
        let sql = "SELECT s.n FROM (SELECT 1 AS n) AS s";
        assert_eq!(target(sql, "s.", 1).as_deref(), Some("declared s at 35"));
    }

    #[test]
    fn a_column_leads_to_its_tables_and_a_function_nowhere() {
        let sql = "SELECT u.name, lower(u.name) FROM users u";
        assert_eq!(target(sql, "name", 1).as_deref(), Some("column name of users"));
        assert_eq!(target(sql, "lower", 1), None);
    }
}
