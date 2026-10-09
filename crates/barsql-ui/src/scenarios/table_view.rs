use barsql_core::Value;
use gpui_kit::{Entity, Modifiers, ScrollDelta, ScrollWheelEvent, TestAppContext, TouchPhase, point, px};

use super::driver::{Driver, open, selector};
use crate::cell_viewer::{CellViewer, Opened};
use crate::export_dialog::Opened as ExportOpened;
use crate::grid::Grid;
use crate::table_tab::TableTab;

fn prepared<'a>(cx: &'a mut TestAppContext, seed: &str) -> Driver<'a> {
    let mut app = open(cx);
    app.seed(seed);
    app.connect();
    app.refresh_schema();
    app
}

fn view(app: &mut Driver) -> Entity<TableTab> {
    app.cx.update(|_, cx| app.workspace.read(cx).table_tab()).expect("a table view is active")
}

fn wait(app: &mut Driver) -> Entity<TableTab> {
    let tab = view(app);
    app.settle(|cx| cx.update(|_, cx| tab.read(cx).idle()));
    tab
}

fn browse(app: &mut Driver, table: &str) -> Entity<TableTab> {
    app.hover(selector(format!("tree:t:main.{table}")));
    app.click(selector(format!("browse-main-{table}")));
    wait(app)
}

fn grid(app: &mut Driver) -> Entity<Grid> {
    let tab = view(app);
    app.cx.update(|_, cx| tab.read(cx).grid())
}

fn cell(app: &mut Driver, row: usize, col: usize) -> Option<String> {
    let grid = grid(app);
    app.cx.update(|_, cx| grid.read(cx).text_at(row, col))
}

fn column(app: &mut Driver, col: usize) -> Vec<Option<String>> {
    let grid = grid(app);
    app.cx.update(|_, cx| (0..grid.read(cx).rows_shown()).map(|row| grid.read(cx).text_at(row, col)).collect())
}

fn rows(app: &mut Driver) -> usize {
    let grid = grid(app);
    app.cx.update(|_, cx| grid.read(cx).rows_shown())
}

fn pending(app: &mut Driver) -> (usize, usize) {
    let tab = view(app);
    app.cx.update(|_, cx| tab.read(cx).pending())
}

fn click_cell(app: &mut Driver, row: usize, col: usize) {
    let grid = grid(app);
    let at = app.grid_point(&grid, |grid| grid.cell_point(row, col));
    app.click_at(at, Modifiers::none());
}

// Double-click opens the editor on the current value, so select it before typing.
fn edit_cell(app: &mut Driver, row: usize, col: usize, text: &str) {
    let grid = grid(app);
    let at = app.grid_point(&grid, |grid| grid.cell_point(row, col));
    app.double_click_at(at);
    app.keys("mod-a");
    app.type_text(text);
    app.keys("enter");
}

fn sort_by(app: &mut Driver, col: usize) {
    let grid = grid(app);
    let at = app.grid_point(&grid, |grid| grid.chevron_point(col));
    app.click_at(at, Modifiers::none());
    wait(app);
}

fn toggle_column(app: &mut Driver, ix: usize) {
    app.click("column-picker-button");
    app.click(selector(format!("column-picker-item-{ix}")));
    app.keys("escape");
}

fn visible(app: &mut Driver) -> Vec<String> {
    let grid = grid(app);
    app.cx.update(|_, cx| grid.read(cx).visible_names())
}

fn apply_and_refresh(app: &mut Driver) {
    app.click("table-apply");
    wait(app);
    assert_eq!(pending(app), (0, 0), "the pills clear");
    app.click("table-refresh");
    wait(app);
}

const PEOPLE: &str = "(id INTEGER PRIMARY KEY, name VARCHAR(50))";

#[gpui_kit::test]
fn browsing_a_table_shows_its_rows(cx: &mut TestAppContext) {
    let mut app =
        prepared(cx, &format!("CREATE TABLE e2e_tv {PEOPLE}; INSERT INTO e2e_tv VALUES (1, 'Alice'), (2, 'Bob')"));
    browse(&mut app, "e2e_tv");
    assert_eq!(column(&mut app, 1), [Some("Alice".into()), Some("Bob".into())]);
}

#[gpui_kit::test]
fn shift_enter_opens_a_json_cell_to_beautify_and_minify(cx: &mut TestAppContext) {
    let mut app = prepared(
        cx,
        r#"CREATE TABLE e2e_json (id INTEGER PRIMARY KEY, data JSON); INSERT INTO e2e_json VALUES (1, '{"a":1,"b":2,"c":3}')"#,
    );
    browse(&mut app, "e2e_json");
    click_cell(&mut app, 0, 1);
    app.keys("shift-enter");
    assert!(app.dialog_open(), "the cell viewer");
    let viewer: Entity<CellViewer> =
        app.cx.update(|_, cx| cx.try_global::<Opened>().and_then(|opened| opened.0.upgrade())).unwrap();
    app.click("cell-viewer-beautify");
    assert!(app.cx.update(|_, cx| viewer.read(cx).lines()) > 1, "N lines");
    app.click("cell-viewer-minify");
    assert_eq!(app.cx.update(|_, cx| viewer.read(cx).lines()), 1, "1 line");
    app.keys("escape");
    assert!(!app.dialog_open());
}

#[gpui_kit::test]
fn set_to_null_from_the_cell_viewer_stages_a_null(cx: &mut TestAppContext) {
    let mut app = prepared(
        cx,
        r#"CREATE TABLE e2e_jsonnull (id INTEGER PRIMARY KEY, data JSON); INSERT INTO e2e_jsonnull VALUES (1, '{"a":1}')"#,
    );
    browse(&mut app, "e2e_jsonnull");
    click_cell(&mut app, 0, 1);
    app.keys("shift-enter");
    app.click("cell-viewer-set-null");
    assert!(!app.dialog_open());
    assert_eq!(cell(&mut app, 0, 1), None, "NULL");
    assert_eq!(pending(&mut app), (1, 0));
}

#[gpui_kit::test]
fn hiding_a_column_scopes_the_export_to_the_visible_ones(cx: &mut TestAppContext) {
    let mut app = prepared(
        cx,
        &format!("CREATE TABLE e2e_tvcols {PEOPLE}; INSERT INTO e2e_tvcols VALUES (1, 'Alice'), (2, 'Bob')"),
    );
    browse(&mut app, "e2e_tvcols");
    toggle_column(&mut app, 1);
    assert_eq!(visible(&mut app), ["id"]);
    app.click("export-results");
    let dialog = app.cx.update(|_, cx| cx.try_global::<ExportOpened>().and_then(|opened| opened.0.upgrade())).unwrap();
    let (scopes, active) = app.cx.update(|_, cx| dialog.read(cx).column_scopes());
    assert_eq!((scopes, active), (vec!["all", "visible", "selected"], "visible"));
    app.click("export-dismiss");
    toggle_column(&mut app, 1);
    assert_eq!(visible(&mut app), ["id", "name"]);
}

#[gpui_kit::test]
fn a_reopened_table_view_keeps_its_sort_and_hidden_columns(cx: &mut TestAppContext) {
    let mut app = prepared(
        cx,
        &format!(
            "CREATE TABLE e2e_tvreopen {PEOPLE}; INSERT INTO e2e_tvreopen VALUES (1, 'Charlie'), (2, 'Alice'), (3, 'Bob')"
        ),
    );
    browse(&mut app, "e2e_tvreopen");
    sort_by(&mut app, 1);
    toggle_column(&mut app, 1);
    assert_eq!((cell(&mut app, 0, 0).as_deref(), visible(&mut app)), (Some("2"), vec!["id".into()]));
    app.keys("mod-w");
    app.keys("mod-shift-t");
    wait(&mut app);
    assert_eq!((cell(&mut app, 0, 0).as_deref(), visible(&mut app)), (Some("2"), vec!["id".into()]));
}

#[gpui_kit::test]
fn the_add_row_dialog_inserts_a_row(cx: &mut TestAppContext) {
    let mut app =
        prepared(cx, &format!("CREATE TABLE e2e_addrow {PEOPLE}; INSERT INTO e2e_addrow VALUES (1, 'Alice')"));
    browse(&mut app, "e2e_addrow");
    assert_eq!(rows(&mut app), 1);
    app.click("table-add-row");
    assert!(app.dialog_open());
    app.menu_pick("insert-mode-name", 2);
    app.click("insert-field-name");
    app.type_text("Bob");
    app.click("insert-row-submit");
    app.settle_dialog_closed();
    wait(&mut app);
    assert_eq!(column(&mut app, 1), [Some("Alice".into()), Some("Bob".into())]);
}

#[gpui_kit::test]
fn an_edited_cell_persists_after_apply(cx: &mut TestAppContext) {
    let mut app = prepared(cx, &format!("CREATE TABLE e2e_edit {PEOPLE}; INSERT INTO e2e_edit VALUES (1, 'Alice')"));
    browse(&mut app, "e2e_edit");
    edit_cell(&mut app, 0, 1, "Updated");
    assert_eq!((cell(&mut app, 0, 1).as_deref(), pending(&mut app)), (Some("Updated"), (1, 0)));
    apply_and_refresh(&mut app);
    assert_eq!(cell(&mut app, 0, 1).as_deref(), Some("Updated"));
    assert_eq!(app.query("SELECT name FROM e2e_edit")[0][0], Value::Text("Updated".into()));
}

#[gpui_kit::test]
fn typing_in_a_double_clicked_cell_appends_to_its_value(cx: &mut TestAppContext) {
    let mut app = prepared(cx, &format!("CREATE TABLE e2e_caret {PEOPLE}; INSERT INTO e2e_caret VALUES (1, 'Alice')"));
    browse(&mut app, "e2e_caret");
    let grid = grid(&mut app);
    let at = app.grid_point(&grid, |grid| grid.cell_point(0, 1));
    app.double_click_at(at);
    app.type_text("!");
    app.keys("enter");
    assert_eq!((cell(&mut app, 0, 1).as_deref(), pending(&mut app)), (Some("Alice!"), (1, 0)));
}

#[gpui_kit::test]
fn focus_returns_to_the_committed_cell(cx: &mut TestAppContext) {
    let mut app =
        prepared(cx, &format!("CREATE TABLE e2e_edit_focus {PEOPLE}; INSERT INTO e2e_edit_focus VALUES (1, 'Alice')"));
    browse(&mut app, "e2e_edit_focus");
    edit_cell(&mut app, 0, 1, "Updated");
    let grid = grid(&mut app);
    let focused = app.cx.update(|window, cx| {
        let grid = grid.read(cx);
        (grid.focused_display(), gpui_kit::Focusable::focus_handle(grid, cx).is_focused(window))
    });
    assert_eq!(focused, (Some((0, 1)), true));
    app.keys("mod-z");
    assert_eq!((cell(&mut app, 0, 1).as_deref(), pending(&mut app)), (Some("Alice"), (0, 0)));
}

#[gpui_kit::test]
fn a_cell_set_to_null_persists_after_apply(cx: &mut TestAppContext) {
    let mut app = prepared(cx, &format!("CREATE TABLE e2e_null {PEOPLE}; INSERT INTO e2e_null VALUES (1, 'Alice')"));
    browse(&mut app, "e2e_null");
    click_cell(&mut app, 0, 1);
    app.keys("mod-delete");
    assert_eq!((cell(&mut app, 0, 1), pending(&mut app)), (None, (1, 0)));
    apply_and_refresh(&mut app);
    assert_eq!(cell(&mut app, 0, 1), None);
}

#[gpui_kit::test]
fn a_row_marked_for_delete_goes_on_apply(cx: &mut TestAppContext) {
    let mut app =
        prepared(cx, &format!("CREATE TABLE e2e_del {PEOPLE}; INSERT INTO e2e_del VALUES (1, 'Alice'), (2, 'Bob')"));
    browse(&mut app, "e2e_del");
    let grid = grid(&mut app);
    let at = app.grid_point(&grid, |grid| grid.gutter_point(0));
    app.double_click_at(at);
    assert_eq!(pending(&mut app), (0, 1));
    apply_and_refresh(&mut app);
    assert_eq!(column(&mut app, 1), [Some("Bob".into())]);
}

#[gpui_kit::test]
fn reset_discards_the_pending_edits(cx: &mut TestAppContext) {
    let mut app = prepared(cx, &format!("CREATE TABLE e2e_reset {PEOPLE}; INSERT INTO e2e_reset VALUES (1, 'Alice')"));
    browse(&mut app, "e2e_reset");
    edit_cell(&mut app, 0, 1, "Temp");
    assert_eq!(pending(&mut app), (1, 0));
    app.click("table-reset");
    wait(&mut app);
    assert_eq!((cell(&mut app, 0, 1).as_deref(), pending(&mut app)), (Some("Alice"), (0, 0)));
}

#[gpui_kit::test]
fn a_pending_edit_undoes_and_redoes(cx: &mut TestAppContext) {
    let mut app = prepared(cx, &format!("CREATE TABLE e2e_undo {PEOPLE}; INSERT INTO e2e_undo VALUES (1, 'Alice')"));
    browse(&mut app, "e2e_undo");
    edit_cell(&mut app, 0, 1, "Changed");
    click_cell(&mut app, 0, 0);
    app.keys("mod-z");
    assert_eq!((cell(&mut app, 0, 1).as_deref(), pending(&mut app)), (Some("Alice"), (0, 0)));
    app.keys("mod-shift-z");
    assert_eq!((cell(&mut app, 0, 1).as_deref(), pending(&mut app)), (Some("Changed"), (1, 0)));
}

#[gpui_kit::test]
fn scrolling_to_the_bottom_loads_the_next_page(cx: &mut TestAppContext) {
    let mut app = prepared(
        cx,
        "CREATE TABLE e2e_page (id INTEGER PRIMARY KEY, label TEXT);
         INSERT INTO e2e_page (label) WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 150) SELECT 'row ' || i FROM n",
    );
    browse(&mut app, "e2e_page");
    assert_eq!(rows(&mut app), 100);
    let grid = grid(&mut app);
    for _ in 0..20 {
        let position = app.grid_point(&grid, |grid| grid.cell_point(0, 0));
        let delta = ScrollDelta::Pixels(point(px(0.), px(-4000.)));
        app.cx.simulate_event(ScrollWheelEvent {
            position,
            delta,
            modifiers: Modifiers::none(),
            touch_phase: TouchPhase::Moved,
        });
        wait(&mut app);
        if rows(&mut app) == 150 {
            break;
        }
    }
    assert_eq!(rows(&mut app), 150);
}

#[gpui_kit::test]
fn a_restart_restores_the_table_views_sort_and_hidden_columns(cx: &mut TestAppContext) {
    let mut app = prepared(
        cx,
        &format!(
            "CREATE TABLE e2e_tvrestore {PEOPLE}; INSERT INTO e2e_tvrestore VALUES (1, 'Charlie'), (2, 'Alice'), (3, 'Bob')"
        ),
    );
    browse(&mut app, "e2e_tvrestore");
    sort_by(&mut app, 1);
    sort_by(&mut app, 1);
    toggle_column(&mut app, 1);
    assert_eq!((cell(&mut app, 0, 0).as_deref(), visible(&mut app)), (Some("1"), vec!["id".into()]));
    app.pause();
    app.pause();
    let saved = app.env.bar.editor_session();
    let table_view =
        saved.tabs.iter().find_map(|tab| tab.table_view.clone()).expect("the table view is in the session");
    assert_eq!(
        (table_view.order_by.as_str(), table_view.order_dir.as_str(), table_view.hidden_columns.clone()),
        ("name", "DESC", vec!["name".to_string()])
    );
    app.reopen();
    let workspace = app.workspace.clone();
    app.settle(|cx| cx.update(|_, cx| workspace.read(cx).table_tab().is_some()));
    wait(&mut app);
    assert_eq!((cell(&mut app, 0, 0).as_deref(), visible(&mut app)), (Some("1"), vec!["id".into()]));
}

#[gpui_kit::test]
fn the_filter_suggests_columns_and_enter_takes_the_suggestion(cx: &mut TestAppContext) {
    let mut app = prepared(
        cx,
        "CREATE TABLE e2e_sug (id INTEGER PRIMARY KEY, name VARCHAR(20));
         INSERT INTO e2e_sug VALUES (1, 'a'), (2, 'b')",
    );
    let tab = browse(&mut app, "e2e_sug");
    app.click("table-filter");
    app.type_text("na");
    let completion = app.cx.update(|_, cx| tab.read(cx).completion());
    let labels = app.cx.update(|_, cx| completion.read(cx).labels(cx));
    assert_eq!(labels.first().map(String::as_str), Some("name"), "{labels:?}");
    app.keys("enter");
    let filter = |app: &mut Driver| {
        app.cx.update(|_, cx| (tab.read(cx).filter_text(cx), tab.read(cx).applied_filter().to_string()))
    };
    assert_eq!(filter(&mut app), ("name".to_string(), String::new()), "Enter took the suggestion, not the filter");
    app.type_text(" = 'b'");
    app.keys("escape");
    app.keys("enter");
    wait(&mut app);
    assert_eq!(filter(&mut app).1, "name = 'b'");
    assert_eq!(column(&mut app, 0), [Some("2".into())]);
    app.keys("mod-a backspace");
    wait(&mut app);
    assert_eq!(filter(&mut app), (String::new(), String::new()), "emptied, the filter is lifted");
    assert_eq!(rows(&mut app), 2);
}

#[gpui_kit::test]
fn a_foreign_key_cell_jumps_to_the_referenced_row(cx: &mut TestAppContext) {
    let mut app = prepared(
        cx,
        "CREATE TABLE e2e_fk_parent (id INTEGER PRIMARY KEY, name VARCHAR(50));
         INSERT INTO e2e_fk_parent VALUES (1, 'Alice'), (2, 'Bob');
         CREATE TABLE e2e_fk_child (id INTEGER PRIMARY KEY, parent_id INTEGER, FOREIGN KEY (parent_id) REFERENCES e2e_fk_parent (id));
         INSERT INTO e2e_fk_child VALUES (1, 2), (2, NULL)",
    );
    browse(&mut app, "e2e_fk_child");
    let grid = grid(&mut app);
    app.grid_point(&grid, |_| point(px(0.), px(0.)));
    let buttons = app.cx.update(|_, cx| {
        let grid = grid.read(cx);
        (grid.jumps_at(0, 1), grid.jumps_at(0, 0), grid.jumps_at(1, 1))
    });
    assert_eq!(buttons, (true, false, false), "only the filled foreign-key cell has the button");
    let at = app.grid_point(&grid, |grid| grid.jump_point(0, 1));
    app.click_at(at, Modifiers::none());
    let workspace = app.workspace.clone();
    app.settle(|cx| cx.update(|_, cx| workspace.read(cx).titles(cx).iter().any(|title| title == "e2e_fk_parent")));
    let tab = wait(&mut app);
    let filter = app.cx.update(|_, cx| tab.read(cx).applied_filter().to_string());
    assert!(filter.contains("id") && filter.contains('2'), "{filter}");
    assert_eq!(column(&mut app, 1), [Some("Bob".into())]);
}

#[gpui_kit::test]
fn the_row_menu_marks_every_selected_row_for_delete(cx: &mut TestAppContext) {
    let mut app = prepared(
        cx,
        &format!("CREATE TABLE e2e_tvrows {PEOPLE}; INSERT INTO e2e_tvrows VALUES (1, 'Alice'), (2, 'Bob'), (3, 'Cy')"),
    );
    browse(&mut app, "e2e_tvrows");
    let grid = grid(&mut app);
    let first = app.grid_point(&grid, |grid| grid.gutter_point(0));
    app.click_at(first, Modifiers::none());
    let second = app.grid_point(&grid, |grid| grid.gutter_point(1));
    app.click_at(second, Modifiers::shift());
    // Copy, Copy as, Export…, then Mark 2 rows for delete.
    app.context_menu_at(second, 3);
    assert_eq!(pending(&mut app), (0, 2));
    // A row outside the selection is selected alone first.
    let third = app.grid_point(&grid, |grid| grid.gutter_point(2));
    app.context_menu_at(third, 3);
    assert_eq!(pending(&mut app), (0, 3));
    app.context_menu_at(third, 3);
    assert_eq!(pending(&mut app), (0, 2), "a marked row unmarks");
}

#[gpui_kit::test]
fn a_cell_menu_filters_the_table_to_its_value(cx: &mut TestAppContext) {
    let mut app = prepared(
        cx,
        &format!(
            "CREATE TABLE e2e_tvfilter {PEOPLE}; INSERT INTO e2e_tvfilter VALUES (1, 'Alice'), (2, 'Bob'), (3, 'Bob')"
        ),
    );
    browse(&mut app, "e2e_tvfilter");
    let grid = grid(&mut app);
    let at = app.grid_point(&grid, |grid| grid.cell_point(1, 1));
    // Copy, Copy as, Export…, Edit…, then Filter by this value.
    app.context_menu_at(at, 4);
    wait(&mut app);
    assert_eq!(column(&mut app, 0), [Some("2".into()), Some("3".into())]);
    let at = app.grid_point(&grid, |grid| grid.cell_point(1, 0));
    app.context_menu_at(at, 4);
    wait(&mut app);
    assert_eq!(column(&mut app, 0), [Some("3".into())], "a second value narrows the first filter");
}

#[gpui_kit::test]
fn a_right_click_in_the_cell_editor_leaves_the_edit_open(cx: &mut TestAppContext) {
    let mut app =
        prepared(cx, &format!("CREATE TABLE e2e_tveditmenu {PEOPLE}; INSERT INTO e2e_tveditmenu VALUES (1, 'Alice')"));
    browse(&mut app, "e2e_tveditmenu");
    let grid = grid(&mut app);
    let at = app.grid_point(&grid, |grid| grid.cell_point(0, 1));
    app.double_click_at(at);
    app.keys("mod-a");
    app.type_text("Zed");
    app.input_menu_at(at, 0);
    assert!(app.cx.update(|_, cx| grid.read(cx).is_editing()), "still editing");
    assert_eq!(pending(&mut app), (0, 0), "nothing committed");
}

// GPUI Kit asks for a text field's menu while it updates the field, so the menu reads the field afterwards.
#[gpui_kit::test]
fn a_text_fields_menu_copies_its_selection(cx: &mut TestAppContext) {
    let mut app =
        prepared(cx, &format!("CREATE TABLE e2e_tvtext {PEOPLE}; INSERT INTO e2e_tvtext VALUES (1, 'Alice')"));
    browse(&mut app, "e2e_tvtext");
    app.click("table-filter");
    app.type_text("1 = 1");
    app.keys("mod-a");
    // On the text: a right-click past its end would move the caret there and drop the selection. Cut, then Copy.
    let field = app.bounds("table-filter").expect("the filter");
    app.input_menu_at(point(field.left() + px(16.), field.center().y), 1);
    let copied = app.cx.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text()));
    assert_eq!(copied.as_deref(), Some("1 = 1"));
}
