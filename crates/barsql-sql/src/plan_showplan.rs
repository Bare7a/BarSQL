// SQL Server's XML plans, as SHOWPLAN_XML (estimated) and STATISTICS XML (measured) give them. Elements are
// matched by local name, so the namespace and the schema version don't matter.

use roxmltree::{Document, Node};

use crate::plan::{PlanField, PlanNode, PlanRows, QueryPlan, finalize_node};

// Operator children that describe the operator rather than name it.
const GENERIC: &[&str] =
    &["OutputList", "Warnings", "MemoryFractions", "RunTimeInformation", "RunTimePartitionSummary", "InternalInfo"];

// Each row holds one XML document, a batch of statements with a plan each.
pub(crate) fn parse_showplan(plan: &mut QueryPlan, rows: &PlanRows) -> Result<(), String> {
    let (mut total_cost, mut planning, mut execution) = (None::<f64>, None::<f64>, None::<f64>);
    let add = |sum: &mut Option<f64>, value: Option<f64>| {
        if let Some(value) = value {
            *sum = Some(sum.unwrap_or(0.0) + value);
        }
    };
    for row in &rows.rows {
        let Some(barsql_core::Value::Text(xml)) = row.first() else { continue };
        let doc = Document::parse(xml).map_err(|err| format!("could not read the SQL Server plan: {err}"))?;
        // SET and other statements without a plan have no QueryPlan.
        for statement in doc.descendants().filter(|n| named(n, "StmtSimple")) {
            let Some(query_plan) = child(statement, "QueryPlan") else { continue };
            let Some(root) = child(query_plan, "RelOp") else { continue };
            plan.nodes.push(rel_op(root));
            add(&mut total_cost, number(statement, "StatementSubTreeCost"));
            add(&mut planning, number(query_plan, "CompileTime"));
            add(&mut execution, child(query_plan, "QueryTimeStats").and_then(|stats| number(stats, "ElapsedTime")));
        }
    }
    plan.total_cost = total_cost;
    plan.planning_ms = planning;
    if plan.analyzed {
        plan.execution_ms = execution;
    }
    Ok(())
}

fn named(node: &Node, name: &str) -> bool {
    node.is_element() && node.tag_name().name() == name
}

fn child<'a, 'i>(node: Node<'a, 'i>, name: &str) -> Option<Node<'a, 'i>> {
    node.children().find(|c| named(c, name))
}

fn number(node: Node, attribute: &str) -> Option<f64> {
    node.attribute(attribute).and_then(|v| v.parse().ok())
}

// [dbo] to dbo.
fn bare(name: &str) -> &str {
    name.strip_prefix('[').and_then(|n| n.strip_suffix(']')).unwrap_or(name)
}

fn rel_op(node: Node) -> PlanNode {
    let physical = node.attribute("PhysicalOp").unwrap_or("Operator");
    let logical = node.attribute("LogicalOp").unwrap_or_default();
    let mut label = if logical.is_empty() || logical == physical {
        physical.to_string()
    } else {
        format!("{physical} ({logical})")
    };
    if matches!(node.attribute("Parallel"), Some("1" | "true")) && physical != "Parallelism" {
        label = format!("Parallel {label}");
    }
    let operator = node.children().find(|c| c.is_element() && !GENERIC.contains(&c.tag_name().name()));
    let object = operator.and_then(|op| child(op, "Object"));
    let mut n = PlanNode {
        label,
        relation: object.map(relation).unwrap_or_default(),
        index: object.and_then(|o| o.attribute("Index")).map(|i| bare(i).to_string()).unwrap_or_default(),
        detail: operator.map(detail).unwrap_or_default(),
        cost_total: number(node, "EstimatedTotalSubtreeCost"),
        rows_planned: number(node, "EstimateRows"),
        fields: fields(node),
        ..Default::default()
    };
    measure(node, &mut n);
    n.children = children(node).into_iter().map(rel_op).collect();
    finalize_node(&mut n);
    n
}

// `orders o`, like the other engines' relation and alias.
fn relation(object: Node) -> String {
    let table = object.attribute("Table").map(bare).unwrap_or_default();
    match object.attribute("Alias").map(bare) {
        Some(alias) if !alias.is_empty() && alias != table => format!("{table} {alias}"),
        _ => table.to_string(),
    }
}

// The RelOps right below this one, wherever the operator nests them.
fn children<'a, 'i>(node: Node<'a, 'i>) -> Vec<Node<'a, 'i>> {
    node.descendants()
        .skip(1)
        .filter(|d| named(d, "RelOp") && d.ancestors().skip(1).find(|a| named(a, "RelOp")) == Some(node))
        .collect()
}

// Row mode reports elapsed time including the children, batch mode each operator's own. Threads are summed for
// rows and runs, and the slowest thread gives the time.
fn measure(node: Node, n: &mut PlanNode) {
    let Some(info) = child(node, "RunTimeInformation") else { return };
    let threads: Vec<Node> = info.children().filter(|c| named(c, "RunTimeCountersPerThread")).collect();
    if threads.is_empty() {
        return;
    }
    let sum = |attribute: &str| threads.iter().filter_map(|t| number(*t, attribute)).sum::<f64>();
    let max = |attribute: &str| threads.iter().filter_map(|t| number(*t, attribute)).reduce(f64::max);
    let runs = sum("ActualExecutions");
    n.rows_actual = Some(sum("ActualRows"));
    n.loops = Some(runs);
    n.never_run = runs == 0.0;
    let batch = threads.iter().any(|t| t.attribute("ActualExecutionMode") == Some("Batch"));
    match (batch, max("ActualElapsedms")) {
        (true, Some(own)) => n.self_time_ms = Some(own),
        (false, Some(total)) => n.time_ms = Some(total),
        (_, None) => {}
    }
    for (attribute, key) in [("ActualLogicalReads", "Logical reads"), ("ActualPhysicalReads", "Physical reads")] {
        if threads.iter().any(|t| t.attribute(attribute).is_some()) {
            n.fields.push(PlanField { key: key.into(), value: format_count(sum(attribute)) });
        }
    }
}

fn format_count(value: f64) -> String {
    if value.fract() == 0.0 { format!("{value:.0}") } else { value.to_string() }
}

fn fields(node: Node) -> Vec<PlanField> {
    let mut out: Vec<PlanField> = [
        ("EstimateIO", "Estimated I/O"),
        ("EstimateCPU", "Estimated CPU"),
        ("AvgRowSize", "Row size"),
        ("TableCardinality", "Table rows"),
        ("EstimatedRowsRead", "Rows read (estimated)"),
        ("EstimatedExecutionMode", "Execution mode"),
    ]
    .into_iter()
    .filter_map(|(attribute, key)| node.attribute(attribute).map(|v| PlanField { key: key.into(), value: v.into() }))
    .collect();
    if let Some(warnings) = child(node, "Warnings") {
        let names: Vec<&str> = warnings.children().filter(Node::is_element).map(|w| w.tag_name().name()).collect();
        if !names.is_empty() {
            out.push(PlanField { key: "Warnings".into(), value: names.join(", ") });
        }
    }
    out
}

// What the operator filters or seeks on, as SQL Server writes it.
fn detail(operator: Node) -> String {
    let mut parts = Vec::new();
    if let Some(seek) = child(operator, "SeekPredicates") {
        let keys: Vec<String> = seek.descendants().filter(|n| named(n, "SeekKeys")).flat_map(seek_keys).collect();
        if !keys.is_empty() {
            parts.push(format!("Seek: {}", keys.join(" AND ")));
        }
    }
    for name in ["Predicate", "ProbeResidual", "Residual"] {
        if let Some(text) = child(operator, name).and_then(scalar) {
            parts.push(text);
        }
    }
    parts.join("; ")
}

// `customer < (10)`, from a seek key's columns, comparison and values.
fn seek_keys(keys: Node) -> Vec<String> {
    keys.children()
        .filter(Node::is_element)
        .filter_map(|range| {
            let op = match range.attribute("ScanType")? {
                "EQ" => "=",
                "LT" => "<",
                "LE" => "<=",
                "GT" => ">",
                "GE" => ">=",
                "IS" => "IS",
                "IS NOT" => "IS NOT",
                other => other,
            };
            let columns = child(range, "RangeColumns")?;
            let values = child(range, "RangeExpressions")?;
            let pairs: Vec<String> = columns
                .children()
                .filter(|c| named(c, "ColumnReference"))
                .zip(values.children().filter(|c| named(c, "ScalarOperator")))
                .map(|(column, value)| {
                    let name = column.attribute("Column").map(bare).unwrap_or_default();
                    format!("{name} {op} {}", value.attribute("ScalarString").unwrap_or_default())
                })
                .collect();
            Some(pairs.join(" AND "))
        })
        .collect()
}

fn scalar(node: Node) -> Option<String> {
    child(node, "ScalarOperator").and_then(|s| s.attribute("ScalarString")).map(str::to_string)
}

#[cfg(test)]
mod tests {
    use barsql_core::{DriverType, Value};

    use crate::plan::{PlanRows, parse_plan};

    const NS: &str = "http://schemas.microsoft.com/sqlserver/2004/07/showplan";

    fn plan(xml: &str, analyze: bool) -> crate::plan::QueryPlan {
        let rows = PlanRows { columns: vec![crate::SHOWPLAN_COLUMN.into()], rows: vec![vec![Value::Text(xml.into())]] };
        parse_plan(&DriverType::SqlServer, "SELECT …", "SET SHOWPLAN_XML ON", analyze, &rows).unwrap()
    }

    // A sort over a seek, as SQL Server 2019 measures it, trimmed to what the parser reads.
    fn measured() -> String {
        format!(
            r#"<ShowPlanXML xmlns="{NS}" Version="1.557"><BatchSequence><Batch><Statements>
            <StmtSimple StatementText="SELECT …" StatementSubTreeCost="0.0150316" StatementType="SELECT">
              <QueryPlan CompileTime="2"><QueryTimeStats ElapsedTime="5" CpuTime="1"/>
                <RelOp NodeId="0" PhysicalOp="Sort" LogicalOp="Sort" EstimateRows="10" EstimatedTotalSubtreeCost="0.0150316" Parallel="0">
                  <RunTimeInformation><RunTimeCountersPerThread Thread="0" ActualRows="10" ActualElapsedms="4" ActualExecutions="1" ActualExecutionMode="Row"/></RunTimeInformation>
                  <Sort Distinct="0">
                    <RelOp NodeId="1" PhysicalOp="Index Seek" LogicalOp="Index Seek" EstimateRows="195" EstimatedTotalSubtreeCost="0.0034965" Parallel="0" TableCardinality="2000">
                      <RunTimeInformation><RunTimeCountersPerThread Thread="0" ActualRows="195" ActualElapsedms="1" ActualExecutions="1" ActualLogicalReads="2" ActualExecutionMode="Row"/></RunTimeInformation>
                      <IndexScan Ordered="1">
                        <Object Database="[db]" Schema="[dbo]" Table="[plan_orders]" Index="[ix_customer]" Alias="[o]"/>
                        <SeekPredicates><SeekPredicateNew><SeekKeys>
                          <EndRange ScanType="LT"><RangeColumns><ColumnReference Column="customer"/></RangeColumns>
                            <RangeExpressions><ScalarOperator ScalarString="(10)"/></RangeExpressions></EndRange>
                        </SeekKeys></SeekPredicateNew></SeekPredicates>
                        <Predicate><ScalarOperator ScalarString="[o].[total]&gt;(5.00)"/></Predicate>
                      </IndexScan>
                    </RelOp>
                  </Sort>
                </RelOp>
              </QueryPlan>
            </StmtSimple>
            <StmtSimple StatementText="SET STATISTICS XML OFF" StatementType="SET STATS"/>
            </Statements></Batch></BatchSequence></ShowPlanXML>"#
        )
    }

    #[test]
    fn operators_nest_with_their_costs_and_counts() {
        let plan = plan(&measured(), true);
        assert_eq!(plan.nodes.len(), 1, "the SET statement has no plan");
        let sort = &plan.nodes[0];
        assert_eq!((sort.label.as_str(), sort.rows_planned, sort.rows_actual), ("Sort", Some(10.0), Some(10.0)));
        assert_eq!((sort.time_ms, sort.self_time_ms), (Some(4.0), Some(3.0)));
        let seek = &sort.children[0];
        assert_eq!((seek.relation.as_str(), seek.index.as_str()), ("plan_orders o", "ix_customer"));
        assert_eq!(seek.detail, "Seek: customer < (10); [o].[total]>(5.00)");
        assert_eq!((seek.rows_actual, seek.loops), (Some(195.0), Some(1.0)));
        assert!(seek.fields.iter().any(|f| f.key == "Logical reads" && f.value == "2"), "{:?}", seek.fields);
        let self_cost = sort.cost_self.unwrap();
        assert!((self_cost - (0.0150316 - 0.0034965)).abs() < 1e-9, "{self_cost}");
        assert_eq!((plan.total_cost, plan.planning_ms, plan.execution_ms), (Some(0.0150316), Some(2.0), Some(5.0)));
    }

    #[test]
    fn an_estimate_has_no_actuals_and_a_parallel_label() {
        let xml = format!(
            r#"<ShowPlanXML xmlns="{NS}"><BatchSequence><Batch><Statements><StmtSimple StatementSubTreeCost="1.5"><QueryPlan>
              <RelOp PhysicalOp="Hash Match" LogicalOp="Inner Join" EstimateRows="3" EstimatedTotalSubtreeCost="1.5" Parallel="1">
                <Hash><RelOp PhysicalOp="Clustered Index Scan" LogicalOp="Clustered Index Scan" EstimateRows="1" EstimatedTotalSubtreeCost="0.5" Parallel="1">
                  <IndexScan><Object Table="[a]" Index="[pk_a]"/></IndexScan></RelOp>
                <RelOp PhysicalOp="Table Scan" LogicalOp="Table Scan" EstimateRows="2" EstimatedTotalSubtreeCost="0.25" Parallel="0">
                  <TableScan><Object Table="[b]"/></TableScan></RelOp></Hash>
              </RelOp></QueryPlan></StmtSimple></Statements></Batch></BatchSequence></ShowPlanXML>"#
        );
        let plan = plan(&xml, false);
        let join = &plan.nodes[0];
        assert_eq!(join.label, "Parallel Hash Match (Inner Join)");
        assert_eq!(join.children.iter().map(|c| c.relation.as_str()).collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!((join.rows_actual, join.time_ms, plan.execution_ms), (None, None, None));
        assert!((join.cost_self.unwrap() - 0.75).abs() < 1e-9);
    }

    #[test]
    fn broken_xml_is_an_error() {
        let rows =
            PlanRows { columns: vec![crate::SHOWPLAN_COLUMN.into()], rows: vec![vec![Value::Text("<oops".into())]] };
        assert!(parse_plan(&DriverType::SqlServer, "SELECT 1", "", false, &rows).is_err());
    }
}
