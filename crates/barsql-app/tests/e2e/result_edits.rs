use std::time::Duration;

use barsql_app::ResultEdits;
use barsql_core::schema::primary_keys;
use barsql_core::{DriverType, RowDelete, RowUpdate, Value};
use barsql_db::Cancel;

use crate::harness::{row, unique_table};
use crate::support::{meta, rows};

each_engine!(async fn result_edits(e) {
    require!(e, row_editing);
    let table = unique_table("result_edit");
    e.create_temp_table(&e.auto_pk_table(&table), &table).await;
    let target = e.qualified(&table);
    e.exec(&format!("INSERT INTO {target} (name) VALUES ('ann')")).await;
    e.exec(&format!("INSERT INTO {target} (name) VALUES ('bob')")).await;
    e.exec(&format!("INSERT INTO {target} (name) VALUES ('cy')")).await;
    e.exec(&format!("INSERT INTO {target} (name) VALUES ('dee')")).await;
    let tab = format!("tab-{table}");
    let sql = format!("SELECT id, name AS n, upper(name) AS shout FROM {target} ORDER BY id");
    let events = e.run_on_tab(&tab, &sql).await;
    let found = e.app.result_table(&e.id, &tab, &sql, &meta(&events, 0), &Cancel::new()).await.unwrap();
    let found = found.expect("one table");
    assert!(found.table.eq_ignore_ascii_case(&table), "{} for {table}", found.table);
    assert_eq!(found.columns, [Some("id".into()), Some("name".into()), None]);
    assert_eq!(primary_keys(&found.catalog), ["id"]);
    let sql_server = e.driver() == DriverType::SqlServer;
    if sql_server {
        // Inside a transaction a pooled session describes it, which is in the tab's database here.
        e.app.begin_transaction(&e.id, &tab).await.unwrap();
        let traced = e.app.result_table(&e.id, &tab, &sql, &meta(&events, 0), &Cancel::new()).await.unwrap();
        assert_eq!(traced.map(|traced| traced.columns), Some(found.columns.clone()));
        e.app.rollback_transaction(&tab).await.unwrap();
    }

    let ids = e.query(&format!("SELECT id FROM {target} ORDER BY id")).await.rows;
    let key = |ix: usize| row(&[("id", ids[ix][0].clone())]);
    let edits = ResultEdits {
        // Two rows and one that isn't there, all in one statement.
        delete: RowDelete {
            schema: found.schema.clone(),
            table: found.table.clone(),
            primary_keys: vec![key(1), key(2), row(&[("id", Value::Int(-1))])],
        },
        // The same change to two rows, also one statement.
        updates: [0, 3]
            .map(|ix| RowUpdate {
                schema: found.schema.clone(),
                table: found.table.clone(),
                primary_key: key(ix),
                changes: row(&[("name", Value::from("x"))]),
            })
            .to_vec(),
        columns: found.catalog.clone(),
        database: found.database.clone(),
        fingerprint: found.fingerprint.clone(),
    };
    // Renaming a row that's gone fails after the delete ran, which rolls back with it.
    let gone = RowUpdate { primary_key: row(&[("id", Value::Int(-1))]), ..edits.updates[0].clone() };
    let failed = e.app.apply_result_edits(&e.id, &tab, &ResultEdits { updates: vec![gone], ..edits.clone() }).await;
    assert!(failed.unwrap_err().ran);
    let names = e.query(&format!("SELECT name FROM {target} ORDER BY id")).await.rows;
    assert_eq!(names, ["ann", "bob", "cy", "dee"].map(|name| [Value::from(name)]));
    // Settings a script left on the tab's session neither hide nor cap the counts, and stay as they were.
    if sql_server {
        e.run_on_tab(&tab, "SET NOCOUNT ON").await;
        e.run_on_tab(&tab, "SET ROWCOUNT 1").await;
    }
    assert_eq!(e.app.apply_result_edits(&e.id, &tab, &edits).await.unwrap(), 1, "-1 was never there");
    let names = e.query(&format!("SELECT name FROM {target} ORDER BY id")).await.rows;
    assert_eq!(names, [[Value::from("x")], [Value::from("x")]]);
    if sql_server {
        let options = e.run_on_tab(&tab, "SELECT @@OPTIONS & 512").await;
        assert_eq!(rows(&options, 0), [[Some("512".to_string())]], "NOCOUNT is still on");
        e.run_on_tab(&tab, "SET ROWCOUNT 0").await;
    }
    // SQL Server traces a result only on the session it came from, which the idle sweep closes.
    e.app.release_idle_tabs(Duration::ZERO);
    let late = e.app.result_table(&e.id, &tab, &sql, &meta(&events, 0), &Cancel::new()).await;
    assert_eq!(late.is_err(), sql_server, "{late:?}");

    // A view stays read-only, as in a table view.
    let view = format!("{table}_v");
    e.exec(&format!("CREATE VIEW {} AS SELECT * FROM {target}", e.qualified(&view))).await;
    e.defer(format!("DROP VIEW IF EXISTS {}", e.qualified(&view)));
    let sql = format!("SELECT * FROM {}", e.qualified(&view));
    let events = e.run_on_tab(&tab, &sql).await;
    let found = e.app.result_table(&e.id, &tab, &sql, &meta(&events, 0), &Cancel::new()).await.unwrap();
    assert!(found.as_ref().is_none_or(|found| primary_keys(&found.catalog).is_empty()), "{found:?}");
    e.app.cleanup_tab(&tab).await;
});
