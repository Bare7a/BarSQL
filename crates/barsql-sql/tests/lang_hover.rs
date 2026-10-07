use barsql_core::{DriverType, FunctionKind, SchemaInfo, TableInfo};
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

fn function(sql: &str, needle: &str) -> Option<(String, FunctionKind)> {
    match subject(sql, needle, 1)? {
        HoverSubject::Function(doc) => Some((doc.name, doc.kind)),
        _ => None,
    }
}

#[test]
fn describes_a_called_function() {
    assert_eq!(function("SELECT count(*) FROM users", "count"), Some(("count".into(), FunctionKind::Aggregate)));
    assert_eq!(function("SELECT LOWER(email) FROM users", "LOWER"), Some(("lower".into(), FunctionKind::Scalar)));
    assert_eq!(
        function("SELECT * FROM generate_series(1, 3)", "generate"),
        Some(("generate_series".into(), FunctionKind::Table))
    );
    assert_eq!(
        function("SELECT current_date", "current_date").map(|f| f.0),
        Some("current_date".into()),
        "no parentheses"
    );
    let doc = match subject("SELECT jsonb_build_object('a', 1)", "jsonb", 1) {
        Some(HoverSubject::Function(doc)) => doc,
        other => panic!("{other:?}"),
    };
    assert!(doc.builtin && !doc.summary.is_empty() && !doc.signatures.is_empty(), "{doc:?}");
}

#[test]
fn a_name_before_parentheses_isnt_always_a_call() {
    // Column lists and definitions.
    for (sql, needle) in [
        ("INSERT INTO users (id) VALUES (1)", "users"),
        ("CREATE TABLE lower (id int)", "lower"),
        ("CREATE INDEX i ON users (lower(email))", "users"),
        ("ALTER TABLE orders ADD FOREIGN KEY (uid) REFERENCES users (id)", "users (id)"),
    ] {
        assert!(!matches!(subject(sql, needle, 1), Some(HoverSubject::Function(_))), "{sql}");
    }
    // Keywords that a parenthesis follows.
    for (sql, needle) in [
        ("SELECT 1 WHERE 1 IN (1)", "IN"),
        ("INSERT INTO t VALUES (1)", "VALUES"),
        ("SELECT row_number() OVER (ORDER BY 1)", "OVER"),
        ("SELECT 1 WHERE EXISTS (SELECT 1)", "EXISTS"),
    ] {
        assert_eq!(subject(sql, needle, 1), None, "{sql}");
    }
    // An unknown call describes nothing, rather than a column of the same name.
    assert_eq!(subject("SELECT nosuchfn(id) FROM users", "nosuchfn", 1), None);
    // A plain word that happens to name a function isn't a call.
    assert!(!matches!(subject("SELECT lower FROM users", "lower", 1), Some(HoverSubject::Function(_))));
}
