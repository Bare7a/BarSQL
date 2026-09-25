use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::Rc;
use std::sync::Arc;

use barsql_db::{ChunkBuilder, ColumnMeta};
use gpui_kit::component::Root;
use gpui_kit::{
    AppContext as _, Bounds, Entity, Modifiers, MouseButton, Pixels, Point, ScrollDelta, ScrollWheelEvent,
    TestAppContext, TouchPhase, VisualTestContext, point, px,
};

use super::{Grid, GridEvent, MAX_CELL_CHARS, Metrics, Width, display_text};
use crate::grid::range::CellRange;
use crate::test_support::Env;

#[test]
fn cell_text_collapses_whitespace_like_nowrap() {
    assert_eq!(display_text("plain text"), "plain text");
    assert_eq!(display_text("  a\n\tb  c \r\n"), "a b c");
    assert_eq!(display_text(&"x".repeat(5000)).len(), MAX_CELL_CHARS);
}

fn modifier() -> &'static str {
    if cfg!(target_os = "macos") { "cmd" } else { "ctrl" }
}

// `rows` rows of (id, name, note) where every third note is NULL.
fn open(cx: &mut TestAppContext, rows: usize) -> (Env, Entity<Grid>, &mut VisualTestContext) {
    let env = Env::new(cx);
    let columns: Arc<[ColumnMeta]> =
        ["id", "name", "note"].map(|name| ColumnMeta { name: name.into(), type_name: "TEXT".into() }).to_vec().into();
    let mut builder = ChunkBuilder::new(3, rows);
    for row in 0..rows {
        builder.push_number(|s| s.push_str(&(rows - row).to_string()));
        builder.push_text(|s| s.push_str(&format!("name {row}")));
        if row % 3 == 2 {
            builder.push_null();
        } else {
            builder.push_text(|s| s.push_str(&format!("note {row}")));
        }
        builder.end_row();
    }
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
    (env, grid, cx)
}

fn geometry(grid: &Entity<Grid>, cx: &mut VisualTestContext) -> (Bounds<Pixels>, Metrics, Vec<Pixels>) {
    cx.update(|_, cx| {
        let grid = grid.read(cx);
        (*grid.bounds.borrow(), grid.metrics, grid.col_x.clone())
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
