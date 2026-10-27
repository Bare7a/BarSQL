use barsql_core::StructIndex;
use barsql_sql::structure::{NameKind, suggested_name};
use gpui_kit::*;

use super::StructureTab;
use super::sheet::{Shape, Value, flag, names};
use crate::grid::Choices;
use crate::i18n::t;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    Name,
    Columns,
    Unique,
    Method,
    Constraint,
}

// Constraint shows only where one of the indexes backs a unique constraint.
fn fields(tab: &StructureTab) -> Vec<Field> {
    let mut fields = vec![Field::Name, Field::Columns, Field::Unique];
    if !tab.connection.driver.capabilities().structure.index_methods.is_empty() {
        fields.push(Field::Method);
    }
    if tab.draft.indexes.iter().any(|row| row.value.constraint) {
        fields.push(Field::Constraint);
    }
    fields
}

fn heading(field: Field, cx: &App) -> (SharedString, bool) {
    let key = match field {
        Field::Name => "structure.index.name",
        Field::Columns => "structure.index.columns",
        Field::Unique => "structure.index.unique",
        Field::Method => "structure.index.method",
        Field::Constraint => "structure.index.constraint",
    };
    (t(cx, key), matches!(field, Field::Unique | Field::Constraint))
}

fn method(index: &StructIndex, methods: &[&str]) -> String {
    match index.method.is_empty() {
        true => methods.first().copied().unwrap_or_default().to_string(),
        false => index.method.clone(),
    }
}

// An index the editor can't model, like an expression one, shows its definition for its columns.
fn value(field: Field, index: &StructIndex, methods: &[&str], cx: &App) -> Value {
    match field {
        Field::Name => Value::Text(index.name.clone()),
        Field::Columns if !index.definition.is_empty() => {
            Value::Text(format!("{}: {}", t(cx, "structure.index.definition"), index.definition))
        }
        Field::Columns => Value::Text(index.columns.join(", ")),
        Field::Unique => Value::Flag(index.unique),
        Field::Method => Value::Text(method(index, methods)),
        Field::Constraint => Value::Flag(index.constraint),
    }
}

pub(super) fn shape(tab: &StructureTab, cx: &App) -> Shape {
    let methods = tab.connection.driver.capabilities().structure.index_methods;
    let fields = fields(tab);
    let mut shape = Shape::new(fields.iter().map(|&field| heading(field, cx)).collect());
    for row in &tab.draft.indexes {
        let old = row.origin.and_then(|origin| tab.original.as_ref()?.indexes.get(origin));
        let base = old.map(|old| fields.iter().map(|&field| value(field, old, methods, cx)).collect());
        let shown = fields.iter().map(|&field| value(field, &row.value, methods, cx)).collect();
        let modeled = row.value.definition.is_empty();
        let locked: Vec<usize> = (0..fields.len()).filter(|&ix| !modeled || fields[ix] == Field::Constraint).collect();
        shape.row(base, shown, row.dropped, &locked);
    }
    let columns: Vec<String> =
        tab.draft.columns.iter().filter(|row| !row.dropped).map(|row| row.column.name.clone()).collect();
    let at = |wanted: Field| fields.iter().position(|&field| field == wanted);
    if let Some(ix) = at(Field::Columns) {
        shape.choices.insert(ix, Choices { values: columns.into(), nullable: false, open: true });
    }
    if let Some(ix) = at(Field::Method) {
        let values: Vec<String> = methods.iter().map(|method| method.to_string()).collect();
        shape.choices.insert(ix, Choices { values: values.into(), nullable: false, open: false });
    }
    shape
}

// A new index is named after its columns until it's named by hand.
pub(super) fn edit(tab: &mut StructureTab, row: usize, column: usize, value: Option<String>) {
    let Some(&field) = fields(tab).get(column) else { return };
    let driver = tab.connection.driver.clone();
    let methods = driver.capabilities().structure.index_methods;
    let table = if tab.table.is_empty() { tab.draft.name.clone() } else { tab.table.clone() };
    let Some(line) = tab.draft.indexes.get_mut(row) else { return };
    let index = &mut line.value;
    match field {
        Field::Name => index.name = value.unwrap_or_default().trim().to_string(),
        Field::Columns => {
            let suggested = |columns: &[String]| match columns.is_empty() {
                true => String::new(),
                false => suggested_name(&driver, NameKind::Index, &table, columns, ""),
            };
            let follows = line.origin.is_none() && index.name == suggested(&index.columns);
            index.columns = names(value.as_deref());
            if follows {
                index.name = suggested(&index.columns);
            }
        }
        Field::Unique => index.unique = flag(value.as_deref()),
        // The engine's default shows when none is set, so picking it again leaves the index as it is.
        Field::Method => {
            let picked = value.unwrap_or_default();
            if picked != method(index, methods) {
                index.method = picked;
            }
        }
        Field::Constraint => {}
    }
}
