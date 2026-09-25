use std::path::PathBuf;

use barsql_core::{DriverType, SchemaInfo, TableInfo};
use barsql_sql::lang::{
    Catalog, SqlLabels, TxnControl, collect_schema_diagnostics, detect_transaction_control, parse_query,
};

fn table(name: &str) -> TableInfo {
    TableInfo { schema: "public".into(), name: name.into(), kind: "table".into() }
}

fn catalog_of(driver: DriverType, tables: &[&str]) -> Catalog {
    Catalog::new(driver, vec![SchemaInfo { name: "public".into() }], tables.iter().map(|t| table(t)).collect())
}

fn catalog(driver: DriverType) -> Catalog {
    catalog_of(driver, &["users", "orders"])
}

fn table_names(sql: &str, driver: DriverType) -> Vec<String> {
    parse_query(sql, &catalog(driver)).query_tables.iter().map(|t| t.table.clone()).collect()
}

fn pg(sql: &str) -> Vec<String> {
    table_names(sql, DriverType::Postgres)
}

#[test]
fn comments_and_strings_bind_no_phantom_tables() {
    assert_eq!(pg("SELECT * FROM users -- JOIN phantom p\nWHERE "), ["users"]);
    assert_eq!(pg("-- SELECT * FROM old_table\nSELECT * FROM users WHERE "), ["users"]);
    assert_eq!(pg("SELECT * FROM users /* JOIN phantom p */ WHERE "), ["users"]);
    assert_eq!(pg("SELECT * FROM users WHERE note = 'copied from backups' AND "), ["users"]);
    assert_eq!(table_names("SELECT * FROM users WHERE name = \"John from accounting\"", DriverType::MySql), ["users"]);
    assert_eq!(table_names("SELECT * FROM users # JOIN phantom\nWHERE ", DriverType::MySql), ["users"]);
}

#[test]
fn quoted_identifier_tables_still_parse() {
    let parsed = parse_query("SELECT * FROM \"users\" u JOIN `orders` o ON ", &catalog(DriverType::Postgres));
    assert_eq!(parsed.query_tables.iter().map(|t| t.table.as_str()).collect::<Vec<_>>(), ["users", "orders"]);
    assert_eq!(parsed.bindings.get("u").map(|b| b.table.as_str()), Some("users"));
}

#[test]
fn ctes_come_from_code_not_comments() {
    let pg_catalog = catalog(DriverType::Postgres);
    assert!(parse_query("-- WITH x AS (SELECT 1)\nSELECT * FROM users", &pg_catalog).ctes.is_empty());
    let parsed = parse_query("-- recent signups\nWITH recent AS (SELECT 1) SELECT * FROM ", &pg_catalog);
    assert_eq!(parsed.ctes, ["recent"]);
}

#[test]
fn is_distinct_from_is_not_a_table_source() {
    assert_eq!(pg("SELECT * FROM users WHERE name IS DISTINCT FROM email"), ["users"]);
    assert_eq!(pg("SELECT * FROM users WHERE name IS NOT DISTINCT FROM email AND "), ["users"]);
    assert_eq!(pg("SELECT DISTINCT name FROM users JOIN orders o ON "), ["users", "orders"]);
}

#[test]
fn write_targets_are_bound() {
    let parsed = parse_query("INSERT INTO users (id) VALUES (1)", &catalog(DriverType::Postgres));
    assert_eq!(parsed.query_tables.iter().map(|t| t.table.as_str()).collect::<Vec<_>>(), ["users"]);
    assert_eq!(parsed.bindings.get("users").map(|b| b.table.as_str()), Some("users"));
    assert_eq!(pg("INSERT INTO public.orders (id) VALUES (1)"), ["orders"]);
    assert_eq!(table_names("REPLACE INTO orders SET id = 1", DriverType::MySql), ["orders"]);
    assert_eq!(pg("UPDATE users SET name = 1 WHERE "), ["users"]);
}

fn diags(sql: &str) -> Vec<(String, String)> {
    collect_schema_diagnostics(sql, &catalog(DriverType::Postgres), &SqlLabels::default())
        .into_iter()
        .map(|d| (sql[d.start..d.end].to_string(), d.message))
        .collect()
}

#[test]
fn flags_an_unknown_table_with_its_exact_span() {
    assert_eq!(diags("SELECT * FROM userz WHERE id = 1"), [("userz".into(), "Unknown table \"userz\"".into())]);
}

#[test]
fn known_tables_ctes_and_schema_qualifiers_are_not_flagged() {
    assert!(diags("SELECT * FROM users JOIN orders o ON o.id = users.id").is_empty());
    assert!(diags("SELECT * FROM USERS").is_empty());
    assert!(diags("WITH recent AS (SELECT 1) SELECT * FROM recent").is_empty());
    assert!(diags("SELECT * FROM public.").is_empty());
    assert!(diags("SELECT * FROM users -- FROM phantom\nWHERE note = 'from ghost'").is_empty());
}

#[test]
fn flags_unknown_write_targets_and_qualified_misses() {
    let message = |sql: &str| diags(sql).first().map(|d| d.1.clone());
    assert_eq!(message("INSERT INTO userz (id) VALUES (1)").as_deref(), Some("Unknown table \"userz\""));
    assert_eq!(message("UPDATE userz SET x = 1").as_deref(), Some("Unknown table \"userz\""));
    assert_eq!(diags("SELECT * FROM public.userz"), [("userz".into(), "Unknown table \"userz\"".into())]);
}

#[test]
fn diagnostic_offsets_are_absolute_across_statements() {
    let sql = "SELECT * FROM users;\nSELECT * FROM ghost;";
    assert_eq!(diags(sql).iter().map(|d| d.0.as_str()).collect::<Vec<_>>(), ["ghost"]);
    let sql = "SELECT 'é';\nSELECT * FROM ghost;";
    assert_eq!(diags(sql).iter().map(|d| d.0.as_str()).collect::<Vec<_>>(), ["ghost"]);
}

#[test]
fn quoted_identifiers_are_matched_exactly_in_spans() {
    let caps = Catalog::new(DriverType::Postgres, vec![SchemaInfo { name: "public".into() }], vec![table("Users")]);
    let sql = "SELECT * FROM \"Userz\"";
    let out = collect_schema_diagnostics(sql, &caps, &SqlLabels::default());
    assert_eq!(out.iter().map(|d| &sql[d.start..d.end]).collect::<Vec<_>>(), ["\"Userz\""]);
    assert!(collect_schema_diagnostics("SELECT * FROM \"Users\"", &caps, &SqlLabels::default()).is_empty());
}

#[test]
fn detects_transaction_control() {
    use TxnControl::*;
    for (sql, want) in [
        ("BEGIN", Some(Begin)),
        ("COMMIT", Some(Commit)),
        ("ROLLBACK", Some(Rollback)),
        ("  begin ;  ", Some(Begin)),
        ("Commit;", Some(Commit)),
        ("\nROLLBACK;\n", Some(Rollback)),
        ("BEGIN WORK", Some(Begin)),
        ("BEGIN TRANSACTION;", Some(Begin)),
        ("START TRANSACTION", Some(Begin)),
        ("COMMIT WORK", Some(Commit)),
        ("ROLLBACK TRANSACTION", Some(Rollback)),
        ("/* go */ COMMIT; -- done", Some(Commit)),
        ("-- start\nBEGIN;", Some(Begin)),
        ("SELECT 1", None),
        ("SELECT 'COMMIT'", None),
        ("", None),
        ("ROLLBACK TO SAVEPOINT sp1", None),
        ("SAVEPOINT sp1", None),
        ("BEGIN; SELECT 1; COMMIT;", None),
        ("BEGIN; UPDATE t SET x = 1", None),
    ] {
        assert_eq!(detect_transaction_control(sql), want, "{sql:?}");
    }
}

fn locale(lang: &str) -> serde_json::Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("../barsql-ui/locales/base/{lang}.json"));
    serde_json::from_str(&std::fs::read_to_string(path).expect("locale")).expect("locale json")
}

#[test]
fn labels_come_from_the_locale_files() {
    let english = SqlLabels::from_locale(&locale("en"));
    assert_eq!(english, SqlLabels::default(), "the built-in copy matches en.json");
    assert_eq!(english.not_null, "not null");
    let german = SqlLabels::from_locale(&locale("de"));
    assert_eq!((german.table.as_str(), german.not_null.as_str()), ("Tabelle", "nicht null"));
    assert_eq!(SqlLabels::from_locale(&serde_json::json!({})), SqlLabels::default());
}
