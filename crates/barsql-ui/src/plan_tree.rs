use std::collections::HashSet;

use barsql_sql::QueryPlan;
use barsql_sql::plan::PlanNode;

use crate::i18n::format_number;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Metric {
    Time,
    Cost,
    Rows,
}

pub const METRICS: [Metric; 3] = [Metric::Time, Metric::Cost, Metric::Rows];

// An estimate off by this factor either way means the planner was guessing.
const ESTIMATE_WARN_FACTOR: f64 = 10.;

impl Metric {
    pub fn key(self) -> &'static str {
        match self {
            Self::Time => "time",
            Self::Cost => "cost",
            Self::Rows => "rows",
        }
    }
}

pub struct PlanRow<'a> {
    pub node: &'a PlanNode,
    // Child-index path like "0.1.2". Stable, and used as the collapse key.
    pub key: String,
    pub depth: usize,
    pub has_children: bool,
    // 0..1, relative to the largest own metric in the plan.
    pub heat: f64,
    pub hottest: bool,
    pub estimate_factor: Option<f64>,
}

// Own share only, so a parent isn't hot just because its children are.
fn heat_value(node: &PlanNode, metric: Metric) -> Option<f64> {
    match metric {
        Metric::Time => node.self_time_ms,
        Metric::Cost => node.cost_self,
        Metric::Rows => node.rows_actual.or(node.rows_planned),
    }
}

fn has_metric(nodes: &[PlanNode], metric: Metric) -> bool {
    nodes.iter().any(|node| heat_value(node, metric).is_some() || has_metric(&node.children, metric))
}

// SQLite reports no metrics, and EXPLAIN without ANALYZE has no timings.
pub fn available_metrics(plan: &QueryPlan) -> Vec<Metric> {
    METRICS.into_iter().filter(|&metric| has_metric(&plan.nodes, metric)).collect()
}

pub fn default_metric(plan: &QueryPlan) -> Option<Metric> {
    available_metrics(plan).first().copied()
}

fn max_heat(nodes: &[PlanNode], metric: Metric) -> f64 {
    nodes.iter().fold(0., |max: f64, node| {
        max.max(heat_value(node, metric).unwrap_or(0.)).max(max_heat(&node.children, metric))
    })
}

pub fn estimate_factor(node: &PlanNode) -> Option<f64> {
    match (node.rows_actual, node.rows_planned) {
        (Some(actual), Some(planned)) if planned > 0. => Some(actual / planned),
        _ => None,
    }
}

// A node the executor never reached produced no rows by definition, so its factor says nothing.
pub fn is_estimate_off(factor: Option<f64>, never_run: bool) -> bool {
    match factor {
        Some(factor) if !never_run => factor >= ESTIMATE_WARN_FACTOR || factor <= 1. / ESTIMATE_WARN_FACTOR,
        _ => false,
    }
}

fn child_key(prefix: &str, ix: usize) -> String {
    if prefix.is_empty() { ix.to_string() } else { format!("{prefix}.{ix}") }
}

pub fn collect_parent_keys(nodes: &[PlanNode], prefix: &str) -> Vec<String> {
    let mut keys = Vec::new();
    for (ix, node) in nodes.iter().enumerate() {
        if !node.children.is_empty() {
            let key = child_key(prefix, ix);
            let nested = collect_parent_keys(&node.children, &key);
            keys.push(key);
            keys.extend(nested);
        }
    }
    keys
}

// Heat scales against the whole plan, so collapsing never recolours a row.
pub fn flatten<'a>(plan: &'a QueryPlan, metric: Option<Metric>, collapsed: &HashSet<String>) -> Vec<PlanRow<'a>> {
    let max = metric.map_or(0., |metric| max_heat(&plan.nodes, metric));
    let mut rows = Vec::new();
    fn walk<'a>(
        nodes: &'a [PlanNode],
        depth: usize,
        prefix: &str,
        metric: Option<Metric>,
        max: f64,
        collapsed: &HashSet<String>,
        rows: &mut Vec<PlanRow<'a>>,
    ) {
        for (ix, node) in nodes.iter().enumerate() {
            let key = child_key(prefix, ix);
            let value = metric.and_then(|metric| heat_value(node, metric));
            let (heat, hottest) = match value {
                Some(value) if max > 0. => (value / max, value == max),
                _ => (0., false),
            };
            let open = !node.children.is_empty() && !collapsed.contains(&key);
            rows.push(PlanRow {
                node,
                key: key.clone(),
                depth,
                has_children: !node.children.is_empty(),
                heat,
                hottest,
                estimate_factor: estimate_factor(node),
            });
            if open {
                walk(&node.children, depth + 1, &key, metric, max, collapsed, rows);
            }
        }
    }
    walk(&plan.nodes, 0, "", metric, max, collapsed, &mut rows);
    rows
}

// Sub-millisecond timings keep three decimals. 1000 ms and up switch to seconds.
pub fn format_ms(ms: Option<f64>, lang: &str) -> String {
    match ms {
        Some(ms) if ms.is_finite() && ms >= 1000. => format!("{} s", format_number(ms / 1000., 2, lang)),
        Some(ms) if ms.is_finite() && ms >= 1. => format!("{} ms", format_number(ms, 1, lang)),
        Some(0.) => "0 ms".into(),
        Some(ms) if ms.is_finite() => format!("{} ms", format_number(ms, 3, lang)),
        _ => String::new(),
    }
}

// Whole counts stay exact since spotting a bad estimate is what the column is for.
pub fn format_rows(rows: Option<f64>, lang: &str) -> String {
    match rows {
        Some(rows) if rows.is_finite() && rows.fract() == 0. => format_number(rows, 0, lang),
        Some(rows) if rows.is_finite() => format_number(rows, 1, lang),
        _ => String::new(),
    }
}

pub fn format_cost(cost: Option<f64>, lang: &str) -> String {
    match cost {
        Some(cost) if cost.is_finite() && cost >= 1000. => format_number(cost, 0, lang),
        Some(cost) if cost.is_finite() => format_number(cost, 2, lang),
        _ => String::new(),
    }
}

// Multiplier like "12x" or "0.05x".
pub fn format_factor(factor: Option<f64>, lang: &str) -> String {
    match factor {
        Some(factor) if factor.is_finite() && factor >= 10. => format!("{}x", format_number(factor, 0, lang)),
        Some(factor) if factor.is_finite() => {
            format!("{}x", format_number(factor, if factor < 1. { 2 } else { 1 }, lang))
        }
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(label: &str) -> PlanNode {
        PlanNode { label: label.into(), ..Default::default() }
    }

    fn plan(nodes: Vec<PlanNode>, analyzed: bool) -> QueryPlan {
        QueryPlan { analyzed, nodes, ..Default::default() }
    }

    // Three nodes. The Seq Scan child dominates time and cost.
    fn measured() -> QueryPlan {
        let scan = |label: &str, relation: &str, time: f64, cost: f64| PlanNode {
            relation: relation.into(),
            self_time_ms: Some(time),
            time_ms: Some(time),
            cost_self: Some(cost),
            cost_total: Some(cost),
            rows_actual: Some(9.),
            ..node(label)
        };
        plan(
            vec![PlanNode {
                self_time_ms: Some(2.),
                time_ms: Some(12.),
                cost_self: Some(5.),
                cost_total: Some(40.),
                rows_actual: Some(9.),
                rows_planned: Some(10.),
                children: vec![scan("Seq Scan", "orders", 8., 30.), scan("Index Scan", "customers", 2., 5.)],
                ..node("Nested Loop")
            }],
            true,
        )
    }

    #[test]
    fn metrics_are_the_ones_the_engine_reported() {
        assert_eq!(available_metrics(&measured()), METRICS);
        let estimated =
            plan(vec![PlanNode { cost_self: Some(3.), rows_planned: Some(100.), ..node("Seq Scan") }], false);
        assert_eq!(available_metrics(&estimated), [Metric::Cost, Metric::Rows]);
        assert_eq!(default_metric(&estimated), Some(Metric::Cost));
        let shape = plan(vec![PlanNode { children: vec![node("SCAN")], ..node("SCAN") }], false);
        assert!(available_metrics(&shape).is_empty() && default_metric(&shape).is_none());
        let nested = plan(
            vec![PlanNode { children: vec![PlanNode { cost_self: Some(1.), ..node("Seq Scan") }], ..node("Result") }],
            true,
        );
        assert_eq!(available_metrics(&nested), [Metric::Cost]);
    }

    #[test]
    fn rows_walk_depth_first_with_path_keys_and_own_heat() {
        let plan = measured();
        let rows = flatten(&plan, Some(Metric::Time), &HashSet::new());
        assert_eq!(rows.iter().map(|r| r.key.as_str()).collect::<Vec<_>>(), ["0", "0.0", "0.1"]);
        assert_eq!(rows.iter().map(|r| r.depth).collect::<Vec<_>>(), [0, 1, 1]);
        assert_eq!(rows.iter().map(|r| r.heat).collect::<Vec<_>>(), [2. / 8., 1., 2. / 8.]);
        assert_eq!(rows.iter().filter(|r| r.hottest).map(|r| r.node.label.as_str()).collect::<Vec<_>>(), ["Seq Scan"]);
        let by_cost = flatten(&plan, Some(Metric::Cost), &HashSet::new());
        assert_eq!(by_cost.iter().map(|r| r.heat).collect::<Vec<_>>(), [5. / 30., 1., 5. / 30.]);
        let collapsed = flatten(&plan, Some(Metric::Time), &HashSet::from(["0".to_string()]));
        assert_eq!(collapsed.len(), 1);
        assert_eq!(
            (collapsed[0].heat, collapsed[0].has_children),
            (2. / 8., true),
            "the hidden scan still scales the heat"
        );
    }

    #[test]
    fn rows_stay_cold_without_metrics_and_fall_back_to_estimates() {
        let shape = plan(vec![PlanNode { children: vec![node("SEARCH")], ..node("SCAN") }], false);
        let rows = flatten(&shape, None, &HashSet::new());
        assert!(rows.iter().all(|r| r.heat == 0. && !r.hottest));
        let estimated = plan(
            vec![
                PlanNode { rows_planned: Some(50.), ..node("Seq Scan") },
                PlanNode { rows_planned: Some(100.), ..node("Index Scan") },
            ],
            false,
        );
        let heat: Vec<f64> = flatten(&estimated, Some(Metric::Rows), &HashSet::new()).iter().map(|r| r.heat).collect();
        assert_eq!(heat, [0.5, 1.]);
        assert!(flatten(&plan(Vec::new(), true), Some(Metric::Time), &HashSet::new()).is_empty());
    }

    #[test]
    fn parent_keys_are_the_collapsible_nodes() {
        assert_eq!(collect_parent_keys(&measured().nodes, ""), ["0"]);
        let nested =
            vec![PlanNode { children: vec![PlanNode { children: vec![node("c")], ..node("b") }], ..node("a") }];
        assert_eq!(collect_parent_keys(&nested, ""), ["0", "0.0"]);
    }

    #[test]
    fn estimates_off_by_ten_times_are_flagged() {
        let rows = |actual: Option<f64>, planned: Option<f64>| PlanNode {
            rows_actual: actual,
            rows_planned: planned,
            ..node("n")
        };
        assert_eq!(estimate_factor(&rows(Some(500.), Some(10.))), Some(50.));
        assert_eq!(estimate_factor(&rows(Some(500.), None)), None);
        assert_eq!(estimate_factor(&rows(None, Some(10.))), None);
        assert_eq!(estimate_factor(&rows(Some(5.), Some(0.))), None);
        let off = |factor: Option<f64>| is_estimate_off(factor, false);
        assert_eq!(
            [off(Some(1.)), off(Some(9.)), off(Some(10.)), off(Some(0.1)), off(Some(0.5)), off(None)],
            [false, false, true, true, false, false]
        );
        assert!(!is_estimate_off(Some(0.), true) && is_estimate_off(Some(0.), false));
    }

    #[test]
    fn metrics_format_for_display() {
        let en = "en";
        assert_eq!(
            [Some(0.), Some(0.0216), Some(4.27), Some(2500.), None].map(|ms| format_ms(ms, en)),
            ["0 ms", "0.022 ms", "4.3 ms", "2.5 s", ""]
        );
        assert_eq!(
            [Some(0.), Some(1234.), Some(2.25), None].map(|rows| format_rows(rows, en)),
            ["0", "1,234", "2.3", ""]
        );
        assert_eq!(
            [Some(18.1), Some(0.295), Some(12345.6), None].map(|cost| format_cost(cost, en)),
            ["18.1", "0.3", "12,346", ""]
        );
        assert_eq!(
            [Some(50.), Some(2.5), Some(0.05), None].map(|f| format_factor(f, en)),
            ["50x", "2.5x", "0.05x", ""]
        );
    }
}
