use barsql_core::{RowDelete, RowUpdate, TableDataRequest, Value};

use crate::harness::{row, unique_table};

each_engine!(async fn query_table(e) {
    let table = unique_table("browse");
    e.create_temp_table(&e.auto_pk_table(&table), &table).await;
    for name in ["a", "b", "c", "d", "e"] {
        e.exec(&format!("INSERT INTO {} (name) VALUES ('{name}')", e.qualified(&table))).await;
    }
    let req = |limit: i64, offset: i64, order_by: &str, order_dir: &str, filter: &str| TableDataRequest {
        schema: e.schema(),
        table: table.clone(),
        limit,
        offset,
        order_by: order_by.into(),
        order_dir: order_dir.into(),
        filter: filter.into(),
    };

    let all = e.table_page(req(100, 0, "", "", "")).await.unwrap();
    assert_eq!(all.summary.row_count, 5);
    assert_eq!(all.summary.table_name, table);
    assert_eq!(all.summary.primary_keys, ["id"]);

    let page = e.table_page(req(2, 1, "id", "ASC", "")).await.unwrap();
    assert_eq!(page.column(1), ["b", "c"], "offset 1 over a..e ordered by id");

    let sorted = e.table_page(req(100, 0, "name", "DESC", "")).await.unwrap();
    assert_eq!(sorted.column(1).first().map(String::as_str), Some("e"));

    let filtered = e.table_page(req(100, 0, "", "", "name = 'c'")).await.unwrap();
    assert_eq!(filtered.column(1), ["c"]);

    let malicious = e.table_page(req(100, 0, "", "", &format!("1=1; DROP TABLE {table}"))).await;
    assert!(malicious.is_err(), "a filter containing ';' must be rejected");
    assert_eq!(e.count(&table).await, 5);
});

each_engine!(async fn grid_editing(e) {
    let table = unique_table("edit");
    e.create_temp_table(&e.auto_pk_table(&table), &table).await;
    let inserted = e.app.insert_row(&e.id, &e.schema(), &table, &row(&[("name", Value::from("first"))])).await.unwrap();
    assert_eq!(inserted.get("name"), Some(&Value::from("first")));
    let id = inserted.get("id").filter(|v| **v != Value::Null).cloned();
    let id = id.unwrap_or_else(|| panic!("the generated id must come back: {inserted:?}"));

    let update = RowUpdate {
        schema: e.schema(),
        table: table.clone(),
        primary_key: row(&[("id", id.clone())]),
        changes: row(&[("name", Value::from("edited"))]),
    };
    e.app.update_row(&e.id, &update).await.unwrap();
    assert_eq!(e.query(&format!("SELECT name FROM {}", e.qualified(&table))).await.rows, [[Value::from("edited")]]);

    let delete = RowDelete { schema: e.schema(), table: table.clone(), primary_keys: vec![row(&[("id", id)])] };
    let (deleted, error) = e.app.delete_rows(&e.id, &delete).await;
    assert!(error.is_none(), "{error:?}");
    assert_eq!(deleted, 1);
    assert_eq!(e.count(&table).await, 0);
});

// Without a primary key there is no WHERE that addresses a single row.
each_engine!(async fn update_requires_primary_key(e) {
    let table = unique_table("nopk");
    let t = e.qualified(&table);
    e.create_temp_table(&format!("CREATE TABLE {t} (a INT, b VARCHAR(50))"), &table).await;
    e.exec(&format!("INSERT INTO {t} (a, b) VALUES (1, 'x')")).await;
    let update = RowUpdate {
        schema: e.schema(),
        table: table.clone(),
        primary_key: row(&[("a", Value::Int(1))]),
        changes: row(&[("b", Value::from("y"))]),
    };
    assert!(e.app.update_row(&e.id, &update).await.is_err());
});
