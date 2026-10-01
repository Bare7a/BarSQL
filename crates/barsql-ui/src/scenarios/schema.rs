use barsql_core::Value;
use gpui_kit::component::scroll::Scrollbar;
use gpui_kit::{Entity, Modifiers, TestAppContext, point, px};

use super::driver::{Driver, open, selector};
use crate::schema_tree::SchemaTree;

// Table menu positions, counted from Browse data at 0.
const SELECT_IN_NEW_TAB: usize = 1;
const COUNT_ROWS: usize = 2;
const BACKUP: usize = 4;
const COPY_DDL: usize = 5;
const OPEN_DDL: usize = 6;
const RENAME: usize = 10;
const TRUNCATE: usize = 11;
const DROP: usize = 12;
// Column menus start with Copy name. Object menus start with Copy DDL, Open DDL and Copy name.
const COLUMN_RENAME: usize = 1;
const COLUMN_DROP: usize = 2;
const OBJECT_DROP: usize = 3;

fn tree(app: &mut Driver) -> Entity<SchemaTree> {
    let sidebar = app.sidebar();
    app.cx.update(|_, cx| sidebar.read(cx).panels().0)
}

fn listed(app: &mut Driver) -> Vec<(String, String, String)> {
    let tree = tree(app);
    app.cx.update(|_, cx| tree.read(cx).listed())
}

fn ids(app: &mut Driver) -> Vec<String> {
    listed(app).into_iter().map(|(id, ..)| id).collect()
}

fn has(app: &mut Driver, id: &str) -> bool {
    ids(app).iter().any(|row| row == id)
}

fn open_row(app: &mut Driver, row: &str, id: &str) {
    app.click(selector(format!("tree:{row}")));
    let tree = tree(app);
    let id = id.to_string();
    app.settle(|cx| cx.update(|_, cx| tree.read(cx).listed().iter().any(|(row, ..)| *row == id)));
}

fn clipboard(app: &mut Driver) -> String {
    app.cx.read_from_clipboard().and_then(|item| item.text()).unwrap_or_default()
}

fn prepared<'a>(cx: &'a mut TestAppContext, seed: &str) -> Driver<'a> {
    let mut app = open(cx);
    app.seed(seed);
    app.connect();
    app.refresh_schema();
    app
}

#[gpui_kit::test]
fn the_schema_search_filters_tables_and_says_when_nothing_matches(cx: &mut TestAppContext) {
    let mut app = prepared(cx, "CREATE TABLE e2e_keep (id INTEGER); CREATE TABLE e2e_other (id INTEGER)");
    assert!(has(&mut app, "t:main.e2e_keep") && has(&mut app, "t:main.e2e_other"));
    app.click("schema-search");
    app.type_text("e2e_keep");
    assert!(has(&mut app, "t:main.e2e_keep") && !has(&mut app, "t:main.e2e_other"));
    app.keys("mod-a");
    app.type_text("zzz_definitely_no_such_table");
    let tree = tree(&mut app);
    assert!(app.cx.update(|_, cx| tree.read(cx).says_no_matches()), "No matches");
    app.keys("mod-a backspace");
    app.pause();
    assert!(has(&mut app, "t:main.e2e_keep") && has(&mut app, "t:main.e2e_other"));
}

#[gpui_kit::test]
fn select_in_new_tab_opens_the_tables_rows(cx: &mut TestAppContext) {
    let mut app = prepared(
        cx,
        "CREATE TABLE e2e_sel (id INTEGER PRIMARY KEY, name VARCHAR(50)); INSERT INTO e2e_sel VALUES (1, 'Alice')",
    );
    app.context_menu("tree:t:main.e2e_sel", SELECT_IN_NEW_TAB);
    assert_eq!(app.titles().last().map(String::as_str), Some("e2e_sel"));
    app.click("run-all");
    app.wait_idle();
    assert_eq!(app.cell(0, 0).as_deref(), Some("1"));
}

fn toasted(app: &mut Driver, message: &str) {
    let message = message.to_string();
    app.settle(|cx| cx.update(|_, cx| crate::toast::messages(cx).iter().any(|(_, text)| *text == message)));
}

fn listed_row(app: &mut Driver, id: &str, shown: bool) {
    let tree = tree(app);
    let id = id.to_string();
    app.settle(|cx| cx.update(|_, cx| tree.read(cx).listed().iter().any(|(row, ..)| *row == id) == shown));
}

#[gpui_kit::test]
fn count_rows_says_how_many_without_a_tab(cx: &mut TestAppContext) {
    let mut app = prepared(
        cx,
        "CREATE TABLE e2e_cnt (id INTEGER PRIMARY KEY, name VARCHAR(50)); INSERT INTO e2e_cnt (name) VALUES ('Alice'), ('Bob')",
    );
    let tabs = app.titles().len();
    app.context_menu("tree:t:main.e2e_cnt", COUNT_ROWS);
    toasted(&mut app, "e2e_cnt has 2 rows");
    assert_eq!(app.titles().len(), tabs);
}

// Rename opens with the name selected, so typing replaces it.
#[gpui_kit::test]
fn a_renamed_table_is_followed_by_the_tree_and_its_view(cx: &mut TestAppContext) {
    let mut app = prepared(cx, "CREATE TABLE e2e_old (id INTEGER PRIMARY KEY); INSERT INTO e2e_old VALUES (1)");
    app.context_menu("tree:t:main.e2e_old", 0);
    assert_eq!(app.titles().last().map(String::as_str), Some("e2e_old"));
    app.context_menu("tree:t:main.e2e_old", RENAME);
    assert!(app.dialog_open());
    app.type_text("e2e_new");
    app.keys("enter");
    app.settle_dialog_closed();
    listed_row(&mut app, "t:main.e2e_new", true);
    assert!(!has(&mut app, "t:main.e2e_old"));
    toasted(&mut app, r#"Renamed "e2e_old" to "e2e_new""#);
    assert_eq!(app.titles().last().map(String::as_str), Some("e2e_new"));
    assert_eq!(app.query("SELECT COUNT(*) FROM e2e_new"), [[Value::Int(1)]]);
}

#[gpui_kit::test]
fn a_dropped_table_leaves_the_tree_and_closes_its_view(cx: &mut TestAppContext) {
    let mut app = prepared(cx, "CREATE TABLE e2e_gone (id INTEGER PRIMARY KEY)");
    app.context_menu("tree:t:main.e2e_gone", 0);
    let tabs = app.titles().len();
    app.context_menu("tree:t:main.e2e_gone", DROP);
    assert!(app.dialog_open());
    app.click("change-confirm");
    app.settle_dialog_closed();
    listed_row(&mut app, "t:main.e2e_gone", false);
    assert_eq!(app.titles().len(), tabs - 1);
    assert!(app.query("SELECT name FROM sqlite_master WHERE name = 'e2e_gone'").is_empty());
}

#[gpui_kit::test]
fn truncate_empties_the_table(cx: &mut TestAppContext) {
    let mut app =
        prepared(cx, "CREATE TABLE e2e_trunc (id INTEGER PRIMARY KEY); INSERT INTO e2e_trunc VALUES (1), (2)");
    app.context_menu("tree:t:main.e2e_trunc", TRUNCATE);
    app.click("change-confirm");
    app.settle_dialog_closed();
    toasted(&mut app, r#"Emptied "e2e_trunc""#);
    assert_eq!(app.query("SELECT COUNT(*) FROM e2e_trunc"), [[Value::Int(0)]]);
}

#[gpui_kit::test]
fn columns_rename_and_drop_from_their_menu(cx: &mut TestAppContext) {
    let mut app = prepared(cx, "CREATE TABLE e2e_cols (id INTEGER PRIMARY KEY, old_col TEXT, gone TEXT)");
    open_row(&mut app, "t:main.e2e_cols", "c:main.e2e_cols.old_col");
    app.context_menu("tree:c:main.e2e_cols.old_col", COLUMN_RENAME);
    app.type_text("new_col");
    app.keys("enter");
    app.settle_dialog_closed();
    listed_row(&mut app, "c:main.e2e_cols.new_col", true);
    app.context_menu("tree:c:main.e2e_cols.gone", COLUMN_DROP);
    app.click("change-confirm");
    app.settle_dialog_closed();
    listed_row(&mut app, "c:main.e2e_cols.gone", false);
    let columns: Vec<String> =
        ids(&mut app).into_iter().filter_map(|id| id.strip_prefix("c:main.e2e_cols.").map(str::to_string)).collect();
    assert_eq!(columns, ["id", "new_col"]);
}

#[gpui_kit::test]
fn an_index_drops_from_its_menu(cx: &mut TestAppContext) {
    let mut app = prepared(
        cx,
        "CREATE TABLE e2e_ix (id INTEGER PRIMARY KEY, name TEXT); CREATE INDEX e2e_ix_name ON e2e_ix (name)",
    );
    open_row(&mut app, "t:main.e2e_ix", "g:main:e2e_ix:indexes");
    open_row(&mut app, "g:main:e2e_ix:indexes", "o:main:e2e_ix:indexes:e2e_ix_name");
    app.context_menu("tree:o:main:e2e_ix:indexes:e2e_ix_name", OBJECT_DROP);
    app.click("change-confirm");
    app.settle_dialog_closed();
    listed_row(&mut app, "o:main:e2e_ix:indexes:e2e_ix_name", false);
    assert!(app.query("SELECT name FROM sqlite_master WHERE type = 'index' AND name = 'e2e_ix_name'").is_empty());
}

#[gpui_kit::test]
fn a_failed_change_says_why_in_the_dialog(cx: &mut TestAppContext) {
    let mut app = prepared(cx, "CREATE TABLE e2e_taken (id INTEGER); CREATE TABLE e2e_other (id INTEGER)");
    app.context_menu("tree:t:main.e2e_taken", RENAME);
    app.type_text("e2e_other");
    app.keys("enter");
    app.settle(|cx| cx.debug_bounds("change-error").is_some());
    assert!(app.dialog_open());
    assert!(has(&mut app, "t:main.e2e_taken"));
}

#[gpui_kit::test]
fn a_table_backs_up_to_the_picked_file(cx: &mut TestAppContext) {
    let mut app =
        prepared(cx, "CREATE TABLE e2e_bak (id INTEGER PRIMARY KEY, v TEXT); INSERT INTO e2e_bak VALUES (1, 'a')");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("e2e_bak.sql");
    let stub = path.clone();
    app.cx.update(|_, cx| cx.set_global(crate::file_dialogs::Stub(Some(stub))));
    app.context_menu("tree:t:main.e2e_bak", BACKUP);
    assert!(app.dialog_open());
    app.click("backup-save");
    toasted(&mut app, "Backed up 1 row to e2e_bak.sql");
    app.settle_dialog_closed();
    let script = std::fs::read_to_string(&path).unwrap();
    assert!(script.contains("CREATE TABLE e2e_bak") && script.contains(r#"INSERT INTO "e2e_bak" ("id", "v") VALUES"#));
}

#[gpui_kit::test]
fn clicking_a_column_inserts_its_name_into_the_editor(cx: &mut TestAppContext) {
    let mut app = prepared(cx, "CREATE TABLE e2e_col (id INTEGER PRIMARY KEY, distinctive_col TEXT)");
    open_row(&mut app, "t:main.e2e_col", "c:main.e2e_col.distinctive_col");
    app.set_sql("");
    app.click("tree:c:main.e2e_col.distinctive_col");
    assert!(app.sql().contains("distinctive_col"), "{}", app.sql());
}

#[gpui_kit::test]
fn a_created_table_shows_in_the_schema_browser_with_its_columns(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.connect();
    app.run("CREATE TABLE e2e_schema (id INTEGER PRIMARY KEY, name VARCHAR(50));");
    app.refresh_schema();
    assert!(has(&mut app, "t:main.e2e_schema"));
    open_row(&mut app, "t:main.e2e_schema", "c:main.e2e_schema.name");
    assert!(has(&mut app, "c:main.e2e_schema.id"));
}

#[gpui_kit::test]
fn a_tables_indexes_constraints_and_triggers_are_listed(cx: &mut TestAppContext) {
    let mut app = prepared(
        cx,
        "CREATE TABLE ddl_parent (id INTEGER PRIMARY KEY);
         CREATE TABLE ddl_child (id INTEGER PRIMARY KEY, email TEXT NOT NULL UNIQUE, parent_id INTEGER REFERENCES ddl_parent(id));
         CREATE INDEX ddl_child_email_idx ON ddl_child (email)",
    );
    open_row(&mut app, "t:main.ddl_child", "g:main:ddl_child:triggers");
    open_row(&mut app, "g:main:ddl_child:indexes", "o:main:ddl_child:indexes:ddl_child_email_idx");
    app.click("tree:g:main:ddl_child:constraints");
    app.click("tree:g:main:ddl_child:triggers");
    app.pause();
    let rows = listed(&mut app);
    let constraints: Vec<&(String, String, String)> =
        rows.iter().filter(|(id, ..)| id.starts_with("o:main:ddl_child:constraints:")).collect();
    assert!(constraints.iter().any(|(_, _, extra)| extra.starts_with("pk ")), "{constraints:?}");
    assert!(
        constraints.iter().any(|(_, _, extra)| extra.starts_with("fk ") && extra.contains("ddl_parent")),
        "{constraints:?}"
    );
    assert!(!rows.iter().any(|(id, ..)| id.starts_with("o:main:ddl_child:triggers:")), "no triggers");
    assert!(rows.iter().any(|(id, label, _)| id == "g:main:ddl_child:triggers" && label == "triggers"));
}

#[gpui_kit::test]
fn a_view_is_marked_apart_from_a_table(cx: &mut TestAppContext) {
    let mut app = prepared(
        cx,
        "CREATE TABLE e2e_ddl_v (id INTEGER PRIMARY KEY, name VARCHAR(50)); CREATE VIEW e2e_ddl_v_view AS SELECT * FROM e2e_ddl_v",
    );
    let kinds: Vec<(String, String)> = listed(&mut app)
        .into_iter()
        .filter(|(id, ..)| id.starts_with("t:main.e2e_ddl_v"))
        .map(|(_, label, kind)| (label, kind.to_lowercase()))
        .collect();
    assert_eq!(kinds, [("e2e_ddl_v".to_string(), "table".to_string()), ("e2e_ddl_v_view".into(), "view".into())]);
}

#[gpui_kit::test]
fn copy_ddl_puts_the_create_table_on_the_clipboard(cx: &mut TestAppContext) {
    let mut app = prepared(cx, "CREATE TABLE e2e_ddl_copy (id INTEGER PRIMARY KEY, email VARCHAR(50) NOT NULL)");
    app.context_menu("tree:t:main.e2e_ddl_copy", COPY_DDL);
    app.settle(|cx| cx.read_from_clipboard().and_then(|item| item.text()).is_some_and(|text| text.contains("CREATE")));
    let ddl = clipboard(&mut app);
    for part in ["CREATE TABLE", "e2e_ddl_copy", "email", "NOT NULL", "PRIMARY KEY"] {
        assert!(ddl.contains(part), "{part} in {ddl}");
    }
}

#[gpui_kit::test]
fn open_ddl_puts_the_create_table_in_a_new_tab(cx: &mut TestAppContext) {
    let mut app = prepared(cx, "CREATE TABLE e2e_ddl_tab (id INTEGER PRIMARY KEY)");
    app.context_menu("tree:t:main.e2e_ddl_tab", OPEN_DDL);
    app.settle(|cx| cx.update(|_, cx| cx.windows().len() == 1));
    let workspace = app.workspace.clone();
    app.settle(|cx| cx.update(|_, cx| workspace.read(cx).titles(cx).len() == 2));
    assert_eq!(app.titles().last().map(String::as_str), Some("DDL: e2e_ddl_tab"));
    let sql = app.sql();
    assert!(sql.contains("CREATE TABLE") && sql.contains("e2e_ddl_tab"), "{sql}");
}

#[gpui_kit::test]
fn copy_ddl_on_an_index_copies_the_create_index(cx: &mut TestAppContext) {
    let mut app = prepared(
        cx,
        "CREATE TABLE e2e_ddl_idx (id INTEGER PRIMARY KEY, name VARCHAR(50)); CREATE INDEX e2e_ddl_idx_name_idx ON e2e_ddl_idx (name)",
    );
    open_row(&mut app, "t:main.e2e_ddl_idx", "g:main:e2e_ddl_idx:indexes");
    open_row(&mut app, "g:main:e2e_ddl_idx:indexes", "o:main:e2e_ddl_idx:indexes:e2e_ddl_idx_name_idx");
    app.context_menu("tree:o:main:e2e_ddl_idx:indexes:e2e_ddl_idx_name_idx", 0);
    app.settle(|cx| cx.read_from_clipboard().and_then(|item| item.text()).is_some_and(|text| text.contains("CREATE")));
    let ddl = clipboard(&mut app);
    assert!(ddl.contains("CREATE INDEX") && ddl.contains("e2e_ddl_idx_name_idx"), "{ddl}");
}

#[gpui_kit::test]
fn a_long_tree_scrolls_by_its_bar(cx: &mut TestAppContext) {
    let seed: Vec<String> = (0..60).map(|n| format!("CREATE TABLE e2e_long_{n:02} (id INTEGER)")).collect();
    let mut app = prepared(cx, &seed.join(";"));
    assert!(has(&mut app, "t:main.e2e_long_59"));
    app.scrolls_by_its_bar("schema-tree", "tree:s:main");
}

// The tree shows its bar while it's hovered, so a table's Browse button keeps clear of the bar's track.
#[gpui_kit::test]
fn a_long_trees_browse_buttons_keep_clear_of_its_bar(cx: &mut TestAppContext) {
    let seed: Vec<String> = (0..60).map(|n| format!("CREATE TABLE e2e_long_{n:02} (id INTEGER)")).collect();
    let mut app = prepared(cx, &seed.join(";"));
    app.overlay_scrollbars();
    app.hover("tree:t:main.e2e_long_00");
    let tree = app.bounds("schema-tree").expect("the tree is drawn");
    let browse = app.bounds("browse-main-e2e_long_00").expect("hovering shows Browse");
    assert!(tree.right() - browse.right() >= Scrollbar::width() - px(0.5), "{browse:?} is under the bar of {tree:?}");
    let tabs = app.titles().len();
    app.click_at(point(browse.right() - px(2.), browse.center().y), Modifiers::none());
    assert_eq!(app.titles().len(), tabs + 1, "a click at Browse's edge opens the table");
}

#[gpui_kit::test]
fn a_long_tree_shows_its_bar_on_hover(cx: &mut TestAppContext) {
    let seed: Vec<String> = (0..60).map(|n| format!("CREATE TABLE e2e_long_{n:02} (id INTEGER)")).collect();
    let mut app = prepared(cx, &seed.join(";"));
    app.overlay_scrollbars();
    app.shows_its_bar_on_hover("schema-tree", "tree:s:main");
}
