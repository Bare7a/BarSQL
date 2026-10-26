use gpui_kit::TestAppContext;

use super::driver::{Driver, open};

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
