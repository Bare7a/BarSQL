use std::collections::HashMap;
use std::sync::LazyLock;

use barsql_core::{DriverType, SqlDialect, Value};
use regex::Regex;
use serde::Serialize;
use serde_json::{Map, Value as Json};

use crate::float_format::format_float_g;

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct PlanField {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanNode {
    pub label: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub detail: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub relation: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub index: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_total: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_self: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rows_planned: Option<f64>,
    // Totals across every loop, not the per-loop averages Postgres and MariaDB report.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rows_actual: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loops: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub self_time_ms: Option<f64>,
    #[serde(skip_serializing_if = "is_false")]
    pub never_run: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<PlanField>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<PlanNode>,
}

pub const NOTE_NO_METRICS: &str = "noMetrics";
pub const NOTE_ROLLED_BACK: &str = "rolledBack";
pub const NOTE_TAB_TRANSACTION: &str = "tabTransaction";

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryPlan {
    pub driver: DriverType,
    pub statement: String,
    pub explain_sql: String,
    pub analyzed: bool,
    pub nodes: Vec<PlanNode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_cost: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub planning_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_ms: Option<f64>,
    pub duration_ms: i64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    pub raw: String,
}

impl QueryPlan {
    pub fn add_note(&mut self, code: &str) {
        if !self.has_note(code) {
            self.notes.push(code.to_string());
        }
    }

    pub fn has_note(&self, code: &str) -> bool {
        self.notes.iter().any(|n| n == code)
    }
}

// One text column for the JSON formats and MySQL's tree. SQLite has id, parent, notused and detail.
#[derive(Debug, Clone, Default)]
pub struct PlanRows {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
}

pub fn parse_plan(
    driver: &DriverType,
    stmt: &str,
    explain_sql: &str,
    analyze: bool,
    res: &PlanRows,
) -> Result<QueryPlan, String> {
    if res.rows.is_empty() {
        return Err("the server returned no plan".into());
    }
    let mut plan = QueryPlan {
        driver: driver.clone(),
        statement: stmt.to_string(),
        explain_sql: explain_sql.to_string(),
        analyzed: analyze,
        raw: join_first_column(res),
        ..Default::default()
    };
    match driver.dialect() {
        Some(SqlDialect::Postgres) => parse_postgres_plan(&mut plan)?,
        Some(SqlDialect::MySql) => parse_mysql_plan(&mut plan)?,
        Some(SqlDialect::Sqlite) => parse_sqlite_plan(&mut plan, res),
        Some(SqlDialect::ClickHouse) => parse_clickhouse_plan(&mut plan)?,
        Some(SqlDialect::TSql) => crate::plan_showplan::parse_showplan(&mut plan, res)?,
        None => return Err(format!("unsupported driver: {driver}")),
    }
    if plan.nodes.is_empty() {
        return Err("the server returned no plan".into());
    }
    Ok(plan)
}

// For trees whose shape is only known while scanning. A node is finalized after its children.
#[derive(Default)]
struct PlanTree {
    nodes: Vec<(PlanNode, Vec<usize>)>,
}

impl PlanTree {
    fn add(&mut self, node: PlanNode) -> usize {
        self.nodes.push((node, Vec::new()));
        self.nodes.len() - 1
    }

    fn attach(&mut self, parent: usize, child: usize) {
        self.nodes[parent].1.push(child);
    }

    fn finish(&mut self, ix: usize) -> PlanNode {
        let children = std::mem::take(&mut self.nodes[ix].1);
        let mut node = std::mem::take(&mut self.nodes[ix].0);
        node.children.extend(children.into_iter().map(|c| self.finish(c)));
        finalize_node(&mut node);
        node
    }
}

// Fills in whichever of the inclusive or exclusive metrics the engine left out. Clamped at zero because
// Postgres leaves InitPlan and SubPlan children out of the parent's total.
pub(crate) fn finalize_node(n: &mut PlanNode) {
    let child_cost: Vec<f64> = n.children.iter().filter_map(|c| c.cost_total).collect();
    let child_time: Vec<f64> = n.children.iter().filter_map(|c| c.time_ms).collect();
    (n.cost_total, n.cost_self) =
        derive_totals(n.cost_total, n.cost_self, child_cost.iter().sum(), !child_cost.is_empty());
    (n.time_ms, n.self_time_ms) =
        derive_totals(n.time_ms, n.self_time_ms, child_time.iter().sum(), !child_time.is_empty());
}

fn derive_totals(
    total: Option<f64>,
    self_value: Option<f64>,
    child_sum: f64,
    have_children: bool,
) -> (Option<f64>, Option<f64>) {
    match (total, self_value) {
        (Some(total), None) => (Some(total), Some((total - child_sum).max(0.0))),
        (None, Some(own)) => (Some(own + child_sum), Some(own)),
        (None, None) if have_children => (Some(child_sum), Some(0.0)),
        other => other,
    }
}

const PG_FIRST_CLASS_KEYS: &[&str] = &[
    "Node Type",
    "Plans",
    "Relation Name",
    "Alias",
    "Index Name",
    "Total Cost",
    "Plan Rows",
    "Join Type",
    "Strategy",
    "Subplan Name",
    "Parallel Aware",
    "Actual Total Time",
    "Actual Rows",
    "Actual Loops",
];

// Most specific clause first.
const PG_DETAIL_KEYS: &[&str] = &[
    "Index Cond",
    "Recheck Cond",
    "TID Cond",
    "Hash Cond",
    "Merge Cond",
    "Join Filter",
    "Filter",
    "One-Time Filter",
    "Sort Key",
    "Group Key",
    "Presorted Key",
    "Cache Key",
    "Function Call",
    "Table Function Name",
    "CTE Name",
    "Conflict Filter",
];

fn parse_postgres_plan(plan: &mut QueryPlan) -> Result<(), String> {
    let envelopes: Vec<Json> =
        serde_json::from_str(&plan.raw).map_err(|err| format!("could not read the Postgres plan: {err}"))?;
    for env in envelopes.iter().filter_map(Json::as_object) {
        let Some(root) = env.get("Plan").and_then(Json::as_object) else {
            continue;
        };
        plan.nodes.push(pg_node(root));
        if let Some(v) = opt_float(env.get("Planning Time")) {
            plan.planning_ms = Some(v);
        }
        if let Some(v) = opt_float(env.get("Execution Time")) {
            plan.execution_ms = Some(v);
        }
    }
    plan.total_cost = plan.nodes.first().and_then(|n| n.cost_total);
    Ok(())
}

fn pg_node(raw: &Map<String, Json>) -> PlanNode {
    let (detail_key, detail) = first_string_field(raw, PG_DETAIL_KEYS);
    let mut n = PlanNode {
        label: pg_label(raw),
        detail,
        relation: pg_relation(raw),
        index: plan_string(raw.get("Index Name")),
        cost_total: opt_float(raw.get("Total Cost")),
        rows_planned: opt_float(raw.get("Plan Rows")),
        fields: plan_fields(Some(raw), PG_FIRST_CLASS_KEYS, detail_key),
        ..Default::default()
    };
    // Postgres reports per-loop averages. Multiply by loops so nested-loop nodes compare fairly.
    let mut loops = 1.0;
    if let Some(v) = plan_float(raw.get("Actual Loops")) {
        n.loops = Some(v);
        n.never_run = v == 0.0;
        loops = v;
    }
    if let Some(v) = plan_float(raw.get("Actual Total Time")) {
        n.time_ms = Some(v * loops);
    }
    if let Some(v) = plan_float(raw.get("Actual Rows")) {
        n.rows_actual = Some(v * loops);
    }
    n.children = object_slice(raw.get("Plans")).into_iter().map(pg_node).collect();
    finalize_node(&mut n);
    n
}

fn pg_label(raw: &Map<String, Json>) -> String {
    let mut label = plan_string(raw.get("Node Type"));
    if label.is_empty() {
        label = "Node".into();
    }
    let join = plan_string(raw.get("Join Type"));
    if !join.is_empty() && join != "Inner" {
        label = format!("{label} ({join})");
    }
    let strategy = plan_string(raw.get("Strategy"));
    if !strategy.is_empty() && strategy != "Plain" {
        label = format!("{label} ({strategy})");
    }
    if raw.get("Parallel Aware") == Some(&Json::Bool(true)) {
        label = format!("Parallel {label}");
    }
    let subplan = plan_string(raw.get("Subplan Name"));
    if !subplan.is_empty() {
        label = format!("{subplan}: {label}");
    }
    label
}

fn pg_relation(raw: &Map<String, Json>) -> String {
    let relation = plan_string(raw.get("Relation Name"));
    let alias = plan_string(raw.get("Alias"));
    if alias.is_empty() || alias == relation {
        relation
    } else if relation.is_empty() {
        alias
    } else {
        format!("{relation} {alias}")
    }
}

const CH_FIRST_CLASS_KEYS: &[&str] = &["Node Type", "Node Id", "Description", "Plans", "Indexes"];

// EXPLAIN PLAN json = 1, indexes = 1. ClickHouse estimates no costs or rows, so a plan is its steps and how
// far each index narrowed the read.
fn parse_clickhouse_plan(plan: &mut QueryPlan) -> Result<(), String> {
    let envelopes: Vec<Json> =
        serde_json::from_str(&plan.raw).map_err(|err| format!("could not read the ClickHouse plan: {err}"))?;
    for env in envelopes.iter().filter_map(Json::as_object) {
        if let Some(root) = env.get("Plan").and_then(Json::as_object) {
            plan.nodes.push(ch_node(root));
        }
    }
    plan.add_note(NOTE_NO_METRICS);
    Ok(())
}

fn ch_node(raw: &Map<String, Json>) -> PlanNode {
    let label = plan_string(raw.get("Node Type"));
    let description = plan_string(raw.get("Description"));
    // A ReadFrom step's description names the table it reads.
    let reads = label.starts_with("ReadFrom");
    let indexes = object_slice(raw.get("Indexes"));
    let mut fields = plan_fields(Some(raw), CH_FIRST_CLASS_KEYS, "");
    fields.extend(indexes.iter().map(|index| PlanField { key: ch_index_name(index), value: ch_index_summary(index) }));
    let named: Vec<String> = indexes.iter().map(|i| ch_index_name(i)).collect();
    PlanNode {
        label: if label.is_empty() { "Step".into() } else { label },
        detail: if reads { String::new() } else { description.clone() },
        relation: if reads { description } else { String::new() },
        index: named.join(", "),
        fields,
        children: object_slice(raw.get("Plans")).into_iter().map(ch_node).collect(),
        ..Default::default()
    }
}

// PrimaryKey, or a skip index by name.
fn ch_index_name(index: &Map<String, Json>) -> String {
    let (kind, name) = (plan_string(index.get("Type")), plan_string(index.get("Name")));
    if name.is_empty() { kind } else { name }
}

// Keys and condition, then how many parts and granules the index kept of those it was given.
fn ch_index_summary(index: &Map<String, Json>) -> String {
    let mut parts = Vec::new();
    let keys: Vec<String> = index.get("Keys").and_then(Json::as_array).into_iter().flatten().map(json_string).collect();
    if !keys.is_empty() {
        parts.push(keys.join(", "));
    }
    for key in ["Condition", "Description"] {
        let text = plan_string(index.get(key));
        if !text.is_empty() {
            parts.push(text);
        }
    }
    for (unit, selected, initial) in
        [("parts", "Selected Parts", "Initial Parts"), ("granules", "Selected Granules", "Initial Granules")]
    {
        if let (Some(selected), Some(initial)) = (opt_float(index.get(selected)), opt_float(index.get(initial))) {
            parts.push(format!("{selected} of {initial} {unit}"));
        }
    }
    parts.join(" · ")
}

fn parse_mysql_plan(plan: &mut QueryPlan) -> Result<(), String> {
    if plan.raw.trim().starts_with('{') {
        return parse_mysql_json_plan(plan);
    }
    parse_mysql_tree_plan(plan);
    Ok(())
}

// Anything unlisted falls back to the humanized key.
fn mysql_op_label(key: &str) -> Option<&'static str> {
    Some(match key {
        "query_block" => "Query block",
        "table" => "Table",
        "nested_loop" => "Nested loop",
        "ordering_operation" => "Ordering",
        "grouping_operation" => "Grouping",
        "duplicates_removal" => "Duplicates removal",
        "materialized_from_subquery" => "Materialized subquery",
        "union_result" => "Union result",
        "query_specifications" => "Query specification",
        "buffer_result" => "Buffer result",
        "block-nl-join" => "Block nested loop join",
        "read_sorted_file" => "Read sorted file",
        "temporary_table" => "Temporary table",
        "attached_subqueries" => "Attached subquery",
        "select_list_subqueries" => "Select list subquery",
        "having_subqueries" => "Having subquery",
        "optimized_away_subqueries" => "Optimized-away subquery",
        "update_value_subqueries" => "Update value subquery",
        "subqueries" => "Subquery",
        "insert_from" => "Insert from",
        _ => return None,
    })
}

const MYSQL_FIRST_CLASS_KEYS: &[&str] = &[
    "table_name",
    "access_type",
    "key",
    "cost_info",
    "cost",
    "rows_produced_per_join",
    "rows",
    "r_rows",
    "r_loops",
    "r_total_time_ms",
    "r_table_time_ms",
    "r_other_time_ms",
];

const MYSQL_DETAIL_KEYS: &[&str] = &["attached_condition", "index_condition"];

fn parse_mysql_json_plan(plan: &mut QueryPlan) -> Result<(), String> {
    let root: Map<String, Json> =
        serde_json::from_str(&plan.raw).map_err(|err| format!("could not read the MySQL plan: {err}"))?;
    for key in sorted_keys(&root) {
        if let Some(obj) = root[key].as_object() {
            plan.nodes.push(mysql_node(key, obj));
        }
    }
    if let Some(first) = plan.nodes.first() {
        plan.total_cost = first.cost_total;
        plan.execution_ms = first.time_ms;
    }
    Ok(())
}

fn mysql_node(key: &str, raw: &Map<String, Json>) -> PlanNode {
    let (detail_key, detail) = first_string_field(raw, MYSQL_DETAIL_KEYS);
    let mut n = PlanNode {
        label: mysql_label(key, raw),
        detail,
        relation: plan_string(raw.get("table_name")),
        index: plan_string(raw.get("key")),
        fields: plan_fields(Some(raw), MYSQL_FIRST_CLASS_KEYS, detail_key),
        ..Default::default()
    };
    mysql_cost(&mut n, raw);
    mysql_rows_and_time(&mut n, raw);
    n.children = mysql_children(raw);
    finalize_node(&mut n);
    n
}

// MySQL nests cost under cost_info while MariaDB 11 reports a flat per-node cost. prefix_cost is a running
// total for the join prefix and double-counts if summed, so use read + eval as the node's own cost.
fn mysql_cost(n: &mut PlanNode, raw: &Map<String, Json>) {
    let info = raw.get("cost_info").and_then(Json::as_object);
    n.fields.extend(plan_fields(info, &[], ""));
    if let Some(flat) = opt_float(raw.get("cost")) {
        n.cost_self = Some(flat);
        return;
    }
    let Some(info) = info else {
        return;
    };
    n.cost_total = opt_float(info.get("query_cost"));
    let read = plan_float(info.get("read_cost"));
    let eval = plan_float(info.get("eval_cost"));
    if read.is_some() || eval.is_some() {
        n.cost_self = Some(read.unwrap_or(0.0) + eval.unwrap_or(0.0));
    }
}

fn mysql_rows_and_time(n: &mut PlanNode, raw: &Map<String, Json>) {
    n.rows_planned =
        ["rows_produced_per_join", "rows_examined_per_scan", "rows"].iter().find_map(|key| opt_float(raw.get(*key)));
    // MariaDB's r_rows is per loop like Postgres, but its time counters are already totals.
    let mut loops = 1.0;
    if let Some(v) = plan_float(raw.get("r_loops")) {
        n.loops = Some(v);
        n.never_run = v == 0.0;
        loops = v;
    }
    if let Some(v) = plan_float(raw.get("r_rows")) {
        n.rows_actual = Some(v * loops);
    }
    if let Some(v) = opt_float(raw.get("r_total_time_ms")) {
        n.time_ms = Some(v);
        return;
    }
    let table = plan_float(raw.get("r_table_time_ms"));
    let other = plan_float(raw.get("r_other_time_ms"));
    if table.is_some() || other.is_some() {
        n.self_time_ms = Some(table.unwrap_or(0.0) + other.unwrap_or(0.0));
    }
}

fn mysql_label(key: &str, raw: &Map<String, Json>) -> String {
    let mut label = mysql_op_label(key).map_or_else(|| humanize_key(key), str::to_string);
    let access = plan_string(raw.get("access_type"));
    if !access.is_empty() {
        label = format!("{label} ({access})");
    }
    if key == "query_block" {
        let id = plan_string(raw.get("select_id"));
        if !id.is_empty() {
            label = format!("{label} #{id}");
        }
    }
    label
}

// Each nested object is a child. An array of objects becomes one grouping node.
fn mysql_children(raw: &Map<String, Json>) -> Vec<PlanNode> {
    let mut children = Vec::new();
    for key in sorted_keys(raw) {
        if key == "cost_info" {
            continue;
        }
        match &raw[key] {
            Json::Object(obj) => children.push(mysql_node(key, obj)),
            Json::Array(arr) => children.extend(mysql_group_node(key, arr)),
            _ => {}
        }
    }
    children
}

// None for scalar arrays like possible_keys and used_columns, which plan_fields shows instead.
fn mysql_group_node(key: &str, arr: &[Json]) -> Option<PlanNode> {
    let objects: Vec<&Map<String, Json>> = arr.iter().filter_map(Json::as_object).collect();
    if objects.is_empty() || objects.len() != arr.len() {
        return None;
    }
    let label = mysql_op_label(key).map_or_else(|| humanize_key(key), str::to_string);
    let mut group = PlanNode { label, ..Default::default() };
    for obj in objects {
        // Single-key envelopes are unwrapped so the child is labelled by its operation.
        match sole_object_entry(obj) {
            Some((inner_key, inner)) => group.children.push(mysql_node(inner_key, inner)),
            None => group.children.push(mysql_node(key, obj)),
        }
    }
    finalize_node(&mut group);
    Some(group)
}

static TREE_LINE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^([\t\n\f\r ]*)->[\t\n\f\r ]*(.*)$").unwrap());
static TREE_COST: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\(cost=([0-9.eE+-]+)(?:\.\.([0-9.eE+-]+))?[\t\n\f\r ]+rows=([0-9.eE+-]+)\)").unwrap()
});
static TREE_ACTUAL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"\(actual time=([0-9.eE+-]+)\.\.([0-9.eE+-]+)[\t\n\f\r ]+rows=([0-9.eE+-]+)[\t\n\f\r ]+loops=([0-9.eE+-]+)\)",
    )
    .unwrap()
});
static TREE_NEVER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\(never executed\)").unwrap());
static TREE_ON: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?-u:\b)on[\t\n\f\r ]+([^\t\n\f\r (]+)").unwrap());
static TREE_USING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?-u:\b)using[\t\n\f\r ]+([^\t\n\f\r (]+)").unwrap());

fn parse_mysql_tree_plan(plan: &mut QueryPlan) {
    let mut tree = PlanTree::default();
    let mut roots = Vec::new();
    let mut stack: Vec<(usize, usize)> = Vec::new();
    for line in plan.raw.split('\n') {
        if line.trim().is_empty() {
            continue;
        }
        let (indent, text) = tree_line_parts(line);
        let ix = tree.add(tree_node(text));
        while stack.last().is_some_and(|&(depth, _)| depth >= indent) {
            stack.pop();
        }
        match stack.last() {
            Some(&(_, parent)) => tree.attach(parent, ix),
            None => roots.push(ix),
        }
        stack.push((indent, ix));
    }
    plan.nodes = roots.into_iter().map(|ix| tree.finish(ix)).collect();
    if let Some(first) = plan.nodes.first() {
        plan.total_cost = first.cost_total;
        plan.execution_ms = first.time_ms;
    }
}

// A line without the "-> " marker belongs to the root, at indent 0.
fn tree_line_parts(line: &str) -> (usize, &str) {
    match TREE_LINE.captures(line) {
        Some(caps) => (caps[1].len(), caps.get(2).map_or("", |m| m.as_str()).trim()),
        None => (0, line.trim()),
    }
}

fn tree_node(text: &str) -> PlanNode {
    let mut n = PlanNode::default();
    let mut text = text.to_string();
    if let Some(caps) = TREE_COST.captures(&text) {
        n.cost_total = caps[1].parse().ok();
        n.rows_planned = caps[3].parse().ok();
        let matched = caps[0].to_string();
        text = text.replacen(&matched, "", 1);
    }
    if let Some(caps) = TREE_ACTUAL.captures(&text) {
        let mut loops = 1.0;
        if let Ok(v) = caps[4].parse::<f64>() {
            n.loops = Some(v);
            loops = v;
        }
        // actual time=first-row..last-row, both per loop.
        if let Ok(v) = caps[2].parse::<f64>() {
            n.time_ms = Some(v * loops);
        }
        if let Ok(v) = caps[3].parse::<f64>() {
            n.rows_actual = Some(v * loops);
        }
        let matched = caps[0].to_string();
        text = text.replacen(&matched, "", 1);
    }
    if let Some(found) = TREE_NEVER.find(&text).map(|m| m.as_str().to_string()) {
        n.never_run = true;
        text = text.replacen(&found, "", 1);
    }
    (n.label, n.detail) = split_label_detail(text.trim());
    // Match against the label only, since a filter clause can contain "on" or "using".
    if let Some(caps) = TREE_ON.captures(&n.label) {
        n.relation = caps[1].to_string();
    }
    if let Some(caps) = TREE_USING.captures(&n.label) {
        n.index = caps[1].to_string();
    }
    n
}

fn split_label_detail(text: &str) -> (String, String) {
    match text.find(": ") {
        Some(ix) if ix > 0 => (text[..ix].trim().to_string(), text[ix + 2..].trim().to_string()),
        _ => (text.to_string(), String::new()),
    }
}

static SQLITE_SCAN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^(SCAN|SEARCH)[\t\n\f\r ]+(?:TABLE[\t\n\f\r ]+|SUBQUERY[\t\n\f\r ]+)?([^\t\n\f\r ]+)[\t\n\f\r ]*(.*)$",
    )
    .unwrap()
});
static SQLITE_INDEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(?-u:\b)USING[\t\n\f\r ]+(?:COVERING[\t\n\f\r ]+)?(?:AUTOMATIC[\t\n\f\r ]+)?(?:PARTIAL[\t\n\f\r ]+)?INDEX[\t\n\f\r ]+([^\t\n\f\r (]+)",
    )
    .unwrap()
});

// SQLite only reports the plan shape. The tree comes from the id and parent columns.
fn parse_sqlite_plan(plan: &mut QueryPlan, res: &PlanRows) {
    let id_col = column_index(res, "id", 0);
    let parent_col = column_index(res, "parent", 1);
    let detail_col = column_index(res, "detail", 3);
    let mut tree = PlanTree::default();
    let mut by_id: HashMap<i64, usize> = HashMap::new();
    let mut roots = Vec::new();
    let mut raw = String::new();
    for row in &res.rows {
        let id = value_float(row.get(id_col)).unwrap_or(0.0) as i64;
        let parent = value_float(row.get(parent_col)).unwrap_or(0.0) as i64;
        let detail = value_string(row.get(detail_col));
        let ix = tree.add(sqlite_node(&detail));
        by_id.insert(id, ix);
        match by_id.get(&parent) {
            Some(&p) if parent != id => tree.attach(p, ix),
            _ => roots.push(ix),
        }
        raw.push_str(&format!("{id}|{parent}|{detail}\n"));
    }
    plan.nodes = roots.into_iter().map(|ix| tree.finish(ix)).collect();
    plan.raw = raw.trim_end_matches('\n').to_string();
    plan.add_note(NOTE_NO_METRICS);
}

fn sqlite_node(detail: &str) -> PlanNode {
    let Some(caps) = SQLITE_SCAN.captures(detail.trim()) else {
        return PlanNode { label: detail.to_string(), ..Default::default() };
    };
    let mut n = PlanNode {
        label: caps[1].to_uppercase(),
        relation: caps[2].to_string(),
        detail: caps[3].trim().to_string(),
        ..Default::default()
    };
    if let Some(index) = SQLITE_INDEX.captures(&n.detail) {
        n.index = index[1].to_string();
    }
    n
}

// Scalars not in `skip` or `also_skip`. Objects and arrays of objects are children, not fields.
fn plan_fields(raw: Option<&Map<String, Json>>, skip: &[&str], also_skip: &str) -> Vec<PlanField> {
    let Some(raw) = raw else {
        return Vec::new();
    };
    let mut fields = Vec::new();
    for key in sorted_keys(raw) {
        if skip.contains(&key) || (!also_skip.is_empty() && key == also_skip) {
            continue;
        }
        let value = &raw[key];
        let skip_value = match value {
            Json::Null | Json::Object(_) => true,
            Json::Array(arr) => arr.iter().any(Json::is_object),
            _ => false,
        };
        if skip_value {
            continue;
        }
        let text = json_string(value);
        if !text.is_empty() {
            fields.push(PlanField { key: humanize_key(key), value: text });
        }
    }
    fields
}

fn plan_string(v: Option<&Json>) -> String {
    v.map(json_string).unwrap_or_default()
}

fn json_string(v: &Json) -> String {
    match v {
        Json::Null => String::new(),
        Json::String(s) => s.clone(),
        Json::Bool(b) => b.to_string(),
        Json::Number(n) => n.as_f64().map(format_float_g).unwrap_or_else(|| n.to_string()),
        Json::Array(items) => items.iter().map(json_string).filter(|s| !s.is_empty()).collect::<Vec<_>>().join(", "),
        Json::Object(map) => serde_json::to_string(map).unwrap_or_default(),
    }
}

// Engines mix JSON numbers and quoted ones like "1.35".
fn plan_float(v: Option<&Json>) -> Option<f64> {
    match v? {
        Json::Number(n) => n.as_f64(),
        Json::String(s) => parse_float(s.trim()),
        _ => None,
    }
}

fn opt_float(v: Option<&Json>) -> Option<f64> {
    plan_float(v)
}

// Overflow like "1e999" isn't a number here, but a spelled-out "inf" is.
fn parse_float(s: &str) -> Option<f64> {
    let v: f64 = s.parse().ok()?;
    let spelled_inf = s.trim_start_matches(['+', '-']).to_ascii_lowercase().starts_with("inf");
    (!v.is_infinite() || spelled_inf).then_some(v)
}

fn value_float(v: Option<&Value>) -> Option<f64> {
    match v? {
        Value::Int(i) => Some(*i as f64),
        Value::Float(f) => Some(*f),
        Value::Text(s) => parse_float(s.trim()),
        _ => None,
    }
}

fn value_string(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::Text(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(Value::Int(i)) => i.to_string(),
        Some(Value::Float(f)) => format_float_g(*f),
    }
}

fn first_string_field<'a>(raw: &Map<String, Json>, keys: &[&'a str]) -> (&'a str, String) {
    for key in keys {
        let value = plan_string(raw.get(*key));
        if !value.is_empty() {
            return (key, value);
        }
    }
    ("", String::new())
}

fn object_slice(v: Option<&Json>) -> Vec<&Map<String, Json>> {
    match v {
        Some(Json::Array(items)) => items.iter().filter_map(Json::as_object).collect(),
        _ => Vec::new(),
    }
}

fn sole_object_entry(obj: &Map<String, Json>) -> Option<(&str, &Map<String, Json>)> {
    if obj.len() != 1 {
        return None;
    }
    let (key, value) = obj.iter().next()?;
    value.as_object().map(|inner| (key.as_str(), inner))
}

fn sorted_keys(map: &Map<String, Json>) -> Vec<&str> {
    let mut keys: Vec<&str> = map.keys().map(String::as_str).collect();
    keys.sort_unstable();
    keys
}

fn humanize_key(key: &str) -> String {
    let spaced = key.replace(['_', '-'], " ");
    let mut chars = spaced.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

// Both JSON formats arrive as one row. MySQL's tree arrives as one row of newline-separated lines.
fn join_first_column(res: &PlanRows) -> String {
    res.rows.iter().filter_map(|row| row.first()).map(|v| value_string(Some(v))).collect::<Vec<_>>().join("\n")
}

fn column_index(res: &PlanRows, name: &str, fallback: usize) -> usize {
    res.columns.iter().position(|c| c.eq_ignore_ascii_case(name)).unwrap_or(fallback)
}

fn is_false(v: &bool) -> bool {
    !*v
}
