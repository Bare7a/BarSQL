use barsql_core::{Row, RowDelete, RowUpdate, TableDataRequest, Value};

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
    // ClickHouse keys aren't unique, so none is reported and the grid stays read-only.
    let keys: &[&str] = if e.driver().capabilities().row_editing { &["id"] } else { &[] };
    assert_eq!(all.summary.primary_keys, keys);

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
    require!(e, row_editing);
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

// last_insert_id is the AUTO_INCREMENT column's, not the key's first, and the older row's tenant equals the new id.
each_engine!(async fn an_insert_finds_its_row_by_the_auto_increment_column(e) {
    if !e.is_mysql() {
        return;
    }
    let table = unique_table("tenants");
    let t = e.qualified(&table);
    let ddl = format!(
        "CREATE TABLE {t} (tenant INT NOT NULL, id INT NOT NULL AUTO_INCREMENT, name VARCHAR(20), \
         PRIMARY KEY (tenant, id), KEY (id))"
    );
    e.create_temp_table(&ddl, &table).await;
    e.exec(&format!("INSERT INTO {t} (tenant, id, name) VALUES (2, 1, 'old')")).await;
    let values = row(&[("tenant", Value::Int(7)), ("name", Value::from("new"))]);
    let inserted = e.app.insert_row(&e.id, &e.schema(), &table, &values).await.unwrap();
    assert_eq!(inserted, row(&[("tenant", Value::Int(7)), ("id", Value::Int(2)), ("name", Value::from("new"))]));
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

each_engine!(async fn bit_cells_save_the_digits_they_show(e) {
    if !e.is_mysql() {
        return;
    }
    let table = unique_table("bits");
    let t = e.qualified(&table);
    let ddl = format!("CREATE TABLE {t} (id INT AUTO_INCREMENT PRIMARY KEY, b4 BIT(4), b64 BIT(64))");
    e.create_temp_table(&ddl, &table).await;
    e.exec(&format!("INSERT INTO {t} (b4, b64) VALUES (b'0001', b'1')")).await;
    let select = format!("SELECT b4, b64, BIN(b4), HEX(b64) FROM {t} ORDER BY id");
    let (ones, top) = ("1".repeat(64), format!("1{}", "0".repeat(63)));
    let edit = |changes: Row| RowUpdate {
        schema: e.schema(),
        table: table.clone(),
        primary_key: row(&[("id", Value::Int(1))]),
        changes,
    };

    let changes = row(&[("b4", "1010".into()), ("b64", ones.as_str().into())]);
    e.app.update_row(&e.id, &edit(changes)).await.unwrap();
    let values = row(&[("b4", "0101".into()), ("b64", top.as_str().into())]);
    let inserted = e.app.insert_row(&e.id, &e.schema(), &table, &values).await.unwrap();
    assert_eq!([&inserted["b4"], &inserted["b64"]], [&values["b4"], &values["b64"]], "the stored row comes back");
    let want = [
        ["1010", ones.as_str(), "1010", "FFFFFFFFFFFFFFFF"].map(Value::from),
        ["0101", top.as_str(), "101", "8000000000000000"].map(Value::from),
    ];
    assert_eq!(e.query(&select).await.rows, want);

    let err = e.app.update_row(&e.id, &edit(row(&[("b4", "12".into())]))).await.unwrap_err();
    assert_eq!(err.message, r#"column "b4" takes binary digits like 1010, got "12""#);
    let err = e.app.update_row(&e.id, &edit(row(&[("b4", "10000".into())]))).await.unwrap_err();
    assert_eq!(err.code, "1406", "data too long: {err:?}");
    assert_eq!(e.query(&select).await.rows, want, "a refused edit changes nothing");

    let keyed = unique_table("bit_keys");
    let k = e.qualified(&keyed);
    e.create_temp_table(&format!("CREATE TABLE {k} (k BIT(8) PRIMARY KEY, note VARCHAR(10))"), &keyed).await;
    e.exec(&format!("INSERT INTO {k} VALUES (b'10000001', 'a'), (b'00000001', 'b')")).await;
    let key = || row(&[("k", Value::from("10000001"))]);
    let changes = row(&[("note", "c".into())]);
    let update = RowUpdate { schema: e.schema(), table: keyed.clone(), primary_key: key(), changes };
    e.app.update_row(&e.id, &update).await.unwrap();
    let notes = format!("SELECT k, note FROM {k} ORDER BY k");
    assert_eq!(e.query(&notes).await.rows, [["00000001", "b"].map(Value::from), ["10000001", "c"].map(Value::from)]);
    let delete = RowDelete { schema: e.schema(), table: keyed.clone(), primary_keys: vec![key()] };
    assert_eq!(e.app.delete_rows(&e.id, &delete).await, (1, None));
    assert_eq!(e.query(&notes).await.rows, [["00000001", "b"].map(Value::from)]);
});
