use std::collections::{BTreeMap, HashMap, HashSet};

use barsql_core::{DriverType, Row, Value};
use barsql_db::{Cell, ResultSet};
use barsql_sql::lang::quoting::format_sql_identifier;
use serde_json::Value as Json;

// Staged edits and deletes are keyed by primary key. Undo and redo swap whole snapshots.

// Undo and redo keep this many steps each.
const HISTORY_LIMIT: usize = 100;

// Primary key columns as a JSON object with sorted keys, like {"id":1}.
pub type PkKey = String;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pending {
    pub edits: BTreeMap<PkKey, BTreeMap<String, Option<String>>>,
    pub deletes: Vec<PkKey>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Changed,
    Unchanged,
    // Several loaded rows share the key, as with a view or a non-unique key. Editing by it would hit them all.
    Ambiguous,
    Invalid,
}

// Setting a cell back to its original drops the edit, and repeating the recorded value is a no-op.
pub fn reconcile_cell_edit(
    edits: &mut BTreeMap<PkKey, BTreeMap<String, Option<String>>>,
    key: &str,
    column: &str,
    original: Option<&str>,
    value: Option<&str>,
) -> Outcome {
    let mut row = edits.get(key).cloned().unwrap_or_default();
    let recorded = row.get(column).cloned();
    if value == original {
        if recorded.is_none() {
            return Outcome::Unchanged;
        }
        row.remove(column);
    } else {
        if recorded.as_ref().is_some_and(|recorded| recorded.as_deref() == value) {
            return Outcome::Unchanged;
        }
        row.insert(column.to_string(), value.map(str::to_string));
    }
    if row.is_empty() {
        edits.remove(key);
    } else {
        edits.insert(key.to_string(), row);
    }
    Outcome::Changed
}

fn cell_json(cell: Cell<'_>) -> Json {
    match cell {
        Cell::Null => Json::Null,
        Cell::Bool(b) => Json::Bool(b),
        Cell::Number(text) => serde_json::from_str(text).unwrap_or_else(|_| Json::String(text.to_string())),
        Cell::Text(text) => Json::String(text.to_string()),
    }
}

pub struct Keys {
    by_row: Vec<PkKey>,
    ambiguous: HashSet<PkKey>,
}

impl Keys {
    pub fn new(set: &ResultSet, primary_keys: &[String]) -> Self {
        let columns: Vec<(usize, String)> = primary_keys
            .iter()
            .filter_map(|name| set.columns.iter().position(|c| &c.name == name).map(|ix| (ix, name.clone())))
            .collect();
        let by_row: Vec<PkKey> = (0..set.rows())
            .map(|row| {
                let key: BTreeMap<&str, Json> =
                    columns.iter().map(|(ix, name)| (name.as_str(), cell_json(set.cell(row, *ix)))).collect();
                serde_json::to_string(&key).unwrap_or_default()
            })
            .collect();
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for key in &by_row {
            *counts.entry(key).or_default() += 1;
        }
        let ambiguous = counts.into_iter().filter(|(_, n)| *n > 1).map(|(key, _)| key.to_string()).collect();
        Self { by_row, ambiguous }
    }

    pub fn key(&self, row: usize) -> Option<&PkKey> {
        self.by_row.get(row)
    }

    fn usable(&self, row: usize) -> Result<&PkKey, Outcome> {
        let key = self.by_row.get(row).ok_or(Outcome::Invalid)?;
        if self.ambiguous.contains(key) { Err(Outcome::Ambiguous) } else { Ok(key) }
    }
}

// History lasts only as long as the tab.
#[derive(Debug, Clone, Default)]
pub struct Staging {
    pub pending: Pending,
    undo: Vec<Pending>,
    redo: Vec<Pending>,
}

impl Staging {
    // Applying, resetting or reloading makes old snapshots point at rows that may be gone.
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    fn commit(&mut self, next: Pending) {
        self.undo.push(std::mem::replace(&mut self.pending, next));
        if self.undo.len() > HISTORY_LIMIT {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    pub fn undo(&mut self) -> bool {
        let Some(prev) = self.undo.pop() else { return false };
        self.redo.push(std::mem::replace(&mut self.pending, prev));
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(next) = self.redo.pop() else { return false };
        self.undo.push(std::mem::replace(&mut self.pending, next));
        true
    }

    // Edits of rows staged for deletion are dropped on apply, so they don't count.
    pub fn edit_count(&self) -> usize {
        self.pending.edits.keys().filter(|key| !self.pending.deletes.contains(key)).count()
    }

    pub fn delete_count(&self) -> usize {
        self.pending.deletes.len()
    }

    pub fn has_pending(&self) -> bool {
        self.edit_count() > 0 || self.delete_count() > 0
    }

    pub fn edit_cell(
        &mut self,
        set: &ResultSet,
        keys: &Keys,
        row: usize,
        column: usize,
        value: Option<String>,
    ) -> Outcome {
        self.paste(set, keys, &[(row, column, value)])
    }

    // One undo step for the whole batch. Ambiguous rows are skipped and reported.
    pub fn paste(&mut self, set: &ResultSet, keys: &Keys, cells: &[(usize, usize, Option<String>)]) -> Outcome {
        let mut edits = self.pending.edits.clone();
        let (mut changed, mut ambiguous) = (false, false);
        for (row, column, value) in cells {
            let key = match keys.usable(*row) {
                Ok(key) => key,
                Err(Outcome::Ambiguous) => {
                    ambiguous = true;
                    continue;
                }
                Err(_) => continue,
            };
            let Some(meta) = set.columns.get(*column) else { continue };
            let original = set.display(*row, *column);
            changed |= reconcile_cell_edit(&mut edits, key, &meta.name, original, value.as_deref()) == Outcome::Changed;
        }
        if changed {
            self.commit(Pending { edits, deletes: self.pending.deletes.clone() });
        }
        match (ambiguous, changed) {
            (true, _) => Outcome::Ambiguous,
            (false, true) => Outcome::Changed,
            (false, false) => Outcome::Unchanged,
        }
    }

    pub fn toggle_delete(&mut self, keys: &Keys, row: usize) -> Outcome {
        let key = match keys.usable(row) {
            Ok(key) => key.clone(),
            Err(outcome) => return outcome,
        };
        let mut deletes = self.pending.deletes.clone();
        match deletes.iter().position(|k| *k == key) {
            Some(ix) => {
                deletes.remove(ix);
            }
            None => deletes.push(key),
        }
        self.commit(Pending { edits: self.pending.edits.clone(), deletes });
        Outcome::Changed
    }

    #[cfg(test)]
    fn is_deleted(&self, keys: &Keys, row: usize) -> bool {
        keys.key(row).is_some_and(|key| self.pending.deletes.contains(key))
    }

    // Some(None) is a staged NULL.
    #[cfg(test)]
    fn staged(&self, keys: &Keys, row: usize, column: &str) -> Option<Option<&str>> {
        let key = keys.key(row)?;
        self.pending.edits.get(key)?.get(column).map(Option::as_deref)
    }
}

// Tab-separated if there's any tab, as from spreadsheets or our Text copy, else comma-separated.
// Quotes follow RFC 4180, so only a leading one opens a quoted field. A trailing newline adds no row.
pub fn parse_clipboard_grid(text: &str) -> Vec<Vec<String>> {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    if normalized.is_empty() {
        return Vec::new();
    }
    let delimiter = if normalized.contains('\t') { '\t' } else { ',' };
    let (mut grid, mut row, mut field, mut quoted) = (Vec::new(), Vec::new(), String::new(), false);
    let mut chars = normalized.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    field.push('"');
                    chars.next();
                }
                '"' => quoted = false,
                c => field.push(c),
            }
            continue;
        }
        match c {
            '"' if field.is_empty() => quoted = true,
            '\n' => {
                row.push(std::mem::take(&mut field));
                grid.push(std::mem::take(&mut row));
            }
            c if c == delimiter => row.push(std::mem::take(&mut field)),
            c => field.push(c),
        }
    }
    row.push(field);
    grid.push(row);
    if grid.len() > 1 && grid.last().is_some_and(|last| last.len() == 1 && last[0].is_empty()) {
        grid.pop();
    }
    grid
}

// Text can't say NULL, so an empty field pastes as one.
pub fn text_cells(text: &str) -> Vec<Vec<Option<String>>> {
    let field = |field: String| (!field.is_empty()).then_some(field);
    parse_clipboard_grid(text).into_iter().map(|row| row.into_iter().map(field).collect()).collect()
}

// `anchor_row` is a display row and `anchor_col` a column position. Cells past the loaded rows or visible
// columns are dropped.
pub fn paste_cells(
    grid: &[Vec<Option<String>>],
    anchor_row: usize,
    anchor_col: usize,
    rows: usize,
    columns: &[usize],
) -> Vec<(usize, usize, Option<String>)> {
    let mut cells = Vec::new();
    for (dr, fields) in grid.iter().enumerate() {
        let row = anchor_row + dr;
        if row >= rows {
            continue;
        }
        for (dc, field) in fields.iter().enumerate() {
            let Some(&column) = columns.get(anchor_col + dc) else { continue };
            cells.push((row, column, field.clone()));
        }
    }
    cells
}

// Numeric-looking text stays quoted, since unquoting a text key like '007' would match another row.
pub fn foreign_key_filter(column: &str, value: Cell<'_>, driver: &DriverType) -> String {
    let ident = format_sql_identifier(column, driver);
    let literal = match value {
        Cell::Null => return format!("{ident} IS NULL"),
        Cell::Bool(true) => "TRUE".to_string(),
        Cell::Bool(false) => "FALSE".to_string(),
        Cell::Number(text) if !matches!(text, "NaN" | "Infinity" | "-Infinity") => text.to_string(),
        Cell::Number(text) | Cell::Text(text) => format!("'{}'", text.replace('\'', "''")),
    };
    format!("{ident} = {literal}")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnedCell {
    Null,
    Bool(bool),
    Number(String),
    Text(String),
}

impl OwnedCell {
    pub fn new(cell: Cell<'_>) -> Self {
        match cell {
            Cell::Null => Self::Null,
            Cell::Bool(b) => Self::Bool(b),
            Cell::Number(text) => Self::Number(text.to_string()),
            Cell::Text(text) => Self::Text(text.to_string()),
        }
    }

    pub fn cell(&self) -> Cell<'_> {
        match self {
            Self::Null => Cell::Null,
            Self::Bool(b) => Cell::Bool(*b),
            Self::Number(text) => Cell::Number(text),
            Self::Text(text) => Cell::Text(text),
        }
    }
}

// In the shape update_row expects, with None as NULL.
pub fn changes_row(changes: &BTreeMap<String, Option<String>>) -> Row {
    changes.iter().map(|(column, value)| (column.clone(), value.clone().map_or(Value::Null, Value::Text))).collect()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use barsql_db::{ChunkBuilder, ColumnMeta};

    use super::*;

    type Edits = BTreeMap<PkKey, BTreeMap<String, Option<String>>>;
    type Entry<'a> = (&'a str, &'a [(&'a str, Option<&'a str>)]);

    fn draft(entries: &[Entry]) -> Edits {
        entries
            .iter()
            .map(|(key, cols)| {
                (key.to_string(), cols.iter().map(|(c, v)| (c.to_string(), v.map(str::to_string))).collect())
            })
            .collect()
    }

    #[test]
    fn reconciling_records_reverts_and_skips_repeats() {
        let mut edits = draft(&[]);
        assert_eq!(reconcile_cell_edit(&mut edits, "pk1", "name", Some("alice"), Some("bob")), Outcome::Changed);
        assert_eq!(edits, draft(&[("pk1", &[("name", Some("bob"))])]));
        let mut edits = draft(&[]);
        assert_eq!(reconcile_cell_edit(&mut edits, "pk1", "name", Some("alice"), Some("alice")), Outcome::Unchanged);
        let mut edits = draft(&[("pk1", &[("name", Some("bob"))])]);
        assert_eq!(reconcile_cell_edit(&mut edits, "pk1", "name", Some("alice"), Some("alice")), Outcome::Changed);
        assert!(edits.is_empty());
        let mut edits = draft(&[("pk1", &[("name", Some("bob")), ("age", Some("30"))])]);
        reconcile_cell_edit(&mut edits, "pk1", "name", Some("alice"), Some("alice"));
        assert_eq!(edits, draft(&[("pk1", &[("age", Some("30"))])]));
        let mut edits = draft(&[("pk1", &[("name", Some("bob"))])]);
        assert_eq!(reconcile_cell_edit(&mut edits, "pk1", "name", Some("alice"), Some("bob")), Outcome::Unchanged);
    }

    #[test]
    fn nulls_reconcile_like_values() {
        let mut edits = draft(&[]);
        assert_eq!(reconcile_cell_edit(&mut edits, "pk1", "age", Some("30"), Some("30")), Outcome::Unchanged);
        assert_eq!(reconcile_cell_edit(&mut edits, "pk1", "age", Some("30"), Some("31")), Outcome::Changed);
        assert_eq!(edits, draft(&[("pk1", &[("age", Some("31"))])]));
        let mut edits = draft(&[]);
        assert_eq!(reconcile_cell_edit(&mut edits, "pk1", "note", None, None), Outcome::Unchanged);
        assert_eq!(reconcile_cell_edit(&mut edits, "pk1", "note", Some("text"), None), Outcome::Changed);
        assert_eq!(edits, draft(&[("pk1", &[("note", None)])]));
        assert_eq!(reconcile_cell_edit(&mut edits, "pk1", "note", Some("text"), Some("text")), Outcome::Changed);
        assert!(edits.is_empty());
    }

    fn set(rows: &[(&str, &str)]) -> ResultSet {
        let columns: Arc<[ColumnMeta]> =
            ["id", "name"].map(|name| ColumnMeta { name: name.into(), type_name: "TEXT".into() }).to_vec().into();
        let mut builder = ChunkBuilder::new(2, rows.len());
        for (id, name) in rows {
            builder.push_number(|s| s.push_str(id));
            builder.push_text(|s| s.push_str(name));
            builder.end_row();
        }
        let mut set = ResultSet::new(columns);
        set.push(Arc::new(builder.finish()));
        set
    }

    #[test]
    fn staging_edits_deletes_undo_and_redo() {
        let set = set(&[("1", "ann"), ("2", "bob")]);
        let keys = Keys::new(&set, &["id".into()]);
        assert_eq!(keys.key(0).map(String::as_str), Some("{\"id\":1}"));
        let mut staging = Staging::default();
        assert_eq!(staging.edit_cell(&set, &keys, 0, 1, Some("anna".into())), Outcome::Changed);
        assert_eq!(staging.staged(&keys, 0, "name"), Some(Some("anna")));
        assert_eq!(staging.toggle_delete(&keys, 1), Outcome::Changed);
        assert!(staging.is_deleted(&keys, 1));
        assert_eq!((staging.edit_count(), staging.delete_count()), (1, 1));
        assert!(staging.undo());
        assert!(!staging.is_deleted(&keys, 1));
        assert!(staging.redo());
        assert!(staging.is_deleted(&keys, 1));
        assert_eq!(staging.edit_cell(&set, &keys, 0, 1, Some("ann".into())), Outcome::Changed, "back to the original");
        assert_eq!(staging.staged(&keys, 0, "name"), None);
        assert!(!staging.redo(), "a new change drops the redo branch");
        staging.clear();
        assert!(!staging.has_pending() && !staging.undo());
    }

    #[test]
    fn rows_sharing_a_key_cannot_be_staged() {
        let set = set(&[("1", "ann"), ("1", "dup"), ("2", "bob")]);
        let keys = Keys::new(&set, &["id".into()]);
        let mut staging = Staging::default();
        assert_eq!(staging.edit_cell(&set, &keys, 0, 1, Some("x".into())), Outcome::Ambiguous);
        assert_eq!(staging.toggle_delete(&keys, 1), Outcome::Ambiguous);
        let cells = [(0, 1, Some("x".into())), (2, 1, Some("y".into()))];
        assert_eq!(staging.paste(&set, &keys, &cells), Outcome::Ambiguous, "the rest of a paste still lands");
        assert_eq!(staging.staged(&keys, 2, "name"), Some(Some("y")));
        assert_eq!(Keys::new(&set, &[]).key(0).map(String::as_str), Some("{}"), "no key, no staging by key");
    }

    #[test]
    fn clipboard_text_parses_into_fields() {
        let grid = |text: &str| parse_clipboard_grid(text);
        let rows =
            |rows: &[&[&str]]| rows.iter().map(|r| r.iter().map(|s| s.to_string()).collect()).collect::<Vec<Vec<_>>>();
        assert!(grid("").is_empty());
        assert_eq!(grid("hello"), rows(&[&["hello"]]));
        assert_eq!(grid("Doe, John"), rows(&[&["Doe", " John"]]));
        assert_eq!(grid("a,b\tc,d"), rows(&[&["a,b", "c,d"]]));
        assert_eq!(grid("a\tb\tc\n1\t2\t3"), rows(&[&["a", "b", "c"], &["1", "2", "3"]]));
        assert_eq!(grid("a,b,c\n1,2,3"), rows(&[&["a", "b", "c"], &["1", "2", "3"]]));
        assert_eq!(grid("\"a,b\",\"c\"\"d\",\"e\nf\""), rows(&[&["a,b", "c\"d", "e\nf"]]));
        assert_eq!(grid("a\tb\r\nc\td\re\tf"), rows(&[&["a", "b"], &["c", "d"], &["e", "f"]]));
        assert_eq!(grid("a\tb\n1\t2\n"), rows(&[&["a", "b"], &["1", "2"]]));
        assert_eq!(grid("a\t\tc"), rows(&[&["a", "", "c"]]));
        assert_eq!(grid("has \"quote\"\t{\"b\":1}"), rows(&[&["has \"quote\"", "{\"b\":1}"]]), "quotes inside a field");
        assert_eq!(grid("\"a\tb\"\tc"), rows(&[&["a\tb", "c"]]));
        assert_eq!(text_cells("a\t\tc"), [[Some("a".into()), None, Some("c".into())]], "an empty field is NULL");
    }

    #[test]
    fn pasted_grids_land_from_the_anchor_within_the_rows_and_columns() {
        let columns = [0, 1, 2, 3, 4];
        let field = |s: &str| Some(s.to_string());
        let grid: Vec<Vec<Option<String>>> =
            (0..3).map(|r| (1..=3).map(|c| field(&(r * 3 + c).to_string())).collect()).collect();
        let cells = paste_cells(&grid, 1, 1, 100, &columns);
        assert_eq!(cells.len(), 9);
        assert_eq!(
            (cells[0].clone(), cells[2].clone(), cells[8].clone()),
            ((1, 1, field("1")), (1, 3, field("3")), (3, 3, field("9")))
        );
        assert_eq!(paste_cells(&[vec![field("x")]], 5, 2, 100, &columns), [(5, 2, field("x"))]);
        assert_eq!(paste_cells(&[vec![field(""), None]], 0, 0, 100, &columns), [(0, 0, field("")), (0, 1, None)]);
        let column_of_three = [vec![field("1")], vec![field("2")], vec![field("3")]];
        assert_eq!(paste_cells(&column_of_three, 1, 0, 2, &columns), [(1, 0, field("1"))]);
        assert_eq!(paste_cells(&[vec![field("1"), field("2"), field("3")]], 0, 4, 100, &columns), [(0, 4, field("1"))]);
        assert!(paste_cells(&[], 0, 0, 100, &columns).is_empty());
    }

    #[test]
    fn foreign_key_filters_quote_text_and_identifiers() {
        let pg = DriverType::Postgres;
        assert_eq!(foreign_key_filter("id", Cell::Number("1"), &pg), "id = 1");
        assert_eq!(foreign_key_filter("code", Cell::Text("ab"), &pg), "code = 'ab'");
        assert_eq!(foreign_key_filter("name", Cell::Text("O'Brien"), &pg), "name = 'O''Brien'");
        assert_eq!(foreign_key_filter("code", Cell::Text("007"), &pg), "code = '007'");
        assert_eq!(foreign_key_filter("flag", Cell::Bool(true), &pg), "flag = TRUE");
        assert_eq!(foreign_key_filter("flag", Cell::Bool(false), &pg), "flag = FALSE");
        assert_eq!(foreign_key_filter("id", Cell::Number("9007199254740993"), &pg), "id = 9007199254740993");
        assert_eq!(foreign_key_filter("id", Cell::Null, &pg), "id IS NULL");
        assert_eq!(foreign_key_filter("order", Cell::Number("1"), &pg), "\"order\" = 1");
        assert_eq!(foreign_key_filter("order", Cell::Number("1"), &DriverType::MySql), "`order` = 1");
        assert_eq!(foreign_key_filter("user id", Cell::Number("1"), &DriverType::Sqlite), "\"user id\" = 1");
    }
}
