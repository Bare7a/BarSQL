use barsql_core::{DriverType, Value};
use barsql_sql::plan::NOTE_NO_METRICS;
use barsql_sql::{
    PlanField, PlanRows, QueryPlan, ServerVersion, build_explain_sql, detect_plan_request, parse_plan, single_statement,
};

const PG: DriverType = DriverType::Postgres;
const MY: DriverType = DriverType::MySql;
const LITE: DriverType = DriverType::Sqlite;

fn fixture(driver: &DriverType, analyze: bool, raw: &str) -> QueryPlan {
    let rows = PlanRows { columns: vec!["EXPLAIN".into()], rows: vec![vec![Value::Text(raw.into())]] };
    let plan = parse_plan(driver, "SELECT 1", "EXPLAIN …", analyze, &rows).expect("plan");
    assert!(!plan.nodes.is_empty());
    plan
}

fn near(what: &str, got: Option<f64>, want: f64) {
    let got = got.unwrap_or_else(|| panic!("{what} is missing, want {want}"));
    assert!((got - want).abs() <= 1e-9, "{what} = {got}, want {want}");
}

fn field<'a>(fields: &'a [PlanField], key: &str) -> Option<&'a str> {
    fields.iter().find(|f| f.key == key).map(|f| f.value.as_str())
}

#[test]
fn server_versions() {
    let cases = [
        ("8.0.36", false, 8, 0, 36),
        ("8.0.18-log", false, 8, 0, 18),
        ("9.1.0", false, 9, 1, 0),
        ("5.7.44-log", false, 5, 7, 44),
        ("11.4.2-MariaDB-ubu2404", true, 11, 4, 2),
        ("5.5.5-10.6.12-MariaDB-1:10.6.12+maria~ubu2004", true, 10, 6, 12),
        ("", false, 0, 0, 0),
    ];
    for (raw, mariadb, major, minor, patch) in cases {
        assert_eq!(ServerVersion::parse(raw), ServerVersion { mariadb, major, minor, patch }, "{raw}");
    }
    for (raw, analyzable) in [
        ("8.0.18", true),
        ("8.0.36", true),
        ("8.4.0", true),
        ("9.0.0", true),
        ("8.0.17", false),
        ("5.7.44", false),
        ("8.0.2", false),
    ] {
        let got = build_explain_sql(&MY, ServerVersion::parse(raw), "SELECT 1", true);
        assert_eq!(got.is_ok(), analyzable, "{raw}");
    }
}

#[test]
fn builds_explain_sql() {
    let mysql8 = ServerVersion::parse("8.0.36");
    let mysql57 = ServerVersion::parse("5.7.44");
    let maria = ServerVersion::parse("11.4.2-MariaDB");
    let none = ServerVersion::default();
    type Case = (&'static str, DriverType, ServerVersion, &'static str, bool, Option<&'static str>);
    let cases: [Case; 11] = [
        ("postgres plan", PG, none, "SELECT 1", false, Some("EXPLAIN (FORMAT JSON) SELECT 1")),
        ("postgres analyze", PG, none, "SELECT 1", true, Some("EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) SELECT 1")),
        ("mysql plan", MY, mysql8, "SELECT 1", false, Some("EXPLAIN FORMAT=JSON SELECT 1")),
        ("mysql analyze uses tree", MY, mysql8, "SELECT 1", true, Some("EXPLAIN ANALYZE SELECT 1")),
        ("mariadb analyze", MY, maria, "SELECT 1", true, Some("ANALYZE FORMAT=JSON SELECT 1")),
        ("mysql 5.7 cannot analyze", MY, mysql57, "SELECT 1", true, None),
        ("sqlite plan", LITE, none, "SELECT 1", false, Some("EXPLAIN QUERY PLAN SELECT 1")),
        ("sqlite cannot analyze", LITE, none, "SELECT 1", true, None),
        ("empty statement", PG, none, "   ", false, None),
        ("unknown driver", DriverType::parse("oracle"), none, "SELECT 1", false, None),
        ("trailing semicolon dropped", PG, none, "SELECT 1;  ", false, Some("EXPLAIN (FORMAT JSON) SELECT 1")),
    ];
    for (name, driver, version, stmt, analyze, want) in cases {
        assert_eq!(build_explain_sql(&driver, version, stmt, analyze).ok().as_deref(), want, "{name}");
    }
}

#[test]
fn a_typed_explain_is_replaced_not_nested() {
    let cases = [
        (PG, "EXPLAIN SELECT 1", "EXPLAIN (FORMAT JSON) SELECT 1"),
        (PG, "EXPLAIN ANALYZE SELECT 1", "EXPLAIN (FORMAT JSON) SELECT 1"),
        (PG, "explain (analyze, buffers) select 1", "EXPLAIN (FORMAT JSON) select 1"),
        (
            PG,
            "EXPLAIN (FORMAT TEXT) WITH x AS (SELECT 1) SELECT * FROM x",
            "EXPLAIN (FORMAT JSON) WITH x AS (SELECT 1) SELECT * FROM x",
        ),
        (MY, "EXPLAIN FORMAT=JSON SELECT 1", "EXPLAIN FORMAT=JSON SELECT 1"),
        (MY, "EXPLAIN ANALYZE SELECT 1", "EXPLAIN FORMAT=JSON SELECT 1"),
        (MY, "ANALYZE FORMAT=JSON SELECT 1", "EXPLAIN FORMAT=JSON SELECT 1"),
        (LITE, "EXPLAIN QUERY PLAN SELECT 1", "EXPLAIN QUERY PLAN SELECT 1"),
        (MY, "ANALYZE TABLE t", "EXPLAIN FORMAT=JSON ANALYZE TABLE t"),
        (PG, "EXPLAIN", "EXPLAIN (FORMAT JSON) EXPLAIN"),
    ];
    for (driver, stmt, want) in cases {
        assert_eq!(build_explain_sql(&driver, ServerVersion::default(), stmt, false).unwrap(), want, "{driver} {stmt}");
    }
}

#[test]
fn single_statement_rules() {
    assert_eq!(single_statement(&PG, " SELECT 1; ").unwrap(), "SELECT 1");
    assert!(single_statement(&PG, "SELECT 1; SELECT 2").is_err());
    assert!(single_statement(&PG, "  -- just a comment\n").is_err());
}

const PG_ANALYZE: &str = r#"[
  {
    "Plan": {
      "Node Type": "Nested Loop", "Parallel Aware": false, "Join Type": "Inner",
      "Startup Cost": 0.29, "Total Cost": 42.58, "Plan Rows": 10, "Plan Width": 68,
      "Actual Startup Time": 0.021, "Actual Total Time": 0.185, "Actual Rows": 9, "Actual Loops": 1,
      "Plans": [
        {
          "Node Type": "Seq Scan", "Parent Relationship": "Outer", "Relation Name": "orders", "Alias": "o",
          "Startup Cost": 0.00, "Total Cost": 18.10, "Plan Rows": 10, "Plan Width": 36,
          "Actual Startup Time": 0.010, "Actual Total Time": 0.032, "Actual Rows": 9, "Actual Loops": 1,
          "Filter": "(total > 100)", "Rows Removed by Filter": 3, "Shared Hit Blocks": 5
        },
        {
          "Node Type": "Index Scan", "Parent Relationship": "Inner", "Relation Name": "customers", "Alias": "c",
          "Index Name": "customers_pkey", "Startup Cost": 0.29, "Total Cost": 2.44, "Plan Rows": 1, "Plan Width": 32,
          "Actual Startup Time": 0.003, "Actual Total Time": 0.004, "Actual Rows": 1, "Actual Loops": 9,
          "Index Cond": "(id = o.customer_id)"
        }
      ]
    },
    "Planning Time": 0.153,
    "Execution Time": 0.221
  }
]"#;

#[test]
fn postgres_analyze_plan() {
    let plan = fixture(&PG, true, PG_ANALYZE);
    assert_eq!(plan.nodes.len(), 1);
    near("planning ms", plan.planning_ms, 0.153);
    near("execution ms", plan.execution_ms, 0.221);
    near("total cost", plan.total_cost, 42.58);
    assert!(plan.notes.is_empty());
    let root = &plan.nodes[0];
    assert_eq!(root.label, "Nested Loop");
    near("root time", root.time_ms, 0.185);
    near("root rows", root.rows_actual, 9.0);
    near("root self time", root.self_time_ms, 0.185 - (0.032 + 0.036));
    near("root self cost", root.cost_self, 42.58 - (18.10 + 2.44));
    assert_eq!(root.children.len(), 2);
    let seq = &root.children[0];
    assert_eq!(
        (seq.label.as_str(), seq.relation.as_str(), seq.detail.as_str()),
        ("Seq Scan", "orders o", "(total > 100)")
    );
    near("seq scan cost", seq.cost_total, 18.10);
    near("seq scan self cost", seq.cost_self, 18.10);
    assert_eq!(field(&seq.fields, "Filter"), None, "the detail clause is not repeated");
    assert_eq!(field(&seq.fields, "Rows Removed by Filter"), Some("3"));
    assert_eq!(field(&seq.fields, "Shared Hit Blocks"), Some("5"));
    let idx = &root.children[1];
    assert_eq!((idx.index.as_str(), idx.detail.as_str()), ("customers_pkey", "(id = o.customer_id)"));
    near("index scan loops", idx.loops, 9.0);
    near("index scan rows", idx.rows_actual, 9.0);
    near("index scan time", idx.time_ms, 0.036);
}

#[test]
fn postgres_plan_only_has_no_measurements() {
    let raw = r#"[{"Plan":{"Node Type":"Seq Scan","Relation Name":"t","Alias":"t","Total Cost":12.5,"Plan Rows":420,"Plan Width":8}}]"#;
    let plan = fixture(&PG, false, raw);
    let root = &plan.nodes[0];
    assert!(root.time_ms.is_none() && root.rows_actual.is_none() && root.loops.is_none());
    near("rows planned", root.rows_planned, 420.0);
    assert!(plan.notes.is_empty());
}

#[test]
fn postgres_labels() {
    let cases = [
        (r#"{"Node Type":"Hash Join","Join Type":"Left"}"#, "Hash Join (Left)"),
        (r#"{"Node Type":"Hash Join","Join Type":"Inner"}"#, "Hash Join"),
        (r#"{"Node Type":"Aggregate","Strategy":"Hashed"}"#, "Aggregate (Hashed)"),
        (r#"{"Node Type":"Aggregate","Strategy":"Plain"}"#, "Aggregate"),
        (r#"{"Node Type":"Seq Scan","Parallel Aware":true}"#, "Parallel Seq Scan"),
        (r#"{"Node Type":"Aggregate","Subplan Name":"InitPlan 1 (returns $0)"}"#, "InitPlan 1 (returns $0): Aggregate"),
        ("{}", "Node"),
    ];
    for (raw, want) in cases {
        assert_eq!(fixture(&PG, false, &format!(r#"[{{"Plan":{raw}}}]"#)).nodes[0].label, want, "{raw}");
    }
}

#[test]
fn postgres_self_metrics_never_go_negative() {
    let raw = r#"[{"Plan":{
        "Node Type":"Result","Total Cost":1.0,"Actual Total Time":0.05,"Actual Loops":1,
        "Plans":[{"Node Type":"Aggregate","Subplan Name":"InitPlan 1","Total Cost":9.0,"Actual Total Time":0.4,"Actual Loops":1}]
    }}]"#;
    let plan = fixture(&PG, true, raw);
    near("self time", plan.nodes[0].self_time_ms, 0.0);
    near("self cost", plan.nodes[0].cost_self, 0.0);
    let never =
        r#"[{"Plan":{"Node Type":"Result","Total Cost":1,"Actual Total Time":0,"Actual Rows":0,"Actual Loops":0}}]"#;
    assert!(fixture(&PG, true, never).nodes[0].never_run);
}

#[test]
fn unusable_output_is_rejected() {
    let garbage = PlanRows { columns: vec!["QUERY PLAN".into()], rows: vec![vec![Value::Text("not json".into())]] };
    assert!(parse_plan(&PG, "SELECT 1", "EXPLAIN …", false, &garbage).is_err());
    let empty = PlanRows { columns: vec!["QUERY PLAN".into()], rows: vec![] };
    assert!(parse_plan(&PG, "SELECT 1", "EXPLAIN …", false, &empty).is_err());
}

const MYSQL_JSON: &str = r#"{
  "query_block": {
    "select_id": 1,
    "cost_info": {"query_cost": "3.60"},
    "nested_loop": [
      {"table": {
        "table_name": "o", "access_type": "ALL", "rows_examined_per_scan": 10, "rows_produced_per_join": 3,
        "filtered": "33.33",
        "cost_info": {"read_cost": "1.25", "eval_cost": "0.33", "prefix_cost": "1.58", "data_read_per_join": "160"},
        "used_columns": ["id", "customer_id", "total"],
        "attached_condition": "(`shop`.`o`.`total` > 100)"
      }},
      {"table": {
        "table_name": "c", "access_type": "eq_ref", "possible_keys": ["PRIMARY"], "key": "PRIMARY",
        "used_key_parts": ["id"], "key_length": "4", "ref": ["shop.o.customer_id"],
        "rows_examined_per_scan": 1, "rows_produced_per_join": 3, "filtered": "100.00",
        "cost_info": {"read_cost": "1.69", "eval_cost": "0.33", "prefix_cost": "3.60", "data_read_per_join": "192"}
      }}
    ]
  }
}"#;

#[test]
fn mysql_json_plan() {
    let plan = fixture(&MY, false, MYSQL_JSON);
    assert_eq!(plan.nodes.len(), 1);
    let root = &plan.nodes[0];
    assert_eq!(root.label, "Query block #1");
    near("total cost", plan.total_cost, 3.60);
    assert_eq!(root.children.len(), 1);
    let group = &root.children[0];
    assert_eq!(group.label, "Nested loop");
    near("join cost", group.cost_total, 3.60);
    assert_eq!(group.children.len(), 2);
    let outer = &group.children[0];
    assert_eq!((outer.label.as_str(), outer.relation.as_str()), ("Table (ALL)", "o"));
    assert!(outer.detail.contains("> 100"));
    near("outer rows", outer.rows_planned, 3.0);
    near("outer self cost", outer.cost_self, 1.58);
    assert_eq!(field(&outer.fields, "Prefix cost"), Some("1.58"));
    assert_eq!(field(&outer.fields, "Rows examined per scan"), Some("10"));
    assert_eq!(field(&outer.fields, "Used columns"), Some("id, customer_id, total"));
    let inner = &group.children[1];
    assert_eq!((inner.label.as_str(), inner.index.as_str()), ("Table (eq_ref)", "PRIMARY"));
    near("inner self cost", inner.cost_self, 2.02);
    assert!(inner.time_ms.is_none());
}

const MARIA_ANALYZE: &str = r#"{
  "query_block": {
    "select_id": 1, "r_loops": 1, "r_total_time_ms": 0.4521,
    "nested_loop": [
      {"table": {
        "table_name": "o", "access_type": "ALL", "r_loops": 1, "rows": 10, "r_rows": 9,
        "r_table_time_ms": 0.0521, "r_other_time_ms": 0.0129, "filtered": 100, "r_filtered": 90,
        "attached_condition": "o.total > 100"
      }},
      {"table": {
        "table_name": "c", "access_type": "eq_ref", "possible_keys": ["PRIMARY"], "key": "PRIMARY",
        "r_loops": 9, "rows": 1, "r_rows": 1, "r_table_time_ms": 0.1521, "r_other_time_ms": 0.0221
      }}
    ]
  }
}"#;

#[test]
fn mariadb_analyze_plan() {
    let plan = fixture(&MY, true, MARIA_ANALYZE);
    let root = &plan.nodes[0];
    near("root time", root.time_ms, 0.4521);
    near("execution ms", plan.execution_ms, 0.4521);
    let group = &root.children[0];
    let outer = &group.children[0];
    near("outer self time", outer.self_time_ms, 0.065);
    near("outer rows planned", outer.rows_planned, 10.0);
    near("outer rows actual", outer.rows_actual, 9.0);
    let inner = &group.children[1];
    near("inner loops", inner.loops, 9.0);
    near("inner rows actual", inner.rows_actual, 9.0);
    near("inner self time", inner.self_time_ms, 0.1742);
    near("join time", group.time_ms, 0.065 + 0.1742);
    near("root self time", root.self_time_ms, 0.4521 - (0.065 + 0.1742));
}

const MYSQL_TREE: &str = "-> Nested loop inner join  (cost=3.60 rows=3) (actual time=0.0451..0.0912 rows=9 loops=1)
    -> Filter: (o.total > 100)  (cost=1.58 rows=3) (actual time=0.0312..0.0451 rows=9 loops=1)
        -> Table scan on o  (cost=1.58 rows=10) (actual time=0.0221..0.0356 rows=10 loops=1)
    -> Single-row index lookup on c using PRIMARY (id=o.customer_id)  (cost=0.67 rows=1) (actual time=0.0021..0.0024 rows=1 loops=9)
";

#[test]
fn mysql_tree_plan() {
    let plan = fixture(&MY, true, MYSQL_TREE);
    assert_eq!(plan.nodes.len(), 1);
    let root = &plan.nodes[0];
    assert_eq!(root.label, "Nested loop inner join");
    near("root cost", root.cost_total, 3.60);
    near("root rows planned", root.rows_planned, 3.0);
    near("root time", root.time_ms, 0.0912);
    near("root rows actual", root.rows_actual, 9.0);
    assert_eq!(root.children.len(), 2);
    let filter = &root.children[0];
    assert_eq!((filter.label.as_str(), filter.detail.as_str()), ("Filter", "(o.total > 100)"));
    assert_eq!(filter.children.len(), 1);
    assert_eq!(filter.children[0].relation, "o");
    let lookup = &root.children[1];
    assert_eq!((lookup.relation.as_str(), lookup.index.as_str()), ("c", "PRIMARY"));
    near("lookup time", lookup.time_ms, 0.0216);
    near("lookup rows", lookup.rows_actual, 9.0);
    near("root self time", root.self_time_ms, 0.0912 - (0.0451 + 0.0216));
}

#[test]
fn mysql_tree_never_executed_and_clause_keywords() {
    let raw = "-> Limit: 1 row(s)  (cost=0.35 rows=1) (actual time=0.01..0.02 rows=1 loops=1)
    -> Index lookup on t using ix (a=1)  (cost=0.35 rows=1) (never executed)
";
    let child = &fixture(&MY, true, raw).nodes[0].children[0];
    assert!(child.never_run);
    assert!(child.label.starts_with("Index lookup on t using ix"), "{}", child.label);
    let raw = "-> Filter: (t.status = 'on' and t.mode = 'using idx')  (cost=1.0 rows=1)\n";
    let node = &fixture(&MY, false, raw).nodes[0];
    assert_eq!((node.relation.as_str(), node.index.as_str()), ("", ""));
}

fn sqlite_rows(rows: &[(i64, i64, &str)]) -> PlanRows {
    PlanRows {
        columns: ["id", "parent", "notused", "detail"].map(String::from).to_vec(),
        rows: rows
            .iter()
            .map(|(id, parent, detail)| {
                vec![Value::Int(*id), Value::Int(*parent), Value::Int(0), Value::Text(detail.to_string())]
            })
            .collect(),
    }
}

#[test]
fn sqlite_plan() {
    let rows = sqlite_rows(&[(4, 0, "CO-ROUTINE t1"), (8, 4, "SCAN x"), (20, 0, "SEARCH c USING INDEX idx_c (a=?)")]);
    let plan = parse_plan(&LITE, "SELECT 1", "EXPLAIN QUERY PLAN SELECT 1", false, &rows).unwrap();
    assert_eq!(plan.nodes.len(), 2);
    let coroutine = &plan.nodes[0];
    assert_eq!(coroutine.label, "CO-ROUTINE t1");
    assert_eq!(coroutine.children.len(), 1);
    assert_eq!((coroutine.children[0].label.as_str(), coroutine.children[0].relation.as_str()), ("SCAN", "x"));
    let search = &plan.nodes[1];
    assert_eq!((search.label.as_str(), search.relation.as_str(), search.index.as_str()), ("SEARCH", "c", "idx_c"));
    assert_eq!(search.detail, "USING INDEX idx_c (a=?)");
    assert!(search.cost_total.is_none() && search.rows_planned.is_none() && search.time_ms.is_none());
    assert!(plan.has_note(NOTE_NO_METRICS));
    assert_eq!(plan.raw, "4|0|CO-ROUTINE t1\n8|4|SCAN x\n20|0|SEARCH c USING INDEX idx_c (a=?)");
}

#[test]
fn sqlite_scan_variants() {
    let cases = [
        ("SCAN t", "SCAN", "t", ""),
        ("SCAN TABLE t", "SCAN", "t", ""),
        ("SEARCH t USING COVERING INDEX ix_t (a=?)", "SEARCH", "t", "ix_t"),
        ("SEARCH t USING AUTOMATIC PARTIAL COVERING INDEX ix (a=?)", "SEARCH", "t", ""),
        ("SEARCH t USING INTEGER PRIMARY KEY (rowid=?)", "SEARCH", "t", ""),
        ("USE TEMP B-TREE FOR ORDER BY", "USE TEMP B-TREE FOR ORDER BY", "", ""),
    ];
    for (detail, label, relation, index) in cases {
        let plan = parse_plan(&LITE, "SELECT 1", "", false, &sqlite_rows(&[(1, 0, detail)])).unwrap();
        let node = &plan.nodes[0];
        assert_eq!(
            (node.label.as_str(), node.relation.as_str(), node.index.as_str()),
            (label, relation, index),
            "{detail}"
        );
    }
}

fn detect(driver: &DriverType, stmt: &str) -> Option<(String, bool)> {
    detect_plan_request(driver, stmt).map(|r| (r.sql, r.analyze))
}

#[test]
fn detects_postgres_plan_requests() {
    let cases: [(&str, Option<(&str, bool)>); 14] = [
        ("EXPLAIN SELECT 1", Some(("EXPLAIN (FORMAT JSON) SELECT 1", false))),
        ("EXPLAIN ANALYZE SELECT 1", Some(("EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) SELECT 1", true))),
        ("explain analyze verbose select 1", Some(("EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) select 1", true))),
        ("EXPLAIN (ANALYZE, BUFFERS) SELECT 1", Some(("EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) SELECT 1", true))),
        ("EXPLAIN (FORMAT JSON) SELECT 1", Some(("EXPLAIN (FORMAT JSON) SELECT 1", false))),
        ("EXPLAIN (ANALYZE, FORMAT JSON) SELECT 1", Some(("EXPLAIN (ANALYZE, FORMAT JSON) SELECT 1", true))),
        (
            "EXPLAIN (ANALYZE false, FORMAT JSON) SELECT 1",
            Some(("EXPLAIN (ANALYZE false, FORMAT JSON) SELECT 1", false)),
        ),
        ("EXPLAIN (FORMAT TEXT) SELECT 1", None),
        ("EXPLAIN (FORMAT YAML) SELECT 1", None),
        ("EXPLAIN (ANALYZE, FORMAT XML) SELECT 1", None),
        ("SELECT 1", None),
        ("EXPLAIN", None),
        ("ANALYZE my_table", None),
        ("-- a comment\nEXPLAIN SELECT 1", Some(("EXPLAIN (FORMAT JSON) SELECT 1", false))),
    ];
    for (stmt, want) in cases {
        assert_eq!(detect(&PG, stmt), want.map(|(s, a)| (s.to_string(), a)), "{stmt}");
    }
}

#[test]
fn detects_mysql_plan_requests() {
    let cases: [(&str, Option<(&str, bool)>); 11] = [
        ("EXPLAIN SELECT 1", Some(("EXPLAIN FORMAT=JSON SELECT 1", false))),
        ("EXPLAIN ANALYZE SELECT 1", Some(("EXPLAIN ANALYZE SELECT 1", true))),
        ("EXPLAIN FORMAT=JSON SELECT 1", Some(("EXPLAIN FORMAT=JSON SELECT 1", false))),
        ("EXPLAIN FORMAT=TREE SELECT 1", Some(("EXPLAIN FORMAT=TREE SELECT 1", false))),
        ("EXPLAIN ANALYZE FORMAT=JSON SELECT 1", Some(("EXPLAIN ANALYZE FORMAT=JSON SELECT 1", true))),
        ("ANALYZE FORMAT=JSON SELECT 1", Some(("ANALYZE FORMAT=JSON SELECT 1", true))),
        ("ANALYZE SELECT 1", Some(("ANALYZE FORMAT=JSON SELECT 1", true))),
        ("EXPLAIN FORMAT=TRADITIONAL SELECT 1", None),
        ("ANALYZE TABLE users", None),
        ("EXPLAIN users", None),
        ("DESCRIBE users", None),
    ];
    for (stmt, want) in cases {
        assert_eq!(detect(&MY, stmt), want.map(|(s, a)| (s.to_string(), a)), "{stmt}");
    }
}

#[test]
fn detects_sqlite_plan_requests_and_keeps_writes() {
    assert_eq!(detect(&LITE, "EXPLAIN QUERY PLAN SELECT 1"), Some(("EXPLAIN QUERY PLAN SELECT 1".into(), false)));
    assert_eq!(detect(&LITE, "EXPLAIN SELECT 1"), None, "bytecode is not a plan");
    let (sql, analyze) = detect(&PG, "EXPLAIN ANALYZE DELETE FROM t").expect("plan request");
    assert!(analyze && sql.contains("DELETE FROM t"));
}
