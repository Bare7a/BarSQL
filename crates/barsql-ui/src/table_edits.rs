use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::rc::Rc;

use barsql_core::{DriverType, Row, Value};
use barsql_db::{Cell, ResultSet};
use barsql_io::sql_literal;
use barsql_sql::lang::quoting::format_sql_identifier;
use serde_json::Value as Json;

// Staged edits and deletes are keyed by primary key. Undo and redo replay what each step changed, so a large result's
// staging isn't copied per step.

// Undo and redo keep this many steps each.
const HISTORY_LIMIT: usize = 100;

// Primary key columns as a JSON object with sorted keys, like {"id":1}.
pub type PkKey = String;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pending {
    pub edits: BTreeMap<PkKey, RowEdits>,
    pub deletes: BTreeSet<PkKey>,
}

// A row's staged values by column, where None is a staged NULL.
type RowEdits = BTreeMap<String, Option<String>>;

// Each row a step touched, with its edits before and after the step, and the rows whose delete mark it flipped.
#[derive(Debug, Clone, Default)]
struct Step {
    edits: Vec<(PkKey, Option<RowEdits>, Option<RowEdits>)>,
    deletes: Vec<PkKey>,
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

// Primary keys of a result's rows. Keying a large result is a pass of hashes, which also finds keys rows share, and a
// row's key text is made only once it stages or is asked for.
pub struct Keys {
    set: ResultSet,
    columns: Vec<(usize, String)>,
    // Each row's key cells, hashed.
    hashes: Vec<u64>,
    ambiguous: HashSet<u64>,
    made: RefCell<Made>,
}

#[derive(Default)]
struct Made {
    by_row: HashMap<usize, Rc<str>>,
    rows: HashMap<Rc<str>, usize>,
}

impl Keys {
    pub fn new(set: &ResultSet, primary_keys: &[String]) -> Self {
        let columns: Vec<(usize, String)> = primary_keys
            .iter()
            .filter_map(|name| set.columns.iter().position(|c| &c.name == name).map(|ix| (ix, name.clone())))
            .collect();
        Self::for_columns(set, &columns)
    }

    // Each key column's place in the result and its name in the table, which a query may have aliased.
    pub fn for_columns(set: &ResultSet, columns: &[(usize, String)]) -> Self {
        let mut hashes = Vec::with_capacity(set.rows());
        for (_, chunk) in set.chunks() {
            for row in 0..chunk.rows() {
                let mut hasher = DefaultHasher::new();
                for (column, _) in columns {
                    hash_cell(chunk.cell(row, *column), &mut hasher);
                }
                hashes.push(hasher.finish());
            }
        }
        let mut sorted = hashes.clone();
        sorted.sort_unstable();
        let ambiguous = sorted.windows(2).filter(|pair| pair[0] == pair[1]).map(|pair| pair[0]).collect();
        Self { set: set.clone(), columns: columns.to_vec(), hashes, ambiguous, made: RefCell::default() }
    }

    pub fn key(&self, row: usize) -> Option<Rc<str>> {
        if row >= self.hashes.len() {
            return None;
        }
        if let Some(key) = self.made.borrow().by_row.get(&row) {
            return Some(key.clone());
        }
        let key: BTreeMap<&str, Json> =
            self.columns.iter().map(|(ix, name)| (name.as_str(), cell_json(self.set.cell(row, *ix)))).collect();
        let key: Rc<str> = serde_json::to_string(&key).unwrap_or_default().into();
        let mut made = self.made.borrow_mut();
        made.by_row.insert(row, key.clone());
        made.rows.insert(key.clone(), row);
        Some(key)
    }

    // A staged key's row. Rows make their keys as they stage, so only a key made elsewhere needs a scan.
    pub fn row_of(&self, key: &str) -> Option<usize> {
        {
            let made = self.made.borrow();
            if let Some(&row) = made.rows.get(key) {
                return Some(row);
            }
            if made.by_row.len() == self.hashes.len() {
                return None;
            }
        }
        (0..self.hashes.len()).find(|&row| self.key(row).as_deref() == Some(key))
    }

    fn usable(&self, row: usize) -> Result<Rc<str>, Outcome> {
        let hash = self.hashes.get(row).ok_or(Outcome::Invalid)?;
        if self.ambiguous.contains(hash) {
            return Err(Outcome::Ambiguous);
        }
        self.key(row).ok_or(Outcome::Invalid)
    }
}

fn hash_cell(cell: Cell<'_>, hasher: &mut impl Hasher) {
    match cell {
        Cell::Null => 0u8.hash(hasher),
        Cell::Bool(value) => (1u8, value).hash(hasher),
        Cell::Number(text) => (2u8, text).hash(hasher),
        Cell::Text(text) => (3u8, text).hash(hasher),
    }
}

// History lasts only as long as the tab.
#[derive(Debug, Clone, Default)]
pub struct Staging {
    pub pending: Pending,
    undo: Vec<Step>,
    redo: Vec<Step>,
    // Kept as steps change `pending`, since it's read on every redraw.
    counted_edits: usize,
    // The rows the last change touched, for a view that updates only those.
    touched: Vec<PkKey>,
}

impl Staging {
    // Applying, resetting or reloading makes old steps point at rows that may be gone.
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    // The step is already in `pending`.
    fn commit(&mut self, step: Step) {
        self.undo.push(step);
        if self.undo.len() > HISTORY_LIMIT {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    pub fn undo(&mut self) -> bool {
        self.touched.clear();
        let Some(step) = self.undo.pop() else { return false };
        self.replay(&step, false);
        self.redo.push(step);
        true
    }

    pub fn redo(&mut self) -> bool {
        self.touched.clear();
        let Some(step) = self.redo.pop() else { return false };
        self.replay(&step, true);
        self.undo.push(step);
        true
    }

    // Puts the rows the step touched as it left them, or as it found them. A flipped delete mark flips back either way.
    fn replay(&mut self, step: &Step, forward: bool) {
        let touched: Vec<PkKey> = step.edits.iter().map(|(key, ..)| key).chain(&step.deletes).cloned().collect();
        let counted = |pending: &Pending| {
            touched.iter().filter(|key| pending.edits.contains_key(*key) && !pending.deletes.contains(*key)).count()
        };
        let before = counted(&self.pending);
        for (key, before, after) in &step.edits {
            match if forward { after } else { before } {
                Some(edits) => self.pending.edits.insert(key.clone(), edits.clone()),
                None => self.pending.edits.remove(key),
            };
        }
        for key in &step.deletes {
            if !self.pending.deletes.remove(key) {
                self.pending.deletes.insert(key.clone());
            }
        }
        self.counted_edits = self.counted_edits - before + counted(&self.pending);
        self.touched = touched;
    }

    pub fn touched(&self) -> &[PkKey] {
        &self.touched
    }

    fn flip_deletes(&mut self, keys: Vec<PkKey>) {
        let step = Step { deletes: keys, ..Default::default() };
        self.replay(&step, true);
        self.commit(step);
    }

    // Edits of rows staged for deletion are dropped on apply, so they don't count.
    pub fn edit_count(&self) -> usize {
        self.counted_edits
    }

    pub fn delete_count(&self) -> usize {
        self.pending.deletes.len()
    }

    // With no deletes, every edit counts.
    pub fn has_pending(&self) -> bool {
        !self.pending.deletes.is_empty() || !self.pending.edits.is_empty()
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
        let names: Vec<Option<String>> = set.columns.iter().map(|c| Some(c.name.clone())).collect();
        self.paste_as(set, keys, cells, &names)
    }

    // A column `names` leaves unnamed isn't the table's, so it's skipped.
    pub fn paste_as(
        &mut self,
        set: &ResultSet,
        keys: &Keys,
        cells: &[(usize, usize, Option<String>)],
        names: &[Option<String>],
    ) -> Outcome {
        self.touched.clear();
        // The rows the paste touches, changed apart from `pending` until the step is whole.
        let mut rows: BTreeMap<PkKey, RowEdits> = BTreeMap::new();
        let mut seen: BTreeSet<Rc<str>> = BTreeSet::new();
        let mut ambiguous = false;
        for (row, column, value) in cells {
            let key = match keys.usable(*row) {
                Ok(key) => key,
                Err(Outcome::Ambiguous) => {
                    ambiguous = true;
                    continue;
                }
                Err(_) => continue,
            };
            let Some(name) = names.get(*column).and_then(Option::as_deref) else { continue };
            if seen.insert(key.clone())
                && let Some(edits) = self.pending.edits.get(&*key)
            {
                rows.insert(key.to_string(), edits.clone());
            }
            reconcile_cell_edit(&mut rows, &key, name, set.display(*row, *column), value.as_deref());
        }
        let edits: Vec<_> = seen
            .into_iter()
            .map(|key| (key.to_string(), self.pending.edits.get(&*key).cloned(), rows.remove(&*key)))
            .filter(|(_, before, after)| before != after)
            .collect();
        let changed = !edits.is_empty();
        if changed {
            let step = Step { edits, ..Default::default() };
            self.replay(&step, true);
            self.commit(step);
        }
        match (ambiguous, changed) {
            (true, _) => Outcome::Ambiguous,
            (false, true) => Outcome::Changed,
            (false, false) => Outcome::Unchanged,
        }
    }

    pub fn toggle_delete(&mut self, keys: &Keys, row: usize) -> Outcome {
        self.touched.clear();
        let key = match keys.usable(row) {
            Ok(key) => key.to_string(),
            Err(outcome) => return outcome,
        };
        self.flip_deletes(vec![key]);
        Outcome::Changed
    }

    // Marks every row, or unmarks them all when each is marked already. One undo step, and ambiguous rows are
    // skipped and reported.
    pub fn toggle_deletes(&mut self, keys: &Keys, rows: &[usize]) -> Outcome {
        self.touched.clear();
        let mut ambiguous = false;
        let mut picked: BTreeSet<Rc<str>> = BTreeSet::new();
        for &row in rows {
            match keys.usable(row) {
                Ok(key) => {
                    picked.insert(key);
                }
                Err(outcome) => ambiguous |= outcome == Outcome::Ambiguous,
            }
        }
        if picked.is_empty() {
            return if ambiguous { Outcome::Ambiguous } else { Outcome::Unchanged };
        }
        let all = picked.iter().all(|key| self.pending.deletes.contains(&**key));
        let flipped = picked.into_iter().filter(|key| all || !self.pending.deletes.contains(&**key));
        self.flip_deletes(flipped.map(|key| key.to_string()).collect());
        if ambiguous { Outcome::Ambiguous } else { Outcome::Changed }
    }

    #[cfg(test)]
    fn is_deleted(&self, keys: &Keys, row: usize) -> bool {
        keys.key(row).is_some_and(|key| self.pending.deletes.contains(&*key))
    }

    // Some(None) is a staged NULL.
    #[cfg(test)]
    fn staged(&self, keys: &Keys, row: usize, column: &str) -> Option<Option<&str>> {
        let key = keys.key(row)?;
        self.pending.edits.get(&*key)?.get(column).map(Option::as_deref)
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
    let value = match value {
        Cell::Null => return format!("{ident} IS NULL"),
        Cell::Number(text @ ("NaN" | "Infinity" | "-Infinity")) => Cell::Text(text),
        value => value,
    };
    format!("{ident} = {}", sql_literal(value, driver.dialect(), false))
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

// Decodes a PkKey for update_row and delete_rows.
pub fn key_row(key: &str) -> Row {
    let map: serde_json::Map<String, Json> = serde_json::from_str(key).unwrap_or_default();
    map.into_iter()
        .map(|(column, value)| {
            let value = match value {
                Json::Null => Value::Null,
                Json::Bool(b) => Value::Bool(b),
                Json::Number(n) => {
                    n.as_i64().map(Value::Int).unwrap_or_else(|| Value::Float(n.as_f64().unwrap_or(f64::NAN)))
                }
                Json::String(s) => Value::Text(s),
                other => Value::Text(other.to_string()),
            };
            (column, value)
        })
        .collect()
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
        let columns: Arc<[ColumnMeta]> = ["id", "name"]
            .map(|name| ColumnMeta { name: name.into(), type_name: "TEXT".into(), ..Default::default() })
            .to_vec()
            .into();
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

    fn names(staging: &Staging, keys: &Keys) -> Vec<Option<String>> {
        (0..3).map(|row| staging.staged(keys, row, "name").flatten().map(str::to_string)).collect()
    }

    // Keying hashes every row, which finds keys rows share, and makes a key's text only for the rows that ask.
    #[test]
    fn keys_make_their_text_only_for_rows_that_ask() {
        let set = set(&[("1", "ann"), ("2", "bob"), ("1", "ann again")]);
        let keys = Keys::new(&set, &["id".into()]);
        assert!(keys.made.borrow().by_row.is_empty());
        assert_eq!(keys.usable(1).as_deref(), Ok("{\"id\":2}"));
        assert_eq!(keys.usable(0), Err(Outcome::Ambiguous));
        assert_eq!(keys.row_of("{\"id\":2}"), Some(1));
        assert_eq!(keys.made.borrow().by_row.len(), 1, "only the row that asked");
        assert_eq!(keys.row_of("{\"id\":9}"), None, "after one scan of every row");
        assert_eq!(keys.row_of("{\"id\":9}"), None);
    }

    // The count kept per step matches one taken over everything staged, and each change says which rows it touched.
    #[test]
    fn counts_and_touched_rows_follow_each_step() {
        let set = set(&[("1", "ann"), ("2", "bob"), ("3", "cy")]);
        let keys = Keys::new(&set, &["id".into()]);
        let key = |row: usize| keys.key(row).expect("a key").to_string();
        let walked = |staging: &Staging| {
            staging.pending.edits.keys().filter(|key| !staging.pending.deletes.contains(*key)).count()
        };
        let mut staging = Staging::default();
        staging.paste(&set, &keys, &[(0, 1, Some("a".into())), (1, 1, Some("b".into()))]);
        assert_eq!((staging.edit_count(), staging.touched()), (2, &[key(0), key(1)][..]));
        staging.toggle_deletes(&keys, &[1, 2]);
        assert_eq!((staging.edit_count(), staging.touched()), (1, &[key(1), key(2)][..]));
        staging.edit_cell(&set, &keys, 2, 1, Some("c".into()));
        assert_eq!(staging.edit_count(), walked(&staging));
        assert!(staging.undo() && staging.undo());
        assert_eq!((staging.edit_count(), staging.touched()), (2, &[key(1), key(2)][..]));
        assert!(staging.redo());
        assert_eq!(staging.edit_count(), walked(&staging));
        assert_eq!(staging.paste(&set, &keys, &[(0, 1, Some("a".into()))]), Outcome::Unchanged);
        assert!(staging.touched().is_empty(), "nothing to show again");
    }

    // Each step keeps only what it changed, so taking one back leaves the rows other steps changed as they are.
    #[test]
    fn undo_and_redo_replay_one_step_of_many() {
        let set = set(&[("1", "ann"), ("2", "bob"), ("3", "cy")]);
        let keys = Keys::new(&set, &["id".into()]);
        let mut staging = Staging::default();
        assert_eq!(staging.paste(&set, &keys, &[(0, 1, Some("a".into())), (1, 1, Some("b".into()))]), Outcome::Changed);
        assert_eq!(staging.edit_cell(&set, &keys, 1, 1, Some("bee".into())), Outcome::Changed);
        assert_eq!(staging.toggle_deletes(&keys, &[0, 2]), Outcome::Changed);
        assert_eq!(staging.edit_cell(&set, &keys, 1, 1, Some("bob".into())), Outcome::Changed, "back to the original");
        let deleted = |staging: &Staging| [0, 1, 2].map(|row| staging.is_deleted(&keys, row));
        assert_eq!(
            (names(&staging, &keys), deleted(&staging)),
            (vec![Some("a".into()), None, None], [true, false, true])
        );

        assert!(staging.undo() && staging.undo());
        assert_eq!(names(&staging, &keys), [Some("a".into()), Some("bee".into()), None]);
        assert_eq!(deleted(&staging), [false; 3]);
        assert!(staging.undo());
        assert_eq!(names(&staging, &keys), [Some("a".into()), Some("b".into()), None]);
        assert!(staging.redo() && staging.redo());
        assert_eq!(
            (names(&staging, &keys), deleted(&staging)),
            (vec![Some("a".into()), Some("bee".into()), None], [true, false, true])
        );
        assert_eq!(staging.edit_cell(&set, &keys, 2, 1, Some("c".into())), Outcome::Changed);
        assert!(!staging.redo(), "a new step drops what was undone");
        assert_eq!(
            staging.paste(&set, &keys, &[(0, 1, Some("a".into()))]),
            Outcome::Unchanged,
            "no step for no change"
        );
        assert!(staging.undo());
        assert_eq!(names(&staging, &keys), [Some("a".into()), Some("bee".into()), None]);
    }

    #[test]
    fn staging_edits_deletes_undo_and_redo() {
        let set = set(&[("1", "ann"), ("2", "bob")]);
        let keys = Keys::new(&set, &["id".into()]);
        assert_eq!(keys.key(0).as_deref(), Some("{\"id\":1}"));
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
    fn several_rows_are_marked_for_delete_in_one_step() {
        let set = set(&[("1", "ann"), ("2", "bob"), ("3", "cy")]);
        let keys = Keys::new(&set, &["id".into()]);
        let mut staging = Staging::default();
        staging.toggle_delete(&keys, 1);
        assert_eq!(staging.toggle_deletes(&keys, &[0, 1]), Outcome::Changed, "marks the rest");
        assert!(staging.is_deleted(&keys, 0) && staging.is_deleted(&keys, 1) && !staging.is_deleted(&keys, 2));
        assert_eq!(staging.toggle_deletes(&keys, &[0, 1]), Outcome::Changed, "all marked, so all unmarked");
        assert_eq!(staging.delete_count(), 0);
        assert!(staging.undo());
        assert_eq!(staging.delete_count(), 2, "one undo step");
        assert_eq!(staging.toggle_deletes(&keys, &[]), Outcome::Unchanged);
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
        assert_eq!(Keys::new(&set, &[]).key(0).as_deref(), Some("{}"), "no key, no staging by key");
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
        assert_eq!(foreign_key_filter("path", Cell::Text(r"C:\tmp"), &pg), r"path = E'C:\\tmp'");
        assert_eq!(foreign_key_filter("path", Cell::Text(r"C:\tmp"), &DriverType::Sqlite), r"path = 'C:\tmp'");
    }

    #[test]
    fn mysql_foreign_key_filters_read_backslashes_the_same_in_any_mode() {
        let my = DriverType::MySql;
        assert_eq!(foreign_key_filter("path", Cell::Text(r"C:\tmp"), &my), "path = _utf8mb4 X'433A5C746D70'");
        assert_eq!(foreign_key_filter("name", Cell::Text("O'Brien"), &my), "name = 'O''Brien'");
        assert_eq!(foreign_key_filter("flag", Cell::Bool(true), &my), "flag = TRUE");
    }

    #[test]
    fn tsql_foreign_key_filters_compare_bits_with_numbers() {
        let ms = DriverType::SqlServer;
        assert_eq!(foreign_key_filter("flag", Cell::Bool(true), &ms), "flag = 1");
        assert_eq!(foreign_key_filter("flag", Cell::Bool(false), &ms), "flag = 0");
        assert_eq!(foreign_key_filter("name", Cell::Text("Zoë's"), &ms), "name = N'Zoë''s'");
        assert_eq!(foreign_key_filter("code", Cell::Text("007"), &ms), "code = N'007'");
        assert_eq!(foreign_key_filter("id", Cell::Number("7"), &ms), "id = 7");
        assert_eq!(foreign_key_filter("id", Cell::Null, &ms), "id IS NULL");
        assert_eq!(foreign_key_filter("user id", Cell::Number("7"), &ms), "[user id] = 7");
    }
}
