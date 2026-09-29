use barsql_core::SavedQuery;
use gpui_kit::{Entity, TestAppContext, px};

use super::driver::{Driver, open};
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
    app.click("run-all");
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

#[gpui_kit::test]
fn long_saved_and_recent_lists_scroll_by_their_bars(cx: &mut TestAppContext) {
    let mut app = open(cx);
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
    app.scrolls_by_its_bar("saved-list", "saved-row-0");

    app.seed(&(0..60).map(|n| format!("SELECT {n} AS n")).collect::<Vec<_>>().join(";"));
    let sidebar = app.sidebar();
    sidebar.update(app.cx, |sidebar, cx| sidebar.refresh_history(cx));
    show_recent(&mut app);
    app.scrolls_by_its_bar("history-list", "history-row-0");
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
