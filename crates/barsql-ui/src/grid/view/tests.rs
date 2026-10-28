use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use barsql_db::{ChunkBuilder, ColumnMeta, ResultSet};
use barsql_io::ExportFormat;
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::Root;
use gpui_kit::{
    AppContext as _, Bounds, Context, Entity, InteractiveElement as _, IntoElement, Modifiers, MouseButton,
    MouseDownEvent, MouseUpEvent, ParentElement as _, Pixels, Point, Render, ScrollDelta, ScrollWheelEvent,
    Styled as _, TestAppContext, TouchPhase, VisualTestContext, Window, div, point, px,
};

use super::{Choices, Grid, GridEvent, LANE, MAX_CELL_CHARS, Metrics, TableOverlay, Target, Width, display_text};
use crate::grid::copy::flag;
use crate::grid::range::CellRange;
use crate::test_support::Env;

// The engine's own true and false, or 0 and 1 in a column declared boolean, as SQLite stores them.
#[test]
fn booleans_come_from_the_engine_or_a_boolean_column() {
    use barsql_db::Cell;
    assert_eq!(flag(Cell::Bool(true), "bool"), Some(true));
    assert_eq!(flag(Cell::Bool(false), "text"), Some(false));
    assert_eq!(flag(Cell::Number("1"), "BOOLEAN"), Some(true));
    assert_eq!(flag(Cell::Number("0"), "boolean"), Some(false));
    assert_eq!(flag(Cell::Number("2"), "boolean"), None, "not a boolean value");
    assert_eq!(flag(Cell::Number("1"), "INTEGER"), None);
    assert_eq!(flag(Cell::Null, "boolean"), None, "NULL shows as NULL");
    assert_eq!(flag(Cell::Text("false"), "boolean"), Some(false), "a staged one");
}

#[test]
fn cell_text_collapses_whitespace_like_nowrap() {
    assert_eq!(display_text("plain text"), "plain text");
    assert_eq!(display_text("  a\n\tb  c \r\n"), "a b c");
    assert_eq!(display_text(&"x".repeat(5000)).len(), MAX_CELL_CHARS);
}

fn modifier() -> &'static str {
    if cfg!(target_os = "macos") { "cmd" } else { "ctrl" }
}

fn open(cx: &mut TestAppContext, rows: usize) -> (Env, Entity<Grid>, &mut VisualTestContext) {
    let env = Env::new(cx);
    let (grid, cx) = mount(sample(rows), cx);
    (env, grid, cx)
}

// `rows` rows of (id, name, note) where every third note is NULL.
fn sample(rows: usize) -> ChunkBuilder {
    sample_of(rows, 0..rows)
}

// The rows of `sample(rows)` that `keep` names.
fn sample_of(rows: usize, keep: impl Iterator<Item = usize>) -> ChunkBuilder {
    let mut builder = ChunkBuilder::new(3, rows);
    for row in keep {
        builder.push_number(|s| s.push_str(&(rows - row).to_string()));
        builder.push_text(|s| s.push_str(&format!("name {row}")));
        if row % 3 == 2 {
            builder.push_null();
        } else {
            builder.push_text(|s| s.push_str(&format!("note {row}")));
        }
        builder.end_row();
    }
    builder
}

fn columns() -> Arc<[ColumnMeta]> {
    ["id", "name", "note"]
        .map(|name| ColumnMeta { name: name.into(), type_name: "TEXT".into(), ..Default::default() })
        .to_vec()
        .into()
}

// A focused grid of (id, name, note) in its own window.
fn mount(builder: ChunkBuilder, cx: &mut TestAppContext) -> (Entity<Grid>, &mut VisualTestContext) {
    mount_with(columns(), builder, cx)
}

fn mount_with(
    columns: Arc<[ColumnMeta]>,
    builder: ChunkBuilder,
    cx: &mut TestAppContext,
) -> (Entity<Grid>, &mut VisualTestContext) {
    let grid = cx.new(|cx| Grid::new(columns, cx));
    let view = grid.clone();
    let window = cx.add_window(move |window, cx| Root::new(view, window, cx));
    let cx = VisualTestContext::from_window(window.into(), cx).into_mut();
    grid.update(cx, |grid, cx| grid.push(Arc::new(builder.finish()), cx));
    cx.update(|window, cx| {
        let focus = grid.read(cx).focus_handle.clone();
        window.focus(&focus, cx);
    });
    cx.run_until_parked();
    (grid, cx)
}

fn geometry(grid: &Entity<Grid>, cx: &mut VisualTestContext) -> (Bounds<Pixels>, Metrics, Vec<Pixels>) {
    cx.update(|_, cx| {
        let grid = grid.read(cx);
        (grid.scroll.grid(), grid.metrics, grid.col_x.clone())
    })
}

fn cell(grid: &Entity<Grid>, row: usize, col: usize, cx: &mut VisualTestContext) -> Point<Pixels> {
    let (bounds, m, x) = geometry(grid, cx);
    point(
        bounds.left() + m.gutter_width + (x[col] + x[col + 1]) / 2.,
        bounds.top() + m.header_height + m.row_height * (row as f32 + 0.5),
    )
}

fn gutter(grid: &Entity<Grid>, row: usize, cx: &mut VisualTestContext) -> Point<Pixels> {
    let (bounds, m, _) = geometry(grid, cx);
    point(bounds.left() + m.gutter_width / 2., bounds.top() + m.header_height + m.row_height * (row as f32 + 0.5))
}

fn header_title(grid: &Entity<Grid>, col: usize, cx: &mut VisualTestContext) -> Point<Pixels> {
    let (bounds, m, x) = geometry(grid, cx);
    point(bounds.left() + m.gutter_width + x[col] + m.cell_pad_x + px(2.), bounds.top() + m.header_height / 2.)
}

fn header_chevron(grid: &Entity<Grid>, col: usize, cx: &mut VisualTestContext) -> Point<Pixels> {
    let (bounds, m, x) = geometry(grid, cx);
    point(
        bounds.left() + m.gutter_width + x[col + 1] - m.cell_pad_x - m.chevron / 2.,
        bounds.top() + m.header_height / 2.,
    )
}

fn column_edge(grid: &Entity<Grid>, col: usize, cx: &mut VisualTestContext) -> Point<Pixels> {
    let (bounds, m, x) = geometry(grid, cx);
    point(bounds.left() + m.gutter_width + x[col + 1] - px(2.), bounds.top() + m.header_height / 2.)
}

fn read<T>(grid: &Entity<Grid>, cx: &mut VisualTestContext, f: impl FnOnce(&Grid) -> T) -> T {
    cx.update(|_, cx| f(grid.read(cx)))
}

fn shift() -> Modifiers {
    Modifiers { shift: true, ..Default::default() }
}

#[gpui_kit::test]
fn a_fresh_result_focuses_its_first_row_and_fits_its_columns(cx: &mut TestAppContext) {
    let (_env, grid, cx) = open(cx, 20);
    let (focus, widths, ch) =
        read(&grid, cx, |g| ((g.selection.focus_row, g.selection.focus_col), g.widths.clone(), g.metrics.ch));
    assert_eq!(focus, (Some(0), 0));
    assert_eq!(widths, [Width::Ch(9), Width::Ch(11), Width::Ch(11)], "id+icon, `name 19`, `note 19`");
    let (_, _, x) = geometry(&grid, cx);
    assert_eq!(x[1], (ch * 9.).round());
}

#[gpui_kit::test]
fn arrows_move_the_focus_and_shift_extends_a_range(cx: &mut TestAppContext) {
    let (_env, grid, cx) = open(cx, 20);
    cx.simulate_keystrokes("down right");
    assert_eq!(read(&grid, cx, |g| (g.selection.focus_row, g.selection.focus_col)), (Some(1), 1));
    cx.simulate_keystrokes("shift-down shift-right");
    assert_eq!(read(&grid, cx, |g| g.selection.range), Some(CellRange { r0: 1, r1: 2, c0: 1, c1: 2 }));
    assert_eq!(read(&grid, cx, |g| g.selection_counts()), (2, 2));
    cx.simulate_keystrokes("escape");
    assert_eq!(read(&grid, cx, |g| g.selection_counts()), (0, 0));
    cx.simulate_keystrokes("left left left");
    assert_eq!(read(&grid, cx, |g| g.selection.focus_col), -1, "past the first column is the row number");
    cx.simulate_keystrokes("end");
    assert_eq!(read(&grid, cx, |g| g.selection.focus_row), Some(19));
}

#[gpui_kit::test]
fn header_titles_select_columns_and_chevrons_sort(cx: &mut TestAppContext) {
    let (_env, grid, cx) = open(cx, 20);
    let title = header_title(&grid, 1, cx);
    cx.simulate_click(title, Modifiers::default());
    assert_eq!(read(&grid, cx, |g| g.selection.columns.clone()), BTreeSet::from([1]));
    assert_eq!(read(&grid, cx, |g| g.selection_counts()), (20, 1));

    let chevron = header_chevron(&grid, 0, cx);
    cx.simulate_click(chevron, Modifiers::default());
    let first = read(&grid, cx, |g| g.order.global_at(0));
    assert_eq!(first, Some(19), "ids count down, so ascending starts at the last row");
    cx.simulate_click(chevron, Modifiers::default());
    assert_eq!(read(&grid, cx, |g| (g.sort.desc, g.order.global_at(0))), (true, Some(0)));
}

#[gpui_kit::test]
fn large_results_sort_in_the_background(cx: &mut TestAppContext) {
    let (_env, grid, cx) = open(cx, 30_000);
    let chevron = header_chevron(&grid, 1, cx);
    cx.simulate_click(chevron, Modifiers::default());
    cx.run_until_parked();
    let (first, second, last) =
        read(&grid, cx, |g| (g.order.global_at(0), g.order.global_at(1), g.order.global_at(29_999)));
    assert_eq!((first, second, last), (Some(0), Some(1), Some(29_999)), "name 0, name 1, … name 29999");
    cx.simulate_click(chevron, Modifiers::default());
    cx.run_until_parked();
    assert_eq!(read(&grid, cx, |g| g.order.global_at(0)), Some(29_999));
}

// Applied deletes drop out of the old order, which a large result shows until it has sorted again.
#[gpui_kit::test]
fn a_replaced_large_result_keeps_its_sorted_rows_in_step(cx: &mut TestAppContext) {
    let (_env, grid, cx) = open(cx, 30_000);
    grid.update(cx, |grid, cx| grid.toggle_sort(0, cx));
    cx.run_until_parked();
    let mut set = ResultSet::new(columns());
    set.push(Arc::new(sample_of(30_000, 1..29_999).finish()));
    grid.update(cx, |grid, cx| grid.replace_set(set, &[0, 29_999], cx));
    let ids = |g: &Grid| {
        [0, 1, 29_997, 29_998]
            .map(|display| g.order.global_at(display).and_then(|row| g.set.display(row, 0)).map(str::to_owned))
    };
    let shown = read(&grid, cx, ids);
    assert_eq!(shown, [Some("2".into()), Some("3".into()), Some("29999".into()), None], "ids 1 and 30000 are gone");
    assert!(read(&grid, cx, |g| g.sorting.is_some()), "still sorting");
    cx.run_until_parked();
    assert_eq!(read(&grid, cx, ids), shown);
}

// The status bar's totals read staged values, so they follow an overlay that's set or patched.
#[gpui_kit::test]
fn the_totals_follow_staged_values(cx: &mut TestAppContext) {
    let (_env, grid, cx) = open(cx, 3);
    let title = header_title(&grid, 0, cx);
    cx.simulate_click(title, Modifiers::default());
    let sum = |cx: &mut VisualTestContext| read(&grid, cx, |g| g.aggregate().map(|a| a.columns[0].sum));
    assert_eq!(sum(cx), Some(6.0), "ids 3, 2 and 1");
    let staged = Arc::new(HashMap::from([((0, 0), Some("10".to_string()))]));
    grid.update(cx, |grid, cx| grid.set_overlay(TableOverlay { staged, editable: true, ..Default::default() }, cx));
    assert_eq!(sum(cx), Some(13.0));
    grid.update(cx, |grid, cx| {
        grid.patch_overlay(|overlay| drop(Arc::make_mut(&mut overlay.staged).insert((1, 0), Some("20".into()))), cx)
    });
    assert_eq!(sum(cx), Some(31.0));
}

fn focus_changes(grid: &Entity<Grid>, cx: &mut VisualTestContext) -> Rc<RefCell<usize>> {
    let heard = Rc::new(RefCell::new(0));
    let count = heard.clone();
    cx.update(|_, cx| {
        cx.subscribe(grid, move |_, event: &GridEvent, _| {
            if matches!(event, GridEvent::FocusedRowChanged) {
                *count.borrow_mut() += 1;
            }
        })
        .detach()
    });
    heard
}

// The JSON viewer shows the gutter number, which a sort changes without moving the focus.
#[gpui_kit::test]
fn the_focused_rows_number_follows_the_sort(cx: &mut TestAppContext) {
    let (_env, grid, cx) = open(cx, 3);
    let heard = focus_changes(&grid, cx);
    assert_eq!(read(&grid, cx, |g| g.focused_row().map(|row| (row.row, row.number))), Some((0, 1)));
    grid.update(cx, |grid, cx| grid.toggle_sort(0, cx));
    cx.run_until_parked();
    let focused = read(&grid, cx, |g| g.focused_row().map(|row| (row.row, row.number)));
    assert_eq!(focused, Some((0, 3)), "ids count down, so the first row is now the last");
    assert_eq!(*heard.borrow(), 1);
}

#[gpui_kit::test]
fn the_focused_rows_number_follows_a_sort_in_the_background(cx: &mut TestAppContext) {
    let (_env, grid, cx) = open(cx, 30_000);
    let heard = focus_changes(&grid, cx);
    grid.update(cx, |grid, cx| grid.toggle_sort(0, cx));
    cx.run_until_parked();
    assert_eq!(read(&grid, cx, |g| g.focused_row().map(|row| row.number)), Some(30_000));
    assert_eq!(*heard.borrow(), 1, "once the order is in");
}

#[gpui_kit::test]
fn dragging_selects_a_range_and_a_click_clears_it(cx: &mut TestAppContext) {
    let (_env, grid, cx) = open(cx, 20);
    let (start, end) = (cell(&grid, 0, 0, cx), cell(&grid, 2, 1, cx));
    cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_move(end, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_up(end, MouseButton::Left, Modifiers::default());
    assert_eq!(read(&grid, cx, |g| g.selection.range), Some(CellRange { r0: 0, r1: 2, c0: 0, c1: 1 }));

    let shift_target = cell(&grid, 4, 2, cx);
    cx.simulate_click(shift_target, shift());
    assert_eq!(read(&grid, cx, |g| g.selection.range), Some(CellRange { r0: 0, r1: 4, c0: 0, c1: 2 }));

    let elsewhere = cell(&grid, 5, 1, cx);
    cx.simulate_click(elsewhere, Modifiers::default());
    assert!(read(&grid, cx, |g| g.selection.is_empty()));
    assert_eq!(read(&grid, cx, |g| (g.selection.focus_row, g.selection.focus_col)), (Some(5), 1));
}

#[gpui_kit::test]
fn the_gutter_selects_whole_rows(cx: &mut TestAppContext) {
    let (_env, grid, cx) = open(cx, 20);
    let (one, three) = (gutter(&grid, 1, cx), gutter(&grid, 3, cx));
    cx.simulate_click(one, Modifiers::default());
    cx.simulate_click(three, shift());
    assert_eq!(read(&grid, cx, |g| g.selection.rows.clone()), BTreeSet::from([1, 2, 3]));
    assert_eq!(read(&grid, cx, |g| g.selection_counts()), (3, 3));
}

#[gpui_kit::test]
fn copy_takes_a_lone_cell_raw_and_a_selection_in_the_copy_format(cx: &mut TestAppContext) {
    let (_env, _grid, cx) = open(cx, 3);
    cx.simulate_keystrokes("right");
    cx.simulate_keystrokes(&format!("{}-c", modifier()));
    assert_eq!(cx.read_from_clipboard().and_then(|item| item.text()).as_deref(), Some("name 0"));

    cx.simulate_keystrokes(&format!("{}-a", modifier()));
    cx.simulate_keystrokes(&format!("{}-c", modifier()));
    cx.run_until_parked();
    let text = cx.read_from_clipboard().and_then(|item| item.text());
    assert_eq!(text.as_deref(), Some("id,name,note\n3,name 0,note 0\n2,name 1,note 1\n1,name 2,"));
}

// The grid rounds a long fraction, and sizes its column for what it shows. A copy keeps every digit.
#[gpui_kit::test]
fn long_fractions_show_rounded_and_copy_whole(cx: &mut TestAppContext) {
    let _env = Env::new(cx);
    let mut builder = ChunkBuilder::new(3, 1);
    builder.push_number(|s| s.push_str("133.28571428571428"));
    builder.push_text(|s| s.push('a'));
    builder.push_text(|s| s.push('b'));
    builder.end_row();
    let (grid, cx) = mount(builder, cx);
    assert_eq!(read(&grid, cx, |g| g.widths[0]), Width::Ch(13), "`133.285714` and padding");
    cx.simulate_keystrokes(&format!("{}-c", modifier()));
    assert_eq!(cx.read_from_clipboard().and_then(|item| item.text()).as_deref(), Some("133.28571428571428"));
}

// SQL INSERTs copy in the connection's own quoting, so they run there as they are.
#[gpui_kit::test]
fn a_sql_copy_quotes_for_the_connections_dialect(cx: &mut TestAppContext) {
    let (_env, grid, cx) = open(cx, 1);
    grid.update(cx, |grid, _| grid.set_dialect(Some(barsql_core::SqlDialect::TSql)));
    cx.update(|_, cx| super::set_copy_format(ExportFormat::Sql, cx));
    cx.simulate_keystrokes(&format!("{}-a", modifier()));
    cx.simulate_keystrokes(&format!("{}-c", modifier()));
    cx.run_until_parked();
    let text = cx.read_from_clipboard().and_then(|item| item.text());
    assert_eq!(text.as_deref(), Some("INSERT INTO [results] ([id], [name], [note]) VALUES (1, N'name 0', N'note 0');"));
}

// (id, active, flag): SQLite's 1 and 0 in a column declared BOOLEAN, then the engine's own true and false.
fn booleans(cx: &mut TestAppContext) -> (Entity<Grid>, &mut VisualTestContext) {
    let columns: Arc<[ColumnMeta]> = [("id", "INTEGER"), ("active", "BOOLEAN"), ("flag", "bool")]
        .map(|(name, type_name)| ColumnMeta { name: name.into(), type_name: type_name.into(), ..Default::default() })
        .to_vec()
        .into();
    let mut builder = ChunkBuilder::new(3, 2);
    for (id, active, flag) in [("1", "1", true), ("2", "0", false)] {
        builder.push_number(|s| s.push_str(id));
        builder.push_number(|s| s.push_str(active));
        builder.push_bool(flag);
        builder.end_row();
    }
    mount_with(columns, builder, cx)
}

// The cells the grid asked to stage, as (row, column, value).
type Edits = Rc<RefCell<Vec<(usize, usize, Option<String>)>>>;

fn edits(grid: &Entity<Grid>, cx: &mut VisualTestContext) -> Edits {
    let edits = Rc::new(RefCell::new(Vec::new()));
    let sink = edits.clone();
    cx.update(|_, cx| {
        cx.subscribe(grid, move |_, event: &GridEvent, _| {
            if let GridEvent::EditCell { row, column, value } = event {
                sink.borrow_mut().push((*row, *column, value.clone()));
            }
        })
        .detach()
    });
    edits
}

// A click or Space flips a boolean in an editable table view, and does nothing in query results.
#[gpui_kit::test]
fn a_checkbox_flips_a_boolean_only_where_the_grid_can_be_edited(cx: &mut TestAppContext) {
    let _env = Env::new(cx);
    let (grid, cx) = booleans(cx);
    let edits = edits(&grid, cx);
    let at = read(&grid, cx, |g| g.check_point(0, 1));
    cx.simulate_click(at, Modifiers::none());
    cx.simulate_keystrokes("space");
    assert!(edits.borrow().is_empty(), "query results stay as they are");
    grid.update(cx, |grid, cx| grid.set_overlay(TableOverlay { editable: true, ..Default::default() }, cx));
    cx.simulate_click(at, Modifiers::none());
    cx.simulate_keystrokes("right space");
    assert_eq!(*edits.borrow(), [(0, 1, Some("0".into())), (0, 2, Some("false".into()))]);
}

// Space flips a checkbox, but in a cell's editor it's still a space.
#[gpui_kit::test]
fn space_types_in_a_cells_editor(cx: &mut TestAppContext) {
    let (_env, grid, cx) = open(cx, 3);
    let edits = edits(&grid, cx);
    grid.update(cx, |grid, cx| grid.set_overlay(TableOverlay { editable: true, ..Default::default() }, cx));
    cx.simulate_keystrokes("right enter");
    cx.simulate_keystrokes("space x enter");
    assert_eq!(*edits.borrow(), [(0, 1, Some("name 0 x".into()))]);
}

// (id, kind) in a table view, rows (1, like) and (2, love), where kind takes like, love or wow.
fn reactions(nullable: bool, cx: &mut TestAppContext) -> (Entity<Grid>, &mut VisualTestContext) {
    reactions_rows(nullable, 2, cx)
}

// As reactions, with `rows` rows alternating like and love.
fn reactions_rows(nullable: bool, rows: usize, cx: &mut TestAppContext) -> (Entity<Grid>, &mut VisualTestContext) {
    let columns: Arc<[ColumnMeta]> = [("id", "INTEGER"), ("kind", "TEXT")]
        .map(|(name, type_name)| ColumnMeta { name: name.into(), type_name: type_name.into(), ..Default::default() })
        .to_vec()
        .into();
    let mut builder = ChunkBuilder::new(2, rows);
    for row in 0..rows {
        builder.push_number(|s| s.push_str(&(row + 1).to_string()));
        builder.push_text(|s| s.push_str(if row % 2 == 0 { "like" } else { "love" }));
        builder.end_row();
    }
    let (grid, cx) = mount_with(columns, builder, cx);
    let choices = Choices { values: ["like", "love", "wow"].map(String::from).into(), nullable, open: false };
    let overlay = TableOverlay { editable: true, choices: HashMap::from([(1, choices)]), ..Default::default() };
    grid.update(cx, |grid, cx| grid.set_overlay(overlay, cx));
    (grid, cx)
}

// What the open list holds, NULL as None, and the one Enter takes.
fn picker(grid: &Entity<Grid>, cx: &mut VisualTestContext) -> Option<(Vec<Option<String>>, usize)> {
    read(grid, cx, |g| g.picker())
}

fn kinds(names: &[&str]) -> Vec<Option<String>> {
    names.iter().map(|name| Some(name.to_string())).collect()
}

// A frame, as the app draws after every event. Focus changes and debug bounds arrive with it.
fn draw(cx: &mut VisualTestContext) {
    cx.update(|window, cx| window.draw(cx).clear(cx));
}

// An enum's cell edits with a list of its values under it, starting at the one the cell holds.
#[gpui_kit::test]
fn an_enum_cell_picks_from_its_values(cx: &mut TestAppContext) {
    let _env = Env::new(cx);
    let (grid, cx) = reactions(false, cx);
    let edits = edits(&grid, cx);
    cx.simulate_keystrokes("right down enter");
    assert_eq!(picker(&grid, cx), Some((kinds(&["like", "love", "wow"]), 1)), "the second row holds love");
    draw(cx);
    assert!(cx.debug_bounds("grid-choices").is_some(), "the list shows");
    cx.simulate_keystrokes("down down enter");
    assert_eq!(*edits.borrow(), [(1, 1, Some("wow".into()))], "the list ends at its last value");
    assert_eq!(picker(&grid, cx), None);
}

// Typing narrows the list to the values holding the text, and NULL when the column takes it.
#[gpui_kit::test]
fn typing_narrows_an_enums_values(cx: &mut TestAppContext) {
    let _env = Env::new(cx);
    let (grid, cx) = reactions(true, cx);
    let edits = edits(&grid, cx);
    cx.simulate_keystrokes("right enter");
    assert_eq!(picker(&grid, cx).map(|(list, _)| list.len()), Some(4), "the three values and NULL");
    cx.simulate_keystrokes("o");
    assert_eq!(picker(&grid, cx), Some((kinds(&["love", "wow"]), 0)), "the typed text replaces the value");
    cx.simulate_keystrokes("backspace n");
    assert_eq!(picker(&grid, cx), Some((vec![None], 0)));
    cx.simulate_keystrokes("enter");
    assert_eq!(*edits.borrow(), [(0, 1, None)]);
}

// A click on a value takes it. Escape, or a click away, leaves the cell as it was, whatever is highlighted or typed.
#[gpui_kit::test]
fn a_click_picks_a_value_and_escape_or_a_click_away_keeps_the_cell(cx: &mut TestAppContext) {
    let _env = Env::new(cx);
    let (grid, cx) = reactions(false, cx);
    let edits = edits(&grid, cx);
    // Blurs only reach listeners in an active window.
    cx.update(|window, _| window.activate_window());
    cx.simulate_keystrokes("right enter down escape");
    assert_eq!(picker(&grid, cx), None);
    cx.simulate_keystrokes("enter l");
    draw(cx);
    let away = cell(&grid, 1, 0, cx);
    cx.simulate_click(away, Modifiers::none());
    draw(cx);
    assert_eq!(picker(&grid, cx), None);
    assert!(edits.borrow().is_empty(), "{:?}", edits.borrow());
    cx.simulate_keystrokes("right enter");
    draw(cx);
    let wow = cx.debug_bounds("grid-choice-2").expect("the third value shows").center();
    cx.simulate_click(wow, Modifiers::none());
    assert_eq!(*edits.borrow(), [(1, 1, Some("wow".into()))]);
    assert_eq!(read(&grid, cx, |g| g.selection.focus_row), Some(1), "the click didn't land on the grid under it");
}

// The list goes with its cell when the grid scrolls the cell out of sight.
#[gpui_kit::test]
fn the_list_hides_once_its_cell_scrolls_away(cx: &mut TestAppContext) {
    let _env = Env::new(cx);
    let (grid, cx) = reactions_rows(false, 200, cx);
    cx.simulate_keystrokes("right enter");
    draw(cx);
    assert!(cx.debug_bounds("grid-choices").is_some());
    grid.update(cx, |grid, cx| {
        grid.scroll.scroll_to(point(px(0.), px(2000.)));
        cx.notify();
    });
    draw(cx);
    assert!(cx.debug_bounds("grid-choices").is_none());
}

// Copied from a grid that can't be edited, like query results, and pasted into a table view.
#[gpui_kit::test]
fn a_text_copy_pastes_exact_values_into_an_editable_grid(cx: &mut TestAppContext) {
    let _env = Env::new(cx);
    let mut builder = ChunkBuilder::new(3, 2);
    for (id, name, note) in [("1", "tab\there", Some("has \"quote\"")), ("2", "line1\nline2", None)] {
        builder.push_number(|s| s.push_str(id));
        builder.push_text(|s| s.push_str(name));
        match note {
            Some(note) => builder.push_text(|s| s.push_str(note)),
            None => builder.push_null(),
        }
        builder.end_row();
    }
    let (grid, cx) = mount(builder, cx);
    cx.update(|_, cx| super::set_copy_format(ExportFormat::Text, cx));
    cx.simulate_keystrokes(&format!("{}-a", modifier()));
    cx.simulate_keystrokes(&format!("{}-c", modifier()));
    cx.run_until_parked();
    let text = cx.read_from_clipboard().and_then(|item| item.text());
    assert_eq!(text.as_deref(), Some("1\ttab\there\thas \"quote\"\n2\tline1\nline2\t"), "tabs between columns");

    let pasted = Rc::new(RefCell::new(Vec::new()));
    let sink = pasted.clone();
    cx.update(|_, cx| {
        cx.subscribe(&grid, move |_, event: &GridEvent, _| {
            if let GridEvent::PasteCells(cells) = event {
                sink.borrow_mut().extend(cells.iter().cloned());
            }
        })
        .detach()
    });
    grid.update(cx, |grid, cx| grid.set_overlay(TableOverlay { editable: true, ..Default::default() }, cx));
    cx.simulate_keystrokes(&format!("{}-v", modifier()));
    let value = |s: &str| Some(s.to_string());
    assert_eq!(
        *pasted.borrow(),
        [
            (0, 0, value("1")),
            (0, 1, value("tab\there")),
            (0, 2, value("has \"quote\"")),
            (1, 0, value("2")),
            (1, 1, value("line1\nline2")),
            (1, 2, None),
        ]
    );
}

#[gpui_kit::test]
fn column_edges_resize_and_fit_resets_them(cx: &mut TestAppContext) {
    let (_env, grid, cx) = open(cx, 20);
    let before = read(&grid, cx, |g| g.width_px(0));
    let edge = column_edge(&grid, 0, cx);
    cx.simulate_mouse_down(edge, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_move(point(edge.x + px(50.), edge.y), MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_up(point(edge.x + px(50.), edge.y), MouseButton::Left, Modifiers::default());
    assert_eq!(read(&grid, cx, |g| g.width_px(0)), before + px(50.));
    assert!(read(&grid, cx, |g| g.selection.columns.is_empty()), "a resize is no column click");

    grid.update(cx, |grid, cx| grid.fit_columns(cx));
    cx.run_until_parked();
    assert_eq!(read(&grid, cx, |g| g.width_px(0)), before);
}

#[gpui_kit::test]
fn the_wheel_scrolls_down_and_shift_turns_it_sideways(cx: &mut TestAppContext) {
    let (_env, grid, cx) = open(cx, 500);
    let position = cell(&grid, 1, 0, cx);
    let wheel = |y: f32, modifiers: Modifiers| ScrollWheelEvent {
        position,
        delta: ScrollDelta::Pixels(point(px(0.), px(y))),
        modifiers,
        touch_phase: TouchPhase::Moved,
    };
    cx.simulate_event(wheel(-100., Modifiers::default()));
    assert_eq!(read(&grid, cx, |g| g.scroll.position().y), px(100.));
    cx.simulate_keystrokes("home");
    assert_eq!(read(&grid, cx, |g| g.scroll.position().y), px(0.), "the focused row scrolls into view");
    grid.update(cx, |grid, cx| {
        grid.widths = vec![Width::Px(px(3000.)); 3];
        grid.user_sized = (0..3).collect();
        cx.notify();
    });
    cx.run_until_parked();
    cx.simulate_event(wheel(-80., shift()));
    assert_eq!(read(&grid, cx, |g| g.scroll.position()), point(px(80.), px(0.)));
}

// 500 rows of three columns, each a third of the grid wide, so the grid scrolls both ways and has both lanes.
fn scrolling_both_ways(cx: &mut TestAppContext) -> (Env, Entity<Grid>, &mut VisualTestContext) {
    let (env, grid, cx) = open(cx, 500);
    widen(&grid, cx);
    (env, grid, cx)
}

fn widen(grid: &Entity<Grid>, cx: &mut VisualTestContext) {
    let (bounds, ..) = geometry(grid, cx);
    grid.update(cx, |grid, cx| {
        grid.widths = vec![Width::Px(bounds.size.width / 3.); 3];
        grid.user_sized = (0..3).collect();
        cx.notify();
    });
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
}

// Points in the right lane, beside the cells, and in the bottom lane, below them.
fn lanes(grid: &Entity<Grid>, cx: &mut VisualTestContext) -> [Point<Pixels>; 2] {
    let body = read(grid, cx, |g| g.scroll.viewport());
    [point(body.right() + LANE / 2., body.center().y), point(body.center().x, body.bottom() + LANE / 2.)]
}

fn press(cx: &mut VisualTestContext, at: Point<Pixels>, button: MouseButton) {
    let modifiers = Modifiers::default();
    cx.simulate_event(MouseDownEvent { button, position: at, modifiers, click_count: 1, first_mouse: false });
    cx.simulate_event(MouseUpEvent { button, position: at, modifiers, click_count: 1 });
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
}

fn focused(grid: &Entity<Grid>, cx: &mut VisualTestContext) -> bool {
    cx.update(|window, cx| grid.read(cx).focus_handle.is_focused(window))
}

// The lanes hold no cells, though at the start rows and columns go on under them. Scrolled to the end, the last row
// and column stop just short of them.
#[gpui_kit::test]
fn the_bars_never_cover_the_last_row_or_column(cx: &mut TestAppContext) {
    let (_env, grid, cx) = scrolling_both_ways(cx);
    let hit = |at: Point<Pixels>, cx: &mut VisualTestContext| read(&grid, cx, |g| g.hit(at));
    let body = read(&grid, cx, |g| g.scroll.viewport());
    assert!(matches!(hit(point(body.left() + px(1.), body.bottom() - px(1.)), cx), Some(Target::Cell { .. })));
    assert_eq!(hit(point(body.left() + px(1.), body.bottom() + px(1.)), cx), None, "the bottom lane, over a row");
    assert!(matches!(hit(point(body.right() - px(1.), body.top() + px(1.)), cx), Some(Target::Cell { .. })));
    assert_eq!(hit(point(body.right() + px(1.), body.top() + px(1.)), cx), None, "the right lane, over a column");

    cx.simulate_keystrokes("end right right");
    let (bounds, ..) = geometry(&grid, cx);
    let corner = bounds.bottom_right() - point(LANE, LANE);
    assert_eq!(hit(corner - point(px(1.), px(1.)), cx), Some(Target::Cell { row: 499, col: 2 }));
}

// GPUI Kit's bar takes every button, so a right-click would scroll and the menu would act on the focused cell.
#[gpui_kit::test]
fn a_right_click_in_a_lane_neither_scrolls_nor_opens_the_menu(cx: &mut TestAppContext) {
    let (_env, grid, cx) = scrolling_both_ways(cx);
    for at in lanes(&grid, cx) {
        press(cx, at, MouseButton::Right);
        assert_eq!(read(&grid, cx, |g| g.scroll.position()), point(px(0.), px(0.)), "no jump at {at:?}");
        assert!(focused(&grid, cx), "no menu at {at:?}");
    }
    let cell = cell(&grid, 0, 0, cx);
    press(cx, cell, MouseButton::Right);
    assert!(!focused(&grid, cx), "a cell's menu still opens, and takes the focus");
}

// The grid with something opaque over its right lane, like a dialog or popover.
struct Covered {
    grid: Entity<Grid>,
}

impl Render for Covered {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let cover = div().id("cover").absolute().top_0().right_0().w(px(60.)).h(px(400.)).occlude();
        div().relative().size_full().child(self.grid.clone()).child(cover)
    }
}

#[gpui_kit::test]
fn a_click_on_something_over_a_lane_doesnt_scroll_the_grid(cx: &mut TestAppContext) {
    let _env = Env::new(cx);
    let grid = cx.new(|cx| Grid::new(columns(), cx));
    grid.update(cx, |grid, cx| grid.push(Arc::new(sample(500).finish()), cx));
    let host = grid.clone();
    let window = cx.add_window(move |window, cx| {
        let covered = cx.new(|_| Covered { grid: host });
        Root::new(covered, window, cx)
    });
    let cx = VisualTestContext::from_window(window.into(), cx).into_mut();
    cx.run_until_parked();
    widen(&grid, cx);
    let [right, _] = lanes(&grid, cx);
    let under_cover = point(right.x, read(&grid, cx, |g| g.scroll.viewport()).top() + px(100.));
    press(cx, under_cover, MouseButton::Left);
    assert_eq!(read(&grid, cx, |g| g.scroll.position()), point(px(0.), px(0.)));
    press(cx, point(right.x, right.y + px(150.)), MouseButton::Left);
    assert!(read(&grid, cx, |g| g.scroll.position()).y > px(0.), "the uncovered part of the bar still scrolls");
}

// The bars' tracks are pinned to the lanes, so a theme with wider tracks can't put them over cells.
#[gpui_kit::test]
fn a_wider_theme_track_stays_in_its_lane(cx: &mut TestAppContext) {
    let (_env, grid, cx) = scrolling_both_ways(cx);
    cx.update(|window, cx| {
        let scrollbar = gpui_kit::base::Theme::global(cx).scrollbar.clone();
        let styles = scrollbar.styles().clone().track(|track| track.width(px(30.)));
        gpui_kit::base::Theme::global_mut(cx).scrollbar = scrollbar.with_styles(styles);
        window.refresh();
    });
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    let body = read(&grid, cx, |g| g.scroll.viewport());
    press(cx, point(body.right() - px(8.), body.center().y), MouseButton::Left);
    assert_eq!(read(&grid, cx, |g| g.scroll.position()), point(px(0.), px(0.)), "a click on the cells beside the lane");
}

#[gpui_kit::test]
fn enter_and_double_click_ask_to_view_the_focused_cell(cx: &mut TestAppContext) {
    let (_env, grid, cx) = open(cx, 5);
    let seen = Rc::new(RefCell::new(Vec::new()));
    let sink = seen.clone();
    cx.update(|_, cx| {
        cx.subscribe(&grid, move |_, event: &GridEvent, _| {
            if let GridEvent::ViewCell { row, column } = event {
                sink.borrow_mut().push((*row, *column));
            }
        })
        .detach()
    });
    cx.simulate_keystrokes("down enter");
    let target = cell(&grid, 3, 2, cx);
    cx.simulate_event(gpui_kit::MouseDownEvent {
        position: target,
        modifiers: Modifiers::default(),
        button: MouseButton::Left,
        click_count: 2,
        first_mouse: false,
    });
    cx.simulate_mouse_up(target, MouseButton::Left, Modifiers::default());
    assert_eq!(*seen.borrow(), [(1, 0), (3, 2)]);
}

// An open list only suggests: Enter takes the typed text, unless the arrows chose a value.
#[gpui_kit::test]
fn an_open_list_takes_the_typed_text(cx: &mut TestAppContext) {
    let _env = Env::new(cx);
    let (grid, cx) = reactions(false, cx);
    let choices = Choices { values: ["like", "love", "wow"].map(String::from).into(), nullable: false, open: true };
    let overlay = TableOverlay { editable: true, choices: HashMap::from([(1, choices)]), ..Default::default() };
    grid.update(cx, |grid, cx| grid.set_overlay(overlay, cx));
    let edits = edits(&grid, cx);
    cx.simulate_keystrokes("right enter l o enter");
    cx.simulate_keystrokes("down enter l o down enter");
    assert_eq!(*edits.borrow(), [(0, 1, Some("lo".into())), (1, 1, Some("love".into()))]);
}

// A row pushed since the last frame scrolls into view when its editor opens, as a new row's does.
#[gpui_kit::test]
fn a_row_pushed_since_the_last_frame_scrolls_into_view(cx: &mut TestAppContext) {
    let (_env, grid, cx) = open(cx, 100);
    grid.update(cx, |grid, cx| grid.set_overlay(TableOverlay { editable: true, ..Default::default() }, cx));
    draw(cx);
    cx.update(|window, cx| {
        grid.update(cx, |grid, cx| {
            grid.push(Arc::new(sample(1).finish()), cx);
            grid.edit_cell(100, 1, window, cx);
        })
    });
    let (position, viewport, row) =
        read(&grid, cx, |g| (g.scroll.position(), g.scroll.viewport(), g.metrics.row_height));
    assert!(position.y + viewport.size.height >= row * 101., "{position:?} {viewport:?}");
}

// A cell's own values, like a foreign key's referenced columns, win over its column's.
#[gpui_kit::test]
fn a_cells_own_values_win_over_its_columns(cx: &mut TestAppContext) {
    let _env = Env::new(cx);
    let (grid, cx) = reactions(false, cx);
    let column = Choices { values: ["like", "love", "wow"].map(String::from).into(), nullable: false, open: false };
    let cell = Choices { values: ["meh".to_string()].into(), nullable: false, open: false };
    let overlay = TableOverlay {
        editable: true,
        choices: HashMap::from([(1, column)]),
        cell_choices: HashMap::from([((1, 1), cell)]),
        ..Default::default()
    };
    grid.update(cx, |grid, cx| grid.set_overlay(overlay, cx));
    cx.simulate_keystrokes("right enter");
    assert_eq!(picker(&grid, cx), Some((kinds(&["like", "love", "wow"]), 0)));
    cx.simulate_keystrokes("escape down enter");
    assert_eq!(picker(&grid, cx), Some((kinds(&["meh"]), 0)));
}

// Cells the owner marks invalid, and the icons it puts on cells, are the overlay's.
#[gpui_kit::test]
fn invalid_cells_and_marks_come_from_the_overlay(cx: &mut TestAppContext) {
    let _env = Env::new(cx);
    let (grid, cx) = reactions(false, cx);
    let overlay = TableOverlay {
        editable: true,
        invalid: HashSet::from([(1, 1)]),
        marks: HashMap::from([((0, 1), vec![Lucide::Link, Lucide::ListTree])]),
        ..Default::default()
    };
    grid.update(cx, |grid, cx| grid.set_overlay(overlay, cx));
    assert!(read(&grid, cx, |g| g.is_invalid(1, 1) && !g.is_invalid(0, 1)));
    assert_eq!(read(&grid, cx, |g| g.marks(0, 1)), [Lucide::Link, Lucide::ListTree]);
    assert!(read(&grid, cx, |g| g.marks(1, 1)).is_empty());
}

// Tab moves the editor along the row past checkboxes and locked cells, Shift+Tab back, and at the end only commits.
#[gpui_kit::test]
fn tab_moves_the_editor_along_the_row(cx: &mut TestAppContext) {
    let _env = Env::new(cx);
    let columns: Arc<[ColumnMeta]> = [("name", "TEXT"), ("active", "BOOLEAN"), ("note", "TEXT"), ("extra", "TEXT")]
        .map(|(name, type_name)| ColumnMeta { name: name.into(), type_name: type_name.into(), ..Default::default() })
        .to_vec()
        .into();
    let mut builder = ChunkBuilder::new(4, 1);
    builder.push_text(|s| s.push_str("ann"));
    builder.push_number(|s| s.push('1'));
    builder.push_text(|s| s.push('n'));
    builder.push_text(|s| s.push('x'));
    builder.end_row();
    let (grid, cx) = mount_with(columns, builder, cx);
    let overlay = TableOverlay { editable: true, locked: HashSet::from([(0, 3)]), ..Default::default() };
    grid.update(cx, |grid, cx| grid.set_overlay(overlay, cx));
    let edits = edits(&grid, cx);
    cx.simulate_keystrokes("enter x tab y tab");
    assert_eq!(*edits.borrow(), [(0, 0, Some("annx".into())), (0, 2, Some("ny".into()))]);
    assert!(!read(&grid, cx, |g| g.is_editing()), "the locked last cell takes no editor");
    cx.simulate_keystrokes("enter shift-tab escape");
    assert_eq!(edits.borrow().last(), Some(&(0, 2, Some("n".into()))));
    assert_eq!(read(&grid, cx, |g| (g.focused_cell(), g.is_editing())), (Some((0, 0)), false));
}

// An owner can size the grid to its rows before it first paints: the header and every row, as they're painted.
#[gpui_kit::test]
fn the_fit_height_is_the_header_and_rows(cx: &mut TestAppContext) {
    let (_env, grid, cx) = open(cx, 3);
    let fit = cx.update(|window, cx| grid.read(cx).fit_height(window));
    draw(cx);
    let m = read(&grid, cx, |g| g.metrics);
    assert_eq!(fit, m.header_height + m.row_height * 3.);
}

// A fitted column makes room for its cells' marks beside its text, as it does for a header's key icon.
#[gpui_kit::test]
fn a_fitted_column_makes_room_for_its_marks(cx: &mut TestAppContext) {
    let (_env, grid, cx) = open(cx, 3);
    draw(cx);
    let plain = read(&grid, cx, |g| g.col_x[2] - g.col_x[1]);
    let marks = HashMap::from([((0, 1), vec![Lucide::Link, Lucide::ListTree])]);
    grid.update(cx, |grid, cx| grid.set_overlay(TableOverlay { marks, ..Default::default() }, cx));
    draw(cx);
    let (marked, m) = read(&grid, cx, |g| (g.col_x[2] - g.col_x[1], g.metrics));
    assert_eq!(marked, plain + (m.icon + m.gap) * 2.);
}

// A locked cell keeps its value: its checkbox doesn't flip and Enter opens no editor, where the next row's does.
#[gpui_kit::test]
fn a_locked_cell_does_not_edit(cx: &mut TestAppContext) {
    let _env = Env::new(cx);
    let (grid, cx) = booleans(cx);
    let locked = HashSet::from([(0, 0), (0, 1)]);
    grid.update(cx, |grid, cx| grid.set_overlay(TableOverlay { editable: true, locked, ..Default::default() }, cx));
    let edits = edits(&grid, cx);
    let at = read(&grid, cx, |g| g.check_point(0, 1));
    cx.simulate_click(at, Modifiers::none());
    cx.simulate_keystrokes("space left enter");
    assert!(!read(&grid, cx, |g| g.is_editing()), "no editor opens");
    cx.simulate_keystrokes(&format!("{}-delete down enter x enter", modifier()));
    assert_eq!(*edits.borrow(), [(1, 0, Some("2x".into()))]);
}

// A fixed list keeps its order, so its header chevrons don't sort.
#[gpui_kit::test]
fn a_fixed_list_does_not_sort(cx: &mut TestAppContext) {
    let _env = Env::new(cx);
    let (grid, cx) = reactions(false, cx);
    let overlay = TableOverlay { editable: true, fixed: true, ..Default::default() };
    grid.update(cx, |grid, cx| grid.set_overlay(overlay, cx));
    let sorts = Rc::new(RefCell::new(0));
    let sink = sorts.clone();
    cx.update(|_, cx| {
        cx.subscribe(&grid, move |_, event: &GridEvent, _| {
            if matches!(event, GridEvent::SortRequested(_) | GridEvent::SortTo(_)) {
                *sink.borrow_mut() += 1;
            }
        })
        .detach()
    });
    let chevron = header_chevron(&grid, 0, cx);
    cx.simulate_click(chevron, Modifiers::default());
    assert_eq!(*sorts.borrow(), 0);
}
