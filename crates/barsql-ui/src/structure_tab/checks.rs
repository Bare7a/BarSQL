use barsql_core::Check;
use barsql_sql::structure::ProblemKind;
use gpui_kit::*;

use super::StructureTab;
use super::sheet::{Shape, Value};
use crate::i18n::t;

fn values(check: &Check) -> Vec<Value> {
    vec![Value::Text(check.name.clone()), Value::Text(check.expression.clone())]
}

pub(super) fn shape(tab: &StructureTab, cx: &App) -> Shape {
    let headings = ["structure.check.name", "structure.check.expression"].map(|key| (t(cx, key), false));
    let mut shape = Shape::new(headings.to_vec());
    for row in &tab.draft.checks {
        let old = row.origin.and_then(|origin| tab.original.as_ref()?.checks.get(origin));
        shape.row(old.map(values), values(&row.value), row.dropped, &[]);
    }
    shape
}

// Name, then the condition.
pub(super) fn problem_columns(kind: &ProblemKind) -> Vec<usize> {
    match kind {
        ProblemKind::EmptyName | ProblemKind::DuplicateName => vec![0],
        ProblemKind::EmptyExpression => vec![1],
        _ => Vec::new(),
    }
}

pub(super) fn edit(tab: &mut StructureTab, row: usize, column: usize, value: Option<String>) {
    let Some(check) = tab.draft.checks.get_mut(row).map(|line| &mut line.value) else { return };
    let text = value.unwrap_or_default().trim().to_string();
    match column {
        0 => check.name = text,
        1 => check.expression = text,
        _ => {}
    }
}
