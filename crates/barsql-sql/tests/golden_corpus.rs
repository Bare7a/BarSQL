use std::fs;
use std::path::PathBuf;

use barsql_core::{DriverType, Row, Value};
use barsql_sql::dml::{build_delete, build_insert, build_update};
use barsql_sql::{
    PlanRows, assert_read_only, detect_plan_request, is_read_only, parse_plan, qualified_table, quote_ident,
    split_statement_texts, validate_table_filter,
};
use serde_json::{Value as Json, json};

const DRIVERS: [(&str, DriverType); 3] =
    [("postgres", DriverType::Postgres), ("mysql", DriverType::MySql), ("sqlite", DriverType::Sqlite)];

fn fixture(rel: &str) -> Json {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/golden").join(rel);
    serde_json::from_str(&fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())))
        .expect("fixture json")
}

fn cases(rel: &str) -> Vec<Json> {
    fixture(rel).as_array().expect("fixture array").clone()
}

fn input(case: &Json) -> &str {
    case["input"].as_str().expect("input")
}

const SPLIT_DEVIATIONS: &[(&str, &str, &[&str])] = &[(
    "sqlite",
    "CREATE TRIGGER t AFTER INSERT ON x BEGIN SELECT 1; END; SELECT 2",
    &["CREATE TRIGGER t AFTER INSERT ON x BEGIN SELECT 1; END", "SELECT 2"],
)];

fn assert_all(rel: &str, failures: Vec<String>) {
    assert!(failures.is_empty(), "{} {rel} case(s) differ from the fixtures:\n{}", failures.len(), failures.join("\n"));
}

#[test]
fn statement_splits_match() {
    let mut failures = Vec::new();
    for case in cases("corpus/splits.json") {
        for (name, driver) in &DRIVERS {
            let deviation = SPLIT_DEVIATIONS.iter().find(|(d, sql, _)| d == name && *sql == input(&case));
            let want = match deviation {
                Some((_, _, split)) => split.iter().map(|s| json!(s)).collect(),
                None => case["output"][name].as_array().cloned().unwrap_or_default(),
            };
            let got: Vec<Json> = split_statement_texts(driver, input(&case)).into_iter().map(Json::String).collect();
            if got != want {
                failures.push(format!("{name} {:?}\n  want: {want:?}\n  got:  {got:?}", input(&case)));
            }
        }
    }
    assert_all("splits", failures);
}

#[test]
fn read_only_verdicts_match() {
    let mut failures = Vec::new();
    for case in cases("corpus/readonly.json") {
        for (name, driver) in &DRIVERS {
            let mut got = json!({ "readOnly": is_read_only(driver, input(&case)) });
            if let Err(err) = assert_read_only(driver, input(&case)) {
                got["error"] = json!(err);
            }
            let want = &case["output"][name];
            if &got != want {
                failures.push(format!("{name} {:?}: want {want} got {got}", input(&case)));
            }
        }
    }
    assert_all("readonly", failures);
}

#[test]
fn table_filter_validation_matches() {
    let mut failures = Vec::new();
    for case in cases("corpus/table_filter.json") {
        let got = validate_table_filter(input(&case)).err();
        let want = case.get("error").and_then(Json::as_str).map(str::to_string);
        if got != want {
            failures.push(format!("{:?}: want {want:?} got {got:?}", input(&case)));
        }
    }
    assert_all("table_filter", failures);
}

#[test]
fn identifier_quoting_matches() {
    let mut failures = Vec::new();
    for case in cases("corpus/quote_ident.json") {
        for (name, driver) in &DRIVERS {
            let got = json!({
                "quoted": quote_ident(driver, input(&case)),
                "qualified": qualified_table(driver, "s", input(&case)),
            });
            if got != case["output"][name] {
                failures.push(format!("{name} {:?}: want {} got {got}", input(&case), case["output"][name]));
            }
        }
    }
    assert_all("quote_ident", failures);
}

#[test]
fn plan_request_detection_matches() {
    let mut failures = Vec::new();
    for case in cases("corpus/plan_requests.json") {
        for (name, driver) in &DRIVERS {
            let got = match detect_plan_request(driver, input(&case)) {
                Some(req) => json!({ "sql": req.sql, "analyze": req.analyze }),
                None => Json::Null,
            };
            if got != case["output"][name] {
                failures.push(format!("{name} {:?}: want {} got {got}", input(&case), case["output"][name]));
            }
        }
    }
    assert_all("plan_requests", failures);
}

fn json_row(v: &Json) -> Row {
    serde_json::from_value(v.clone()).expect("row")
}

fn strings(v: &Json) -> Vec<String> {
    serde_json::from_value(v.clone()).expect("strings")
}

#[test]
fn row_edit_sql_matches() {
    let all = fixture("corpus/row_edit_sql.json");
    let mut failures = Vec::new();
    for (name, driver) in &DRIVERS {
        for edit in all[name].as_array().expect("edits") {
            let i = &edit["input"];
            let schema = i["schema"].as_str().expect("schema");
            let table = i["table"].as_str().expect("table");
            let built = match edit["kind"].as_str().expect("kind") {
                "update" => build_update(
                    driver,
                    schema,
                    table,
                    &json_row(&i["changes"]),
                    &json_row(&i["pk"]),
                    &strings(&i["pkCols"]),
                ),
                "delete" => build_delete(driver, schema, table, &strings(&i["pkCols"]), &json_row(&i["pk"])),
                "insert" => build_insert(driver, schema, table, &json_row(&i["values"])),
                other => panic!("unknown edit kind {other}"),
            };
            let mut got = json!({ "kind": edit["kind"], "input": edit["input"] });
            match built {
                Ok((sql, args)) => {
                    got["sql"] = json!(sql);
                    if !args.is_empty() {
                        got["args"] = serde_json::to_value(&args).expect("args");
                    }
                }
                Err(err) => got["error"] = json!(err),
            }
            if &got != edit {
                failures.push(format!("{name}\n  want: {edit}\n  got:  {got}"));
            }
        }
    }
    assert_all("row_edit_sql", failures);
}

// Numbers compare by value, as f64.
fn normalized(v: &Json) -> Json {
    match v {
        Json::Number(n) => json!(n.as_f64()),
        Json::Array(items) => Json::Array(items.iter().map(normalized).collect()),
        Json::Object(map) => Json::Object(map.iter().map(|(k, v)| (k.clone(), normalized(v))).collect()),
        other => other.clone(),
    }
}

fn first_difference(path: &str, expected: &Json, actual: &Json) -> Option<String> {
    match (expected, actual) {
        (Json::Object(a), Json::Object(b)) => {
            for key in a.keys().chain(b.keys()) {
                let (x, y) = (a.get(key).unwrap_or(&Json::Null), b.get(key).unwrap_or(&Json::Null));
                if let Some(diff) = first_difference(&format!("{path}.{key}"), x, y) {
                    return Some(diff);
                }
            }
            None
        }
        (Json::Array(a), Json::Array(b)) if a.len() == b.len() => {
            a.iter().zip(b).enumerate().find_map(|(i, (x, y))| first_difference(&format!("{path}[{i}]"), x, y))
        }
        _ if expected == actual => None,
        _ => Some(format!("{path}: expected {expected} actual {actual}")),
    }
}

fn to_value(v: &Json) -> Value {
    match v {
        Json::Null => Value::Null,
        Json::Bool(b) => Value::Bool(*b),
        Json::Number(n) => n.as_i64().map(Value::Int).unwrap_or_else(|| Value::Float(n.as_f64().unwrap_or(0.0))),
        Json::String(s) => Value::Text(s.clone()),
        other => Value::Text(other.to_string()),
    }
}

#[test]
fn stored_explain_output_parses_into_the_same_plans() {
    let mut failures = Vec::new();
    for engine in ["postgres", "mysql", "mariadb", "sqlite"] {
        let stored = fixture(&format!("explain/{engine}.json"));
        let driver = DriverType::parse(stored["driver"].as_str().expect("driver"));
        for entry in stored["plans"].as_array().expect("plans") {
            let rows = PlanRows {
                columns: strings(&entry["raw"]["columns"]),
                rows: entry["raw"]["rows"]
                    .as_array()
                    .expect("rows")
                    .iter()
                    .map(|row| row.as_array().expect("row").iter().map(to_value).collect())
                    .collect(),
            };
            let stmt = entry["statement"].as_str().expect("statement");
            let explain_sql = entry["explainSql"].as_str().expect("explainSql");
            let analyze = entry["analyze"].as_bool().expect("analyze");
            let got = match parse_plan(&driver, stmt, explain_sql, analyze, &rows) {
                Ok(plan) => serde_json::to_value(&plan).expect("plan json"),
                Err(err) => json!({ "error": err }),
            };
            if let Some(diff) = first_difference("", &normalized(&entry["plan"]), &normalized(&got)) {
                failures.push(format!("{engine} {stmt} (analyze={analyze}): {diff}"));
            }
        }
    }
    assert_all("explain", failures);
}
