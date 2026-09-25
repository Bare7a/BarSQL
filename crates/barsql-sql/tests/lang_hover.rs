use barsql_core::{ColumnInfo, DriverType, SchemaInfo, TableInfo};
use barsql_sql::lang::{
    Catalog, ColumnLookup, HoverQuery, SqlLabels, TableBinding, analyze_hover, column_hover_lines, parse_query,
    table_columns_markdown,
};

fn catalog() -> Catalog {
    let table = |name: &str, kind: &str| TableInfo { schema: "public".into(), name: name.into(), kind: kind.into() };
    Catalog::new(
        DriverType::Postgres,
        vec![SchemaInfo { name: "public".into() }],
        vec![table("users", "table"), table("orders", "view")],
    )
}

fn hover_at(sql: &str, needle: &str, occurrence: usize) -> Option<HoverQuery> {
    let offset = sql.match_indices(needle).nth(occurrence - 1).expect("needle").0;
    let catalog = catalog();
    analyze_hover(sql, offset, &parse_query(sql, &catalog), &catalog, &SqlLabels::default())
}

fn lines(sql: &str, needle: &str, occurrence: usize) -> Option<Vec<String>> {
    hover_at(sql, needle, occurrence).and_then(|q| q.lines)
}

fn binding(table: &str) -> TableBinding {
    TableBinding { schema: "public".into(), table: table.into() }
}

fn column(name: &str, data_type: &str, nullable: bool, primary: bool) -> ColumnInfo {
    ColumnInfo {
        name: name.into(),
        data_type: data_type.into(),
        is_nullable: nullable,
        is_primary: primary,
        ..Default::default()
    }
}

#[test]
fn describes_a_table_with_its_type_and_schema() {
    assert_eq!(lines("SELECT * FROM users WHERE id = 1", "users", 1).unwrap(), ["**users** · table", "schema public"]);
    assert_eq!(lines("SELECT * FROM orders", "orders", 1).unwrap(), ["**orders** · view", "schema public"]);
}

#[test]
fn spans_exactly_the_hovered_token() {
    let sql = "SELECT * FROM users";
    let q = hover_at(sql, "users", 1).unwrap();
    assert_eq!(&sql[q.start..q.end], "users");
}

#[test]
fn describes_an_alias_as_pointing_at_its_table() {
    assert_eq!(lines("SELECT * FROM users u WHERE u.id = 1", "u ", 1).unwrap()[0], "**u** · alias for users");
}

#[test]
fn requests_a_column_lookup_for_a_qualified_column() {
    let q = hover_at("SELECT * FROM users u WHERE u.email = 1", "email", 1).unwrap();
    assert_eq!(q.column_lookup, Some(ColumnLookup { bindings: vec![binding("users")], name: "email".into() }));
}

#[test]
fn requests_a_lookup_across_in_scope_tables_for_a_bare_column() {
    let lookup = hover_at("SELECT email FROM users JOIN orders o ON 1=1", "email", 1).unwrap().column_lookup.unwrap();
    assert_eq!(lookup.name, "email");
    let mut tables: Vec<String> = lookup.bindings.into_iter().map(|b| b.table).collect();
    tables.sort();
    assert_eq!(tables, ["orders", "users"]);
}

#[test]
fn answers_ctes_and_their_columns_without_a_lookup() {
    let sql = "WITH recent AS (SELECT id, email AS mail FROM users) SELECT * FROM recent WHERE recent.mail = 1";
    assert_eq!(lines(sql, "recent", 2).unwrap(), ["**recent** · CTE", "columns: id, mail"]);
    // `email` contains `mail`, so the qualified recent.mail is the third occurrence.
    assert_eq!(lines(sql, "mail", 3).unwrap(), ["**mail**", "column of CTE recent"]);
}

#[test]
fn ignores_keywords_strings_and_comments() {
    assert!(hover_at("SELECT * FROM users", "SELECT", 1).is_none());
    assert!(hover_at("SELECT * FROM users WHERE a = 'users'", "users", 2).is_none());
    assert!(hover_at("SELECT * FROM users -- users note", "users", 2).is_none());
}

#[test]
fn describes_a_schema_name() {
    assert_eq!(lines("SELECT * FROM public.users", "public", 1).unwrap(), ["**public** · schema"]);
}

#[test]
fn formats_column_hover_lines() {
    let lines = column_hover_lines(&column("email", "text", false, false), &binding("users"), &SqlLabels::default());
    assert_eq!(lines, ["**email** · text · not null", "column of public.users"]);
}

#[test]
fn table_and_alias_hovers_append_the_column_list() {
    assert_eq!(hover_at("SELECT * FROM users", "users", 1).unwrap().table_columns, Some(binding("users")));
    assert_eq!(
        hover_at("SELECT * FROM users u WHERE u.id = 1", "u ", 1).unwrap().table_columns,
        Some(binding("users"))
    );
}

#[test]
fn renders_the_column_list_as_a_capped_markdown_table() {
    let labels = SqlLabels::default();
    let cols = [column("id", "int", false, true), column("email", "text", false, false)];
    assert_eq!(
        table_columns_markdown(&cols, 30, &labels),
        ["| column | type |\n| --- | --- |\n| id | int · PK |\n| email | text · not null |"]
    );
    assert!(table_columns_markdown(&[], 30, &labels).is_empty());
    let many: Vec<ColumnInfo> = (0..35).map(|i| column(&format!("c{i}"), "int", true, false)).collect();
    assert_eq!(table_columns_markdown(&many, 30, &labels)[1], "… 5 more columns");
}
