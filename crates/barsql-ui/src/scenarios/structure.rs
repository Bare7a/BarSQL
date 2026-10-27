use barsql_core::{ConnectionConfig, Value};
use gpui_kit::{Entity, Modifiers, MouseButton, Pixels, TestAppContext, point, px};

use super::driver::{Driver, open, open_with, selector};
use crate::actions::{CloseTab, EditTableStructure, Quit, ReopenClosedTab};
use crate::grid::Grid;
use crate::schema_tree::SchemaTree;
use crate::workspace::QuitRequests;

// Table menu positions, counted from Browse data at 0.
const BROWSE: usize = 0;
const EDIT_STRUCTURE: usize = 1;
const DROP_TABLE: usize = 13;
const COLUMN_RENAME: usize = 2;

// The Columns grid's columns on SQLite.
const NAME: usize = 0;
const TYPE: usize = 1;
const NULLABLE: usize = 2;

fn prepared<'a>(cx: &'a mut TestAppContext, seed: &str) -> Driver<'a> {
    let mut app = open(cx);
    app.seed(seed);
    app.connect();
    app.refresh_schema();
    app
}

fn loaded(app: &mut Driver) {
    app.wait_shown("structure-body");
}

fn grid(app: &mut Driver) -> Entity<Grid> {
    let tab = app.structure();
    app.cx.update(|_, cx| tab.read(cx).grid())
}

fn cell(app: &mut Driver, row: usize, col: usize) -> Option<String> {
    let grid = grid(app);
    app.cx.update(|_, cx| grid.read(cx).text_at(row, col))
}

fn open_cell(app: &mut Driver, row: usize, col: usize) {
    let grid = grid(app);
    let at = app.grid_point(&grid, |grid| grid.cell_point(row, col));
    app.double_click_at(at);
}

// Double-click opens the editor on the current value, so it's cleared before typing.
fn edit_cell(app: &mut Driver, row: usize, col: usize, text: &str) {
    open_cell(app, row, col);
    app.keys("mod-a backspace");
    app.type_text(text);
    app.keys("enter");
}

// Takes the `nth` of the cell's suggestions, all listed once its text is cleared.
fn pick(app: &mut Driver, row: usize, col: usize, nth: usize) {
    open_cell(app, row, col);
    app.keys("mod-a backspace");
    app.keys(&format!("up {}enter", "down ".repeat(nth)));
}

// The height the grid's header and rows take.
fn fit(app: &mut Driver) -> Pixels {
    let grid = grid(app);
    app.draw();
    app.cx.update(|window, cx| grid.read(cx).fit_height(window))
}

fn bounds_of(app: &mut Driver, selector: &'static str) -> gpui_kit::Bounds<Pixels> {
    app.bounds(selector).unwrap_or_else(|| panic!("no {selector}"))
}

fn tick(app: &mut Driver, row: usize, col: usize) {
    let grid = grid(app);
    let at = app.grid_point(&grid, |grid| grid.check_point(row, col));
    app.click_at(at, Modifiers::none());
}

#[gpui_kit::test]
fn edit_structure_opens_one_tab_per_table(cx: &mut TestAppContext) {
    let mut app = prepared(cx, "CREATE TABLE e2e_s (id INTEGER PRIMARY KEY, name TEXT)");
    let tabs = app.titles().len();
    app.context_menu("tree:t:main.e2e_s", EDIT_STRUCTURE);
    loaded(&mut app);
    assert_eq!([cell(&mut app, 0, NAME), cell(&mut app, 1, NAME)], [Some("id".into()), Some("name".into())]);
    app.context_menu("tree:t:main.e2e_s", EDIT_STRUCTURE);
    assert_eq!(app.titles().len(), tabs + 1);
    assert_eq!(app.titles().last().map(String::as_str), Some("e2e_s"));
}

#[gpui_kit::test]
fn the_table_tab_opens_its_structure(cx: &mut TestAppContext) {
    let mut app = prepared(cx, "CREATE TABLE e2e_s (id INTEGER PRIMARY KEY, name TEXT)");
    app.context_menu("tree:t:main.e2e_s", BROWSE);
    app.wait_shown("table-structure");
    app.click("table-structure");
    app.wait_shown("structure-tab");
    loaded(&mut app);
    assert_eq!(cell(&mut app, 1, NAME).as_deref(), Some("name"));
}

const TABLE: &str = "CREATE TABLE e2e_s (id INTEGER PRIMARY KEY, name TEXT)";

// A table with more columns than fit, so its rows scroll and the preview sits at its least height.
fn long_table<'a>(cx: &'a mut TestAppContext) -> Driver<'a> {
    let columns: Vec<String> = (0..40).map(|ix| format!("c{ix} TEXT")).collect();
    let mut app = prepared(cx, &format!("CREATE TABLE e2e_s (id INTEGER PRIMARY KEY, {})", columns.join(", ")));
    app.context_menu("tree:t:main.e2e_s", EDIT_STRUCTURE);
    loaded(&mut app);
    app
}

fn structure_of<'a>(cx: &'a mut TestAppContext) -> Driver<'a> {
    let mut app = prepared(cx, TABLE);
    app.context_menu("tree:t:main.e2e_s", EDIT_STRUCTURE);
    loaded(&mut app);
    app
}

fn script(app: &mut Driver) -> String {
    let tab = app.structure();
    app.cx.update(|_, cx| tab.read(cx).script().to_string())
}

fn names(app: &mut Driver) -> Vec<String> {
    let tab = app.structure();
    app.cx.update(|_, cx| tab.read(cx).draft.columns.iter().map(|row| row.column.name.clone()).collect())
}

fn retype(app: &mut Driver, field: &'static str, text: &str) {
    app.click(field);
    app.keys("mod-a backspace");
    app.type_text(text);
}

#[gpui_kit::test]
fn typing_a_column_change_updates_the_preview(cx: &mut TestAppContext) {
    let mut app = structure_of(cx);
    edit_cell(&mut app, 1, NAME, "full_name");
    assert!(app.shown("structure-preview"));
    let sql = script(&mut app);
    assert!(sql.contains(r#"ALTER TABLE "e2e_s" RENAME COLUMN "name" TO "full_name""#), "{sql}");
    assert_eq!(cell(&mut app, 1, NAME).as_deref(), Some("full_name"), "the grid shows the draft");
}

#[gpui_kit::test]
fn an_empty_name_disables_apply_and_shows_why(cx: &mut TestAppContext) {
    let mut app = structure_of(cx);
    edit_cell(&mut app, 1, NAME, "");
    assert!(app.shown("structure:problem:columns:1"));
    let tab = app.structure();
    assert!(!app.cx.update(|_, cx| tab.read(cx).can_apply()));
}

// The preview lists what keeps the draft from applying, and a click on one shows its row.
#[gpui_kit::test]
fn a_problem_shows_its_row(cx: &mut TestAppContext) {
    let mut app = structure_of(cx);
    app.click("structure-section:1");
    app.click("structure-add");
    app.keys("escape");
    app.click("structure-section:0");
    app.click("structure:problem:indexes:0");
    let grid = grid(&mut app);
    let (names, focused) = app.cx.update(|_, cx| (grid.read(cx).visible_names(), grid.read(cx).focused_cell()));
    assert_eq!((names[1].as_str(), focused), ("Columns", Some((0, 0))));
}

// The rows take the height they need and the preview the rest, folded or not.
#[gpui_kit::test]
fn the_rows_take_their_height_and_the_preview_the_rest(cx: &mut TestAppContext) {
    let mut app = structure_of(cx);
    let fit = fit(&mut app);
    let body = bounds_of(&mut app, "structure-body");
    let add = bounds_of(&mut app, "structure-add-row");
    let panel = bounds_of(&mut app, "structure-preview-panel");
    let footer = bounds_of(&mut app, "structure-footer");
    assert_eq!(body.size.height, fit);
    assert_eq!(add.top(), body.bottom());
    assert_eq!((panel.top(), panel.bottom()), (add.bottom(), footer.top()));
    app.click("structure-preview-toggle");
    let add = bounds_of(&mut app, "structure-add-row");
    let panel = bounds_of(&mut app, "structure-preview-panel");
    let footer = bounds_of(&mut app, "structure-footer");
    assert_eq!(panel.top(), add.bottom());
    assert!(panel.bottom() <= footer.top(), "{panel:?} over {footer:?}");
}

#[gpui_kit::test]
fn the_add_row_adds_a_row_and_opens_it(cx: &mut TestAppContext) {
    let mut app = structure_of(cx);
    app.click("structure-add-row");
    assert_eq!(names(&mut app).len(), 3);
    let grid = grid(&mut app);
    let editing = app.cx.update(|_, cx| (grid.read(cx).is_editing(), grid.read(cx).focused_cell()));
    assert_eq!(editing, (true, Some((2, NAME))));
}

// More rows than fit scroll, and the Add row and the preview stay in view.
#[gpui_kit::test]
fn a_long_table_scrolls_its_rows_and_keeps_the_preview(cx: &mut TestAppContext) {
    let mut app = long_table(cx);
    let fit = fit(&mut app);
    let body = bounds_of(&mut app, "structure-body");
    let preview = bounds_of(&mut app, "structure-preview");
    let add = bounds_of(&mut app, "structure-add-row");
    let footer = bounds_of(&mut app, "structure-footer");
    assert!(body.size.height < fit, "{body:?} fits {fit:?}");
    assert!(preview.size.height > px(100.), "{preview:?}");
    assert!(add.bottom() <= footer.top(), "{add:?} under {footer:?}");
}

#[gpui_kit::test]
fn revert_discards_the_draft(cx: &mut TestAppContext) {
    let mut app = structure_of(cx);
    edit_cell(&mut app, 1, NAME, "full_name");
    assert!(!script(&mut app).is_empty());
    app.click("structure-revert");
    assert_eq!(script(&mut app), "");
    assert_eq!(names(&mut app), ["id", "name"]);
}

#[gpui_kit::test]
fn read_only_connections_show_a_viewer(cx: &mut TestAppContext) {
    let mut app = open_with(cx, |env| {
        env.runtime.block_on(env.bar.execute_query(&env.connection.id, TABLE)).unwrap();
        let config = ConnectionConfig { read_only: true, ..env.connection.clone() };
        env.runtime.block_on(env.bar.save_connection(config)).unwrap()
    });
    app.connect();
    app.refresh_schema();
    app.context_menu("tree:t:main.e2e_s", EDIT_STRUCTURE);
    loaded(&mut app);
    assert!(!app.shown("structure-apply") && !app.shown("structure-revert") && !app.shown("structure-add"));
    assert!(!app.shown("structure-add-row") && !app.shown("structure-footer") && !app.shown("structure-toolbar"));
    open_cell(&mut app, 1, NAME);
    let grid = grid(&mut app);
    assert!(!app.cx.update(|_, cx| grid.read(cx).is_editing()), "the cells don't take input");
    assert_eq!(names(&mut app), ["id", "name"]);
}

fn tree(app: &mut Driver) -> Entity<SchemaTree> {
    let sidebar = app.sidebar();
    app.cx.update(|_, cx| sidebar.read(cx).panels().0)
}

fn open_row(app: &mut Driver, row: &str, id: &str) {
    app.click(selector(format!("tree:{row}")));
    listed_row(app, id, true);
}

fn listed_row(app: &mut Driver, id: &str, shown: bool) {
    let tree = tree(app);
    let id = id.to_string();
    app.settle(|cx| cx.update(|_, cx| tree.read(cx).listed().iter().any(|(row, ..)| *row == id) == shown));
}

#[gpui_kit::test]
fn applying_renames_a_column_everywhere(cx: &mut TestAppContext) {
    let mut app = prepared(cx, TABLE);
    app.context_menu("tree:t:main.e2e_s", BROWSE);
    open_row(&mut app, "t:main.e2e_s", "c:main.e2e_s.name");
    app.context_menu("tree:t:main.e2e_s", EDIT_STRUCTURE);
    loaded(&mut app);
    edit_cell(&mut app, 1, NAME, "full_name");
    app.click("structure-apply");
    assert!(app.dialog_open() && app.shown("structure-apply-sql"));
    app.click("change-confirm");
    app.settle_dialog_closed();
    listed_row(&mut app, "c:main.e2e_s.full_name", true);
    app.query("SELECT full_name FROM e2e_s");
    let tab = app.structure();
    app.settle(|cx| cx.update(|_, cx| tab.read(cx).script().is_empty() && !tab.read(cx).has_changes()));
    assert_eq!(names(&mut app), ["id", "full_name"]);
    let workspace = app.workspace.clone();
    let table = app.cx.update(|_, cx| workspace.read(cx).table_tab_named("e2e_s", cx)).expect("the table tab");
    app.settle(|cx| cx.update(|_, cx| table.read(cx).grid().read(cx).visible_names().contains(&"full_name".into())));
}

#[gpui_kit::test]
fn a_refused_change_keeps_the_dialog_and_the_draft(cx: &mut TestAppContext) {
    let mut app =
        prepared(cx, "CREATE TABLE e2e_s (id INTEGER PRIMARY KEY, name TEXT); INSERT INTO e2e_s VALUES (1, NULL)");
    app.context_menu("tree:t:main.e2e_s", EDIT_STRUCTURE);
    loaded(&mut app);
    tick(&mut app, 1, NULLABLE);
    app.click("structure-apply");
    app.click("change-confirm");
    app.wait_shown("change-error");
    assert!(app.dialog_open());
    app.click("change-cancel");
    assert!(!app.dialog_open());
    let tab = app.structure();
    assert!(app.cx.update(|_, cx| !tab.read(cx).draft.columns[1].column.nullable), "the draft stays");
    assert_eq!(app.query("PRAGMA foreign_keys"), [[Value::Int(1)]]);
}

#[gpui_kit::test]
fn a_table_dropped_elsewhere_marks_a_dirty_tab_gone(cx: &mut TestAppContext) {
    let mut app = structure_of(cx);
    edit_cell(&mut app, 1, NAME, "full_name");
    app.context_menu("tree:t:main.e2e_s", DROP_TABLE);
    app.click("change-confirm");
    app.settle_dialog_closed();
    app.wait_shown("structure-gone");
    let tab = app.structure();
    assert!(!app.cx.update(|_, cx| tab.read(cx).can_apply()));
}

#[gpui_kit::test]
fn a_table_altered_elsewhere_marks_a_dirty_tab_stale(cx: &mut TestAppContext) {
    let mut app = structure_of(cx);
    edit_cell(&mut app, 1, TYPE, "VARCHAR(20)");
    open_row(&mut app, "t:main.e2e_s", "c:main.e2e_s.id");
    app.context_menu("tree:c:main.e2e_s.id", COLUMN_RENAME);
    app.type_text("pk");
    app.keys("enter");
    app.settle_dialog_closed();
    app.wait_shown("structure-stale");
    assert!(app.shown("structure-reload"));
}

#[gpui_kit::test]
fn ctrl_s_opens_the_apply_dialog(cx: &mut TestAppContext) {
    let mut app = structure_of(cx);
    edit_cell(&mut app, 1, NAME, "full_name");
    app.keys("mod-s");
    assert!(app.dialog_open());
}

// Schema menu position, counted from the ER diagram at 0.
const NEW_TABLE: usize = 3;

#[gpui_kit::test]
fn new_table_creates_and_becomes_its_structure_tab(cx: &mut TestAppContext) {
    let mut app = prepared(cx, TABLE);
    app.context_menu("tree:s:main", NEW_TABLE);
    app.wait_shown("structure:table-name");
    retype(&mut app, "structure:table-name", "people");
    app.click("structure-add");
    app.type_text("email");
    app.keys("enter");
    edit_cell(&mut app, 1, TYPE, "TEXT");
    app.click("structure-apply");
    app.click("change-confirm");
    app.settle_dialog_closed();
    listed_row(&mut app, "t:main.people", true);
    app.query("SELECT email FROM people");
    let active = app.active();
    assert_eq!(app.titles()[active], "people");
    let tab = app.structure();
    app.settle(|cx| cx.update(|_, cx| tab.read(cx).persisted() && !tab.read(cx).has_changes()));
    assert_eq!(names(&mut app), ["id", "email"]);
}

#[gpui_kit::test]
fn closing_a_dirty_structure_tab_asks_to_discard(cx: &mut TestAppContext) {
    let mut app = structure_of(cx);
    let tabs = app.titles().len();
    edit_cell(&mut app, 1, NAME, "full_name");
    app.dispatch(CloseTab);
    assert!(app.dialog_open());
    assert_eq!(app.asked(), ("Discard structure changes?".into(), Some("e2e_s".into())));
    app.confirm_dialog();
    assert_eq!(app.titles().len(), tabs - 1);
    app.query("SELECT name FROM e2e_s");
}

fn quits(app: &mut Driver) -> usize {
    app.cx.update(|_, cx| cx.try_global::<QuitRequests>().map_or(0, |quits| quits.0))
}

#[gpui_kit::test]
fn quitting_with_a_draft_asks_once(cx: &mut TestAppContext) {
    let mut app = structure_of(cx);
    edit_cell(&mut app, 1, NAME, "full_name");
    assert!(!app.close_window(), "the window stays open");
    assert!(app.dialog_open());
    app.dispatch(Quit);
    app.keys("escape");
    assert!(!app.dialog_open(), "Quit again didn't stack a second confirmation");
    assert_eq!(quits(&mut app), 0);
    app.dispatch(Quit);
    app.confirm_dialog();
    assert_eq!(quits(&mut app), 1);
    app.query("SELECT name FROM e2e_s");
}

#[gpui_kit::test]
fn adding_an_index_lands_in_the_tree(cx: &mut TestAppContext) {
    let mut app = structure_of(cx);
    open_row(&mut app, "t:main.e2e_s", "g:main:e2e_s:indexes");
    open_row(&mut app, "g:main:e2e_s:indexes", "n:main:e2e_s:indexes");
    app.click("structure-section:1");
    app.click("structure-add");
    app.keys("up down enter");
    let tab = app.structure();
    let named =
        app.cx.update(|_, cx| tab.read(cx).draft.indexes.iter().map(|row| row.value.name.clone()).collect::<Vec<_>>());
    assert_eq!(named, ["idx_e2e_s_name"]);
    assert!(script(&mut app).contains(r#"CREATE INDEX "idx_e2e_s_name" ON "e2e_s" ("name")"#));
    app.click("structure-apply");
    app.click("change-confirm");
    app.settle_dialog_closed();
    listed_row(&mut app, "o:main:e2e_s:indexes:idx_e2e_s_name", true);
}

#[gpui_kit::test]
fn adding_a_check_rebuilds_the_table_with_it(cx: &mut TestAppContext) {
    let mut app = structure_of(cx);
    app.click("structure-section:3");
    app.click("structure-add");
    app.type_text("id > 0");
    app.keys("enter");
    let sql = script(&mut app);
    assert!(sql.contains("    CHECK (id > 0)\n)"), "{sql}");
    app.click("structure-apply");
    app.click("change-confirm");
    app.settle_dialog_closed();
    let ddl = format!("{:?}", app.query("SELECT sql FROM sqlite_master WHERE name = 'e2e_s'"));
    assert!(ddl.contains("CHECK (id > 0)"), "{ddl}");
}

#[gpui_kit::test]
fn adding_a_foreign_key_shows_on_the_column(cx: &mut TestAppContext) {
    let mut app = prepared(
        cx,
        "CREATE TABLE e2e_o (id INTEGER PRIMARY KEY); CREATE TABLE e2e_s (id INTEGER PRIMARY KEY, name TEXT, org_id INTEGER)",
    );
    open_row(&mut app, "t:main.e2e_s", "c:main.e2e_s.org_id");
    app.context_menu("tree:t:main.e2e_s", EDIT_STRUCTURE);
    loaded(&mut app);
    app.click("structure-section:2");
    app.click("structure-add");
    app.keys("escape");
    pick(&mut app, 0, 1, 2);
    pick(&mut app, 0, 2, 0);
    let tab = app.structure();
    app.settle(|cx| cx.update(|_, cx| tab.read(cx).draft.foreign_keys[0].value.ref_columns == ["id"]));
    let sql = script(&mut app);
    assert!(sql.contains(r#"CONSTRAINT "fk_e2e_s_e2e_o" FOREIGN KEY ("org_id") REFERENCES "e2e_o" ("id")"#), "{sql}");
    app.click("structure-apply");
    app.click("change-confirm");
    app.settle_dialog_closed();
    let id = app.connection.id.clone();
    app.settle(|cx| {
        cx.update(|_, cx| {
            let columns = crate::schema::get(cx, &id).and_then(|schema| schema.columns("main", "e2e_s").cloned());
            columns.is_some_and(|columns| columns.iter().any(|column| column.name == "org_id" && column.is_foreign))
        })
    });
}

// A new key follows its columns and table until it's named by hand.
#[gpui_kit::test]
fn a_key_named_by_hand_keeps_its_name(cx: &mut TestAppContext) {
    let mut app = prepared(
        cx,
        "CREATE TABLE e2e_o (id INTEGER PRIMARY KEY); CREATE TABLE e2e_s (id INTEGER PRIMARY KEY, name TEXT, org_id INTEGER)",
    );
    app.context_menu("tree:t:main.e2e_s", EDIT_STRUCTURE);
    loaded(&mut app);
    app.click("structure-section:2");
    app.click("structure-add");
    app.keys("escape");
    pick(&mut app, 0, 2, 0);
    pick(&mut app, 0, 1, 2);
    let tab = app.structure();
    let name = |app: &mut Driver| app.cx.update(|_, cx| tab.read(cx).draft.foreign_keys[0].value.name.clone());
    assert_eq!(name(&mut app), "fk_e2e_s_e2e_o");
    edit_cell(&mut app, 0, 0, "fk_org");
    pick(&mut app, 0, 1, 1);
    assert_eq!(name(&mut app), "fk_org");
}

fn palette_labels(app: &mut Driver) -> Vec<String> {
    let palette = app.workspace.update_in(app.cx, |ws, window, cx| ws.open_command_palette(window, cx));
    let palette = palette.expect("the palette opens");
    app.cx.run_until_parked();
    let labels = app.cx.update(|_, cx| palette.read(cx).labels());
    app.keys("escape");
    labels
}

#[gpui_kit::test]
fn the_palette_lists_structure_commands(cx: &mut TestAppContext) {
    let mut app = prepared(cx, TABLE);
    app.context_menu("tree:t:main.e2e_s", BROWSE);
    app.wait_shown("table-structure");
    let labels = palette_labels(&mut app);
    let has = |label: &str| labels.iter().any(|listed| listed == label);
    assert!(has("Edit table structure") && has("New table"), "{labels:?}");
}

#[gpui_kit::test]
fn palette_edit_structure_focuses_the_tab(cx: &mut TestAppContext) {
    let mut app = structure_of(cx);
    app.context_menu("tree:t:main.e2e_s", BROWSE);
    app.wait_shown("table-structure");
    let tabs = app.titles().len();
    app.dispatch(EditTableStructure);
    assert_eq!(app.titles().len(), tabs, "the open Structure tab is reused");
    let workspace = app.workspace.clone();
    assert!(app.cx.update(|_, cx| workspace.read(cx).active_structure().is_some()));
}

#[gpui_kit::test]
fn a_view_has_no_structure_tab(cx: &mut TestAppContext) {
    let mut app = prepared(
        cx,
        "CREATE TABLE e2e_s (id INTEGER PRIMARY KEY, name TEXT); CREATE VIEW e2e_v AS SELECT name FROM e2e_s",
    );
    app.context_menu("tree:t:main.e2e_v", BROWSE);
    app.wait_shown("table-refresh");
    assert!(!app.shown("table-structure"));
    let labels = palette_labels(&mut app);
    assert!(!labels.iter().any(|label| label == "Edit table structure"), "{labels:?}");
}

#[gpui_kit::test]
fn a_closed_new_table_is_not_reopened(cx: &mut TestAppContext) {
    let mut app = prepared(cx, TABLE);
    let tabs = app.titles().len();
    app.context_menu("tree:s:main", NEW_TABLE);
    app.wait_shown("structure:table-name");
    app.dispatch(CloseTab);
    assert_eq!(app.titles().len(), tabs);
    app.dispatch(ReopenClosedTab);
    assert_eq!(app.titles().len(), tabs, "an unapplied new table has nothing to come back as");
}

#[gpui_kit::test]
fn picking_a_type_from_the_list_sets_it(cx: &mut TestAppContext) {
    let mut app = structure_of(cx);
    pick(&mut app, 1, TYPE, 0);
    let first = barsql_sql::structure::type_suggestions(&barsql_core::DriverType::Sqlite)[0];
    let tab = app.structure();
    assert_eq!(app.cx.update(|_, cx| tab.read(cx).draft.columns[1].column.data_type.clone()), first);
}

#[gpui_kit::test]
fn the_header_lines_up_with_the_sidebar_tabs(cx: &mut TestAppContext) {
    let mut app = structure_of(cx);
    let header = app.bounds("structure-header").expect("the header");
    let tabs = app.bounds("sidebar-tabs").expect("the sidebar's tabs");
    assert_eq!((header.origin.y, header.size.height), (tabs.origin.y, tabs.size.height));
}

// Delete marks the focused row, Undo brings it back, and the drop button marks it again.
#[gpui_kit::test]
fn a_dropped_column_comes_back_with_undo(cx: &mut TestAppContext) {
    let mut app = structure_of(cx);
    let dropped = |app: &mut Driver| {
        let tab = app.structure();
        app.cx.update(|_, cx| tab.read(cx).draft.columns[1].dropped)
    };
    let grid = grid(&mut app);
    let at = app.grid_point(&grid, |grid| grid.cell_point(1, NAME));
    app.click_at(at, Modifiers::none());
    app.keys("delete");
    assert!(dropped(&mut app) && app.cx.update(|_, cx| grid.read(cx).is_deleted(1)));
    assert!(script(&mut app).contains(r#"DROP COLUMN "name""#), "{}", script(&mut app));
    app.keys("mod-z");
    assert!(!dropped(&mut app));
    app.click("structure-drop");
    assert!(dropped(&mut app));
}

// The edge sets the least height the preview keeps, which shows once the rows would take more.
#[gpui_kit::test]
fn dragging_the_preview_edge_resizes_it(cx: &mut TestAppContext) {
    let mut app = long_table(cx);
    let before = app.bounds("structure-preview").expect("the preview").size.height;
    let panel = app.bounds("structure-preview-panel").expect("the preview panel");
    let edge = point(panel.center().x, panel.origin.y);
    app.cx.simulate_mouse_down(edge, MouseButton::Left, Modifiers::none());
    for step in 1..=4 {
        app.cx.simulate_mouse_move(point(edge.x, edge.y - px(30. * step as f32)), MouseButton::Left, Modifiers::none());
    }
    app.cx.simulate_mouse_up(point(edge.x, edge.y - px(120.)), MouseButton::Left, Modifiers::none());
    let after = app.bounds("structure-preview").expect("the preview").size.height;
    assert!(after > before + px(100.), "{before:?} -> {after:?}");
}

#[gpui_kit::test]
fn adding_a_row_scrolls_to_it(cx: &mut TestAppContext) {
    let mut app = long_table(cx);
    app.click("structure-add");
    let grid = grid(&mut app);
    let row = app.grid_point(&grid, |grid| grid.cell_point(41, NAME));
    let body = app.bounds("structure-body").expect("the rows");
    assert!(body.contains(&row), "{row:?} outside {body:?}");
    assert!(app.cx.update(|_, cx| grid.read(cx).is_editing() && grid.read(cx).is_added(41)));
}
