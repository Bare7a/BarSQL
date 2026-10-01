use barsql_core::{DriverType, SchemaInfo, TableInfo};
use barsql_sql::lang::{Catalog, ColumnLookup, HoverQuery, HoverSubject, TableBinding, analyze_hover, parse_query};

fn table(name: &str, kind: &str) -> TableInfo {
    TableInfo { schema: "public".into(), name: name.into(), kind: kind.into() }
}

fn catalog() -> Catalog {
    Catalog::new(
        DriverType::Postgres,
        vec![SchemaInfo { name: "public".into() }],
        vec![table("users", "table"), table("orders", "view")],
    )
}

fn hover_at(sql: &str, needle: &str, occurrence: usize) -> Option<HoverQuery> {
    let offset = sql.match_indices(needle).nth(occurrence - 1).expect("needle").0;
    let catalog = catalog();
    analyze_hover(sql, offset, &parse_query(sql, &catalog), &catalog)
}

fn subject(sql: &str, needle: &str, occurrence: usize) -> Option<HoverSubject> {
    hover_at(sql, needle, occurrence).map(|q| q.subject)
}

fn binding(table: &str) -> TableBinding {
    TableBinding { schema: "public".into(), table: table.into() }
}

#[test]
fn describes_a_table_or_view_from_the_catalog() {
    assert_eq!(
        subject("SELECT * FROM users WHERE id = 1", "users", 1),
        Some(HoverSubject::Table { table: table("users", "table"), alias: None })
    );
    assert_eq!(
        subject("SELECT * FROM orders", "orders", 1),
        Some(HoverSubject::Table { table: table("orders", "view"), alias: None })
    );
}

#[test]
fn spans_exactly_the_hovered_token() {
    let sql = "SELECT * FROM users";
    let q = hover_at(sql, "users", 1).unwrap();
    assert_eq!(&sql[q.start..q.end], "users");
}

#[test]
fn describes_an_alias_as_its_table() {
    assert_eq!(
        subject("SELECT * FROM users u WHERE u.id = 1", "u ", 1),
        Some(HoverSubject::Table { table: table("users", "table"), alias: Some("u".into()) })
    );
    assert_eq!(
        subject("SELECT * FROM archive x", "x", 1),
        Some(HoverSubject::Alias { name: "x".into(), table: "archive".into() }),
        "an alias for a table the catalog doesn't know"
    );
}

#[test]
fn requests_a_column_lookup_for_a_qualified_column() {
    assert_eq!(
        subject("SELECT * FROM users u WHERE u.email = 1", "email", 1),
        Some(HoverSubject::Column(ColumnLookup { bindings: vec![binding("users")], name: "email".into() }))
    );
}

#[test]
fn requests_a_lookup_across_in_scope_tables_for_a_bare_column() {
    let Some(HoverSubject::Column(lookup)) = subject("SELECT email FROM users JOIN orders o ON 1=1", "email", 1) else {
        panic!("a column lookup");
    };
    assert_eq!(lookup.name, "email");
    let mut tables: Vec<String> = lookup.bindings.into_iter().map(|b| b.table).collect();
    tables.sort();
    assert_eq!(tables, ["orders", "users"]);
}

#[test]
fn answers_ctes_and_their_columns_without_a_lookup() {
    let sql = "WITH recent AS (SELECT id, email AS mail FROM users) SELECT * FROM recent WHERE recent.mail = 1";
    assert_eq!(
        subject(sql, "recent", 2),
        Some(HoverSubject::Derived { name: "recent".into(), cte: true, columns: vec!["id".into(), "mail".into()] })
    );
    // `email` contains `mail`, so the qualified recent.mail is the third occurrence.
    assert_eq!(
        subject(sql, "mail", 3),
        Some(HoverSubject::DerivedColumn { name: "mail".into(), source: "recent".into(), cte: true })
    );
    let missing = "WITH recent AS (SELECT id FROM users) SELECT recent.nope FROM recent";
    assert_eq!(subject(missing, "nope", 1), None, "a name the CTE doesn't output");
}

#[test]
fn lists_every_column_of_a_cte() {
    let cols: Vec<String> = (0..12).map(|i| format!("c{i}")).collect();
    let sql = format!("WITH wide AS (SELECT {} FROM users) SELECT * FROM wide", cols.join(", "));
    assert_eq!(subject(&sql, "wide", 2), Some(HoverSubject::Derived { name: "wide".into(), cte: true, columns: cols }));
}

#[test]
fn describes_a_subquery_column_with_its_source() {
    let sql = "SELECT s.n FROM (SELECT 1 AS n) S";
    assert_eq!(
        subject(sql, "n", 1),
        Some(HoverSubject::DerivedColumn { name: "n".into(), source: "s".into(), cte: false })
    );
}

#[test]
fn ignores_keywords_strings_and_comments() {
    assert!(hover_at("SELECT * FROM users", "SELECT", 1).is_none());
    assert!(hover_at("SELECT * FROM users WHERE a = 'users'", "users", 2).is_none());
    assert!(hover_at("SELECT * FROM users -- users note", "users", 2).is_none());
}

#[test]
fn describes_a_schema_name() {
    assert_eq!(
        subject("SELECT * FROM public.users", "public", 1),
        Some(HoverSubject::Schema { name: "public".into() })
    );
}
