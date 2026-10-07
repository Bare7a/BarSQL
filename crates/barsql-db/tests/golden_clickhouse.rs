#![cfg(feature = "e2e")]

mod common;

use barsql_core::DriverType;
use barsql_db::Engine;
use common::{
    Report, assert_matches, clickhouse_config, compare_probes, fixture, golden_write, run_all, schema_snapshot,
    snapshot_probes, strings, write_fixture,
};

const CLICKHOUSE: DriverType = DriverType::ClickHouse;

async fn connect() -> Engine {
    Engine::connect(&clickhouse_config())
        .await
        .expect("ClickHouse is not reachable; bring the stack up with `cargo xtask e2e up`")
}

#[tokio::test]
async fn clickhouse_values_and_errors_match_the_fixtures() {
    let _serial = common::serial().await;
    let engine = connect().await;
    let mut report = Report::default();
    for kind in ["values", "errors"] {
        let rel = format!("{kind}/clickhouse.json");
        let mut expected = fixture(&rel);
        let mut session = engine.session().await.unwrap();
        run_all(&mut session, &strings(&expected["setup"])).await;
        if golden_write() {
            expected["probes"] = snapshot_probes(&mut session, &CLICKHOUSE, &expected["probes"]).await;
            write_fixture(&rel, &expected);
            continue;
        }
        compare_probes(&mut session, &CLICKHOUSE, &expected["probes"], kind, &[], &mut report).await;
    }
    report.assert_clean("clickhouse");
}

#[tokio::test]
async fn clickhouse_schema_and_ddl_match_the_fixtures() {
    let _serial = common::serial().await;
    let engine = connect().await;
    let expected = fixture("schema/clickhouse.json");
    let mut session = engine.session().await.unwrap();
    run_all(&mut session, &strings(&expected["setup"])).await;
    drop(session);
    let actual = schema_snapshot(&engine, expected["schema"].as_str().unwrap(), &expected).await;
    if golden_write() {
        write_fixture("schema/clickhouse.json", &actual);
        return;
    }
    assert_matches("schema/clickhouse.json", &expected, &actual);
}

// Records plans for barsql-sql's offline parser test. Plans change between server versions, so this only writes.
#[tokio::test]
async fn clickhouse_plans_are_recorded_on_request() {
    if !golden_write() {
        return;
    }
    let _serial = common::serial().await;
    let engine = connect().await;
    let mut session = engine.session().await.unwrap();
    run_all(&mut session, &strings(&fixture("schema/clickhouse.json")["setup"])).await;
    // Empty tables plan as ReadNothing, so the reads and indexes need rows to show.
    run_all(
        &mut session,
        &[
            "INSERT INTO barsql_golden.customers (id, email) SELECT number, concat('u', toString(number), '@x.io') \
             FROM numbers(1000)"
                .into(),
            "INSERT INTO barsql_golden.orders (id, customer_id, total, day) SELECT number, number % 100, number, \
             toDate('2024-01-01') + number % 60 FROM numbers(1000)"
                .into(),
        ],
    )
    .await;
    let statements = [
        "SELECT * FROM barsql_golden.customers WHERE id = 1",
        "SELECT customer_id, sum(total) FROM barsql_golden.orders WHERE day >= '2024-01-01' GROUP BY customer_id \
         ORDER BY customer_id",
        "SELECT email FROM barsql_golden.customers WHERE email = 'a@x.io'",
    ];
    let mut plans = Vec::new();
    for stmt in statements {
        let explain_sql = barsql_sql::build_explain_sql(&CLICKHOUSE, Default::default(), stmt, false).unwrap();
        let res = session.buffered(&explain_sql, &barsql_db::Cancel::new()).await.unwrap();
        let rows = res.plan_rows();
        let plan = barsql_sql::parse_plan(&CLICKHOUSE, stmt, &explain_sql, false, &rows).unwrap();
        plans.push(serde_json::json!({
            "statement": stmt,
            "analyze": false,
            "explainSql": explain_sql,
            "raw": { "columns": rows.columns, "rows": res.values() },
            "plan": plan,
        }));
    }
    write_fixture(
        "explain/clickhouse.json",
        &serde_json::json!({ "engine": "clickhouse", "driver": "clickhouse", "plans": plans }),
    );
}
