use barsql_core::capabilities::IdentityStyle;
use barsql_core::{Identity, StructColumn};
use barsql_sql::structure::{ProblemKind, type_suggestions};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::*;

use super::StructureTab;
use super::sheet::{Shape, Value, flag};
use crate::grid::Choices;
use crate::i18n::{t, t_with};
use crate::schema;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    Name,
    Type,
    Nullable,
    Primary,
    Default,
    Identity,
    Comment,
}

fn fields(tab: &StructureTab) -> Vec<Field> {
    let editing = &tab.connection.driver.capabilities().structure;
    let mut fields = vec![Field::Name, Field::Type, Field::Nullable, Field::Primary, Field::Default];
    if editing.identity != IdentityStyle::None {
        fields.push(Field::Identity);
    }
    if editing.comments {
        fields.push(Field::Comment);
    }
    fields
}

fn heading(field: Field, cx: &App) -> (SharedString, bool) {
    let key = match field {
        Field::Name => "structure.name",
        Field::Type => "structure.type",
        Field::Nullable => "structure.nullable",
        Field::Primary => "structure.primaryKey",
        Field::Default => "structure.default",
        Field::Identity => "structure.identity.label",
        Field::Comment => "structure.comment",
    };
    (t(cx, key), matches!(field, Field::Nullable | Field::Primary))
}

fn identity_label(identity: Identity, cx: &App) -> SharedString {
    match identity {
        Identity::None => t(cx, "structure.identity.none"),
        Identity::ByDefault => t(cx, "structure.identity.byDefault"),
        Identity::Always => t(cx, "structure.identity.always"),
        Identity::AutoIncrement => t(cx, "structure.identity.autoIncrement"),
        Identity::Seeded { seed, increment } => {
            let (seed, increment) = (seed.to_string(), increment.to_string());
            t_with(cx, "structure.identity.seeded", &[("seed", &seed), ("increment", &increment)])
        }
    }
}

fn identity_choices(style: IdentityStyle) -> &'static [Identity] {
    match style {
        IdentityStyle::Generated => &[Identity::None, Identity::ByDefault, Identity::Always],
        IdentityStyle::AutoIncrement => &[Identity::None, Identity::AutoIncrement],
        IdentityStyle::Seeded => &[Identity::None, Identity::Seeded { seed: 1, increment: 1 }],
        IdentityStyle::None => &[],
    }
}

// The engine's usual types first, then the ones the loaded tables use.
fn suggestions(tab: &StructureTab, cx: &App) -> Vec<String> {
    let mut out: Vec<String> = type_suggestions(&tab.connection.driver).iter().map(|t| t.to_string()).collect();
    let used = schema::column_types(cx, &tab.connection.id);
    let here = tab.draft.columns.iter().map(|row| row.column.data_type.clone());
    for data_type in used.into_iter().chain(here).filter(|t| !t.trim().is_empty()) {
        if !out.iter().any(|known| known.eq_ignore_ascii_case(&data_type)) {
            out.push(data_type);
        }
    }
    out
}

fn value(field: Field, column: &StructColumn, primary: bool, cx: &App) -> Value {
    match field {
        Field::Name => Value::Text(column.name.clone()),
        Field::Type => Value::Text(column.data_type.clone()),
        Field::Nullable => Value::Flag(column.nullable),
        Field::Primary => Value::Flag(primary),
        Field::Default if !column.generated.is_empty() => {
            Value::Text(format!("{} {}", t(cx, "structure.generated"), column.generated))
        }
        Field::Default => Value::Text(column.default.clone()),
        // Most columns have none, so it shows blank.
        Field::Identity if column.identity.is_none() => Value::Text(String::new()),
        Field::Identity => Value::Text(identity_label(column.identity, cx).to_string()),
        Field::Comment => Value::Text(column.comment.clone()),
    }
}

// A generated column keeps its cells, and some engines can't add an identity to a column they have.
pub(super) fn shape(tab: &StructureTab, cx: &App) -> Shape {
    let editing = &tab.connection.driver.capabilities().structure;
    let fields = fields(tab);
    let mut shape = Shape::new(fields.iter().map(|&field| heading(field, cx)).collect());
    let original = tab.original.as_ref();
    let keys = original.and_then(|original| original.primary_key.as_ref()).map_or(&[][..], |key| &key.columns[..]);
    for (ix, row) in tab.draft.columns.iter().enumerate() {
        let name = &row.column.name;
        let in_key = tab.draft.foreign_keys.iter().any(|key| !key.dropped && key.value.columns.contains(name));
        let in_index = tab.draft.indexes.iter().any(|index| !index.dropped && index.value.columns.contains(name));
        let marks: Vec<Lucide> = [(in_key, Lucide::Link), (in_index, Lucide::ListTree)]
            .into_iter()
            .filter_map(|(used, icon)| used.then_some(icon))
            .collect();
        if !marks.is_empty() {
            shape.marks.insert((ix, 0), marks);
        }
        let old = row.origin.and_then(|origin| original?.columns.get(origin));
        let base = old.map(|old| fields.iter().map(|&field| value(field, old, keys.contains(&old.name), cx)).collect());
        let shown = fields.iter().map(|&field| value(field, &row.column, row.primary, cx)).collect();
        let fixed_identity = old.is_some() && !editing.identity_on_existing;
        let locked: Vec<usize> = (0..fields.len())
            .filter(|&ix| !row.column.generated.is_empty() || (fields[ix] == Field::Identity && fixed_identity))
            .collect();
        shape.row(base, shown, row.dropped, &locked);
    }
    let at = |wanted: Field| fields.iter().position(|&field| field == wanted);
    if let Some(ix) = at(Field::Type) {
        shape.choices.insert(ix, Choices { values: suggestions(tab, cx).into(), nullable: false, open: true });
    }
    if let Some(ix) = at(Field::Identity) {
        let labels =
            identity_choices(editing.identity).iter().map(|&identity| identity_label(identity, cx).to_string());
        shape.choices.insert(ix, Choices { values: labels.collect::<Vec<_>>().into(), nullable: false, open: false });
    }
    shape
}

pub(super) fn problem_columns(tab: &StructureTab, kind: &ProblemKind) -> Vec<usize> {
    let named: &[Field] = match kind {
        ProblemKind::EmptyName | ProblemKind::DuplicateName => &[Field::Name],
        ProblemKind::EmptyType => &[Field::Type],
        _ => &[],
    };
    let fields = fields(tab);
    named.iter().filter_map(|field| fields.iter().position(|f| f == field)).collect()
}

pub(super) fn edit(tab: &mut StructureTab, row: usize, column: usize, value: Option<String>, cx: &App) {
    let Some(&field) = fields(tab).get(column) else { return };
    let style = tab.connection.driver.capabilities().structure.identity;
    let on = flag(value.as_deref());
    let text = value.unwrap_or_default();
    let Some(line) = tab.draft.columns.get_mut(row) else { return };
    let column = &mut line.column;
    match field {
        Field::Name => {
            let name = text.trim().to_string();
            if column.name != name {
                tab.draft.rename_column(row, name);
            }
        }
        Field::Type => column.data_type = text.trim().to_string(),
        Field::Nullable => column.nullable = on,
        // A key column can't hold NULL on most engines, so ticking the key unticks nullable.
        Field::Primary => {
            line.primary = on;
            if on {
                line.column.nullable = false;
            }
        }
        Field::Default => column.default = text,
        Field::Identity => {
            let picked = identity_choices(style).iter().copied().find(|&id| identity_label(id, cx).as_ref() == text);
            if let Some(identity) = picked.or(text.is_empty().then_some(Identity::None)) {
                column.identity = identity;
            }
        }
        Field::Comment => column.comment = text,
    }
}
