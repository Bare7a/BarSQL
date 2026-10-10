// GPUI tests of the diagram view, built from tables in memory.
use std::cell::RefCell;
use std::rc::Rc;

use barsql_core::ColumnInfo;
use gpui_kit::component::Root;
use gpui_kit::{
    AppContext as _, Bounds, Entity, Modifiers, MouseButton, PinchEvent, Pixels, Point, ScrollDelta, ScrollWheelEvent,
    TestAppContext, TouchPhase, VisualTestContext, point, px, size,
};

use super::{ErDiagram, zoom};
use crate::test_support::Env;

type Tables = Vec<(String, Vec<ColumnInfo>)>;

fn column(name: &str, primary: bool, references: Option<&str>) -> ColumnInfo {
    ColumnInfo {
        name: name.into(),
        data_type: "integer".into(),
        is_primary: primary,
        is_foreign: references.is_some(),
        foreign_table: references.unwrap_or_default().into(),
        ..Default::default()
    }
}

// users ← posts ← comments, and tags on its own.
fn small() -> Tables {
    vec![
        ("users".into(), vec![column("id", true, None), column("name", false, None)]),
        ("posts".into(), vec![column("id", true, None), column("user_id", false, Some("users"))]),
        ("comments".into(), vec![column("post_id", false, Some("posts")), column("body", false, None)]),
        ("tags".into(), vec![column("label", false, None)]),
    ]
}

// Too big to read when it all fits.
fn big() -> Tables {
    (0..120)
        .map(|i| {
            let mut columns = vec![column("id", true, None)];
            if i > 0 {
                columns.push(column("parent_id", false, Some(&format!("t{:03}", (i * 7 + 3) % i))));
            }
            columns.extend((0..8).map(|c| column(&format!("c{c}"), false, None)));
            (format!("t{i:03}"), columns)
        })
        .collect()
}

struct Opened(Rc<RefCell<Vec<String>>>);

fn diagram(cx: &mut TestAppContext, tables: Tables) -> (Entity<ErDiagram>, Opened, &mut VisualTestContext) {
    Env::new(cx);
    let opened = Rc::new(RefCell::new(Vec::new()));
    let slot = Rc::new(RefCell::new(None));
    let (out, record) = (slot.clone(), opened.clone());
    let window = cx.add_window(move |window, cx| {
        let on_open: super::OnOpen = Rc::new(move |name, _, _| record.borrow_mut().push(name));
        let view = cx.new(|cx| ErDiagram::new("c".into(), "main".into(), on_open, window, cx));
        *out.borrow_mut() = Some(view.clone());
        Root::new(view, window, cx)
    });
    let cx = VisualTestContext::from_window(window.into(), cx).into_mut();
    cx.simulate_resize(size(px(1200.), px(800.)));
    let view = slot.borrow_mut().take().unwrap();
    view.update(cx, |view, _| view.set_tables(tables.len(), tables));
    // The first frame measures the viewport, and the next one fits to it.
    for _ in 0..3 {
        draw(cx);
    }
    (view, Opened(opened), cx)
}

fn draw(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
}

fn bounds(cx: &mut VisualTestContext, selector: &'static str) -> Bounds<Pixels> {
    draw(cx);
    cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} is not drawn"))
}

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let at = bounds(cx, selector).center();
    cx.simulate_click(at, Modifiers::none());
    draw(cx);
}

fn zoom_of(view: &Entity<ErDiagram>, cx: &mut VisualTestContext) -> f32 {
    view.read_with(cx, |view, _| view.zoom)
}

fn offset(view: &Entity<ErDiagram>, cx: &mut VisualTestContext) -> Point<Pixels> {
    view.read_with(cx, |view, _| view.scroll.offset())
}

fn wheel(cx: &mut VisualTestContext, at: Point<Pixels>, dy: f32, modifiers: Modifiers) {
    cx.simulate_event(ScrollWheelEvent {
        position: at,
        delta: ScrollDelta::Pixels(point(px(0.), px(dy))),
        modifiers,
        ..Default::default()
    });
    draw(cx);
}

// Where, in the content at 100%, the point `at` of the window is.
fn under(view: &Entity<ErDiagram>, at: Point<Pixels>, cx: &mut VisualTestContext) -> Point<Pixels> {
    let rem = cx.update(|window, _| window.rem_size());
    view.read_with(cx, |view, _| {
        let origin = view.scroll.bounds().origin + view.margin(rem, view.zoom) + view.scroll.offset();
        let local = at - origin;
        point(local.x / view.zoom, local.y / view.zoom)
    })
}

#[gpui_kit::test]
fn a_small_schema_opens_at_full_size(cx: &mut TestAppContext) {
    let (view, _, cx) = diagram(cx, small());
    assert_eq!(zoom_of(&view, cx), 1.);
}

// It opens fitted, unless that would be too small to read. Fit then shows it all.
#[gpui_kit::test]
fn a_big_schema_opens_readable_and_fit_shows_it_all(cx: &mut TestAppContext) {
    let (view, _, cx) = diagram(cx, big());
    assert_eq!(zoom_of(&view, cx), zoom::READABLE);
    click(cx, "er-fit");
    let rem = cx.update(|window, _| window.rem_size());
    let (zoom, content, viewport) =
        view.read_with(cx, |view, _| (view.zoom, view.content(rem, view.zoom), view.scroll.bounds().size));
    assert!(zoom < zoom::READABLE, "Fit goes further");
    assert!(content.width <= viewport.width + px(1.) && content.height <= viewport.height + px(1.));
}

#[gpui_kit::test]
fn the_buttons_step_through_the_zooms(cx: &mut TestAppContext) {
    let (view, _, cx) = diagram(cx, small());
    click(cx, "er-zoom-in");
    assert_eq!(zoom_of(&view, cx), 1.1);
    click(cx, "er-zoom-out");
    click(cx, "er-zoom-out");
    assert_eq!(zoom_of(&view, cx), 0.9);
    click(cx, "er-zoom-reset");
    assert_eq!(zoom_of(&view, cx), 1.);
}

#[gpui_kit::test]
fn command_wheel_zooms_around_the_pointer_and_a_plain_wheel_scrolls(cx: &mut TestAppContext) {
    let (view, _, cx) = diagram(cx, big());
    let at = bounds(cx, "er-diagram").center();
    let before = under(&view, at, cx);
    wheel(cx, at, 120., Modifiers::command());
    assert!(zoom_of(&view, cx) > zoom::READABLE, "zoomed in");
    let after = under(&view, at, cx);
    assert!((before.x - after.x).abs() < px(2.) && (before.y - after.y).abs() < px(2.), "{before:?} → {after:?}");
    let (zoom, scrolled) = (zoom_of(&view, cx), offset(&view, cx));
    wheel(cx, at, -80., Modifiers::none());
    assert_eq!(zoom_of(&view, cx), zoom, "a plain wheel doesn't zoom");
    assert!(offset(&view, cx).y < scrolled.y, "it scrolls");
}

#[gpui_kit::test]
fn a_pinch_zooms(cx: &mut TestAppContext) {
    let (view, _, cx) = diagram(cx, small());
    let at = bounds(cx, "er-diagram").center();
    cx.simulate_event(PinchEvent { position: at, delta: -0.2, phase: TouchPhase::Moved, ..Default::default() });
    draw(cx);
    assert!((zoom_of(&view, cx) - 0.8).abs() < 1e-4);
}

#[gpui_kit::test]
fn a_drag_pans_without_picking_the_table_it_started_on(cx: &mut TestAppContext) {
    let (view, _, cx) = diagram(cx, big());
    let start = bounds(cx, "er-table-0").center();
    let before = offset(&view, cx);
    cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
    for step in 1..=5 {
        let at = start - point(px(20. * step as f32), px(10. * step as f32));
        cx.simulate_mouse_move(at, MouseButton::Left, Modifiers::none());
    }
    cx.simulate_mouse_up(start - point(px(100.), px(50.)), MouseButton::Left, Modifiers::none());
    draw(cx);
    let after = offset(&view, cx);
    assert_eq!((after.x - before.x, after.y - before.y), (px(-100.), px(-50.)), "the diagram follows the pointer");
    assert_eq!(view.read_with(cx, |view, _| view.selected), None);
}

#[gpui_kit::test]
fn hovering_previews_a_tables_keys_and_a_click_keeps_them(cx: &mut TestAppContext) {
    let (view, opened, cx) = diagram(cx, small());
    let posts = bounds(cx, "er-table-1").center();
    cx.simulate_mouse_move(posts, None, Modifiers::none());
    draw(cx);
    assert_eq!(view.read_with(cx, |view, _| (view.hovered, view.selected)), (Some(1), None));
    click(cx, "er-table-1");
    assert_eq!(view.read_with(cx, |view, _| view.selected), Some(1));
    let empty = bounds(cx, "er-diagram").bottom_right() - point(px(30.), px(30.));
    cx.simulate_click(empty, Modifiers::none());
    draw(cx);
    assert_eq!(view.read_with(cx, |view, _| view.selected), None, "a click on empty space lets go");
    assert!(opened.0.borrow().is_empty(), "single clicks open nothing");
}

#[gpui_kit::test]
fn a_double_click_or_the_open_button_opens_a_table(cx: &mut TestAppContext) {
    let (_, opened, cx) = diagram(cx, small());
    let users = bounds(cx, "er-table-0").center();
    for click_count in [1, 2] {
        cx.simulate_event(gpui_kit::MouseDownEvent {
            button: MouseButton::Left,
            position: users,
            modifiers: Modifiers::none(),
            click_count,
            first_mouse: false,
        });
        cx.simulate_event(gpui_kit::MouseUpEvent {
            button: MouseButton::Left,
            position: users,
            modifiers: Modifiers::none(),
            click_count,
        });
    }
    draw(cx);
    assert_eq!(*opened.0.borrow(), ["users"]);
    click(cx, "er-table-1");
    click(cx, "er-open-1");
    assert_eq!(*opened.0.borrow(), ["users", "posts"]);
}

#[gpui_kit::test]
fn keys_only_shortens_the_boxes(cx: &mut TestAppContext) {
    let (_, _, cx) = diagram(cx, small());
    let full = bounds(cx, "er-table-0").size.height;
    click(cx, "er-keys-only");
    let keys = bounds(cx, "er-table-0").size.height;
    assert!(keys < full, "users drops its name row: {full:?} → {keys:?}");
}

#[gpui_kit::test]
fn search_picks_the_best_match_and_enter_steps_through_the_rest(cx: &mut TestAppContext) {
    let (view, _, cx) = diagram(cx, big());
    click(cx, "er-search");
    cx.simulate_input("t01");
    draw(cx);
    let picked = |cx: &mut VisualTestContext| view.read_with(cx, |view, _| (view.selected, view.matches.len()));
    assert_eq!(picked(cx), (Some(10), 10), "t010 to t019, by name");
    assert!(cx.debug_bounds("er-matches").is_some());
    cx.simulate_keystrokes("enter");
    draw(cx);
    assert_eq!(picked(cx).0, Some(11));
    cx.simulate_keystrokes("shift-enter shift-enter");
    draw(cx);
    assert_eq!(picked(cx).0, Some(19), "back past the first wraps to the last");
}
