use std::path::Path;

use gpui_kit::{Entity, TestAppContext};

use super::driver::{Driver, open};
use crate::file_dialogs::Stub;
use crate::import_dialog::{ImportDialog, Opened, Shown};

const IMPORT_INTO_TABLE: usize = 3;

fn dialog(app: &mut Driver) -> Entity<ImportDialog> {
    app.cx.update(|_, cx| cx.try_global::<Opened>().and_then(|opened| opened.0.upgrade())).expect("the import dialog")
}

fn shown(app: &mut Driver) -> Shown {
    let dialog = dialog(app);
    app.cx.update(|_, cx| dialog.read(cx).shown(cx))
}

fn prepared<'a>(cx: &'a mut TestAppContext, seed: &str) -> Driver<'a> {
    let mut app = open(cx);
    app.seed(seed);
    app.connect();
    app.refresh_schema();
    app
}

// Stub makes the file picker return `path`.
fn browse(app: &mut Driver, path: &Path) {
    app.cx.update(|_, cx| cx.set_global(Stub(Some(path.to_path_buf()))));
    app.click("import-browse");
    let dialog = dialog(app);
    app.settle(|cx| cx.update(|_, cx| dialog.read(cx).shown(cx).preview_rows.is_some()));
}

fn import(app: &mut Driver) -> (i64, i64) {
    app.click("import-run");
    let dialog = dialog(app);
    app.settle(|cx| {
        cx.update(|_, cx| {
            let shown = dialog.read(cx).shown(cx);
            !shown.running && (shown.result.is_some() || shown.error.is_some())
        })
    });
    shown(app).result.expect("an import result")
}

#[gpui_kit::test]
fn the_toolbar_import_opens_on_csv_with_its_parse_options(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.connect();
    app.click("import-data");
    assert!(app.dialog_open());
    let state = shown(&mut app);
    assert!(state.csv && state.has_header && !state.can_run);
    assert!(app.shown("import-delimiter") && app.shown("import-header"));
}

#[gpui_kit::test]
fn sql_mode_hides_the_csv_parse_options(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.connect();
    app.click("import-data");
    app.click("import-kind-sql");
    assert!(!app.shown("import-delimiter") && !app.shown("import-header"));
    assert!(app.shown("import-stop-sql"), "Stop at the first error");
    app.click("import-kind-csv");
    assert!(app.shown("import-delimiter"));
}

#[gpui_kit::test]
fn an_existing_table_target_lists_the_schemas_tables(cx: &mut TestAppContext) {
    let mut app = prepared(cx, "CREATE TABLE e2e_import_target (id INTEGER PRIMARY KEY, name TEXT)");
    app.click("import-data");
    app.click("import-target-existing");
    let state = shown(&mut app);
    assert!(state.tables.iter().any(|table| table == "e2e_import_target"), "{:?}", state.tables);
    assert!(state.existing.is_some() && !state.truncate, "Delete existing rows first starts unchecked");
}

#[gpui_kit::test]
fn the_tables_context_menu_opens_the_import_on_that_table(cx: &mut TestAppContext) {
    let mut app = prepared(cx, "CREATE TABLE e2e_import_ctx (id INTEGER PRIMARY KEY, name TEXT)");
    app.context_menu("tree:t:main.e2e_import_ctx", IMPORT_INTO_TABLE);
    assert!(app.dialog_open());
    assert_eq!(shown(&mut app).existing.as_deref(), Some("e2e_import_ctx"));
}

#[gpui_kit::test]
fn a_csv_loads_into_an_existing_table_counting_rows_against_the_file(cx: &mut TestAppContext) {
    let mut app = prepared(cx, "CREATE TABLE e2e_import_run (id INTEGER PRIMARY KEY, name TEXT)");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("people.csv");
    let rows: String = (1..=1200).map(|i| format!("{i},person {i}\n")).collect();
    std::fs::write(&path, format!("id,name\n{rows}")).unwrap();
    app.context_menu("tree:t:main.e2e_import_run", IMPORT_INTO_TABLE);
    browse(&mut app, &path);
    let state = shown(&mut app);
    assert_eq!((state.path.as_str(), state.preview_rows), (path.to_str().unwrap(), Some(1200)));
    assert_eq!(import(&mut app), (1200, 0), "Imported 1,200 row(s), skipped 0");
    assert!(shown(&mut app).can_run, "Import is enabled again");
    assert_eq!(app.query("SELECT count(*) FROM e2e_import_run")[0][0], barsql_core::Value::Int(1200));
}

#[gpui_kit::test]
fn an_exported_csv_reimports_with_nulls_and_formula_text_intact(cx: &mut TestAppContext) {
    let mut app = prepared(
        cx,
        "CREATE TABLE e2e_roundtrip (id INTEGER, txt TEXT, note TEXT);
         INSERT INTO e2e_roundtrip VALUES (1, NULL, 'plain'), (2, '', '-not a number'), (3, 'x', '=1+2')",
    );
    app.run("SELECT * FROM e2e_roundtrip ORDER BY id;");
    app.click("export-results");
    app.menu_pick("export-format", 1);
    app.click("export-copy");
    app.pause();
    let csv = app.cx.read_from_clipboard().and_then(|item| item.text()).unwrap_or_default();
    assert_eq!(csv, "id,txt,note\n1,,plain\n2,\"\",'-not a number\n3,x,'=1+2");

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("roundtrip.csv");
    std::fs::write(&path, &csv).unwrap();
    app.context_menu("tree:t:main.e2e_roundtrip", IMPORT_INTO_TABLE);
    browse(&mut app, &path);
    assert_eq!(import(&mut app), (3, 0));
    let exact = "SELECT count(*) FROM e2e_roundtrip WHERE (id = 1 AND txt IS NULL AND note = 'plain')
        OR (id = 2 AND txt = '' AND note = '-not a number') OR (id = 3 AND txt = 'x' AND note = '=1+2')";
    assert_eq!(app.query(exact)[0][0], barsql_core::Value::Int(6), "each row came back exactly");
}

// Enter imports. Once the import is through Enter closes, so a second press can't load the file twice.
#[gpui_kit::test]
fn enter_imports_and_then_closes(cx: &mut TestAppContext) {
    let mut app = prepared(cx, "CREATE TABLE e2e_import_enter (id INTEGER PRIMARY KEY, name TEXT)");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("enter.csv");
    std::fs::write(&path, "id,name\n1,ann\n2,bob\n").unwrap();
    app.context_menu("tree:t:main.e2e_import_enter", IMPORT_INTO_TABLE);
    browse(&mut app, &path);
    app.keys("enter");
    let dialog = dialog(&mut app);
    app.settle(|cx| cx.update(|_, cx| dialog.read(cx).shown(cx).result.is_some()));
    assert!(app.dialog_open(), "the result shows");
    app.keys("enter");
    assert!(!app.dialog_open());
    assert_eq!(app.query("SELECT count(*) FROM e2e_import_enter")[0][0], barsql_core::Value::Int(2));
}
