use barsql_core::{ColumnInfo, DriverType, SchemaInfo, TableInfo};
use barsql_sql::lang::quoting::{column_cache_key, identifier_needs_quote};
use barsql_sql::lang::{
    Catalog, ColumnMap, CompletionContext, CompletionItem, CursorSlot, ItemKind, SqlLabels, analyze_cursor,
    bindings_needing_columns, build_completion_items, completion_replace_range, current_statement_start, parse_query,
    parse_statements,
};

const PG: DriverType = DriverType::Postgres;
const ALL_COLS: [&str; 3] = ["id", "email", "name"];

fn col(name: &str, data_type: &str, nullable: bool, primary: bool) -> ColumnInfo {
    ColumnInfo {
        name: name.into(),
        data_type: data_type.into(),
        is_nullable: nullable,
        is_primary: primary,
        ..Default::default()
    }
}

fn user_columns() -> Vec<ColumnInfo> {
    vec![col("id", "int", false, true), col("email", "text", false, false), col("name", "text", true, false)]
}

fn table(name: &str) -> TableInfo {
    TableInfo { schema: "public".into(), name: name.into(), kind: "table".into() }
}

fn tables(names: &[&str]) -> Vec<TableInfo> {
    names.iter().map(|n| table(n)).collect()
}

fn columns(entries: &[(&str, Vec<ColumnInfo>)]) -> ColumnMap {
    entries.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
}

struct Setup {
    catalog: Catalog,
    columns: ColumnMap,
    labels: SqlLabels,
}

impl Setup {
    fn new(driver: DriverType, tables: Vec<TableInfo>, columns: ColumnMap) -> Self {
        Self {
            catalog: Catalog::new(driver, vec![SchemaInfo { name: "public".into() }], tables),
            columns,
            labels: SqlLabels::default(),
        }
    }

    fn standard(driver: DriverType) -> Self {
        Self::new(driver, tables(&["users", "orders"]), columns(&[("public.users", user_columns())]))
    }

    fn items_at(&self, text: &str, position: usize, statement_start: usize) -> Vec<CompletionItem> {
        let ctx = CompletionContext {
            catalog: &self.catalog,
            columns_by_table: &self.columns,
            columns: &[],
            labels: &self.labels,
        };
        build_completion_items(&ctx, text, position, &parse_query(text, &self.catalog), statement_start)
    }

    fn items(&self, text: &str) -> Vec<CompletionItem> {
        self.items_at(text, text.len(), 0)
    }

    // Loads only the columns completion asks for, as the editor does.
    fn loaded_items(&self, text: &str) -> Vec<CompletionItem> {
        let parsed = parse_query(text, &self.catalog);
        let mut loaded = ColumnMap::new();
        for b in bindings_needing_columns(text, &parsed, Some(&self.catalog)) {
            let key = column_cache_key(&b.schema, &b.table);
            loaded.insert(key.clone(), self.columns.get(&key).cloned().unwrap_or_default());
        }
        let ctx =
            CompletionContext { catalog: &self.catalog, columns_by_table: &loaded, columns: &[], labels: &self.labels };
        build_completion_items(&ctx, text, text.len(), &parsed, 0)
    }
}

fn complete(text: &str, driver: DriverType) -> Vec<CompletionItem> {
    Setup::standard(driver).items(text)
}

fn labels_in(text: &str, driver: DriverType) -> Vec<String> {
    complete(text, driver).into_iter().map(|i| i.label).collect()
}

fn labels(text: &str) -> Vec<String> {
    labels_in(text, PG)
}

fn column_labels_in(text: &str, driver: DriverType) -> Vec<String> {
    labels_in(text, driver).into_iter().filter(|l| ALL_COLS.contains(&l.as_str())).collect()
}

fn column_labels(text: &str) -> Vec<String> {
    column_labels_in(text, PG)
}

fn insert_for(text: &str, label: &str) -> Option<String> {
    complete(text, PG).into_iter().find(|i| i.label == label).map(|i| i.insert_text)
}

fn has_all(labels: &[String], wanted: &[&str]) -> bool {
    wanted.iter().all(|w| labels.iter().any(|l| l == w))
}

fn has_any(labels: &[String], wanted: &[&str]) -> bool {
    wanted.iter().any(|w| labels.iter().any(|l| l == w))
}

fn has(labels: &[String], wanted: &str) -> bool {
    labels.iter().any(|l| l == wanted)
}

fn slot(sql: &str) -> CursorSlot {
    analyze_cursor(sql, Some(&PG)).slot
}

// Text a chosen item would overwrite. Assumes a single-line statement.
fn replaced(text: &str) -> &str {
    let range = completion_replace_range(text.len(), text, text.len()..text.len(), None);
    &text[range]
}

const BUTTED: [&str; 5] = [
    "SELECT * FROM users AS \"Users\" WHERE",
    "SELECT * FROM \"Users\" WHERE",
    "SELECT * FROM users \"U\" WHERE",
    "SELECT * FROM users ORDER BY",
    "SELECT * FROM \"Users\" ORDER BY",
];

#[test]
fn columns_appear_right_after_a_clause_keyword() {
    for sql in BUTTED {
        assert!(has_all(&column_labels(sql), &ALL_COLS), "{sql}");
        // Caret touches the keyword, so the insert adds the space.
        assert_eq!(insert_for(sql, "email").as_deref(), Some(" email"), "{sql}");
        let range = completion_replace_range(sql.len(), sql, sql.len()..sql.len(), None);
        assert!(range.is_empty(), "{sql} replaces nothing");
    }
}

#[test]
fn butted_keywords_cover_having_from_join_and_set() {
    assert_eq!(slot("SELECT * FROM users GROUP BY x HAVING"), CursorSlot::FilterStart);
    for sql in ["SELECT * FROM", "SELECT * FROM users JOIN"] {
        assert!(matches!(slot(sql), CursorSlot::Table { leading_space: true, .. }), "{sql}");
    }
    assert!(matches!(slot("UPDATE users SET"), CursorSlot::SetColumn { leading_space: true, .. }));
}

#[test]
fn identifiers_ending_in_a_keyword_are_not_the_keyword() {
    assert!(matches!(slot("SELECT brand"), CursorSlot::General { .. }));
    assert!(matches!(slot("SELECT elsewhere"), CursorSlot::General { .. }));
    let person = slot("SELECT * FROM person");
    assert_eq!(person, CursorSlot::Table { prefix: "person".into(), replace_len: 6, leading_space: false });
}

#[test]
fn order_and_group_by_suggest_columns() {
    for sql in ["SELECT * FROM users ORDER BY ", "SELECT * FROM \"Users\" ORDER BY ", "SELECT * FROM users GROUP BY "] {
        assert!(has_all(&column_labels(sql), &ALL_COLS), "{sql}");
        assert_eq!(insert_for(sql, "email").as_deref(), Some("email"), "{sql}");
    }
}

#[test]
fn directions_follow_a_sort_column_only_in_order_by() {
    assert!(!has_any(&labels("SELECT * FROM users ORDER BY "), &["ASC", "DESC"]));
    assert!(has_all(&labels("SELECT * FROM users ORDER BY name "), &["ASC", "DESC"]));
    assert!(!has_any(&labels("SELECT * FROM users GROUP BY name "), &["ASC", "DESC"]));
}

#[test]
fn order_by_filters_and_resolves_alias_qualifiers() {
    assert_eq!(column_labels("SELECT * FROM \"Users\" ORDER BY na"), ["name"]);
    assert!(has_all(&column_labels("SELECT * FROM users \"U\" ORDER BY U."), &ALL_COLS));
}

#[test]
fn a_terminating_clause_ends_the_by_list() {
    assert!(!matches!(slot("SELECT * FROM users GROUP BY id HAVING "), CursorSlot::OrderGroup { .. }));
    assert!(matches!(slot("SELECT * FROM users ORDER BY id LIMIT "), CursorSlot::Limit { .. }));
}

#[test]
fn inserts_use_the_driver_quoting() {
    let setup = Setup::new(
        DriverType::MySql,
        tables(&["users", "orders"]),
        columns(&[("public.users", vec![col("First Name", "text", true, false)])]),
    );
    let items = setup.items("SELECT * FROM users WHERE ");
    assert_eq!(items.iter().find(|i| i.label == "First Name").map(|i| i.insert_text.as_str()), Some("`First Name`"));
}

#[test]
fn existing_contexts_are_unchanged() {
    assert!(has(&labels("SELECT * FROM us"), "users"));
    assert!(has_all(&column_labels("SELECT * FROM users WHERE id = "), &ALL_COLS));
    assert!(matches!(slot("SELECT * FROM users WHERE id ="), CursorSlot::Value { .. }));
    assert!(has_all(&column_labels("UPDATE users SET "), &ALL_COLS));
    assert!(column_labels("UPDATE users SET name = 1 ").is_empty());
    assert!(has(&labels("UPDATE users SET name = 1 "), "WHERE"));
    assert!(has_all(&column_labels("UPDATE users SET name = 1, "), &ALL_COLS));
    assert!(has_all(&column_labels("SELECT * FROM users AS \"Users\" WHERE \"Users\"."), &ALL_COLS));
    assert!(has_all(&labels("SELECT * FROM users "), &["WHERE", "JOIN"]));
}

fn cap_setup() -> Setup {
    Setup::new(PG, vec![table("Users")], columns(&[("public.Users", user_columns())]))
}

#[test]
fn quoted_tables_label_bare_with_a_quoted_filter_text() {
    let setup = cap_setup();
    for sql in ["SELECT * FROM ", "SELECT * FROM \"Users\" WHERE", "SELECT * FROM \"Users\" WHERE Us"] {
        let items = setup.items(sql);
        let found = items.iter().find(|i| i.kind == ItemKind::Class && i.insert_text.trim() == "\"Users\"");
        let found = found.unwrap_or_else(|| panic!("{sql}: {items:?}"));
        assert_eq!(found.label, "Users");
        assert_eq!(found.filter_text.as_deref(), Some("\"Users\""));
    }
}

#[test]
fn the_replace_range_stops_at_an_earlier_closing_quote() {
    assert_eq!(replaced("SELECT * FROM \"Users\" WHERE "), "");
    assert_eq!(replaced("SELECT * FROM \"Users\" WHERE e"), "e");
    assert_eq!(replaced("SELECT * FROM \"Users\" WHERE Us"), "Us");
    // An open quote is part of the range, so it gets replaced instead of duplicated.
    assert_eq!(replaced("SELECT * FROM \"Users\" WHERE \"Us"), "\"Us");
}

fn scoped_labels(text: &str) -> Vec<String> {
    let start = current_statement_start(&parse_statements(text, None), text.len());
    Setup::standard(PG).items_at(text, text.len(), start).into_iter().map(|i| i.label).collect()
}

#[test]
fn statement_scoped_completion_still_suggests_tables() {
    assert!(has_all(&scoped_labels("SELECT * FROM "), &["users", "orders"]));
    assert!(has_all(&scoped_labels("SELECT 1;\nSELECT * FROM "), &["users", "orders"]));
}

#[test]
fn a_schema_qualifier_lists_its_tables() {
    assert!(has_all(&labels("SELECT * FROM public."), &["users", "orders"]));
}

#[test]
fn reserved_words_need_quotes() {
    assert!(identifier_needs_quote("order", &PG));
    assert!(identifier_needs_quote("select", &DriverType::MySql));
    assert!(identifier_needs_quote("user", &DriverType::Sqlite));
    assert!(!identifier_needs_quote("email", &PG));
    assert!(!identifier_needs_quote("created_at", &PG));
}

fn join_setup() -> Setup {
    Setup::new(
        PG,
        tables(&["accounts", "contracts"]),
        columns(&[
            ("public.accounts", vec![col("id", "int", false, true)]),
            (
                "public.contracts",
                vec![
                    col("id", "int", false, true),
                    col("account_id", "int", false, false),
                    col("amount", "numeric", true, false),
                ],
            ),
        ]),
    )
}

fn join_labels(text: &str) -> Vec<String> {
    join_setup().loaded_items(text).into_iter().map(|i| i.label).collect()
}

#[test]
fn join_targets_resolve() {
    let setup = join_setup();
    let parsed = parse_query("SELECT * FROM accounts JOIN contracts ON ", &setup.catalog);
    assert_eq!(parsed.query_tables.iter().map(|t| t.table.as_str()).collect::<Vec<_>>(), ["accounts", "contracts"]);
    assert_eq!(
        join_labels("SELECT * FROM accounts \nJOIN contracts ON \naccounts.id = contracts."),
        ["id", "account_id", "amount"]
    );
    assert_eq!(join_labels("SELECT * FROM accounts JOIN contracts ON contracts."), ["id", "account_id", "amount"]);
    assert_eq!(join_labels("SELECT * FROM accounts a JOIN contracts c ON c."), ["id", "account_id", "amount"]);
    let quoted_alias = parse_query("SELECT * FROM accounts AS \"join\" WHERE ", &setup.catalog);
    assert_eq!(quoted_alias.query_tables.iter().map(|t| t.table.as_str()).collect::<Vec<_>>(), ["accounts"]);
    assert!(has_all(
        &join_labels("SELECT * FROM accounts JOIN contracts ON accounts.id = "),
        &["accounts", "contracts"]
    ));
}

fn ebay_setup() -> Setup {
    Setup::new(
        PG,
        tables(&["Users", "EBayAccounts"]),
        columns(&[
            ("public.Users", vec![col("id", "int", false, true)]),
            ("public.EBayAccounts", vec![col("id", "int", false, true), col("userId", "int", false, false)]),
        ]),
    )
}

const EBAY: &str = "SELECT * FROM \"Users\" JOIN \"EBayAccounts\" ON \"Users\".id = ";

#[test]
fn the_value_side_offers_joined_table_refs_and_columns() {
    let items = ebay_setup().loaded_items(EBAY);
    let labels: Vec<String> = items.iter().map(|i| i.label.clone()).collect();
    assert!(has_all(&labels, &["Users", "EBayAccounts", "userId"]), "{labels:?}");
    let found = items.iter().find(|i| i.kind == ItemKind::Class && i.insert_text.trim() == "\"EBayAccounts\"").unwrap();
    assert_eq!((found.label.as_str(), found.filter_text.as_deref()), ("EBayAccounts", Some("\"EBayAccounts\"")));
    let qualified: Vec<String> =
        ebay_setup().loaded_items(&format!("{EBAY}\"EBayAccounts\".")).into_iter().map(|i| i.label).collect();
    assert!(has(&qualified, "userId"));
}

#[test]
fn limit_and_offset_take_numbers() {
    assert!(labels("SELECT * FROM users LIMIT ").is_empty());
    assert_eq!(labels("SELECT * FROM users LIMIT 100 "), ["OFFSET"]);
    assert!(labels("SELECT * FROM users LIMIT 100 OFFSET ").is_empty());
    assert!(labels("SELECT * FROM users LIMIT 100 OFFSET 5 ").is_empty());
    for sql in ["SELECT * FROM users LIMIT ", "SELECT * FROM users LIMIT 100 "] {
        assert!(!has_any(&labels(sql), &["users", "id", "email", "WHERE", "JOIN"]), "{sql}");
    }
    assert!(has(&labels("SELECT * FROM users "), "LIMIT"));
    assert!(has(&labels("SELECT * FROM users LIMI"), "LIMIT"));
    assert!(has_all(&labels("SELECT * FROM users ORDER BY id "), &["ASC", "DESC"]));
}

#[test]
fn no_snippet_items() {
    for sql in ["SELECT ", "SELECT * FROM users WHERE ", "SELECT * FROM users ORDER BY id ", "SELECT * FROM users "] {
        let snippets: Vec<String> = labels(sql)
            .into_iter()
            .filter(|l| l.contains("(…)") || l.contains("… ON") || l.starts_with("FROM …"))
            .collect();
        assert!(snippets.is_empty(), "{sql}: {snippets:?}");
    }
}

#[test]
fn identifiers_follow_only_identifier_expecting_tokens() {
    for sql in [
        "SELECT * FROM users WHERE ",
        "SELECT * FROM users WHERE id = 1 AND ",
        "SELECT * FROM users WHERE id = 1 OR ",
        "SELECT * FROM users WHERE id >= ",
        "SELECT * FROM users ORDER BY ",
    ] {
        assert!(has_all(&column_labels(sql), &ALL_COLS), "{sql}");
    }
    for sql in [
        "SELECT * FROM users WHERE id ",
        "SELECT * FROM users WHERE id = 5 ",
        "SELECT * FROM users ORDER BY id ",
        "SELECT * FROM users ORDER BY id DESC ",
    ] {
        assert!(column_labels(sql).is_empty(), "{sql}");
    }
    assert_eq!(labels("SELECT * FROM users ORDER BY id DESC "), ["LIMIT", "OFFSET"]);
    assert!(has_all(&labels("SELECT * FROM users WHERE id "), &["AND", "OR"]));
    assert_eq!(labels("SELECT * FROM users ORDER BY id "), ["ASC", "DESC", "LIMIT", "OFFSET"]);
}

#[test]
fn comma_joined_from_tables() {
    let catalog = Setup::standard(PG).catalog;
    let names = |sql: &str| parse_query(sql, &catalog).query_tables.iter().map(|t| t.table.clone()).collect::<Vec<_>>();
    let mut both = names("SELECT * FROM users, orders WHERE ");
    both.sort();
    assert_eq!(both, ["orders", "users"]);
    assert_eq!(names("SELECT * FROM users, orders").iter().filter(|t| *t == "users").count(), 1);
    let parsed = parse_query("SELECT * FROM users u, orders o WHERE ", &catalog);
    assert_eq!(parsed.bindings.get("u").map(|b| b.table.as_str()), Some("users"));
    assert_eq!(parsed.bindings.get("o").map(|b| b.table.as_str()), Some("orders"));
    assert!(has_all(&labels("SELECT * FROM users, orders WHERE "), &["users", "orders"]));
    assert_eq!(names("SELECT * FROM users JOIN orders ON "), ["users", "orders"]);
}

#[test]
fn ctes_from_a_leading_with() {
    let catalog = Setup::standard(PG).catalog;
    assert_eq!(parse_query("WITH recent AS (SELECT 1) SELECT * FROM ", &catalog).ctes, ["recent"]);
    assert_eq!(parse_query("WITH a AS (SELECT 1), b AS (SELECT 2) SELECT * FROM ", &catalog).ctes, ["a", "b"]);
    assert!(parse_query("SELECT a AS x FROM users", &catalog).ctes.is_empty());
    assert!(has(&labels("WITH recent AS (SELECT 1) SELECT * FROM "), "recent"));
    assert!(has(&labels("WITH recent AS (SELECT 1) SELECT * FROM rec"), "recent"));
}

#[test]
fn a_cte_survives_the_item_cap_and_ranks_first() {
    let many: Vec<TableInfo> = (0..150).map(|i| table(&format!("tbl_{i}"))).collect();
    let items =
        Setup::new(PG, many, ColumnMap::new()).items("WITH asd AS (SELECT * FROM users LIMIT 10) SELECT * FROM ");
    assert_eq!(items.len(), 100);
    assert_eq!(items[0].label, "asd");
}

#[test]
fn insert_column_lists() {
    assert!(has_all(&column_labels("INSERT INTO users ("), &ALL_COLS));
    assert!(has_all(&column_labels("INSERT INTO public.users ("), &ALL_COLS));
    assert_eq!(column_labels("INSERT INTO users (id, em"), ["email"]);
    assert_eq!(column_labels("INSERT INTO users (id, "), ["email", "name"]);
    assert!(column_labels("INSERT INTO users (id) VALUES (").is_empty());
    let setup = Setup::standard(PG);
    let text = "INSERT INTO users (";
    let needed = bindings_needing_columns(text, &parse_query(text, &setup.catalog), Some(&setup.catalog));
    assert_eq!(needed.iter().map(|b| column_cache_key(&b.schema, &b.table)).collect::<Vec<_>>(), ["public.users"]);
}

#[test]
fn nothing_inside_comments() {
    assert!(labels("SELECT * FROM users -- WHERE ").is_empty());
    assert!(labels("SELECT * FROM users -- sel").is_empty());
    assert!(labels("SELECT * FROM users /* WHERE ").is_empty());
    assert!(has_all(&labels("SELECT * FROM users /* note */ "), &["WHERE", "JOIN"]));
    let setup = Setup::standard(PG);
    let text = "SELECT * FROM users -- WHERE ";
    assert!(bindings_needing_columns(text, &parse_query(text, &setup.catalog), Some(&setup.catalog)).is_empty());
}

#[test]
fn clause_detection_ignores_comments_and_strings() {
    assert!(column_labels("SELECT * FROM users /* WHERE */ ").is_empty());
    assert!(has_all(&labels("SELECT * FROM users WHERE note = 'a = b' "), &["AND", "OR"]));
}

#[test]
fn the_item_cap_keeps_the_best_ranked_matches() {
    let mut many: Vec<TableInfo> = (0..150).map(|i| table(&format!("also_tbl_{i}"))).collect();
    many.push(table("tbl_exact"));
    let items = Setup::new(PG, many, ColumnMap::new()).items("SELECT * FROM tbl");
    assert_eq!(items.len(), 100);
    assert_eq!(items[0].label, "tbl_exact");
}

const WHERE_COL: &str = "SELECT * FROM users WHERE name ";

#[test]
fn filter_operators_per_dialect() {
    let (pg, my, lite) =
        (labels_in(WHERE_COL, PG), labels_in(WHERE_COL, DriverType::MySql), labels_in(WHERE_COL, DriverType::Sqlite));
    for l in [&pg, &my, &lite] {
        assert!(has_all(l, &["LIKE", "NOT LIKE", "NOT IN", "NOT BETWEEN"]));
    }
    assert!(has_all(&pg, &["ILIKE", "NOT ILIKE", "SIMILAR TO", "IS DISTINCT FROM", "IS NOT DISTINCT FROM"]));
    assert!(!has_any(&my, &["ILIKE", "GLOB", "MATCH", "IS DISTINCT FROM"]));
    assert!(!has_any(&lite, &["ILIKE", "REGEXP", "IS DISTINCT FROM"]));
    assert!(has_all(&my, &["REGEXP", "RLIKE"]));
    assert!(has_all(&lite, &["GLOB", "MATCH"]));
    assert!(!has_any(&pg, &["REGEXP", "GLOB", "MATCH"]));
    assert!(has(&labels("SELECT * FROM users WHERE name ILI"), "ILIKE"));
    assert!(has(&labels("SELECT * FROM users WHERE name NOT L"), "NOT LIKE"));
    assert!(has(&labels("SELECT * FROM users u JOIN orders o ON u.id "), "ILIKE"));
    assert!(has(&labels("SELECT * FROM users GROUP BY name HAVING name "), "ILIKE"));
    assert!(!has_any(&labels("SELECT * FROM users "), &["ILIKE", "NOT LIKE"]));
    assert!(has_all(&column_labels("SELECT * FROM users WHERE name ILIKE "), &ALL_COLS));
    assert!(has_all(&column_labels("SELECT * FROM users WHERE name SIMILAR TO "), &ALL_COLS));
}

#[test]
fn statement_keywords_per_driver() {
    let pg = labels_in("", PG);
    assert!(has_all(&pg, &["TRUNCATE TABLE", "VACUUM", "EXPLAIN"]));
    assert!(!has_any(&pg, &["REPLACE INTO", "PRAGMA", "SHOW TABLES"]));
    let my = labels_in("", DriverType::MySql);
    assert!(has_all(&my, &["TRUNCATE TABLE", "REPLACE INTO", "SHOW TABLES", "SHOW DATABASES"]));
    assert!(!has_any(&my, &["PRAGMA", "VACUUM"]));
    let lite = labels_in("", DriverType::Sqlite);
    assert!(has_all(&lite, &["PRAGMA", "VACUUM", "REPLACE INTO", "EXPLAIN"]));
    assert!(!has_any(&lite, &["TRUNCATE TABLE", "SHOW TABLES"]));

    assert!(!has_any(&labels_in("SELECT * FROM users ", DriverType::Sqlite), &["PRAGMA", "VACUUM"]));
    assert!(!has(&labels_in("SELECT * FROM users ", DriverType::MySql), "SHOW TABLES"));
    assert!(!has(&labels("SELECT * FROM users "), "EXPLAIN"));

    assert!(has_all(&labels("SELECT * FROM users "), &["FULL JOIN", "CROSS JOIN"]));
    assert!(has(&labels_in("SELECT * FROM users ", DriverType::Sqlite), "FULL JOIN"));
    assert!(!has(&labels_in("SELECT * FROM users ", DriverType::MySql), "FULL JOIN"));
    assert!(has(&labels_in("SELECT * FROM users ", DriverType::MySql), "CROSS JOIN"));
}

#[test]
fn write_statement_tails_per_driver() {
    for sql in ["DELETE FROM users ", "UPDATE users SET name = 1 ", "INSERT INTO users (id) VALUES (1) "] {
        assert!(has(&labels_in(sql, PG), "RETURNING"), "{sql}");
        assert!(has(&labels_in(sql, DriverType::Sqlite), "RETURNING"), "{sql}");
        assert!(!has(&labels_in(sql, DriverType::MySql), "RETURNING"), "{sql}");
    }
    assert!(!has(&labels("SELECT * FROM users "), "RETURNING"));
    let insert = "INSERT INTO users (id) VALUES (1) ";
    assert!(has(&labels_in(insert, PG), "ON CONFLICT"));
    assert!(has(&labels_in(insert, DriverType::Sqlite), "ON CONFLICT"));
    assert!(has(&labels_in(insert, DriverType::MySql), "ON DUPLICATE KEY UPDATE"));
    assert!(!has(&labels_in(insert, DriverType::MySql), "ON CONFLICT"));
    assert!(!has(&labels_in(insert, PG), "ON DUPLICATE KEY UPDATE"));
    assert!(!has(&labels("INSERT INTO users "), "ON CONFLICT"));
    assert!(has(&labels_in("UPDATE users SET name = ", PG), "DEFAULT"));
    assert!(has(&labels_in("UPDATE users SET name = ", DriverType::MySql), "DEFAULT"));
    assert!(!has(&labels_in("UPDATE users SET name = ", DriverType::Sqlite), "DEFAULT"));
}

#[test]
fn is_distinct_from_is_not_a_table_slot() {
    let spaced = "SELECT * FROM users WHERE id IS DISTINCT FROM ";
    assert!(has_all(&column_labels(spaced), &ALL_COLS));
    assert!(!has(&labels(spaced), "orders"));
    let butted = "SELECT * FROM users WHERE id IS DISTINCT FROM";
    assert!(has_all(&column_labels(butted), &ALL_COLS));
    assert_eq!(insert_for(butted, "email").as_deref(), Some(" email"));
    assert!(!has(&labels(butted), "orders"));
    assert!(has_all(&labels("SELECT DISTINCT id FROM "), &["users", "orders"]));
}

fn fk_setup() -> Setup {
    let user_id = ColumnInfo {
        foreign_table: "users".into(),
        foreign_column: "id".into(),
        is_foreign: true,
        ..col("user_id", "int", false, false)
    };
    Setup::new(
        PG,
        tables(&["users", "orders"]),
        columns(&[
            ("public.users", vec![col("id", "int", false, true)]),
            ("public.orders", vec![col("id", "int", false, true), user_id]),
        ]),
    )
}

#[test]
fn foreign_keys_suggest_join_conditions_after_on() {
    let items = fk_setup().loaded_items("SELECT * FROM users JOIN orders ON ");
    assert_eq!(
        (items[0].label.as_str(), items[0].detail.as_deref()),
        ("orders.user_id = users.id", Some("foreign key"))
    );
    assert_eq!(fk_setup().loaded_items("SELECT * FROM users u JOIN orders o ON ")[0].label, "o.user_id = u.id");
    assert_eq!(
        fk_setup().loaded_items("SELECT * FROM users JOIN orders ON")[0].insert_text,
        " orders.user_id = users.id"
    );
    let away = fk_setup().loaded_items("SELECT * FROM users JOIN orders ON orders.user_id = users.id WHERE ");
    assert!(!away.iter().any(|i| i.label.contains('=')));
}

#[test]
fn cte_and_derived_table_projections() {
    let catalog = Setup::standard(PG).catalog;
    let parsed = parse_query(
        "WITH recent AS (SELECT u.id, email AS mail, count(*) AS n FROM users u) SELECT * FROM recent",
        &catalog,
    );
    assert_eq!(parsed.virtual_columns.get("recent").unwrap(), &["id", "mail", "n"]);
    let explicit = parse_query("WITH r (a, b) AS (SELECT 1, 2) SELECT * FROM r", &catalog);
    assert_eq!(explicit.virtual_columns.get("r").unwrap(), &["a", "b"]);

    assert_eq!(
        labels("WITH recent AS (SELECT id, email AS mail FROM users) SELECT * FROM recent WHERE recent."),
        ["id", "mail"]
    );
    assert!(has(&labels("WITH recent AS (SELECT id, email AS mail FROM users) SELECT * FROM recent WHERE "), "mail"));
    assert!(!has(&labels("WITH recent AS (SELECT email AS mail FROM users) SELECT * FROM orders WHERE "), "mail"));

    let derived = "SELECT * FROM (SELECT id, name AS label FROM users) sub WHERE sub.";
    assert_eq!(labels(derived), ["id", "label"]);
    assert_eq!(parse_query(derived, &catalog).virtual_columns.get("sub").unwrap(), &["id", "label"]);
    let listed = parse_query("SELECT * FROM (SELECT 1 AS x) sub, orders WHERE ", &catalog);
    assert!(listed.query_tables.iter().any(|t| t.table == "orders"));
    assert_eq!(listed.virtual_columns.get("sub").unwrap(), &["x"]);
    assert!(labels("WITH r AS (SELECT * FROM users) SELECT * FROM r WHERE r.").is_empty());

    let sql = "WITH recent AS (SELECT id FROM users) SELECT * FROM recent WHERE ";
    let needed = bindings_needing_columns(sql, &parse_query(sql, &catalog), Some(&catalog));
    assert!(!needed.iter().any(|b| b.table == "recent"));
}

#[test]
fn column_details_carry_key_and_null_hints() {
    let items = complete("SELECT * FROM users WHERE ", PG);
    let detail = |l: &str| items.iter().find(|i| i.label == l).and_then(|i| i.detail.clone());
    assert_eq!(detail("id").as_deref(), Some("int · PK"));
    assert_eq!(detail("email").as_deref(), Some("text · not null"));
    assert_eq!(detail("name").as_deref(), Some("text"));
}

fn ambiguous_labels(text: &str) -> Vec<String> {
    let setup = Setup::new(
        PG,
        tables(&["users", "orders"]),
        columns(&[
            ("public.users", vec![col("id", "int", false, true), col("email", "text", false, false)]),
            ("public.orders", vec![col("id", "int", false, true), col("total", "numeric", true, false)]),
        ]),
    );
    setup.items(text).into_iter().map(|i| i.label).collect()
}

#[test]
fn ambiguous_columns_are_offered_qualified() {
    let labels = ambiguous_labels("SELECT * FROM users u JOIN orders o ON u.id = o.id WHERE ");
    assert!(has_all(&labels, &["u.id", "o.id", "email", "total"]) && !has(&labels, "id"), "{labels:?}");
    let labels = ambiguous_labels("SELECT * FROM users JOIN orders ON ");
    assert!(has_all(&labels, &["users.id", "orders.id"]) && !has(&labels, "id"), "{labels:?}");
    let labels = ambiguous_labels("SELECT * FROM users a JOIN users b ON ");
    assert!(
        has_all(&labels, &["a.id", "b.id", "a.email", "b.email"]) && !has_any(&labels, &["id", "email"]),
        "{labels:?}"
    );
    assert!(has_all(&column_labels("SELECT * FROM users WHERE "), &ALL_COLS));
}

#[test]
fn qualified_inserts_quote_the_qualifier() {
    let shared = vec![col("id", "int", false, true)];
    let setup = Setup::new(
        PG,
        tables(&["Users", "orders"]),
        columns(&[("public.Users", shared.clone()), ("public.orders", shared)]),
    );
    let items = setup.items("SELECT * FROM \"Users\" JOIN orders ON ");
    let insert = |l: &str| items.iter().find(|i| i.label == l).map(|i| i.insert_text.clone());
    assert_eq!(insert("Users.id").as_deref(), Some("\"Users\".id"));
    assert_eq!(insert("orders.id").as_deref(), Some("orders.id"));
}

#[test]
fn the_select_list_offers_the_statements_table_refs() {
    let text = "SELECT  FROM users u";
    let labels: Vec<String> =
        Setup::standard(PG).items_at(text, "SELECT ".len(), 0).into_iter().map(|i| i.label).collect();
    assert!(has_all(&labels, &["u", "users", "id", "email", "name"]), "{labels:?}");
}

#[test]
fn keywords_are_offered_by_position() {
    assert!(has(&labels("SEL"), "SELECT"));
    assert!(!has_any(&labels("SELECT * FROM users "), &["SELECT", "INSERT INTO", "CREATE TABLE", "UPDATE", "DELETE"]));
    assert!(!has(&labels("SELECT * FROM users "), "ON"));
    assert!(has(&labels("SELECT * FROM users JOIN orders "), "ON"));
    assert!(!has_any(&labels("SELECT * FROM users "), &["SET", "VALUES"]));
}

#[test]
fn multi_byte_text_uses_byte_offsets() {
    let text = "SELECT 'ä' AS x FROM users WHERE em";
    assert_eq!(column_labels(text), ["email"]);
    let range = completion_replace_range(text.len(), text, text.len()..text.len(), Some(&PG));
    assert_eq!(&text[range], "em");
}
