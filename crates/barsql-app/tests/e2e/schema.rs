use std::collections::HashMap;

use barsql_core::{ColumnInfo, ObjectKind, ObjectRef, TableInfo};
use barsql_sql::split_statement_texts;

use crate::harness::{E2e, unique_table};

fn by_name(columns: Vec<ColumnInfo>) -> HashMap<String, ColumnInfo> {
    columns.into_iter().map(|c| (c.name.clone(), c)).collect()
}

fn table_names(tables: &[TableInfo]) -> Vec<&str> {
    tables.iter().map(|t| t.name.as_str()).collect()
}

// The tree, editable-grid gating and completion all read this metadata.
each_engine!(async fn schema_explorer(e) {
    let parent = unique_table("authors");
    let child = unique_table("books");
    e.create_temp_table(&e.auto_pk_table(&parent), &parent).await;
    let child_ddl = format!(
        "CREATE TABLE {} ({}, isbn VARCHAR(32) NOT NULL, author_id INT, FOREIGN KEY (author_id) REFERENCES {}(id))",
        e.qualified(&child),
        e.pk_column(),
        e.qualified(&parent)
    );
    e.create_temp_table(&child_ddl, &child).await;

    let bundle = e.app.load_schema_data(&e.id).await.unwrap();
    assert!(bundle.status.connected, "{:?}", bundle.status);
    assert!(bundle.schemas.iter().any(|s| s.name == e.schema()), "{:?}", bundle.schemas);
    let preloaded = bundle.loaded_tables.iter().any(|st| st.schema == e.schema() && st.tables.iter().any(|t| t.name == parent));
    assert!(preloaded, "the browse schema's tables are preloaded: {:?}", bundle.loaded_tables);

    let tables = e.app.list_tables(&e.id, &e.schema()).await.unwrap();
    assert!(tables.iter().any(|t| t.name == parent) && tables.iter().any(|t| t.name == child), "{:?}", table_names(&tables));
    assert!(tables.iter().filter(|t| t.name == parent).all(|t| t.kind == "table"), "{tables:?}");

    let columns = by_name(e.app.list_columns(&e.id, &e.schema(), &child).await.unwrap());
    // ClickHouse has neither unique keys nor foreign keys.
    if !e.is_clickhouse() {
        let id = &columns["id"];
        assert!(id.is_primary && !id.is_foreign, "{id:?}");
        let fk = &columns["author_id"];
        assert!(fk.is_foreign && !fk.is_primary && fk.is_nullable, "{fk:?}");
    }
    let isbn = &columns["isbn"];
    assert!(!isbn.is_nullable && !isbn.data_type.is_empty(), "{isbn:?}");

    let missing = e.app.list_columns(&e.id, &e.schema(), &format!("no_such_table_{child}")).await.unwrap();
    assert!(missing.is_empty(), "{missing:?}");
});

each_engine!(async fn views(e) {
    let base = unique_table("v_base");
    let view = unique_table("v_view");
    e.create_temp_table(&e.auto_pk_table(&base), &base).await;
    e.exec(&format!("INSERT INTO {} (name) VALUES ('x')", e.qualified(&base))).await;
    e.exec(&format!("CREATE VIEW {} AS SELECT id, name FROM {}", e.qualified(&view), e.qualified(&base))).await;
    e.defer(format!("DROP VIEW IF EXISTS {}", e.qualified(&view)));
    let tables = e.app.list_tables(&e.id, &e.schema()).await.unwrap();
    let found = tables.iter().find(|t| t.name == view).unwrap_or_else(|| panic!("{view} not in {:?}", table_names(&tables)));
    assert_eq!(found.kind, "view");
});

// None on SQLite, which has no routines.
fn create_function_sql(e: &E2e, name: &str) -> Option<String> {
    if e.is_lite() {
        return None;
    }
    Some(create_routine_sql(e, name))
}

fn create_routine_sql(e: &E2e, name: &str) -> String {
    if e.is_postgres() {
        format!("CREATE FUNCTION {name}(a int) RETURNS int LANGUAGE sql AS $$ SELECT a + 1 $$")
    } else if e.is_sqlserver() {
        format!("CREATE FUNCTION {name}(@a INT) RETURNS INT AS BEGIN RETURN @a + 1 END")
    } else {
        format!("CREATE FUNCTION {name}(a INT) RETURNS INT DETERMINISTIC RETURN a + 1")
    }
}

fn create_trigger_sql(e: &E2e, trigger: &str, table: &str, helper: &str) -> Vec<String> {
    let target = e.qualified(table);
    if e.is_lite() {
        return vec![format!("CREATE TRIGGER {trigger} AFTER UPDATE ON {target} FOR EACH ROW BEGIN SELECT 1; END")];
    }
    if e.is_postgres() {
        return vec![
            format!("CREATE FUNCTION {helper}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$"),
            format!("CREATE TRIGGER {trigger} AFTER UPDATE ON {target} FOR EACH ROW EXECUTE FUNCTION {helper}()"),
        ];
    }
    if e.is_sqlserver() {
        return vec![format!("CREATE TRIGGER {trigger} ON {target} AFTER UPDATE AS SET NOCOUNT ON")];
    }
    vec![format!("CREATE TRIGGER {trigger} AFTER UPDATE ON {target} FOR EACH ROW SET @barsql_e2e = 1")]
}

fn object(e: &E2e, kind: ObjectKind, name: &str, table: &str) -> ObjectRef {
    ObjectRef { kind, schema: e.schema(), name: name.into(), table: table.into(), ..Default::default() }
}

// Table DDL is checked by round trip. Drop the table, replay the statements and compare the shape. ClickHouse's
// DDL comes from SHOW CREATE and is pinned by its schema golden instead.
each_engine!(async fn object_ddl(e) {
    if e.is_clickhouse() {
        return;
    }
    let parent = unique_table("ddl_orgs");
    let child = unique_table("ddl_users");
    let index = format!("{child}_nickname_idx");
    e.create_temp_table(&e.auto_pk_table(&parent), &parent).await;
    let text = if e.is_postgres() { "TEXT" } else { "VARCHAR(255)" };
    let child_ddl = format!(
        "CREATE TABLE {} ({}, email {text} NOT NULL UNIQUE, nickname {text}, org_id INT, \
         CONSTRAINT {child}_org_fk FOREIGN KEY (org_id) REFERENCES {}(id))",
        e.qualified(&child),
        e.pk_column(),
        e.qualified(&parent)
    );
    e.create_temp_table(&child_ddl, &child).await;
    e.exec(&format!("CREATE INDEX {index} ON {} (nickname)", e.qualified(&child))).await;

    let indexes = e.app.list_indexes(&e.id, &e.schema(), &child).await.unwrap();
    let idx = indexes.iter().find(|i| i.name == index).unwrap_or_else(|| panic!("{index} missing from {indexes:?}"));
    assert!(!idx.is_unique && !idx.is_primary, "a plain index: {idx:?}");
    assert_eq!(idx.columns, ["nickname"]);
    // A rowid table keeps its INTEGER PRIMARY KEY without an index.
    assert!(e.is_lite() || indexes.iter().any(|i| i.is_primary), "a primary-key index: {indexes:?}");

    let constraints = e.app.list_constraints(&e.id, &e.schema(), &child).await.unwrap();
    let of_kind = |kind: &str| constraints.iter().find(|c| c.kind == kind);
    let pk = of_kind("PRIMARY KEY").unwrap_or_else(|| panic!("no PRIMARY KEY among {constraints:?}"));
    assert_eq!(pk.columns, ["id"]);
    assert!(of_kind("UNIQUE").is_some(), "no UNIQUE among {constraints:?}");
    let fk = of_kind("FOREIGN KEY").unwrap_or_else(|| panic!("no FOREIGN KEY among {constraints:?}"));
    assert_eq!(fk.ref_table, parent);
    assert_eq!(fk.columns, ["org_id"]);

    let trigger = unique_table("ddl_trg");
    let helper = unique_table("ddl_trgfn");
    for stmt in create_trigger_sql(&e, &trigger, &child, &helper) {
        e.exec(&stmt).await;
    }
    let drop_helper = format!("DROP FUNCTION IF EXISTS {helper}() CASCADE");
    if e.is_postgres() {
        e.defer(drop_helper.clone());
    }
    let triggers = e.app.list_triggers(&e.id, &e.schema(), &child).await.unwrap();
    let found = triggers.iter().find(|t| t.name == trigger).unwrap_or_else(|| panic!("{trigger} missing from {triggers:?}"));
    assert_eq!(found.timing, "AFTER");
    assert!(found.events.contains("UPDATE"), "{found:?}");
    let ddl = e.app.object_ddl(&e.id, &object(&e, ObjectKind::Trigger, &trigger, &child)).await.unwrap();
    // MySQL puts DEFINER=... between the two words.
    let upper = ddl.to_uppercase();
    assert!(upper.starts_with("CREATE") && upper.contains("TRIGGER") && ddl.contains(&trigger), "{ddl}");
    if e.is_postgres() {
        e.exec(&drop_helper).await;
    }

    let function = unique_table("ddl_fn");
    if let Some(create) = create_function_sql(&e, &function) {
        e.exec(&create).await;
        let drop_function = format!("DROP FUNCTION IF EXISTS {function}{}", if e.is_postgres() { "(int)" } else { "" });
        e.defer(drop_function.clone());
        let routines = e.app.list_routines(&e.id, &e.schema()).await.unwrap();
        let found = routines.iter().find(|r| r.name == function).unwrap_or_else(|| panic!("{function} missing"));
        assert_eq!(found.kind, ObjectKind::Function);
        let routine = ObjectRef { args: found.args.clone(), ..object(&e, ObjectKind::Function, &function, "") };
        let ddl = e.app.object_ddl(&e.id, &routine).await.unwrap();
        assert!(ddl.to_uppercase().contains("FUNCTION"), "{ddl}");
        e.exec(&drop_function).await;
    }

    let ddl = e.app.object_ddl(&e.id, &object(&e, ObjectKind::Index, &index, &child)).await.unwrap();
    assert!(ddl.to_uppercase().contains("CREATE INDEX") && ddl.contains("nickname"), "{ddl}");

    let constraint = object(&e, ObjectKind::Constraint, &format!("{child}_org_fk"), &child);
    let ddl = e.app.object_ddl(&e.id, &constraint).await.unwrap().to_uppercase();
    // SQLite's constraints live in the CREATE TABLE.
    let carrier = if e.is_lite() { "CREATE TABLE" } else { "ALTER TABLE" };
    assert!(ddl.contains(carrier) && ddl.contains("FOREIGN KEY"), "{ddl}");

    let view = unique_table("ddl_view");
    e.exec(&format!("CREATE VIEW {} AS SELECT id, email FROM {}", e.qualified(&view), e.qualified(&child))).await;
    e.defer(format!("DROP VIEW IF EXISTS {}", e.qualified(&view)));
    let ddl = e.app.object_ddl(&e.id, &object(&e, ObjectKind::View, &view, "")).await.unwrap();
    assert!(ddl.to_uppercase().contains("VIEW"), "{ddl}");
    e.exec(&format!("DROP VIEW {}", e.qualified(&view))).await;

    let ddl = e.app.object_ddl(&e.id, &object(&e, ObjectKind::Table, &child, "")).await.unwrap();
    assert!(ddl.to_uppercase().contains("CREATE TABLE"), "{ddl}");
    let before = e.app.list_columns(&e.id, &e.schema(), &child).await.unwrap();
    e.exec(&format!("DROP TABLE {}", e.qualified(&child))).await;
    for stmt in split_statement_texts(&e.driver(), &ddl) {
        if let Err(err) = e.try_query(&stmt).await {
            panic!("replaying the generated DDL failed on {stmt:?}: {err:?}\n{ddl}");
        }
    }
    let after = e.app.list_columns(&e.id, &e.schema(), &child).await.unwrap();
    let shape = |cols: &[ColumnInfo]| cols.iter().map(|c| (c.name.clone(), c.is_primary, c.is_nullable)).collect::<Vec<_>>();
    assert_eq!(shape(&after), shape(&before), "the round trip changed the columns:\n{ddl}");
    let indexes = e.app.list_indexes(&e.id, &e.schema(), &child).await.unwrap();
    assert!(indexes.iter().any(|i| i.name == index), "{index} was lost in the round trip:\n{ddl}");
});
