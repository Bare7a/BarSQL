use barsql_core::{ConnectionConfig, Value};
use gpui_kit::{Entity, TestAppContext};

use super::driver::{Driver, open, open_with, selector};
use crate::schema_tree::SchemaTree;

// Table menu positions, counted from Browse data at 0.
const BROWSE: usize = 0;
const EDIT_STRUCTURE: usize = 1;
const DROP_TABLE: usize = 13;
const COLUMN_RENAME: usize = 2;

fn prepared<'a>(cx: &'a mut TestAppContext, seed: &str) -> Driver<'a> {
    let mut app = open(cx);
    app.seed(seed);
    app.connect();
    app.refresh_schema();
    app
}

#[gpui_kit::test]
fn edit_structure_opens_one_tab_per_table(cx: &mut TestAppContext) {
    let mut app = prepared(cx, "CREATE TABLE e2e_s (id INTEGER PRIMARY KEY, name TEXT)");
    let tabs = app.titles().len();
    app.context_menu("tree:t:main.e2e_s", EDIT_STRUCTURE);
    app.wait_shown("structure:column:id");
    assert!(app.shown("structure:column:name"));
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
    app.wait_shown("structure:column:name");
}

const TABLE: &str = "CREATE TABLE e2e_s (id INTEGER PRIMARY KEY, name TEXT)";

fn structure_of<'a>(cx: &'a mut TestAppContext) -> Driver<'a> {
    let mut app = prepared(cx, TABLE);
    app.context_menu("tree:t:main.e2e_s", EDIT_STRUCTURE);
    app.wait_shown("structure:column:name:name");
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
    retype(&mut app, "structure:column:name:name", "full_name");
    assert!(app.shown("structure-preview"));
    let sql = script(&mut app);
    assert!(sql.contains(r#"ALTER TABLE "e2e_s" RENAME COLUMN "name" TO "full_name""#), "{sql}");
}

#[gpui_kit::test]
fn an_empty_name_disables_apply_and_shows_why(cx: &mut TestAppContext) {
    let mut app = structure_of(cx);
    retype(&mut app, "structure:column:name:name", "");
    assert!(app.shown("structure:problem:columns:1"));
    let tab = app.structure();
    assert!(!app.cx.update(|_, cx| tab.read(cx).can_apply()));
}

#[gpui_kit::test]
fn revert_discards_the_draft(cx: &mut TestAppContext) {
    let mut app = structure_of(cx);
    retype(&mut app, "structure:column:name:name", "full_name");
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
    app.wait_shown("structure:column:name:name");
    assert!(!app.shown("structure-apply") && !app.shown("structure-revert") && !app.shown("structure-add"));
    retype(&mut app, "structure:column:name:name", "full_name");
    assert_eq!(names(&mut app), ["id", "name"], "the fields don't take input");
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
    app.wait_shown("structure:column:name:name");
    retype(&mut app, "structure:column:name:name", "full_name");
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
    app.wait_shown("structure:column:name:nullable");
    app.click("structure:column:name:nullable");
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
    retype(&mut app, "structure:column:name:name", "full_name");
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
    retype(&mut app, "structure:column:name:type", "VARCHAR(20)");
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
    retype(&mut app, "structure:column:name:name", "full_name");
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
    retype(&mut app, "structure:column:new2:type", "TEXT");
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
