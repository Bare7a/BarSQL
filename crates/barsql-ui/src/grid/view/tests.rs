use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::Rc;
use std::sync::Arc;

use barsql_db::{ChunkBuilder, ColumnMeta};
use barsql_io::ExportFormat;
use gpui_kit::component::Root;
use gpui_kit::{
    AppContext as _, Bounds, Context, Entity, InteractiveElement as _, IntoElement, Modifiers, MouseButton,
    MouseDownEvent, MouseUpEvent, ParentElement as _, Pixels, Point, Render, ScrollDelta, ScrollWheelEvent,
    Styled as _, TestAppContext, TouchPhase, VisualTestContext, Window, div, point, px,
};

use super::{Grid, GridEvent, LANE, MAX_CELL_CHARS, Metrics, TableOverlay, Target, Width, display_text};
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

fn open(cx: &mut TestAppContext, rows: usize) -> (Env, Entity<Grid>, &mut VisualTestContext) {
    let env = Env::new(cx);
    let (grid, cx) = mount(sample(rows), cx);
    (env, grid, cx)
}

// `rows` rows of (id, name, note) where every third note is NULL.
fn sample(rows: usize) -> ChunkBuilder {
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
    builder
}

fn columns() -> Arc<[ColumnMeta]> {
    ["id", "name", "note"].map(|name| ColumnMeta { name: name.into(), type_name: "TEXT".into() }).to_vec().into()
}

// A focused grid of (id, name, note) in its own window.
fn mount(builder: ChunkBuilder, cx: &mut TestAppContext) -> (Entity<Grid>, &mut VisualTestContext) {
    let grid = cx.new(|cx| Grid::new(columns(), cx));
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
