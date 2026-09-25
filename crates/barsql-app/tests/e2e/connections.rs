use barsql_core::{ConnectionConfig, RowDelete, RowUpdate, Value};

use crate::harness::{row, run_error, unique_table};
use crate::support::rows;

each_engine!(async fn connectivity(e) {
    let status = e.app.connection_status(&e.id).await.unwrap();
    assert!(status.connected, "{status:?}");
});

each_engine!(async fn test_connection(e) {
    e.app.test_connection(e.kind.config()).await.unwrap();
    let mut bad = e.kind.config();
    bad.password = "definitely-the-wrong-password".into();
    assert!(e.app.test_connection(bad).await.is_err(), "a wrong password must fail");
    let mut dead = e.kind.config();
    dead.port = 1;
    assert!(e.app.test_connection(dead).await.is_err(), "a dead port must fail");
});

each_engine!(async fn connect_disconnect(e) {
    let id = e.app.save_connection(e.kind.config()).await.unwrap().id;
    e.app.connect(&id).await.unwrap();
    assert!(e.app.is_connected(&id));
    let status = e.app.connection_status(&id).await.unwrap();
    assert!(status.connected && !status.database.is_empty() && !status.user.is_empty(), "{status:?}");
    e.app.disconnect(&id).await;
    assert!(!e.app.is_connected(&id));
});

// Writes are blocked at the app gate and again in the session. Reads still work.
each_engine!(async fn read_only_connection(e) {
    let table = unique_table("ro");
    e.create_temp_table(&e.auto_pk_table(&table), &table).await;
    let t = e.qualified(&table);
    e.exec(&format!("INSERT INTO {t} (name) VALUES ('seed')")).await;

    let mut cfg = e.kind.config();
    cfg.name = format!("{}-readonly", e.kind.name());
    cfg.read_only = true;
    let ro = e.app.save_connection(cfg).await.unwrap().id;
    e.app.connect(&ro).await.unwrap();

    let read = e.app.execute_query(&ro, &format!("SELECT name FROM {t}")).await;
    assert!(read.is_ok(), "{read:?}");
    for write in [
        format!("INSERT INTO {t} (name) VALUES ('nope')"),
        format!("UPDATE {t} SET name = 'x'"),
        format!("DELETE FROM {t}"),
        format!("DROP TABLE {t}"),
    ] {
        assert!(e.app.execute_query(&ro, &write).await.is_err(), "a write on a read-only connection: {write:?}");
    }

    let update = RowUpdate {
        schema: e.schema(),
        table: table.clone(),
        primary_key: row(&[("id", Value::Int(1))]),
        changes: row(&[("name", Value::from("x"))]),
    };
    assert!(e.app.update_row(&ro, &update).await.is_err());
    assert!(e.app.insert_row(&ro, &e.schema(), &table, &row(&[("name", Value::from("x"))])).await.is_err());
    let delete = RowDelete { schema: e.schema(), table: table.clone(), primary_keys: vec![row(&[("id", Value::Int(1))])] };
    assert!(e.app.delete_rows(&ro, &delete).await.1.is_some());
    assert_eq!(e.count(&table).await, 1, "a read-only write leaked");
});

// A schema switch in one tab must not leak into another.
each_engine!(async fn sessions_use_the_configured_schema(e) {
    let schema = unique_table("cfg_schema");
    let (create, drop, current, switch) = if e.is_postgres() {
        (
            format!("CREATE SCHEMA {schema}"),
            format!("DROP SCHEMA IF EXISTS {schema} CASCADE"),
            "SELECT current_schema()",
            "SET search_path TO information_schema",
        )
    } else {
        (format!("CREATE DATABASE {schema}"), format!("DROP DATABASE IF EXISTS {schema}"), "SELECT DATABASE()", "USE information_schema")
    };
    e.exec(&create).await;
    e.defer(drop);
    let id = e.app.save_connection(ConnectionConfig { schema: schema.clone(), ..e.kind.config() }).await.unwrap().id;
    assert_eq!(e.app.execute_query(&id, current).await.unwrap().rows, [[Value::from(schema.as_str())]]);

    assert!(run_error(&e.run_on(&id, "tab-a", switch).await).is_none());
    assert_eq!(rows(&e.run_on(&id, "tab-a", current).await, 0), [[Some("information_schema".to_string())]]);
    assert_eq!(rows(&e.run_on(&id, "tab-b", current).await, 0), [[Some(schema.clone())]], "tab A's state leaked");
});
