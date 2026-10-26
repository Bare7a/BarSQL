use barsql_core::ConnectionConfig;
use gpui_kit::TestAppContext;

use super::driver::{Driver, open, open_with};

// Table menu positions, counted from Browse data at 0.
const BROWSE: usize = 0;
const EDIT_STRUCTURE: usize = 1;

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
