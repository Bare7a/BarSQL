use std::collections::HashMap;
use std::fmt::Display;
use std::ops::Range;

use barsql_core::SqlDialect;
use sqlparser::ast::{
    BinaryOperator, Distinct, Expr, GroupByExpr, JoinConstraint, JoinOperator, LimitClause, OrderBy, OrderByKind,
    Query, Select, SelectItem, SetExpr, SetOperator, SetQuantifier, Spanned, Statement, TableAlias, TableFactor,
    TableWithJoins,
};
use sqlparser::dialect::{ClickHouseDialect, Dialect, MsSqlDialect, MySqlDialect, PostgreSqlDialect, SQLiteDialect};
use sqlparser::parser::Parser;
use sqlparser::tokenizer::{Location, Span};

// One operator of a query in relational algebra, with what it works on below it. A table has no symbol and no
// children.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlgebraNode {
    pub symbol: &'static str,
    pub detail: String,
    pub children: Vec<AlgebraNode>,
    // What a projection, a grouping or a sort lists, one by one.
    pub items: Vec<Item>,
    // The bytes of the statement this operator comes from.
    pub span: Option<Range<usize>>,
    // The table a leaf reads.
    pub table: Option<TableRef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    // The alias, else the expression as written.
    pub label: String,
    pub expr: String,
    pub source: Source,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Column { qualifier: Option<String>, name: String },
    // `*`, or `t.*`.
    Wildcard(Option<String>),
    Expr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableRef {
    pub name: String,
    pub alias: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AlgebraError {
    // Not a lone SELECT, or a statement that doesn't return rows.
    NotQuery,
    Parse(String),
}

type Ctes = HashMap<String, AlgebraNode>;

impl AlgebraNode {
    fn new(symbol: &'static str, detail: impl Into<String>, children: Vec<AlgebraNode>) -> Self {
        Self { symbol, detail: detail.into(), children, items: vec![], span: None, table: None }
    }

    fn items(mut self, items: Vec<Item>) -> Self {
        self.items = items;
        self
    }

    fn span(mut self, span: Option<Range<usize>>) -> Self {
        self.span = span;
        self
    }
}

fn list(items: &[impl Display]) -> String {
    items.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ")
}

fn parser_dialect(dialect: SqlDialect) -> Box<dyn Dialect> {
    match dialect {
        SqlDialect::Postgres => Box::new(PostgreSqlDialect {}),
        SqlDialect::MySql => Box::new(MySqlDialect {}),
        SqlDialect::Sqlite => Box::new(SQLiteDialect {}),
        SqlDialect::TSql => Box::new(MsSqlDialect {}),
        SqlDialect::ClickHouse => Box::new(ClickHouseDialect {}),
    }
}

// The query as a tree, from the tables at the leaves to the last operator at the root: ⋈ joins, σ filters, γ groups,
// π projects, δ removes duplicates, τ sorts. A CTE is inlined where it's used.
pub fn algebra(sql: &str, dialect: SqlDialect) -> Result<AlgebraNode, AlgebraError> {
    let mut statements =
        Parser::parse_sql(parser_dialect(dialect).as_ref(), sql).map_err(|e| AlgebraError::Parse(e.to_string()))?;
    match statements.pop() {
        Some(Statement::Query(query)) if statements.is_empty() => {
            let mut starts = vec![0];
            starts.extend(sql.match_indices('\n').map(|(at, _)| at + 1));
            Ok(Builder { sql, starts }.query(&query, &Ctes::new()))
        }
        _ => Err(AlgebraError::NotQuery),
    }
}

struct Builder<'a> {
    sql: &'a str,
    // Where each line starts, for turning the parser's line and column into a byte.
    starts: Vec<usize>,
}

impl Builder<'_> {
    fn offset(&self, at: Location) -> Option<usize> {
        let start = *self.starts.get((at.line as usize).checked_sub(1)?)?;
        let column = (at.column as usize).checked_sub(1)?;
        Some(self.sql[start..].char_indices().nth(column).map_or(self.sql.len(), |(i, _)| start + i))
    }

    fn range(&self, span: Span) -> Option<Range<usize>> {
        let (start, end) = (self.offset(span.start)?, self.offset(span.end)?);
        (start < end).then_some(start..end)
    }

    fn spans(&self, spans: impl IntoIterator<Item = Span>) -> Option<Range<usize>> {
        self.range(Span::union_iter(spans))
    }

    fn query(&self, query: &Query, outer: &Ctes) -> AlgebraNode {
        let mut ctes = outer.clone();
        if let Some(with) = query.with.as_ref().filter(|with| !with.recursive) {
            for cte in &with.cte_tables {
                let body = self.query(&cte.query, &ctes);
                ctes.insert(cte.alias.name.value.to_lowercase(), body);
            }
        }
        let mut root = self.set_expr(&query.body, &ctes);
        if let Some(OrderBy { kind: OrderByKind::Expressions(keys), .. }) = &query.order_by {
            let items =
                keys.iter().map(|key| Item { label: key.to_string(), ..item_of(&key.expr, key.to_string()) }).collect();
            root = AlgebraNode::new("τ", list(keys), vec![root])
                .items(items)
                .span(self.spans(keys.iter().map(|key| key.span())));
        }
        match &query.limit_clause {
            Some(LimitClause::LimitOffset { limit, offset, .. }) => {
                let parts: Vec<String> = limit
                    .iter()
                    .map(|limit| limit.to_string())
                    .chain(offset.iter().map(|offset| format!("OFFSET {}", offset.value)))
                    .collect();
                if !parts.is_empty() {
                    let spans = limit.iter().map(|limit| limit.span()).chain(offset.iter().map(|o| o.span()));
                    root = AlgebraNode::new("limit", parts.join(" "), vec![root]).span(self.spans(spans));
                }
            }
            Some(LimitClause::OffsetCommaLimit { offset, limit }) => {
                root = AlgebraNode::new("limit", format!("{limit} OFFSET {offset}"), vec![root])
                    .span(self.spans([limit.span(), offset.span()]));
            }
            None => {}
        }
        root
    }

    fn set_expr(&self, body: &SetExpr, ctes: &Ctes) -> AlgebraNode {
        match body {
            SetExpr::Select(select) => self.select(select, ctes),
            SetExpr::Query(query) => self.query(query, ctes),
            SetExpr::SetOperation { left, op, set_quantifier, right } => {
                let symbol = match op {
                    SetOperator::Union => "∪",
                    SetOperator::Intersect => "∩",
                    SetOperator::Except | SetOperator::Minus => "−",
                };
                let detail = if matches!(set_quantifier, SetQuantifier::All) { "ALL" } else { "" };
                AlgebraNode::new(symbol, detail, vec![self.set_expr(left, ctes), self.set_expr(right, ctes)])
            }
            SetExpr::Values(values) => AlgebraNode::new("", format!("VALUES ({} rows)", values.rows.len()), vec![]),
            other => AlgebraNode::new("", other.to_string(), vec![]),
        }
    }

    fn select(&self, select: &Select, ctes: &Ctes) -> AlgebraNode {
        let mut root = select
            .from
            .iter()
            .map(|item| self.joins(item, ctes))
            .reduce(|left, right| AlgebraNode::new("×", "", vec![left, right]))
            .unwrap_or_else(|| AlgebraNode::new("", "()", vec![]));
        if let Some(filter) = &select.selection {
            root = AlgebraNode::new("σ", filter.to_string(), vec![root])
                .items(conjuncts(filter))
                .span(self.range(filter.span()));
        }
        match &select.group_by {
            GroupByExpr::Expressions(keys, _) if !keys.is_empty() => {
                let items = keys.iter().map(|key| item_of(key, key.to_string())).collect();
                let span = self.spans(keys.iter().map(|key| key.span()));
                root = AlgebraNode::new("γ", list(keys), vec![root]).items(items).span(span);
            }
            GroupByExpr::All(_) => root = AlgebraNode::new("γ", "ALL", vec![root]),
            GroupByExpr::Expressions(..) => {}
        }
        if let Some(having) = &select.having {
            root = AlgebraNode::new("σ", having.to_string(), vec![root])
                .items(conjuncts(having))
                .span(self.range(having.span()));
        }
        let items = select.projection.iter().map(select_item).collect();
        let span = self.spans(select.projection.iter().map(|item| item.span()));
        root = AlgebraNode::new("π", list(&select.projection), vec![root]).items(items).span(span);
        let keyword = self.range(select.select_token.0.span);
        match &select.distinct {
            Some(Distinct::Distinct) => root = AlgebraNode::new("δ", "", vec![root]).span(keyword),
            Some(Distinct::On(keys)) => {
                root = AlgebraNode::new("δ", format!("ON ({})", list(keys)), vec![root]).span(keyword)
            }
            Some(Distinct::All) | None => {}
        }
        root
    }

    fn joins(&self, item: &TableWithJoins, ctes: &Ctes) -> AlgebraNode {
        item.joins.iter().fold(self.factor(&item.relation, ctes), |left, join| {
            let (kind, constraint) = match &join.join_operator {
                JoinOperator::Left(c) | JoinOperator::LeftOuter(c) => ("LEFT", Some(c)),
                JoinOperator::Right(c) | JoinOperator::RightOuter(c) => ("RIGHT", Some(c)),
                JoinOperator::FullOuter(c) => ("FULL", Some(c)),
                JoinOperator::CrossJoin(c) => ("CROSS", Some(c)),
                JoinOperator::Semi(c) | JoinOperator::LeftSemi(c) | JoinOperator::RightSemi(c) => ("SEMI", Some(c)),
                JoinOperator::Anti(c) | JoinOperator::LeftAnti(c) | JoinOperator::RightAnti(c) => ("ANTI", Some(c)),
                JoinOperator::Join(c) | JoinOperator::Inner(c) => ("", Some(c)),
                _ => ("", None),
            };
            let on = match constraint {
                Some(JoinConstraint::On(expr)) => format!("ON {expr}"),
                Some(JoinConstraint::Using(columns)) => format!("USING ({})", list(columns)),
                Some(JoinConstraint::Natural) => "NATURAL".into(),
                Some(JoinConstraint::None) | None => String::new(),
            };
            let detail = [kind, on.as_str()].iter().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>().join(" ");
            AlgebraNode::new("⋈", detail, vec![left, self.factor(&join.relation, ctes)]).span(self.range(join.span()))
        })
    }

    fn factor(&self, factor: &TableFactor, ctes: &Ctes) -> AlgebraNode {
        let span = self.range(factor.span());
        match factor {
            TableFactor::Table { name, alias, .. } => match ctes.get(&name.to_string().to_lowercase()) {
                Some(cte) => renamed(cte.clone(), alias.as_ref(), span),
                None => {
                    let alias_name = alias.as_ref().map(|alias| alias.name.value.clone());
                    let detail = match &alias_name {
                        Some(alias) => format!("{name} {alias}"),
                        None => name.to_string(),
                    };
                    let mut leaf = AlgebraNode::new("", detail, vec![]).span(span);
                    leaf.table = Some(TableRef { name: name.to_string(), alias: alias_name });
                    leaf
                }
            },
            TableFactor::Derived { subquery, alias, .. } => renamed(self.query(subquery, ctes), alias.as_ref(), span),
            TableFactor::NestedJoin { table_with_joins, alias, .. } => {
                renamed(self.joins(table_with_joins, ctes), alias.as_ref(), span)
            }
            other => AlgebraNode::new("", other.to_string(), vec![]).span(span),
        }
    }
}

// ρ names what a subquery or CTE produces.
fn renamed(inner: AlgebraNode, alias: Option<&TableAlias>, span: Option<Range<usize>>) -> AlgebraNode {
    match alias {
        Some(alias) => AlgebraNode::new("ρ", alias.name.value.clone(), vec![inner]).span(span),
        None => inner,
    }
}

fn source_of(expr: &Expr) -> Source {
    match expr {
        Expr::Identifier(name) => Source::Column { qualifier: None, name: name.value.clone() },
        Expr::CompoundIdentifier(parts) if parts.len() >= 2 => {
            let (name, qualifier) = parts.split_last().expect("two parts");
            let qualifier = qualifier.iter().map(|part| part.value.as_str()).collect::<Vec<_>>().join(".");
            Source::Column { qualifier: Some(qualifier), name: name.value.clone() }
        }
        _ => Source::Expr,
    }
}

// The conditions a predicate requires together: `a AND b AND c` as three.
fn conjuncts(expr: &Expr) -> Vec<Item> {
    match expr {
        Expr::BinaryOp { left, op: BinaryOperator::And, right } => [conjuncts(left), conjuncts(right)].concat(),
        other => vec![item_of(other, other.to_string())],
    }
}

fn item_of(expr: &Expr, text: String) -> Item {
    Item { label: expr.to_string(), expr: text, source: source_of(expr) }
}

fn select_item(item: &SelectItem) -> Item {
    match item {
        SelectItem::UnnamedExpr(expr) => item_of(expr, expr.to_string()),
        SelectItem::ExprWithAlias { expr, alias } => {
            Item { label: alias.value.clone(), expr: expr.to_string(), source: source_of(expr) }
        }
        SelectItem::Wildcard(_) => Item { label: "*".into(), expr: "*".into(), source: Source::Wildcard(None) },
        SelectItem::QualifiedWildcard(kind, _) => {
            let text = kind.to_string();
            let qualifier = text.strip_suffix(".*").unwrap_or(&text).to_string();
            Item { label: text.clone(), expr: text, source: Source::Wildcard(Some(qualifier)) }
        }
        other => Item { label: other.to_string(), expr: other.to_string(), source: Source::Expr },
    }
}

// A table's columns as (name, type) pairs.
pub type TableColumns = Vec<(String, String)>;

// A column that leaves an operator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Column {
    // The table or alias it comes from, empty for a computed one.
    pub table: String,
    pub name: String,
    pub data_type: String,
}

// The columns that come out of `node`, as far as the tables' columns say. `lookup` gives a table's (name, type) pairs.
pub fn columns(node: &AlgebraNode, lookup: &dyn Fn(&str) -> Option<TableColumns>) -> Vec<Column> {
    let child = |ix: usize| node.children.get(ix).map(|child| columns(child, lookup)).unwrap_or_default();
    match node.symbol {
        "" => {
            let Some(table) = &node.table else { return vec![] };
            let shown = table.alias.clone().unwrap_or_else(|| table.name.rsplit('.').next().unwrap_or_default().into());
            lookup(&table.name)
                .unwrap_or_default()
                .into_iter()
                .map(|(name, data_type)| Column { table: shown.clone(), name, data_type })
                .collect()
        }
        "ρ" => child(0).into_iter().map(|column| Column { table: node.detail.clone(), ..column }).collect(),
        "π" | "γ" => project(&node.items, &child(0)),
        "⋈" | "×" => [child(0), child(1)].concat(),
        _ => child(0),
    }
}

fn project(items: &[Item], input: &[Column]) -> Vec<Column> {
    let mut out = vec![];
    for item in items {
        match &item.source {
            Source::Wildcard(qualifier) => out.extend(
                input.iter().filter(|c| qualifier.as_ref().is_none_or(|q| c.table.eq_ignore_ascii_case(q))).cloned(),
            ),
            Source::Column { qualifier, name } => {
                let found = input.iter().find(|c| {
                    c.name.eq_ignore_ascii_case(name)
                        && qualifier.as_ref().is_none_or(|q| c.table.eq_ignore_ascii_case(q))
                });
                // Without an alias it keeps the column's own name, not the `t.name` it was written as.
                let shown = if item.label == item.expr { name.clone() } else { item.label.clone() };
                out.push(Column {
                    table: found.map(|c| c.table.clone()).unwrap_or_default(),
                    name: shown,
                    data_type: found.map(|c| c.data_type.clone()).unwrap_or_default(),
                });
            }
            Source::Expr => {
                let upper = item.expr.to_uppercase();
                let numeric = ["COUNT(", "SUM(", "AVG(", "ROUND("].iter().any(|f| upper.starts_with(f));
                let data_type = if numeric { "number" } else { "" };
                out.push(Column { table: String::new(), name: item.label.clone(), data_type: data_type.into() });
            }
        }
    }
    out
}
