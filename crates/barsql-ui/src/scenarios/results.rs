use gpui_kit::{Entity, Modifiers, TestAppContext, point, px};

use super::driver::{Driver, open, selector};
use crate::actions::ToggleJsonPanel;
use crate::export_dialog::{ExportDialog, Opened};
use crate::grid::Grid;

const SETS: &str = "CREATE TABLE e2e_sets (id INTEGER PRIMARY KEY, name VARCHAR(50));
    INSERT INTO e2e_sets (id, name) VALUES (1, 'Charlie'), (2, 'Alice'), (3, 'Bob')";

fn export_dialog(app: &mut Driver) -> Entity<ExportDialog> {
    app.cx.update(|_, cx| cx.try_global::<Opened>().and_then(|opened| opened.0.upgrade())).expect("the export dialog")
}

fn grid(app: &mut Driver) -> Entity<Grid> {
    app.grid().expect("a grid")
}

fn sort_by(app: &mut Driver, col: usize) {
    let grid = grid(app);
    let at = app.grid_point(&grid, |grid| grid.chevron_point(col));
    app.click_at(at, Modifiers::none());
}

fn toggle_column(app: &mut Driver, ix: usize) {
    app.click("column-picker-button");
    app.click(selector(format!("column-picker-item-{ix}")));
    app.keys("escape");
}

fn layout(app: &mut Driver) -> (Vec<String>, Option<(String, bool)>) {
    let grid = grid(app);
    app.cx.update(|_, cx| (grid.read(cx).visible_names(), grid.read(cx).sorted_by()))
}

#[gpui_kit::test]
fn the_export_summary_follows_the_format_and_offers_no_selection_yet(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.connect();
    app.run("SELECT 1 AS id, 'one' AS label UNION ALL SELECT 2, 'two';");
    app.click("export-results");
    let dialog = export_dialog(&mut app);
    let read = |app: &mut Driver| {
        app.cx.update(|_, cx| {
            let dialog = dialog.read(cx);
            (dialog.summary(cx), dialog.selected_rows(), dialog.column_scopes())
        })
    };
    let (summary, (selectable, _), (scopes, active)) = read(&mut app);
    assert!(summary.contains("2 row"), "{summary}");
    assert!(!selectable, "Selected is disabled without a selection");
    assert_eq!((scopes, active), (vec!["all", "selected"], "all"), "no Visible while every column shows");
    app.menu_pick("export-format", 2);
    let (summary, ..) = read(&mut app);
    assert!(summary.contains("JSON"), "{summary}");
    app.click("export-dismiss");
    assert!(!app.dialog_open());
}

#[gpui_kit::test]
fn selected_rows_export_after_a_gutter_selection(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.connect();
    app.run("SELECT 1 AS id, 'one' AS label UNION ALL SELECT 2, 'two' UNION ALL SELECT 3, 'three';");
    let grid = grid(&mut app);
    let first = app.grid_point(&grid, |grid| grid.gutter_point(0));
    app.click_at(first, Modifiers::none());
    let last = app.grid_point(&grid, |grid| grid.gutter_point(2));
    app.click_at(last, Modifiers::shift());
    app.click("export-results");
    let dialog = export_dialog(&mut app);
    app.click("export-rows-selected");
    let (summary, (selectable, chosen)) =
        app.cx.update(|_, cx| (dialog.read(cx).summary(cx), dialog.read(cx).selected_rows()));
    assert!(selectable && chosen);
    assert!(summary.contains("3 row"), "{summary}");
}

#[gpui_kit::test]
fn each_result_set_keeps_its_own_sort_and_hidden_columns(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.seed(SETS);
    app.connect();
    app.run("SELECT id, name FROM e2e_sets; SELECT id AS num, name AS label FROM e2e_sets;");
    assert_eq!(app.result_tabs(), 2);
    app.click("result-tab-0");
    sort_by(&mut app, 1);
    toggle_column(&mut app, 0);
    assert_eq!(app.cell(0, 0).as_deref(), Some("Alice"));

    app.click("result-tab-1");
    assert_eq!(layout(&mut app), (vec!["num".into(), "label".into()], None), "the second set starts fresh");
    sort_by(&mut app, 0);
    sort_by(&mut app, 0);
    toggle_column(&mut app, 1);
    assert_eq!(app.cell(0, 0).as_deref(), Some("3"));

    app.click("result-tab-0");
    assert_eq!(layout(&mut app), (vec!["name".into()], Some(("name".into(), false))));
    assert_eq!(app.cell(0, 0).as_deref(), Some("Alice"));
    app.click("result-tab-1");
    assert_eq!(layout(&mut app), (vec!["num".into()], Some(("num".into(), true))));
    assert_eq!(app.cell(0, 0).as_deref(), Some("3"));
}

#[gpui_kit::test]
fn the_column_picker_splits_show_all_and_hide_all_evenly(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.seed(SETS);
    app.connect();
    app.run("SELECT id, name FROM e2e_sets;");
    app.click("column-picker-button");
    let show = app.bounds("columns-show-all").expect("Show all is drawn");
    let hide = app.bounds("columns-hide-all").expect("Hide all is drawn");
    assert!((show.size.width - hide.size.width).abs() < px(1.), "{show:?} and {hide:?} take half each");
    assert!(show.right() < hide.left(), "side by side, with the rule between them");
    app.click("columns-hide-all");
    assert!(layout(&mut app).0.is_empty());
    app.click("columns-show-all");
    assert_eq!(layout(&mut app).0, ["id", "name"]);
}

#[gpui_kit::test]
fn a_new_run_starts_with_fresh_sort_and_columns(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.seed(SETS);
    app.connect();
    let sql = "SELECT id, name FROM e2e_sets ORDER BY id;";
    app.run(sql);
    sort_by(&mut app, 1);
    assert_eq!(app.cell(0, 0).as_deref(), Some("2"));
    toggle_column(&mut app, 0);
    assert_eq!(layout(&mut app).0, ["name"]);
    app.run(sql);
    assert_eq!(layout(&mut app), (vec!["id".into(), "name".into()], None));
    assert_eq!(app.cell(0, 0).as_deref(), Some("1"));
}

#[gpui_kit::test]
fn a_table_view_visit_keeps_the_query_grid_state(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.seed(SETS);
    app.connect();
    app.refresh_schema();
    app.run("SELECT id, name FROM e2e_sets;");
    sort_by(&mut app, 1);
    toggle_column(&mut app, 0);
    app.keys("mod-p");
    app.type_text("e2e_sets");
    app.keys("enter");
    assert_eq!(app.titles(), ["Query 1 - Local", "e2e_sets"], "the table view opened in its own tab");
    app.click("editor-tab-0");
    assert_eq!(layout(&mut app), (vec!["name".into()], Some(("name".into(), false))));
    assert_eq!(app.cell(0, 0).as_deref(), Some("Alice"));
}

#[gpui_kit::test]
fn the_json_viewer_mirrors_the_focused_row_and_filters_by_key(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.connect();
    app.run("SELECT 1 AS id, 'Zelda' AS name;");
    app.dispatch(ToggleJsonPanel);
    let grid = grid(&mut app);
    let at = app.grid_point(&grid, |grid| grid.cell_point(0, 0));
    app.click_at(at, Modifiers::none());
    let panel = app.cx.update(|_, cx| app.workspace.read(cx).json_panel()).expect("the JSON viewer is open");
    let text = |app: &mut Driver| app.cx.update(|_, cx| panel.read(cx).text().unwrap_or_default().to_string());
    let shown = text(&mut app);
    assert!(shown.contains("Zelda") && shown.contains("\"name\""), "{shown}");
    app.click("json-filter");
    app.type_text("name");
    app.settle(|cx| cx.update(|_, cx| !panel.read(cx).text().unwrap_or_default().contains("\"id\"")));
    assert!(text(&mut app).contains("\"name\""));
    app.keys("mod-j");
    assert!(!app.shown("json-panel"));
}

#[gpui_kit::test]
fn json_text_in_a_cell_nests_as_an_object(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.connect();
    app.run(r#"SELECT 7 AS id, '{"x":42}' AS data;"#);
    app.keys("mod-j");
    let grid = grid(&mut app);
    let at = app.grid_point(&grid, |grid| grid.cell_point(0, 0));
    app.click_at(at, Modifiers::none());
    let panel = app.cx.update(|_, cx| app.workspace.read(cx).json_panel()).expect("the JSON viewer is open");
    let shown = app.cx.update(|_, cx| panel.read(cx).text().unwrap_or_default().to_string());
    assert!(shown.contains("\"x\": 42"), "{shown}");
}

// The bar has a lane of its own, so it shows even where other bars hide until their area scrolls or is hovered.
#[gpui_kit::test]
fn a_long_result_always_shows_the_grid_bar(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.connect();
    app.run("SELECT * FROM things;");
    app.overlay_scrollbars();
    let grid = grid(&mut app);
    let frame = app.bounds("grid").expect("the grid is drawn");
    let track = point(frame.right() - px(8.), frame.center().y);
    let first = |app: &mut Driver| app.grid_point(&grid, |grid| grid.cell_point(0, 0));
    let before = first(&mut app);
    let editor = app.bounds("query-editor").expect("the editor is drawn");
    app.hover_at(editor.center());
    app.click_at(track, Modifiers::none());
    assert!(first(&mut app).y < before.y - px(200.), "a click on its track jumps, with the pointer away");
}

fn viewer_text(app: &mut Driver) -> String {
    let viewer = app.cx.update(|_, cx| cx.try_global::<crate::cell_viewer::Opened>().and_then(|o| o.0.upgrade()));
    app.cx.update(|_, cx| viewer.expect("the cell viewer").read(cx).shown(cx).1)
}

#[gpui_kit::test]
fn a_right_click_acts_on_the_cell_it_lands_on(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.connect();
    app.run("SELECT 1 AS id, 'one' AS label UNION ALL SELECT 2, 'two';");
    let grid = grid(&mut app);
    // The first cell has the focus. Copy, Copy as, Export…, then View cell value.
    let at = app.grid_point(&grid, |grid| grid.cell_point(1, 1));
    app.context_menu_at(at, 3);
    assert_eq!(viewer_text(&mut app), "two");
    app.keys("escape");
    // Copy as JSON copies the clicked cell alone, not the whole result.
    let at = app.grid_point(&grid, |grid| grid.cell_point(0, 0));
    app.context_submenu_at(at, 1, 2);
    let copied = app.cx.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text())).unwrap_or_default();
    assert!(copied.contains("\"id\": 1") && !copied.contains("two"), "{copied}");
}

#[gpui_kit::test]
fn the_header_menu_sorts_copies_and_hides_its_column(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.connect();
    app.run("SELECT 1 AS id, 'b' AS label UNION ALL SELECT 2, 'a' UNION ALL SELECT 3, 'c';");
    let grid = grid(&mut app);
    let header = app.grid_point(&grid, |grid| grid.header_point(1));
    // Sort ascending, Sort descending, Clear sort, then Copy, Copy as, Export… and Copy column name.
    app.context_menu_at(header, 1);
    assert_eq!(layout(&mut app).1, Some(("label".into(), true)));
    assert_eq!(app.cell(0, 1).as_deref(), Some("c"));
    app.context_menu_at(header, 6);
    let copied = app.cx.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text()));
    assert_eq!(copied.as_deref(), Some("label"));
    app.context_menu_at(header, 2);
    assert_eq!(layout(&mut app).1, None);
    // Keys skip the disabled Clear sort now, so Hide column is one up.
    app.context_menu_at(header, 6);
    assert_eq!(layout(&mut app).0, ["id"]);
}

#[gpui_kit::test]
fn a_right_click_on_a_row_number_selects_that_row(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.connect();
    app.run("SELECT 1 AS id UNION ALL SELECT 2 UNION ALL SELECT 3;");
    let grid = grid(&mut app);
    let row = app.grid_point(&grid, |grid| grid.gutter_point(2));
    app.context_menu_at(row, 0);
    let copied = app.cx.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text())).unwrap_or_default();
    assert!(copied.contains('3') && !copied.contains('1'), "{copied}");
    assert_eq!(app.cx.update(|_, cx| grid.read(cx).selection_counts()), (1, 1));
}

#[gpui_kit::test]
fn a_result_tabs_menu_copies_its_rows_and_statement(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.connect();
    app.run("SELECT 1 AS a; SELECT 2 AS b;");
    // Copy all rows, Copy as, Export…, then Copy statement.
    app.context_menu("result-tab-1", 3);
    let copied = app.cx.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text()));
    assert_eq!(copied.as_deref(), Some("SELECT 2 AS b"));
    app.context_menu("result-tab-0", 0);
    let copied = app.cx.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text())).unwrap_or_default();
    assert!(copied.contains('a') && copied.contains('1'), "{copied}");
}

#[gpui_kit::test]
fn a_failed_results_menu_copies_the_error(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.connect();
    // A lone result has no tabs, so the failure comes second.
    app.run("SELECT 1; SELECT * FROM missing_e2e_table;");
    app.context_menu("result-tab-1", 0);
    let copied = app.cx.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text())).unwrap_or_default();
    assert!(copied.contains("missing_e2e_table"), "{copied}");
}

#[gpui_kit::test]
fn the_status_bar_sums_the_selected_numbers(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.connect();
    app.run("SELECT 1 AS n, 'x' AS t UNION ALL SELECT 2.5, NULL UNION ALL SELECT NULL, 'y';");
    let grid = grid(&mut app);
    let aggregate = |app: &mut Driver| app.cx.update(|_, cx| grid.read(cx).aggregate());
    assert_eq!(aggregate(&mut app), None, "a lone focused cell has nothing to sum");
    let header = app.grid_point(&grid, |grid| grid.header_point(0));
    app.click_at(header, Modifiers::none());
    let total = aggregate(&mut app).expect("a column");
    assert_eq!((total.count, total.numbers, total.sum), (2, 2, 3.5), "NULL isn't counted");
    let text = app.grid_point(&grid, |grid| grid.header_point(1));
    app.click_at(text, Modifiers::none());
    let total = aggregate(&mut app).expect("a column");
    assert_eq!((total.count, total.numbers), (2, 0), "text counts but doesn't sum");
}

// The chart toggle swaps the grid for a chart of the result, and back.
#[gpui_kit::test]
fn a_result_shows_as_a_chart_and_back(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.connect();
    app.run("SELECT 'a' AS k, 1 AS v UNION ALL SELECT 'b', 2;");
    app.click("chart-toggle");
    assert!(app.shown("result-chart"));
    app.click("chart-toggle");
    assert!(!app.shown("result-chart"));
    assert_eq!(app.cell(1, 1).as_deref(), Some("2"), "the grid is back");
}

// The JSON viewer's lines continue the editor's: its header ends with the tab bar and its filter row with the
// toolbar.
#[gpui_kit::test]
fn the_json_viewer_lines_up_with_the_editor(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.connect();
    app.run("SELECT 1 AS id;");
    app.dispatch(ToggleJsonPanel);
    let grid = grid(&mut app);
    let at = app.grid_point(&grid, |grid| grid.cell_point(0, 0));
    app.click_at(at, Modifiers::none());
    let header = app.bounds("json-panel-header").expect("the JSON viewer is open");
    let filter = app.bounds("json-filter").expect("a row is shown");
    let toolbar = app.bounds("query-toolbar").expect("the toolbar");
    assert_eq!(header.bottom(), app.bounds("tab-bar").expect("the tab bar").bottom());
    assert_eq!(filter.bottom(), toolbar.bottom());
}
