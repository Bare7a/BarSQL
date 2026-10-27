use barsql_core::{ForeignKey, RefAction};
use barsql_sql::structure::{NameKind, suggested_name};
use gpui_kit::*;

use super::StructureTab;
use super::sheet::{Shape, Value, names};
use crate::grid::Choices;
use crate::i18n::t;
use crate::schema;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    Name,
    Columns,
    Table,
    RefColumns,
    OnUpdate,
    OnDelete,
}

fn fields(tab: &StructureTab) -> Vec<Field> {
    let mut fields = vec![Field::Name, Field::Columns, Field::Table, Field::RefColumns];
    if !tab.connection.driver.capabilities().structure.ref_actions.is_empty() {
        fields.extend([Field::OnUpdate, Field::OnDelete]);
    }
    fields
}

fn heading(field: Field, cx: &App) -> (SharedString, bool) {
    let key = match field {
        Field::Name => "structure.foreignKey.name",
        Field::Columns => "structure.foreignKey.columns",
        Field::Table => "structure.foreignKey.table",
        Field::RefColumns => "structure.foreignKey.refColumns",
        Field::OnUpdate => "structure.foreignKey.onUpdate",
        Field::OnDelete => "structure.foreignKey.onDelete",
    };
    (t(cx, key), false)
}

fn action_label(action: RefAction, cx: &App) -> SharedString {
    let key = match action {
        RefAction::NoAction => "noAction",
        RefAction::Restrict => "restrict",
        RefAction::Cascade => "cascade",
        RefAction::SetNull => "setNull",
        RefAction::SetDefault => "setDefault",
    };
    t(cx, &format!("structure.action.{key}"))
}

fn value(field: Field, fk: &ForeignKey, cx: &App) -> Value {
    Value::Text(match field {
        Field::Name => fk.name.clone(),
        Field::Columns => fk.columns.join(", "),
        Field::Table => fk.ref_table.clone(),
        Field::RefColumns => fk.ref_columns.join(", "),
        Field::OnUpdate => action_label(fk.on_update, cx).to_string(),
        Field::OnDelete => action_label(fk.on_delete, cx).to_string(),
    })
}

fn listed(values: Vec<String>) -> Choices {
    Choices { values: values.into(), nullable: false, open: true }
}

// The referenced columns wait for a table, and list that table's once its columns are loaded.
pub(super) fn shape(tab: &StructureTab, cx: &App) -> Shape {
    let actions = tab.connection.driver.capabilities().structure.ref_actions;
    let fields = fields(tab);
    let at = |wanted: Field| fields.iter().position(|&field| field == wanted);
    let cached = schema::get(cx, &tab.connection.id);
    let mut shape = Shape::new(fields.iter().map(|&field| heading(field, cx)).collect());
    for (ix, row) in tab.draft.foreign_keys.iter().enumerate() {
        let fk = &row.value;
        let old = row.origin.and_then(|origin| tab.original.as_ref()?.foreign_keys.get(origin));
        let base = old.map(|old| fields.iter().map(|&field| value(field, old, cx)).collect());
        let shown = fields.iter().map(|&field| value(field, fk, cx)).collect();
        let locked: Vec<usize> = at(Field::RefColumns).filter(|_| fk.ref_table.is_empty()).into_iter().collect();
        shape.row(base, shown, row.dropped, &locked);
        let targets = cached.and_then(|cached| cached.columns(&tab.target_schema(fk), &fk.ref_table));
        if let (Some(column), Some(targets)) = (at(Field::RefColumns), targets) {
            shape.cell_choices.insert((ix, column), listed(targets.iter().map(|c| c.name.clone()).collect()));
        }
    }
    let columns = tab.draft.columns.iter().filter(|row| !row.dropped).map(|row| row.column.name.clone()).collect();
    let tables = cached
        .map(|cached| cached.loaded_tables())
        .unwrap_or_default()
        .into_iter()
        .filter(|(schema, _)| *schema == tab.schema)
        .flat_map(|(_, tables)| tables.iter().filter(|t| !schema::is_view_kind(&t.kind)).map(|t| t.name.clone()))
        .collect();
    let labels: Vec<String> = actions.iter().map(|&action| action_label(action, cx).to_string()).collect();
    let lists = [
        (Field::Columns, listed(columns)),
        (Field::Table, listed(tables)),
        (Field::OnUpdate, Choices { values: labels.clone().into(), nullable: false, open: false }),
        (Field::OnDelete, Choices { values: labels.into(), nullable: false, open: false }),
    ];
    for (field, choices) in lists {
        if let Some(ix) = at(field) {
            shape.choices.insert(ix, choices);
        }
    }
    shape
}

pub(super) fn edit(
    tab: &mut StructureTab,
    row: usize,
    column: usize,
    value: Option<String>,
    window: &mut Window,
    cx: &mut Context<StructureTab>,
) {
    let Some(&field) = fields(tab).get(column) else { return };
    let actions = tab.connection.driver.capabilities().structure.ref_actions;
    let text = value.as_deref().unwrap_or_default().trim().to_string();
    let action = || actions.iter().copied().find(|&action| action_label(action, cx).as_ref() == text);
    let Some(fk) = tab.draft.foreign_keys.get_mut(row).map(|line| &mut line.value) else { return };
    match field {
        // Named by hand, it no longer follows its columns.
        Field::Name => fk.name = text,
        Field::Columns => tab.edit_foreign_key(row, |fk| fk.columns = names(value.as_deref())),
        // The same table again keeps the columns picked from it.
        Field::Table if fk.ref_table == text => {}
        Field::Table => tab.pick_target(row, text, window, cx),
        Field::RefColumns => tab.edit_foreign_key(row, |fk| fk.ref_columns = names(value.as_deref())),
        Field::OnUpdate => {
            if let Some(action) = action() {
                tab.edit_foreign_key(row, |fk| fk.on_update = action);
            }
        }
        Field::OnDelete => {
            if let Some(action) = action() {
                tab.edit_foreign_key(row, |fk| fk.on_delete = action);
            }
        }
    }
}

impl StructureTab {
    fn target_schema(&self, fk: &ForeignKey) -> String {
        if fk.ref_schema.is_empty() { self.schema.clone() } else { fk.ref_schema.clone() }
    }

    // A new key is named after its columns and target until it's named by hand.
    fn edit_foreign_key(&mut self, row: usize, edit: impl FnOnce(&mut ForeignKey)) {
        let driver = self.connection.driver.clone();
        let table = if self.table.is_empty() { self.draft.name.clone() } else { self.table.clone() };
        let suggested = |fk: &ForeignKey| match fk.columns.is_empty() || fk.ref_table.is_empty() {
            true => String::new(),
            false => suggested_name(&driver, NameKind::ForeignKey, &table, &fk.columns, &fk.ref_table),
        };
        let Some(line) = self.draft.foreign_keys.get_mut(row) else { return };
        let follows = line.origin.is_none() && line.value.name == suggested(&line.value);
        edit(&mut line.value);
        if follows {
            line.value.name = suggested(&line.value);
        }
    }

    // Loads the referenced table's columns, and fills an empty column list with its primary key.
    fn pick_target(&mut self, row: usize, table: String, window: &mut Window, cx: &mut Context<Self>) {
        let schema = self.schema.clone();
        let target = (schema.clone(), table.clone());
        self.edit_foreign_key(row, |fk| {
            (fk.ref_schema, fk.ref_table) = target;
            fk.ref_columns.clear();
        });
        if table.is_empty() {
            return;
        }
        let load = schema::columns(&self.connection.id, &schema, &table, cx);
        cx.spawn_in(window, async move |this, cx| {
            let Ok(columns) = load.await else { return };
            let keys: Vec<String> = columns.iter().filter(|c| c.is_primary).map(|c| c.name.clone()).collect();
            let _ = this.update_in(cx, |tab, window, cx| {
                // Rows may have moved meanwhile, so only one still waiting on this table is filled.
                let waiting = tab.draft.foreign_keys.get(row).map(|line| &line.value);
                if waiting.is_some_and(|fk| fk.ref_table == table && fk.ref_columns.is_empty()) && !keys.is_empty() {
                    tab.edit_foreign_key(row, |fk| fk.ref_columns = keys);
                    tab.changed(window, cx);
                }
            });
        })
        .detach();
    }

    // So the referenced-column lists are there when first opened.
    pub(super) fn load_targets(&self, cx: &mut App) {
        for fk in self.draft.foreign_keys.iter().map(|row| &row.value) {
            drop(schema::columns(&self.connection.id, &self.target_schema(fk), &fk.ref_table, cx));
        }
    }
}
