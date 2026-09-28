use std::collections::HashMap;
use std::sync::Arc;

use barsql_db::{Cell, ResultSet};
use barsql_io::export::EXPORT_CHUNK_ROWS;
use barsql_io::{ExportChunk, ExportFormat, Exporter, export_to_string};
use gpui_kit::{ClipboardEntry, ClipboardItem};
use serde::{Deserialize, Serialize};

use super::selection::{Selection, View};

// Copies up to this many cells also carry their values, so a paste into any grid puts back exactly what was
// copied, NULLs, tabs and line breaks included. The text alone can't tell those apart.
const EXACT_PASTE_CELLS: usize = 100_000;

// Result-set column and row indices, both in display order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyTarget {
    pub columns: Vec<usize>,
    pub rows: Vec<usize>,
}

pub fn resolve(selection: &Selection, view: &View) -> CopyTarget {
    let columns = if selection.columns.is_empty() {
        view.columns.to_vec()
    } else {
        view.columns.iter().copied().filter(|c| selection.columns.contains(c)).collect()
    };
    let rows = if selection.rows.is_empty() {
        (0..view.order.len()).filter_map(|display| view.order.global_at(display)).collect()
    } else {
        let mut rows: Vec<usize> = selection.rows.iter().copied().collect();
        rows.sort_by_key(|&row| view.order.display_of(row));
        rows
    };
    CopyTarget { columns, rows }
}

// A focused data cell with nothing selected copies just its raw value.
pub fn single_cell(selection: &Selection, view: &View) -> Option<(usize, usize)> {
    if !selection.rows.is_empty() || !selection.columns.is_empty() || selection.focus_col < 0 {
        return None;
    }
    let row = selection.focus_row?;
    let column = *view.columns.get(selection.focus_col as usize)?;
    Some((row, column))
}

// Staged table view edits keyed by (row, column). `None` is a staged NULL.
pub type Staged = Arc<HashMap<(usize, usize), Option<String>>>;

// A staged value wins over the loaded one.
pub fn shown<'a>(set: &'a ResultSet, staged: Option<&'a Staged>, row: usize, column: usize) -> Cell<'a> {
    match staged.and_then(|staged| staged.get(&(row, column))) {
        Some(Some(text)) => Cell::Text(text),
        Some(None) => Cell::Null,
        None => set.cell(row, column),
    }
}

pub fn cell_text(set: &ResultSet, staged: Option<&Staged>, row: usize, column: usize) -> String {
    shown(set, staged, row, column).display().unwrap_or_default().to_string()
}

// Lazy, so a file can be written while the rest is still being formatted.
pub fn chunks(
    set: ResultSet,
    staged: Option<Staged>,
    format: ExportFormat,
    table: Option<String>,
    target: CopyTarget,
) -> impl Iterator<Item = ExportChunk> {
    let names: Vec<String> = target.columns.iter().map(|&c| set.columns[c].name.clone()).collect();
    let types: Vec<String> = target.columns.iter().map(|&c| set.columns[c].type_name.clone()).collect();
    let mut exporter = Some(Exporter::new(format, &names, &types, table.as_deref(), EXPORT_CHUNK_ROWS));
    let mut rows = target.rows.into_iter();
    std::iter::from_fn(move || {
        loop {
            let Some(row) = rows.next() else {
                return exporter.take()?.finish();
            };
            let cells: Vec<_> = target.columns.iter().map(|&c| shown(&set, staged.as_ref(), row, c)).collect();
            if let Some(chunk) = exporter.as_mut()?.push(&cells) {
                return Some(chunk);
            }
        }
    })
}

pub fn export(
    set: &ResultSet,
    staged: Option<&Staged>,
    format: ExportFormat,
    table: Option<&str>,
    target: &CopyTarget,
) -> String {
    let names: Vec<String> = target.columns.iter().map(|&c| set.columns[c].name.clone()).collect();
    let types: Vec<String> = target.columns.iter().map(|&c| set.columns[c].type_name.clone()).collect();
    let rows = target.rows.iter().map(|&row| target.columns.iter().map(|&c| shown(set, staged, row, c)).collect());
    export_to_string(format, &names, &types, table, rows)
}

// Keeps NULLs as None so a paste can put back exactly what was copied.
pub fn values(set: &ResultSet, staged: Option<&Staged>, target: &CopyTarget) -> Vec<Vec<Option<String>>> {
    target
        .rows
        .iter()
        .map(|&row| target.columns.iter().map(|&c| shown(set, staged, row, c).display().map(str::to_string)).collect())
        .collect()
}

#[derive(Serialize, Deserialize)]
struct CopiedCells {
    barsql_cells: Vec<Vec<Option<String>>>,
    // The copy format's id. None for a lone cell's raw value.
    #[serde(default)]
    format: Option<String>,
}

// Other apps get the text. The values ride along as clipboard metadata, which the platform drops once anything
// else replaces the text.
pub fn clipboard_item(
    text: String,
    format: Option<ExportFormat>,
    set: &ResultSet,
    staged: Option<&Staged>,
    target: &CopyTarget,
) -> ClipboardItem {
    if target.rows.len() * target.columns.len() > EXACT_PASTE_CELLS {
        return ClipboardItem::new_string(text);
    }
    let copied =
        CopiedCells { barsql_cells: values(set, staged, target), format: format.map(|format| format.id().into()) };
    ClipboardItem::new_string_with_json_metadata(text, copied)
}

fn copied(item: &ClipboardItem) -> Option<CopiedCells> {
    match item.entries() {
        [ClipboardEntry::String(text)] => text.metadata_json(),
        _ => None,
    }
}

// The values of a grid copy, or None for text from anywhere else.
pub fn copied_cells(item: &ClipboardItem) -> Option<Vec<Vec<Option<String>>>> {
    copied(item).map(|copied| copied.barsql_cells)
}

// A Text copy of several columns, laid out to line up in the SQL editor, which draws tabs too narrowly to show
// them. Every column but the last is padded to its widest value plus two spaces. Line breaks and tabs inside a
// value become spaces, so each row stays on one line.
pub fn aligned_text(item: &ClipboardItem) -> Option<String> {
    let copied = copied(item).filter(|copied| copied.format.as_deref() == Some(ExportFormat::Text.id()))?;
    let flat = |value: &Option<String>| {
        value.as_deref().unwrap_or_default().replace("\r\n", " ").replace(['\r', '\n', '\t'], " ")
    };
    let rows: Vec<Vec<String>> = copied.barsql_cells.iter().map(|row| row.iter().map(flat).collect()).collect();
    let columns = rows.iter().map(Vec::len).max().filter(|&columns| columns > 1)?;
    let widths: Vec<usize> = (0..columns)
        .map(|c| rows.iter().filter_map(|row| row.get(c)).map(|value| value.chars().count()).max().unwrap_or(0))
        .collect();
    let line = |row: &Vec<String>| {
        let mut line = String::new();
        for (c, value) in row.iter().enumerate() {
            line.push_str(value);
            if c + 1 < row.len() {
                line.push_str(&" ".repeat(widths[c] - value.chars().count() + 2));
            }
        }
        line.trim_end().to_string()
    };
    Some(rows.iter().map(line).collect::<Vec<_>>().join("\n"))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use barsql_db::{ChunkBuilder, ColumnMeta};

    use super::*;
    use crate::grid::sort::RowOrder;

    fn set() -> ResultSet {
        let columns: Arc<[ColumnMeta]> = ["id", "name", "note"]
            .map(|name| ColumnMeta { name: name.into(), type_name: "TEXT".into() })
            .to_vec()
            .into();
        let mut builder = ChunkBuilder::new(3, 3);
        for (id, name, note) in [("1", "ann", Some("a,b")), ("2", "bob", None), ("3", "cy", Some("x"))] {
            builder.push_number(|s| s.push_str(id));
            builder.push_text(|s| s.push_str(name));
            match note {
                Some(note) => builder.push_text(|s| s.push_str(note)),
                None => builder.push_null(),
            }
            builder.end_row();
        }
        let mut set = ResultSet::new(columns);
        set.push(Arc::new(builder.finish()));
        set
    }

    #[test]
    fn nothing_selected_copies_the_view_in_display_order() {
        let order = RowOrder::sorted(vec![2, 0, 1]);
        let columns = [2, 0];
        let view = View { order: &order, columns: &columns };
        let target = resolve(&Selection::default(), &view);
        assert_eq!(target, CopyTarget { columns: vec![2, 0], rows: vec![2, 0, 1] });
        assert_eq!(export(&set(), None, ExportFormat::Csv, None, &target), "note,id\nx,3\n\"a,b\",1\n,2");
        let staged: Staged = Arc::new(HashMap::from([((0, 2), Some("staged".into())), ((2, 0), None)]));
        assert_eq!(export(&set(), Some(&staged), ExportFormat::Csv, None, &target), "note,id\nx,\nstaged,1\n,2");
        assert_eq!(values(&set(), Some(&staged), &target)[1], [Some("staged".into()), Some("1".into())]);
    }

    #[test]
    fn selections_narrow_rows_and_columns() {
        let order = RowOrder::sorted(vec![2, 0, 1]);
        let columns = [0, 1, 2];
        let view = View { order: &order, columns: &columns };
        let select = |rows: &[usize], columns: &[usize]| {
            let mut selection = Selection::default();
            selection.rows = rows.iter().copied().collect();
            selection.columns = columns.iter().copied().collect();
            selection
        };
        assert_eq!(resolve(&select(&[0, 2], &[]), &view), CopyTarget { columns: vec![0, 1, 2], rows: vec![2, 0] });
        assert_eq!(resolve(&select(&[1], &[2, 1]), &view), CopyTarget { columns: vec![1, 2], rows: vec![1] });
        let target = resolve(&select(&[], &[1]), &view);
        assert_eq!(export(&set(), None, ExportFormat::Text, None, &target), "cy\nann\nbob");
    }

    #[test]
    fn chunks_join_into_the_whole_export() {
        let order = RowOrder::identity(3);
        let columns = [0, 1, 2];
        let view = View { order: &order, columns: &columns };
        let target = resolve(&Selection::default(), &view);
        for format in barsql_io::EXPORT_FORMATS {
            let joined: String = chunks(set(), None, format, None, target.clone()).map(|chunk| chunk.text).collect();
            assert_eq!(joined, export(&set(), None, format, None, &target), "{format:?}");
        }
        let rows: usize = chunks(set(), None, ExportFormat::Json, None, target).map(|chunk| chunk.rows).sum();
        assert_eq!(rows, 3);
    }

    #[test]
    fn a_lone_focused_cell_copies_its_raw_value() {
        let order = RowOrder::identity(3);
        let columns = [1, 2];
        let view = View { order: &order, columns: &columns };
        let mut selection = Selection::default();
        selection.focus(1, 1);
        assert_eq!(single_cell(&selection, &view), Some((1, 2)));
        assert_eq!(cell_text(&set(), None, 1, 2), "", "NULL copies as nothing");
        selection.focus(1, -1);
        assert_eq!(single_cell(&selection, &view), None, "the gutter is not a cell");
        selection.focus(0, 0);
        selection.rows.insert(0);
        assert_eq!(single_cell(&selection, &view), None);
    }

    #[test]
    fn grid_copies_carry_their_exact_values() {
        let order = RowOrder::identity(3);
        let columns = [0, 1, 2];
        let view = View { order: &order, columns: &columns };
        let target = resolve(&Selection::default(), &view);
        let text = export(&set(), None, ExportFormat::Text, None, &target);
        let item = clipboard_item(text.clone(), Some(ExportFormat::Text), &set(), None, &target);
        assert_eq!(item.text(), Some(text.clone()), "other apps get the text");
        assert_eq!(copied_cells(&item), Some(values(&set(), None, &target)));
        assert_eq!(copied_cells(&ClipboardItem::new_string(text.clone())), None);
        assert_eq!(copied_cells(&ClipboardItem::new_string_with_json_metadata(text, [1, 2])), None, "another app's");
        let huge = CopyTarget { columns: vec![0], rows: vec![0; EXACT_PASTE_CELLS + 1] };
        assert_eq!(copied_cells(&clipboard_item(String::new(), None, &set(), None, &huge)), None, "too big to carry");
    }

    #[test]
    fn text_copies_line_up_for_the_editor() {
        let order = RowOrder::identity(3);
        let columns = [1, 2, 0];
        let view = View { order: &order, columns: &columns };
        let target = resolve(&Selection::default(), &view);
        let item = |format| clipboard_item(String::new(), format, &set(), None, &target);
        assert_eq!(
            aligned_text(&item(Some(ExportFormat::Text))).as_deref(),
            Some("ann  a,b  1\nbob       2\ncy   x    3")
        );
        assert_eq!(aligned_text(&item(Some(ExportFormat::Csv))), None, "other formats paste as copied");
        assert_eq!(aligned_text(&item(None)), None, "a lone cell pastes raw");
        let one = resolve(&Selection::default(), &View { order: &order, columns: &[1] });
        assert_eq!(aligned_text(&clipboard_item(String::new(), Some(ExportFormat::Text), &set(), None, &one)), None);
        assert_eq!(aligned_text(&ClipboardItem::new_string("a\tb".into())), None);

        let staged: Staged =
            Arc::new(HashMap::from([((0, 1), Some("two\nlines".into())), ((1, 2), Some("t\tab".into()))]));
        let item = clipboard_item(String::new(), Some(ExportFormat::Text), &set(), Some(&staged), &target);
        assert_eq!(aligned_text(&item).as_deref(), Some("two lines  a,b   1\nbob        t ab  2\ncy         x     3"));
    }
}
