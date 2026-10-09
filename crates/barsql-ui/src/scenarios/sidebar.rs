use barsql_core::SavedQuery;
use gpui_kit::component::scroll::Scrollbar;
use gpui_kit::{Entity, Pixels, TestAppContext, px};

use super::driver::{Driver, open, selector};
use crate::history_panel::HistoryPanel;
use crate::saved_panel::SavedPanel;
use crate::saved_queries;

// Newest first. SELECTs only, since Env's seed is in the history too.
fn history(app: &mut Driver) -> Vec<String> {
    let sidebar = app.sidebar();
    let panel: Entity<HistoryPanel> = app.cx.update(|_, cx| sidebar.read(cx).panels().2);
    let listed = app.cx.update(|_, cx| panel.read(cx).listed());
    listed.into_iter().filter(|sql| sql.starts_with("SELECT")).collect()
}

fn saved(app: &mut Driver) -> Vec<(String, bool)> {
    let sidebar = app.sidebar();
    let panel: Entity<SavedPanel> = app.cx.update(|_, cx| sidebar.read(cx).panels().1);
    app.cx.update(|_, cx| panel.read(cx).listed(cx))
}

fn names(listed: Vec<(String, bool)>) -> Vec<String> {
    listed.into_iter().map(|(name, _)| name).collect()
}

fn save_as(app: &mut Driver, sql: &str, name: &str) {
    app.set_sql(sql);
    app.click("save-query");
    assert!(app.dialog_open(), "the name prompt");
    app.keys("mod-a");
    app.type_text(name);
    app.keys("enter");
    assert!(!app.dialog_open());
    app.pause();
}

fn show_recent(app: &mut Driver) {
    app.click("sidebar.recent");
}

#[gpui_kit::test]
fn executed_queries_are_recorded_in_recent(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.connect();
    app.run("SELECT 314 AS hist;");
    show_recent(&mut app);
    assert_eq!(history(&mut app), ["SELECT 314 AS hist"]);
}

#[gpui_kit::test]
fn a_history_entry_opens_into_the_editor(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.connect();
    app.run("SELECT 555 AS hist_open;");
    app.set_sql("");
    show_recent(&mut app);
    app.click("history-row-0");
    assert_eq!(app.sql(), "SELECT 555 AS hist_open", "opening restores the SQL without running it");
    app.dispatch(crate::actions::RunAll);
    app.wait_idle();
    assert_eq!(app.cell(0, 0).as_deref(), Some("555"));
}

#[gpui_kit::test]
fn a_history_entry_is_deleted_from_its_hover_button(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.connect();
    app.run("SELECT 777 AS hist_del;");
    show_recent(&mut app);
    app.hover("history-row-0");
    app.click("history-delete-0");
    assert!(history(&mut app).is_empty());
}

#[gpui_kit::test]
fn clear_all_empties_the_history_after_confirming(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.connect();
    app.run("SELECT 888 AS hist_clear;");
    show_recent(&mut app);
    app.menu_pick("history-options", 2);
    assert!(app.dialog_open(), "Clear history asks first");
    app.confirm_dialog();
    assert!(history(&mut app).is_empty(), "No query history");
}

#[gpui_kit::test]
fn the_history_filter_keeps_matching_entries(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.connect();
    app.run("SELECT 111 AS alpha;");
    app.run("SELECT 222 AS beta;");
    show_recent(&mut app);
    assert_eq!(history(&mut app).len(), 2);
    app.click("history-filter");
    app.type_text("111");
    assert_eq!(history(&mut app), ["SELECT 111 AS alpha"]);
}

#[gpui_kit::test]
fn a_query_saved_from_the_toolbar_is_in_the_library(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.connect();
    save_as(&mut app, "SELECT 42 AS answer;", "E2E Saved Query");
    app.click("sidebar.saved");
    assert_eq!(saved(&mut app), [("E2E Saved Query".to_string(), false)]);
}

#[gpui_kit::test]
fn a_saved_query_opens_into_a_tab(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.connect();
    save_as(&mut app, "SELECT 1 AS opened;", "Openable query");
    app.click("close-tab-0");
    assert!(app.titles().is_empty());
    app.click("sidebar.saved");
    app.click("saved-row-0");
    assert_eq!(app.titles(), ["Openable query"]);
    assert_eq!(app.sql(), "SELECT 1 AS opened;");
}

#[gpui_kit::test]
fn a_saved_query_is_renamed_from_its_hover_button(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.connect();
    save_as(&mut app, "SELECT 2 AS r;", "Before rename");
    app.click("sidebar.saved");
    app.hover("saved-row-0");
    app.click("saved-rename-0");
    app.keys("mod-a");
    app.type_text("After rename");
    app.keys("enter");
    app.pause();
    assert_eq!(names(saved(&mut app)), ["After rename"]);
}

#[gpui_kit::test]
fn a_saved_query_is_deleted_after_confirming(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.connect();
    save_as(&mut app, "SELECT 3 AS d;", "Deletable query");
    app.click("sidebar.saved");
    app.hover("saved-row-0");
    app.click("saved-delete-0");
    assert!(app.dialog_open());
    app.confirm_dialog();
    assert!(saved(&mut app).is_empty());
}

#[gpui_kit::test]
fn a_saved_query_is_pinned_from_its_hover_button(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.connect();
    save_as(&mut app, "SELECT 4 AS p;", "Pinnable query");
    app.click("sidebar.saved");
    app.hover("saved-row-0");
    app.click("saved-pin-0");
    assert_eq!(saved(&mut app), [("Pinnable query".to_string(), true)]);
}

#[gpui_kit::test]
fn saved_queries_filter_and_sort_by_name(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.connect();
    save_as(&mut app, "SELECT 1 AS zebra;", "Zebra query");
    app.click("new-tab");
    save_as(&mut app, "SELECT 2 AS apple;", "Apple query");
    app.click("sidebar.saved");
    app.click("saved-filter");
    app.type_text("Apple");
    assert_eq!(names(saved(&mut app)), ["Apple query"]);
    app.keys("mod-a backspace");
    app.pause();
    app.menu_pick("saved-options", 2);
    assert_eq!(names(saved(&mut app)), ["Apple query", "Zebra query"]);
}

// Fills Saved and Recent with 60 queries each, and checks each list and its first row.
fn check_long_lists<'a>(app: &mut Driver<'a>, check: impl Fn(&mut Driver<'a>, &'static str, &'static str)) {
    app.connect();
    for n in 0..60 {
        let sql = format!("SELECT {n} AS n");
        let query = SavedQuery {
            name: format!("Saved {n:02}"),
            connection_id: app.connection.id.clone(),
            sql,
            ..Default::default()
        };
        app.env.bar.save_saved_query(query).unwrap();
    }
    app.cx.update(|_, cx| saved_queries::refresh(cx));
    app.click("sidebar.saved");
    check(app, "saved-list", "saved-row-0");

    app.seed(&(0..60).map(|n| format!("SELECT {n} AS n")).collect::<Vec<_>>().join(";"));
    let sidebar = app.sidebar();
    sidebar.update(app.cx, |sidebar, cx| sidebar.refresh_history(cx));
    show_recent(app);
    check(app, "history-list", "history-row-0");
}

#[gpui_kit::test]
fn long_saved_and_recent_lists_scroll_by_their_bars(cx: &mut TestAppContext) {
    check_long_lists(&mut open(cx), Driver::scrolls_by_its_bar);
}

#[gpui_kit::test]
fn long_saved_and_recent_lists_show_their_bars_on_hover(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.overlay_scrollbars();
    check_long_lists(&mut app, Driver::shows_its_bar_on_hover);
}

// How far the first row's Delete ends from the list's right side, from the frame already drawn.
fn delete_inset(app: &mut Driver, list: &'static str, first_row: &'static str) -> Pixels {
    let delete = selector(first_row.replace("-row-", "-delete-"));
    app.cx.run_until_parked();
    let list = app.cx.debug_bounds(list).expect("the list is drawn");
    let delete = app.cx.debug_bounds(delete).expect("the row's actions are shown");
    list.right() - delete.right()
}

// The lists show their bars while they're hovered, so a row's buttons keep clear of the bar's track.
#[gpui_kit::test]
fn long_saved_and_recent_lists_keep_their_buttons_clear_of_the_bar(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.overlay_scrollbars();
    check_long_lists(&mut app, |app, list, first_row| {
        app.hover(first_row);
        assert!(delete_inset(app, list, first_row) >= Scrollbar::width() - px(0.5), "{first_row} in {list}");
    });
}

// Rows are built from the frame before, so on the frame a list starts scrolling it draws them again.
#[gpui_kit::test]
fn recent_moves_the_buttons_clear_on_the_frame_it_starts_scrolling(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.connect();
    app.seed("SELECT 1 AS n");
    let sidebar = app.sidebar();
    sidebar.update(app.cx, |sidebar, cx| sidebar.refresh_history(cx));
    show_recent(&mut app);
    app.hover("history-row-0");
    assert!(
        delete_inset(&mut app, "history-list", "history-row-0") < Scrollbar::width(),
        "a short list's are at its edge"
    );
    app.seed(&(0..60).map(|n| format!("SELECT {n} AS n")).collect::<Vec<_>>().join(";"));
    sidebar.update(app.cx, |sidebar, cx| sidebar.refresh_history(cx));
    let inset = delete_inset(&mut app, "history-list", "history-row-0");
    assert!(inset >= Scrollbar::width() - px(0.5), "{inset:?} with the pointer still on the row");
}

// The actions float over the row's end, so the text keeps the whole row. Rows pad both sides alike.
#[gpui_kit::test]
fn saved_and_recent_text_spans_the_row_under_its_hover_actions(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.connect();
    save_as(&mut app, "SELECT 6 AS spans;", "Spanning query");
    app.click("sidebar.saved");
    let (row, text) = (app.bounds("saved-row-0").unwrap(), app.bounds("saved-text-0").unwrap());
    let inset = text.left() - row.left();
    assert!((row.right() - text.right() - inset).abs() < px(0.5), "saved text {text:?} fills its row {row:?}");
    app.hover("saved-row-0");
    let delete = app.bounds("saved-delete-0").expect("hovering shows the actions");
    assert!(delete.left() < text.right() && delete.right() <= row.right(), "over the text's end, in the row");

    show_recent(&mut app);
    let (row, text) = (app.bounds("history-row-0").unwrap(), app.bounds("history-text-0").unwrap());
    assert!((row.right() - text.right() - inset).abs() < px(0.5), "history text {text:?} fills its row {row:?}");
    app.hover("history-row-0");
    let delete = app.bounds("history-delete-0").expect("hovering shows the actions");
    assert!(delete.left() < text.right() && delete.right() <= row.right());
    app.click("history-delete-0");
    assert!(history(&mut app).iter().all(|sql| sql != "SELECT 6 AS spans"), "the button still deletes");
}
