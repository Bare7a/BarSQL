use barsql_io::ExportFormat;
use gpui_kit::{Modifiers, ScrollDelta, ScrollWheelEvent, TestAppContext, TouchPhase, point, px};

use super::driver::{Driver, open};
use crate::results::ResultStatus;

#[gpui_kit::test]
fn a_simple_query_shows_its_row(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.connect();
    app.run("SELECT 1 AS one;");
    assert_eq!(app.cell(0, 0).as_deref(), Some("1"));
}

#[gpui_kit::test]
fn two_statements_fill_two_result_tabs_to_switch_between(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.connect();
    app.run("SELECT 1 AS a; SELECT 2 AS b;");
    assert_eq!(app.result_tabs(), 2);
    app.click("result-tab-0");
    assert_eq!(app.cell(0, 0).as_deref(), Some("1"));
    app.click("result-tab-1");
    assert_eq!(app.cell(0, 0).as_deref(), Some("2"));
}

#[gpui_kit::test]
fn run_takes_only_the_selected_statement(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.connect();
    app.set_sql("SELECT 1 AS first;\nSELECT 7 AS sel;");
    app.keys(if cfg!(target_os = "macos") { "cmd-down shift-home" } else { "ctrl-end shift-home" });
    app.click("run");
    app.wait_idle();
    assert_eq!(app.result_tabs(), 1);
    assert_eq!(app.cell(0, 0).as_deref(), Some("7"));
}

#[gpui_kit::test]
fn the_editors_right_click_menu_selects_all(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.connect();
    app.set_sql("SELECT 1 AS first;\nSELECT 2 AS second;");
    // Undo, Redo, Cut, Copy, Paste, Delete, then Select All.
    app.input_menu("query-editor", 6);
    let tab = app.tab();
    let selected = app.cx.update(|_, cx| tab.read(cx).editor().read(cx).selected_text().to_string());
    assert_eq!(selected, "SELECT 1 AS first;\nSELECT 2 AS second;");
}

#[gpui_kit::test]
fn vs_code_line_commands_edit_the_current_line(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.connect();
    app.set_sql("one\ntwo\nthree");
    let tab = app.tab();
    let editor = app.cx.update(|_, cx| tab.read(cx).editor());
    let caret = |app: &mut super::driver::Driver, at: usize| {
        app.cx.update(|_, cx| editor.update(cx, |state, cx| state.set_selected_range(at..at, cx)))
    };
    caret(&mut app, 5);
    app.keys("alt-down");
    assert_eq!(app.sql(), "one\nthree\ntwo");
    app.keys("alt-up");
    assert_eq!(app.sql(), "one\ntwo\nthree");
    app.keys("shift-alt-down");
    assert_eq!(app.sql(), "one\ntwo\ntwo\nthree");
    app.keys("mod-shift-k");
    assert_eq!(app.sql(), "one\ntwo\nthree");
    app.keys("mod-/");
    assert_eq!(app.sql(), "one\ntwo\n-- three");
    app.keys("mod-/");
    assert_eq!(app.sql(), "one\ntwo\nthree");
    caret(&mut app, 5);
    app.keys("mod-x");
    assert_eq!(app.sql(), "one\nthree");
    let copied = app.cx.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text()));
    assert_eq!(copied.as_deref(), Some("two\n"));
    app.keys("mod-d");
    let selected = app.cx.update(|_, cx| editor.read(cx).selected_text().to_string());
    assert_eq!(selected, "three");
    caret(&mut app, 6);
    app.keys("mod-v");
    assert_eq!(app.sql(), "one\ntwo\nthree", "a cut line pastes above the caret's line");
    assert_eq!(app.cx.update(|_, cx| editor.read(cx).cursor()), 10, "the caret keeps its place");
    caret(&mut app, 1);
    app.keys("mod-c mod-v");
    assert_eq!(app.sql(), "one\none\ntwo\nthree");
    assert_eq!(app.cx.update(|_, cx| editor.read(cx).cursor()), 5);
    app.cx.update(|_, cx| cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string("X".into())));
    caret(&mut app, 0);
    app.keys("mod-v");
    assert_eq!(app.sql(), "Xone\none\ntwo\nthree", "other text pastes at the caret");
}

// The editor draws tabs too narrowly to show them, so Text copies from the results line up in columns there.
#[gpui_kit::test]
fn a_text_copy_of_results_pastes_into_the_editor_in_columns(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.connect();
    app.run("SELECT 1 AS id, 'Ann' AS name, NULL AS note UNION ALL SELECT 22, 'Bartholomew', 'x';");
    app.cx.update(|_, cx| crate::grid::set_copy_format(ExportFormat::Text, cx));
    let grid = app.grid().expect("a grid");
    let at = app.grid_point(&grid, |grid| grid.cell_point(0, 0));
    app.click_at(at, Modifiers::none());
    app.keys("mod-a mod-c");
    let copied = app.cx.read_from_clipboard().and_then(|item| item.text());
    assert_eq!(copied.as_deref(), Some("1\tAnn\t\n22\tBartholomew\tx"), "other apps get tabs");
    app.set_sql("");
    app.keys("mod-v");
    assert_eq!(app.sql(), "1   Ann\n22  Bartholomew  x");
}

// On Linux GPUI Kit's own keys duplicate the line, so VS Code's Ctrl+Shift+Down is used there.
#[gpui_kit::test]
fn a_cursor_added_below_types_on_both_lines(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.connect();
    app.set_sql("ab\ncd");
    app.keys(if cfg!(target_os = "macos") {
        "cmd-alt-down"
    } else if cfg!(target_os = "windows") {
        "ctrl-alt-down"
    } else {
        "ctrl-shift-down"
    });
    app.type_text("X");
    assert_eq!(app.sql(), "Xab\nXcd");
}

#[gpui_kit::test]
fn a_failing_query_shows_its_error_card_and_marks_the_status_bar(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.connect();
    app.run("SELECT * FROM definitely_missing_table_e2e;");
    assert!(app.status_is_error());
    let error = app.error().expect("an error card");
    assert!(error.message.contains("definitely_missing_table_e2e"), "{}", error.message);
    assert!(error.info.is_some());
}

#[gpui_kit::test]
fn a_stopped_query_is_reported_calmly_without_an_error_code(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.connect();
    app.set_sql("WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n) SELECT count(*) FROM n;");
    app.click("run-all");
    assert!(app.shown("stop"));
    app.click("stop");
    app.wait_idle();
    let error = app.error().expect("a cancelled card");
    assert_eq!(error.message.as_ref(), "Query cancelled.");
    assert!(error.info.is_none(), "no code chip");
}

#[gpui_kit::test]
fn tabs_open_switch_and_close_from_the_keyboard(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.connect();
    assert_eq!(app.titles().len(), 1);
    app.click("new-tab");
    assert_eq!((app.titles().len(), app.active()), (2, 1));
    app.keys("ctrl-shift-tab");
    assert_eq!(app.active(), 0);
    app.keys("ctrl-tab");
    assert_eq!(app.active(), 1);
    app.keys("mod-w");
    assert_eq!(app.titles().len(), 1);
}

// A vertical wheel scrolls the strip sideways.
#[gpui_kit::test]
fn the_tab_strip_follows_the_active_tab_and_the_wheel(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.connect();
    for _ in 0..19 {
        app.keys("mod-t");
    }
    let strip = app.bounds("editor-tabs").expect("the tab strip");
    let last = app.bounds("editor-tab-19").expect("the newest tab");
    assert!(last.left() >= strip.left() && last.right() <= strip.right(), "the newest tab is in view");
    assert!(app.bounds("editor-tab-0").unwrap().left() < strip.left(), "the first tab scrolled out");
    app.keys("ctrl-tab");
    let first = app.bounds("editor-tab-0").unwrap();
    assert!(first.left() >= strip.left(), "cycling round brings the first tab back");
    app.cx.simulate_event(ScrollWheelEvent {
        position: strip.center(),
        delta: ScrollDelta::Pixels(point(px(0.), px(-200.))),
        modifiers: Modifiers::none(),
        touch_phase: TouchPhase::Moved,
    });
    assert!(app.bounds("editor-tab-0").unwrap().left() < first.left(), "the wheel scrolled the strip");
}

#[gpui_kit::test]
fn a_tab_closes_from_its_close_button(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.connect();
    app.click("new-tab");
    assert_eq!(app.titles().len(), 2);
    app.click("close-tab-1");
    assert_eq!(app.titles().len(), 1);
}

#[gpui_kit::test]
fn the_toolbar_rolls_back_and_commits(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let tab = app.connect();
    app.run("CREATE TABLE e2e_txn (id INTEGER PRIMARY KEY, name VARCHAR(50));");
    let in_txn = |app: &mut super::driver::Driver, expected: bool| {
        app.settle(|cx| cx.update(|_, cx| tab.read(cx).in_transaction() == expected));
    };

    app.click("begin-txn");
    in_txn(&mut app, true);
    assert!(app.shown("commit-txn"), "the transaction badge and its buttons");
    app.run("INSERT INTO e2e_txn (id, name) VALUES (1, 'temp');");
    assert!(matches!(app.status(), ResultStatus::Affected { count: 1, .. }));
    app.click("rollback-txn");
    in_txn(&mut app, false);
    assert!(!app.shown("commit-txn"));
    app.run("SELECT COUNT(*) FROM e2e_txn;");
    assert_eq!(app.cell(0, 0).as_deref(), Some("0"));

    app.click("begin-txn");
    in_txn(&mut app, true);
    app.run("INSERT INTO e2e_txn (id, name) VALUES (2, 'kept');");
    app.click("commit-txn");
    in_txn(&mut app, false);
    app.run("SELECT COUNT(*) FROM e2e_txn;");
    assert_eq!(app.cell(0, 0).as_deref(), Some("1"));
}

pub(super) fn suggestions(app: &mut super::driver::Driver) -> Vec<String> {
    let tab = app.tab();
    app.cx.update(|_, cx| tab.read(cx).completion().read(cx).labels(cx))
}

pub(super) fn suggest(app: &mut super::driver::Driver, sql: &str, expected: &str) -> Vec<String> {
    app.set_sql("");
    app.type_text(sql);
    app.keys("escape");
    assert!(suggestions(app).is_empty(), "Escape closed the menu");
    for _ in 0..50 {
        app.keys("ctrl-space");
        let labels = suggestions(app);
        if labels.iter().any(|label| label == expected) {
            return labels;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("no {expected:?} in {:?}", suggestions(app));
}

#[gpui_kit::test]
fn ctrl_space_suggests_keywords_for_a_prefix(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.connect();
    let labels = suggest(&mut app, "SEL", "SELECT");
    assert!(!labels.is_empty());
}

#[gpui_kit::test]
fn completion_offers_a_table_its_columns_and_the_foreign_key_join(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.seed(
        "CREATE TABLE ac_parent (id INTEGER PRIMARY KEY, name TEXT);
         CREATE TABLE ac_child (id INTEGER PRIMARY KEY, parent_id INTEGER REFERENCES ac_parent (id))",
    );
    app.connect();
    app.refresh_schema();
    suggest(&mut app, "SELECT * FROM ac_par", "ac_parent");
    suggest(&mut app, "SELECT * FROM ac_parent WHERE na", "name");
    suggest(&mut app, "SELECT * FROM ac_parent JOIN ac_child ON ", "ac_child.parent_id = ac_parent.id");
}

#[gpui_kit::test]
fn the_suggestion_list_takes_the_arrow_keys_and_enter(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.seed("CREATE TABLE ac_parent (id INTEGER PRIMARY KEY); CREATE TABLE ac_child (id INTEGER PRIMARY KEY)");
    app.connect();
    app.refresh_schema();
    let labels = suggest(&mut app, "SELECT * FROM ac_", "ac_parent");
    app.keys("down down up enter");
    assert_eq!(app.sql(), format!("SELECT * FROM {}", labels[1]));
    assert!(suggestions(&mut app).is_empty(), "accepting closed the list");
    suggest(&mut app, "SELECT * FROM ac_", "ac_parent");
    app.click("query-editor");
    assert!(suggestions(&mut app).is_empty(), "a click in the editor closed the list");
}

// After overlay_scrollbars, the bar shows while the pointer is over the editor, and typing there keeps it.
#[gpui_kit::test]
fn the_editor_shows_its_bar_while_hovered(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.connect();
    app.set_sql(&(0..200).map(|n| format!("SELECT {n};")).collect::<Vec<_>>().join("\n"));
    app.overlay_scrollbars();
    let frame = app.bounds("query-editor").expect("the editor is drawn");
    let track = point(frame.right() - px(8.), frame.center().y);
    let tab = app.tab();
    let top = |app: &mut Driver| app.cx.update(|_, cx| tab.read(cx).editor().read(cx).scroll_offset().y);
    let before = top(&mut app);
    app.hover_at(point(frame.left() - px(40.), frame.center().y));
    app.click_at(track, Modifiers::none());
    assert_eq!(top(&mut app), before, "away from the editor, its bar is hidden");
    app.hover_at(frame.center());
    app.keys("home");
    // A hover change seen while drawing shows on the frame after.
    app.draw();
    app.draw();
    app.click_at(track, Modifiers::none());
    assert!((top(&mut app) - before).abs() > px(200.), "hovering the editor shows its bar");
}
