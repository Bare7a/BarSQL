use std::fmt::Write;

use barsql_sql::plan::NOTE_ROLLED_BACK;
use barsql_sql::{PlanNode, QueryPlan};

use crate::harness::{E2e, Kind, run, unique_table};

// An indexed table with rows, so the planner has a choice to make.
async fn explain_fixture(e: &E2e) -> String {
    let table = unique_table("explain");
    e.create_temp_table(&e.auto_pk_table(&table), &table).await;
    let index = match e.is_clickhouse() {
        // A data-skipping index, since ClickHouse's ordinary index is the sorting key.
        true => {
            format!("ALTER TABLE {} ADD INDEX {table}_name name TYPE bloom_filter GRANULARITY 1", e.qualified(&table))
        }
        false => format!("CREATE INDEX {table}_name ON {} (name)", e.qualified(&table)),
    };
    e.exec(&index).await;
    for name in ["alpha", "beta", "gamma", "delta"] {
        e.exec(&format!("INSERT INTO {} (name) VALUES ('{name}')", e.qualified(&table))).await;
    }
    table
}

fn count_nodes(nodes: &[PlanNode]) -> usize {
    nodes.iter().map(|n| 1 + count_nodes(&n.children)).sum()
}

fn any_node(nodes: &[PlanNode], matches: &dyn Fn(&PlanNode) -> bool) -> bool {
    nodes.iter().any(|n| matches(n) || any_node(&n.children, matches))
}

fn plan_summary(plan: &QueryPlan) -> String {
    fn write_nodes(out: &mut String, nodes: &[PlanNode], depth: usize) {
        let num = |v: Option<f64>| v.map_or("-".into(), |v| v.to_string());
        for n in nodes {
            let _ = writeln!(
                out,
                "{}{} relation={:?} index={:?} cost={} rows={} actual={} time={}",
                "  ".repeat(depth),
                n.label,
                n.relation,
                n.index,
                num(n.cost_total),
                num(n.rows_planned),
                num(n.rows_actual),
                num(n.time_ms)
            );
            write_nodes(out, &n.children, depth + 1);
        }
    }
    let mut out = format!("\n{}\n", plan.explain_sql);
    write_nodes(&mut out, &plan.nodes, 0);
    out
}

async fn explain(e: &E2e, sql: &str, analyze: bool) -> QueryPlan {
    e.app.explain_query(&e.id, "", sql, analyze).await.unwrap_or_else(|err| panic!("explain {sql:?}: {err:?}"))
}

each_engine!(async fn explain_plan_only(e) {
    require!(e, plan_metrics);
    let table = explain_fixture(&e).await;
    let plan = explain(&e, &format!("SELECT * FROM {} WHERE name = 'beta'", e.qualified(&table)), false).await;
    assert!(!plan.analyzed, "a plan-only run must not report itself as analyzed");
    assert!(!plan.nodes.is_empty());
    assert!(!plan.raw.is_empty(), "the engine's raw output is kept");
    assert!(any_node(&plan.nodes, &|n| n.cost_total.is_some()), "no cost estimate:{}", plan_summary(&plan));
    assert!(any_node(&plan.nodes, &|n| n.rows_planned.is_some()), "no row estimate:{}", plan_summary(&plan));
    assert!(
        !any_node(&plan.nodes, &|n| n.rows_actual.is_some() || n.time_ms.is_some()),
        "nothing ran, so nothing is measured:{}",
        plan_summary(&plan)
    );
    assert!(plan.notes.is_empty(), "{:?}", plan.notes);
});

each_engine!(async fn explain_analyze_measures(e) {
    require!(e, explain_analyze);
    let table = explain_fixture(&e).await;
    let plan = explain(&e, &format!("SELECT * FROM {}", e.qualified(&table)), true).await;
    assert!(plan.analyzed);
    assert!(any_node(&plan.nodes, &|n| n.rows_actual.is_some()), "no measured rows:{}", plan_summary(&plan));
    assert!(any_node(&plan.nodes, &|n| n.time_ms.is_some()), "no measured time:{}", plan_summary(&plan));
    // Self time drives the heat map.
    assert!(any_node(&plan.nodes, &|n| n.self_time_ms.is_some()), "no self time:{}", plan_summary(&plan));
});

each_engine!(async fn explain_analyze_row_counts_are_totals(e) {
    require!(e, explain_analyze);
    let table = explain_fixture(&e).await;
    let plan = explain(&e, &format!("SELECT * FROM {}", e.qualified(&table)), true).await;
    assert!(any_node(&plan.nodes, &|n| n.rows_actual == Some(4.0)), "no node reports the 4 rows:{}", plan_summary(&plan));
});

each_engine!(async fn explain_index_is_named(e) {
    let table = explain_fixture(&e).await;
    // SQLite looks an INTEGER PRIMARY KEY up by rowid, which isn't an index.
    let column = if e.is_lite() { "name = 'beta'" } else { "id = 1" };
    let plan = explain(&e, &format!("SELECT * FROM {} WHERE {column}", e.qualified(&table)), false).await;
    assert!(any_node(&plan.nodes, &|n| !n.index.is_empty()), "no node names its index:{}", plan_summary(&plan));
});

each_engine!(async fn explain_analyze_rolls_back_writes(e) {
    require!(e, explain_analyze);
    let table = explain_fixture(&e).await;
    let plan = explain(&e, &format!("DELETE FROM {}", e.qualified(&table)), true).await;
    assert!(plan.has_note(NOTE_ROLLED_BACK), "{:?}", plan.notes);
    assert_eq!(e.count(&table).await, 4, "EXPLAIN ANALYZE of a DELETE must not delete");
});

each_engine!(async fn explain_write_without_analyze_keeps_rows(e) {
    // ClickHouse explains SELECT and INSERT only.
    if e.is_clickhouse() {
        return;
    }
    let table = explain_fixture(&e).await;
    // SQLite plans a DELETE without a WHERE as nothing at all.
    explain(&e, &format!("DELETE FROM {} WHERE name = 'beta'", e.qualified(&table)), false).await;
    assert_eq!(e.count(&table).await, 4);
});

// T-SQL has no EXPLAIN to type.
each_engine!(async fn explain_of_an_explain(e) {
    require!(e, typed_explain);
    let table = explain_fixture(&e).await;
    let plan = explain(&e, &format!("EXPLAIN SELECT * FROM {}", e.qualified(&table)), false).await;
    assert!(!plan.nodes.is_empty(), "a plan of the underlying statement");
    // Whole words, so the fixture's own explain_* table name does not count.
    let explains = plan.explain_sql.split(|c: char| !c.is_alphanumeric() && c != '_').filter(|w| w.eq_ignore_ascii_case("explain"));
    assert_eq!(explains.count(), 1, "nested EXPLAINs: {:?}", plan.explain_sql);
});

each_engine!(async fn explain_reports_syntax_errors(e) {
    let res = e.app.explain_query(&e.id, "", "SELECT * FROM table_that_is_not_there_barsql", false).await;
    assert!(res.is_err());
});

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explain_postgres_timings() {
    run(Kind::Postgres, |e| async move {
        let table = explain_fixture(&e).await;
        let plan = explain(&e, &format!("SELECT * FROM {}", e.qualified(&table)), true).await;
        assert!(plan.planning_ms.is_some_and(|ms| ms > 0.0), "planning time = {:?}", plan.planning_ms);
        assert!(plan.execution_ms.is_some_and(|ms| ms > 0.0), "execution time = {:?}", plan.execution_ms);
        assert!(plan.total_cost.is_some());
    })
    .await
}

each_engine!(async fn explain_tree_has_depth(e) {
    // SQLite's EXPLAIN QUERY PLAN lists a join's tables side by side.
    if e.is_lite() {
        return;
    }
    let table = explain_fixture(&e).await;
    let t = e.qualified(&table);
    let sql = format!("SELECT a.name FROM {t} a JOIN {t} b ON a.id = b.id JOIN {t} c ON b.id = c.id ORDER BY a.name");
    let plan = explain(&e, &sql, false).await;
    assert!(count_nodes(&plan.nodes) >= 3, "a 3-way join plans several nodes:{}", plan_summary(&plan));
    assert!(any_node(&plan.nodes, &|n| !n.children.is_empty()), "the plan tree is flat:{}", plan_summary(&plan));
});

each_engine!(async fn explain_read_only_connection(e) {
    let table = explain_fixture(&e).await;
    let mut cfg = e.app.list_connections().into_iter().find(|c| c.id == e.id).unwrap();
    cfg.read_only = true;
    e.app.save_connection(cfg).await.unwrap();
    let read = e.app.explain_query(&e.id, "", &format!("SELECT * FROM {}", e.qualified(&table)), false).await;
    assert!(read.is_ok(), "planning a read on a read-only connection should work: {read:?}");
    let write = e.app.explain_query(&e.id, "", &format!("DELETE FROM {}", e.qualified(&table)), true).await;
    assert!(write.is_err(), "a measured plan of a delete is a write");
});
