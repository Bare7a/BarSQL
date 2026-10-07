use barsql_sql::system_views::{ViewScope, views};

use crate::harness::unique_table;

// The table views that always find the table, whatever has happened to it.
const ALWAYS_A_ROW: [&str; 4] = ["tableStats", "indexUsage", "tableStatus", "parts"];

// Every view runs on the live server, and still runs once the connection is read-only.
each_engine!(async fn system_views_run(e) {
    let table = unique_table("stats");
    e.create_temp_table(&e.auto_pk_table(&table), &table).await;
    e.exec(&format!("INSERT INTO {} (name) VALUES ('a')", e.qualified(&table))).await;
    let driver = e.driver();
    let all: Vec<_> = views(&driver, ViewScope::Server).chain(views(&driver, ViewScope::Table)).collect();
    for read_only in [false, true] {
        if read_only {
            let mut cfg = e.app.list_connections().into_iter().find(|c| c.id == e.id).unwrap();
            cfg.read_only = true;
            e.app.save_connection(cfg).await.unwrap();
        }
        for view in &all {
            let sql = view.sql(&driver, &e.schema(), &table);
            let res = e.query(&sql).await;
            if view.scope == ViewScope::Table && ALWAYS_A_ROW.contains(&view.id) {
                assert!(res.row_count > 0, "{} found nothing for {table}:\n{sql}", view.id);
            }
        }
    }
});
