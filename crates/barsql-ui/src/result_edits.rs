use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use barsql_app::{ResultEdits, ResultTable};
use barsql_core::schema::primary_keys;
use barsql_core::{RowDelete, RowUpdate, SqlDialect};
use barsql_db::{Cell, CellKind, ChunkBuilder, ResultSet};
use barsql_sql::{TypeFamily, type_family};

use crate::grid::{Choices, TableOverlay};
use crate::table_edits::{Keys, Outcome, Staging, changes_row, key_row};

// Staged by primary key, as a table view's edits are.
pub struct ResultEditor {
    table: ResultTable,
    key_columns: Vec<(usize, String)>,
    // On the first edit, as most results never get one.
    keyed: Option<Keys>,
    staging: Staging,
    pub applying: bool,
}

impl ResultEditor {
    // Only when the result holds the whole primary key, so each row stands for one row of the table.
    pub fn new(table: ResultTable, set: &ResultSet) -> Option<Self> {
        let primary = primary_keys(&table.catalog);
        if primary.is_empty() || table.columns.len() != set.columns.len() {
            return None;
        }
        let key_columns = primary
            .into_iter()
            .map(|key| Some((table.columns.iter().position(|c| c.as_deref() == Some(key.as_str()))?, key)))
            .collect::<Option<Vec<_>>>()?;
        Some(Self { table, key_columns, keyed: None, staging: Staging::default(), applying: false })
    }

    pub fn table(&self) -> &str {
        &self.table.table
    }

    pub fn has_pending(&self) -> bool {
        self.staging.has_pending()
    }

    pub fn counts(&self) -> (usize, usize) {
        (self.staging.edit_count(), self.staging.delete_count())
    }

    pub fn stage(&mut self, set: &ResultSet, cells: &[(usize, usize, Option<String>)]) -> Outcome {
        let keys = keyed(&mut self.keyed, set, &self.key_columns);
        self.staging.paste_as(set, keys, cells, &self.table.columns)
    }

    pub fn toggle_deletes(&mut self, set: &ResultSet, rows: &[usize]) -> Outcome {
        let keys = keyed(&mut self.keyed, set, &self.key_columns);
        self.staging.toggle_deletes(keys, rows)
    }

    pub fn undo(&mut self) -> bool {
        self.staging.undo()
    }

    pub fn redo(&mut self) -> bool {
        self.staging.redo()
    }

    pub fn revert(&mut self) {
        self.staging.clear();
    }

    // A staged value shows in every column its table column feeds.
    pub fn overlay(&self, set: &ResultSet, editable: bool) -> TableOverlay {
        let pending = &self.staging.pending;
        let mut staged = HashMap::new();
        let mut deleted = HashSet::new();
        if let Some(keys) = &self.keyed {
            for (key, edits) in pending.edits.iter() {
                let Some(row) = keys.row_of(key) else { continue };
                for (name, value) in edits {
                    for (column, _) in self.table.columns.iter().enumerate().filter(|(_, c)| c.as_ref() == Some(name)) {
                        staged.insert((row, column), value.clone());
                    }
                }
            }
            deleted = pending.deletes.iter().filter_map(|key| keys.row_of(key)).collect();
        }
        let info = |column: usize| {
            let name = self.table.columns.get(column)?.as_deref()?;
            self.table.catalog.iter().find(|c| c.name == name)
        };
        let choices = (0..set.columns.len())
            .filter_map(|column| {
                let info = info(column).filter(|c| !c.enum_values.is_empty())?;
                Some((
                    column,
                    Choices { values: info.enum_values.clone().into(), nullable: info.is_nullable, open: false },
                ))
            })
            .collect();
        TableOverlay {
            staged: Arc::new(staged),
            deleted,
            primary: self.key_columns.iter().map(|(column, _)| *column).collect(),
            choices,
            locked_columns: (0..set.columns.len()).filter(|&c| info(c).is_none_or(|c| c.is_computed)).collect(),
            local: true,
            editable,
            ..Default::default()
        }
    }

    // Brings an overlay from `overlay` up to date with the last change alone, as building it again is a pass over
    // everything staged.
    pub fn patch(&self, overlay: &mut TableOverlay) {
        let Some(keys) = &self.keyed else { return };
        let pending = &self.staging.pending;
        let staged = Arc::make_mut(&mut overlay.staged);
        for key in self.staging.touched() {
            let Some(row) = keys.row_of(key) else { continue };
            let edits = pending.edits.get(key);
            for (column, name) in self.table.columns.iter().enumerate() {
                match name.as_ref().and_then(|name| edits?.get(name)) {
                    Some(value) => staged.insert((row, column), value.clone()),
                    None => staged.remove(&(row, column)),
                };
            }
            if pending.deletes.contains(key) {
                overlay.deleted.insert(row);
            } else {
                overlay.deleted.remove(&row);
            }
        }
    }

    pub fn edits(&self) -> ResultEdits {
        let pending = &self.staging.pending;
        let (schema, table) = (self.table.schema.clone(), self.table.table.clone());
        let updates = pending
            .edits
            .iter()
            .filter(|(key, _)| !pending.deletes.contains(*key))
            .map(|(key, changes)| RowUpdate {
                schema: schema.clone(),
                table: table.clone(),
                primary_key: key_row(key),
                changes: changes_row(changes),
            })
            .collect();
        let delete =
            RowDelete { schema, table, primary_keys: pending.deletes.iter().map(|key| key_row(key)).collect() };
        ResultEdits {
            delete,
            updates,
            columns: self.table.catalog.clone(),
            database: self.table.database.clone(),
            fingerprint: self.table.fingerprint.clone(),
        }
    }

    // The rows as the applied edits left them, and the deleted ones' places in `set`, ascending. A chunk none of them
    // touch is kept as it is.
    pub fn apply_to(&mut self, set: &ResultSet, dialect: Option<SqlDialect>) -> (ResultSet, Vec<usize>) {
        let pending = &self.staging.pending;
        let keys = self.keyed.as_ref();
        let at = |key: &String| keys.and_then(|keys| keys.row_of(key));
        let written: HashMap<usize, _> =
            pending.edits.iter().filter_map(|(key, edits)| Some((at(key)?, edits))).collect();
        let mut removed: Vec<usize> = pending.deletes.iter().filter_map(at).collect();
        removed.sort_unstable();
        let mut touched: Vec<usize> = written.keys().copied().chain(removed.iter().copied()).collect();
        touched.sort_unstable();
        let width = set.columns.len();
        let mut applied = ResultSet::new(set.columns.clone());
        for (start, chunk) in set.chunks() {
            let end = start + chunk.rows();
            if touched.get(touched.partition_point(|&row| row < start)).is_none_or(|&row| row >= end) {
                applied.push(chunk.clone());
                continue;
            }
            let mut builder = ChunkBuilder::new(width, chunk.rows());
            for row in (start..end).filter(|row| removed.binary_search(row).is_err()) {
                let written = written.get(&row);
                for column in 0..width {
                    let cell = chunk.cell(row - start, column);
                    let name = self.table.columns[column].as_deref();
                    match written.and_then(|edits| edits.get(name?)) {
                        Some(value) => {
                            push_written(&mut builder, cell, value.as_deref(), &set.columns[column].type_name, dialect)
                        }
                        None => push_cell(&mut builder, cell),
                    }
                }
                builder.end_row();
            }
            if !builder.is_empty() {
                applied.push(Arc::new(builder.finish()));
            }
        }
        self.keyed = None;
        self.staging.clear();
        (applied, removed)
    }
}

fn keyed<'a>(keyed: &'a mut Option<Keys>, set: &ResultSet, columns: &[(usize, String)]) -> &'a Keys {
    keyed.get_or_insert_with(|| Keys::for_columns(set, columns))
}

fn push_cell(builder: &mut ChunkBuilder, cell: Cell<'_>) {
    match cell {
        Cell::Null => builder.push_null(),
        Cell::Text(text) => builder.push_text(|out| out.push_str(text)),
        Cell::Number(text) => builder.push_number(|out| out.push_str(text)),
        Cell::Bool(value) => builder.push_bool(value),
    }
}

// What was written keeps the kind its cell had, where it fits: digits stay a number and true or false a boolean.
fn push_written(
    builder: &mut ChunkBuilder,
    before: Cell<'_>,
    value: Option<&str>,
    type_name: &str,
    dialect: Option<SqlDialect>,
) {
    let Some(text) = value else { return builder.push_null() };
    let number = text.parse::<f64>().is_ok();
    let flag = matches!(text, "true" | "false");
    let kind = match before {
        Cell::Number(_) if number => CellKind::Number,
        Cell::Bool(_) if flag => CellKind::Bool,
        Cell::Null => match type_family(dialect, type_name) {
            TypeFamily::Number if number => CellKind::Number,
            TypeFamily::Boolean if flag => CellKind::Bool,
            _ => CellKind::Text,
        },
        _ => CellKind::Text,
    };
    builder.push_kind(kind, |out| out.push_str(text));
}

#[cfg(test)]
mod tests {
    use barsql_core::ColumnInfo;
    use barsql_db::ColumnMeta;

    use super::*;

    fn set(rows: &[[Option<&str>; 3]]) -> ResultSet {
        let columns: Arc<[ColumnMeta]> = [("user_id", "INTEGER"), ("n", "TEXT"), ("upper(name)", "TEXT")]
            .map(|(name, type_name)| ColumnMeta {
                name: name.into(),
                type_name: type_name.into(),
                ..Default::default()
            })
            .to_vec()
            .into();
        let mut set = ResultSet::new(columns);
        let mut builder = ChunkBuilder::new(3, rows.len());
        for row in rows {
            builder.push_kind(if row[0].is_some() { CellKind::Number } else { CellKind::Null }, |s| {
                s.push_str(row[0].unwrap_or_default())
            });
            for cell in &row[1..] {
                builder.push_kind(if cell.is_some() { CellKind::Text } else { CellKind::Null }, |s| {
                    s.push_str(cell.unwrap_or_default())
                });
            }
            builder.end_row();
        }
        set.push(Arc::new(builder.finish()));
        set
    }

    fn editor(set: &ResultSet) -> Option<ResultEditor> {
        let catalog = ["id", "name"]
            .map(|name| ColumnInfo { name: name.into(), is_primary: name == "id", ..Default::default() })
            .to_vec();
        let table = ResultTable {
            schema: "main".into(),
            table: "users".into(),
            columns: vec![Some("id".into()), Some("name".into()), None],
            catalog,
            ..Default::default()
        };
        ResultEditor::new(table, set)
    }

    #[test]
    fn edits_go_to_the_table_columns_behind_aliases() {
        let set = set(&[[Some("1"), Some("ann"), Some("ANN")], [Some("2"), Some("bob"), Some("BOB")]]);
        let mut editor = editor(&set).expect("editable");
        assert_eq!(editor.stage(&set, &[(0, 1, Some("anna".into()))]), Outcome::Changed);
        assert_eq!(editor.stage(&set, &[(0, 2, Some("x".into()))]), Outcome::Unchanged, "an expression");
        assert_eq!(editor.toggle_deletes(&set, &[1]), Outcome::Changed);
        let overlay = editor.overlay(&set, true);
        assert_eq!(overlay.staged.get(&(0, 1)), Some(&Some("anna".into())));
        assert_eq!(overlay.deleted, HashSet::from([1]));
        assert_eq!(overlay.primary, HashSet::from([0]));
        assert_eq!(overlay.locked_columns, HashSet::from([2]));
        assert!(overlay.local);

        let edits = editor.edits();
        assert_eq!(edits.delete.primary_keys.len(), 1);
        assert_eq!(edits.updates.len(), 1);
        assert_eq!(edits.updates[0].changes.get("name"), Some(&barsql_core::Value::Text("anna".into())));
        assert_eq!(edits.updates[0].primary_key.get("id"), Some(&barsql_core::Value::Int(1)));

        let (applied, removed) = editor.apply_to(&set, Some(SqlDialect::Sqlite));
        assert_eq!((applied.rows(), removed), (1, vec![1]), "the deleted row is gone");
        assert_eq!(applied.cell(0, 0), Cell::Number("1"));
        assert_eq!(applied.cell(0, 1), Cell::Text("anna"));
        assert_eq!(applied.cell(0, 2), Cell::Text("ANN"), "an expression keeps its value until the query runs again");
        assert!(!editor.has_pending());
    }

    // Patching after each change gives the overlay that building it again would.
    #[test]
    fn a_patched_overlay_matches_one_built_again() {
        let set = set(&[[Some("1"), Some("ann"), Some("ANN")], [Some("2"), Some("bob"), Some("BOB")]]);
        let mut editor = editor(&set).expect("editable");
        let mut overlay = editor.overlay(&set, true);
        let check = |editor: &ResultEditor, overlay: &mut TableOverlay| {
            editor.patch(overlay);
            let built = editor.overlay(&set, true);
            assert_eq!((&overlay.staged, &overlay.deleted), (&built.staged, &built.deleted));
        };
        editor.stage(&set, &[(0, 1, Some("anna".into())), (1, 1, Some("bobby".into()))]);
        check(&editor, &mut overlay);
        editor.toggle_deletes(&set, &[0]);
        check(&editor, &mut overlay);
        editor.stage(&set, &[(1, 1, Some("bob".into()))]);
        check(&editor, &mut overlay);
        assert!(overlay.staged.get(&(1, 1)).is_none(), "back to what it was");
        for undo in [true, true, false] {
            assert!(if undo { editor.undo() } else { editor.redo() });
            check(&editor, &mut overlay);
        }
    }

    // A chunk without an edited or deleted row is kept as it is, rather than copied cell by cell.
    #[test]
    fn applying_builds_only_the_chunks_it_touches_again() {
        let first = set(&[[Some("1"), Some("ann"), Some("ANN")], [Some("2"), Some("bob"), Some("BOB")]]);
        let second = set(&[[Some("3"), Some("cy"), Some("CY")]]);
        let mut both = ResultSet::new(first.columns.clone());
        for (_, chunk) in first.chunks().chain(second.chunks()) {
            both.push(chunk.clone());
        }
        let mut editor = editor(&both).expect("editable");
        assert_eq!(editor.toggle_deletes(&both, &[0]), Outcome::Changed);
        assert_eq!(editor.stage(&both, &[(1, 1, Some("bobby".into()))]), Outcome::Changed);
        let (applied, removed) = editor.apply_to(&both, None);
        assert_eq!((applied.rows(), removed), (2, vec![0]));
        assert_eq!([applied.cell(0, 1), applied.cell(1, 1)], [Cell::Text("bobby"), Cell::Text("cy")]);
        let kept = applied.chunks().nth(1).map(|(start, chunk)| (start, chunk.clone()));
        let untouched = both.chunks().nth(1).map(|(_, chunk)| chunk.clone()).expect("two chunks");
        assert!(kept.is_some_and(|(start, chunk)| start == 1 && Arc::ptr_eq(&chunk, &untouched)));
    }

    #[test]
    fn a_result_without_the_whole_key_has_no_editor() {
        let set = set(&[[Some("1"), Some("ann"), Some("ANN")]]);
        let catalog = vec![ColumnInfo { name: "id".into(), is_primary: true, ..Default::default() }];
        let table = ResultTable { columns: vec![None, Some("name".into()), None], catalog, ..Default::default() };
        assert!(ResultEditor::new(table, &set).is_none());
        let catalog = vec![ColumnInfo { name: "id".into(), ..Default::default() }];
        let keyless = ResultTable { columns: vec![Some("id".into()), None, None], catalog, ..Default::default() };
        assert!(ResultEditor::new(keyless, &set).is_none(), "no primary key");
    }

    #[test]
    fn written_values_keep_their_cells_kind() {
        let mut builder = ChunkBuilder::new(5, 1);
        push_written(&mut builder, Cell::Number("1"), Some("2.5"), "INTEGER", None);
        push_written(&mut builder, Cell::Number("1"), Some("many"), "INTEGER", None);
        push_written(&mut builder, Cell::Null, Some("7"), "integer", Some(SqlDialect::Postgres));
        push_written(&mut builder, Cell::Bool(true), Some("false"), "boolean", Some(SqlDialect::Postgres));
        push_written(&mut builder, Cell::Text("x"), None, "text", None);
        builder.end_row();
        let chunk = builder.finish();
        assert_eq!(
            (0..5).map(|c| chunk.cell(0, c)).collect::<Vec<_>>(),
            [Cell::Number("2.5"), Cell::Text("many"), Cell::Number("7"), Cell::Bool(false), Cell::Null]
        );
    }
}
