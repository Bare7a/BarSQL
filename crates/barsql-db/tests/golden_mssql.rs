#![cfg(feature = "e2e")]

mod common;

use barsql_core::DriverType;
use barsql_db::{Cancel, Engine};
use common::{
    Report, assert_matches, compare_probes, fixture, golden_write, mssql_config, mssql_engine, run_all,
    schema_snapshot, snapshot_probes, strings, write_fixture,
};

const MS: DriverType = DriverType::SqlServer;

// The fixtures were written against Azure SQL Edge, the arm64 stand-in. CI's SQL Server 2022 should agree; where it
// doesn't, rewrite them there and review the difference.
async fn connect() -> Option<Engine> {
    let cfg = mssql_config()?;
    Some(mssql_engine(&cfg).await)
}

#[tokio::test]
async fn sqlserver_values_and_errors_match_the_fixtures() {
    let _serial = common::serial().await;
    let Some(engine) = connect().await else { return };
    let mut report = Report::default();
    for kind in ["values", "errors"] {
        let rel = format!("{kind}/sqlserver.json");
        let mut expected = fixture(&rel);
        let mut session = engine.session().await.unwrap();
        run_all(&mut session, &strings(&expected["setup"])).await;
        if golden_write() {
            expected["probes"] = snapshot_probes(&mut session, &MS, &expected["probes"]).await;
            write_fixture(&rel, &expected);
            continue;
        }
        compare_probes(&mut session, &MS, &expected["probes"], kind, &[], &mut report).await;
    }
    report.assert_clean("sqlserver");
}

#[tokio::test]
async fn sqlserver_schema_and_ddl_match_the_fixtures() {
    let _serial = common::serial().await;
    let Some(engine) = connect().await else { return };
    let expected = fixture("schema/sqlserver.json");
    let mut session = engine.session().await.unwrap();
    run_all(&mut session, &strings(&expected["setup"])).await;
    drop(session);
    let actual = schema_snapshot(&engine, expected["schema"].as_str().unwrap(), &expected).await;
    if golden_write() {
        write_fixture("schema/sqlserver.json", &actual);
        return;
    }
    assert_matches("schema/sqlserver.json", &expected, &actual);
}

// Records plans for barsql-sql's offline parser test. Plans change between server versions, so this only writes.
#[tokio::test]
async fn sqlserver_plans_are_recorded_on_request() {
    if !golden_write() {
        return;
    }
    let _serial = common::serial().await;
    let Some(engine) = connect().await else { return };
    let mut session = engine.session().await.unwrap();
    run_all(&mut session, &strings(&fixture("schema/sqlserver.json")["setup"])).await;
    // Rows, so the plans read something and the measured one counts it.
    for insert in [
        "INSERT INTO barsql_golden.customers (email, name) SELECT TOP (500) CONCAT('u', n, '@x.io'), N'user' \
         FROM (SELECT ROW_NUMBER() OVER (ORDER BY (SELECT NULL)) AS n FROM sys.all_columns) t",
        // A status each, for ux_orders_status.
        "INSERT INTO barsql_golden.orders (id, customer_id, total, status) SELECT TOP (2000) n, n % 500 + 1, n % 97, \
         CONCAT('s', n) FROM (SELECT ROW_NUMBER() OVER (ORDER BY (SELECT NULL)) AS n FROM sys.all_columns) t",
        "UPDATE STATISTICS barsql_golden.orders",
        "UPDATE STATISTICS barsql_golden.customers",
    ] {
        session.buffered(insert, &Cancel::new()).await.unwrap();
    }
    let statements = [
        ("SELECT * FROM barsql_golden.customers WHERE id = 1", false),
        (
            "SELECT customer_id, SUM(total) AS total FROM barsql_golden.orders GROUP BY customer_id ORDER BY customer_id",
            false,
        ),
        (
            "SELECT c.email, o.total FROM barsql_golden.customers c JOIN barsql_golden.orders o ON o.customer_id = c.id WHERE c.id < 10",
            false,
        ),
        (
            "SELECT c.email, o.total FROM barsql_golden.customers c JOIN barsql_golden.orders o ON o.customer_id = c.id WHERE c.id < 10",
            true,
        ),
    ];
    let mut plans = Vec::new();
    for (stmt, analyze) in statements {
        let strategy = barsql_sql::build_explain(&MS, Default::default(), stmt, analyze).unwrap();
        let rows = session.plan_rows(&strategy, &Cancel::new()).await.unwrap();
        let explain_sql = strategy.display_sql();
        let plan = barsql_sql::parse_plan(&MS, stmt, &explain_sql, analyze, &rows).unwrap();
        plans.push(serde_json::json!({
            "statement": stmt,
            "analyze": analyze,
            "explainSql": explain_sql,
            "raw": { "columns": rows.columns, "rows": rows.rows },
            "plan": plan,
        }));
    }
    write_fixture(
        "explain/sqlserver.json",
        &serde_json::json!({ "engine": "sqlserver", "driver": "sqlserver", "plans": plans }),
    );
}
