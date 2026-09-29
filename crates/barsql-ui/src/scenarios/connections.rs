// Drag-and-drop reordering is tested in connections_drag_into_order_and_folders.
use gpui_kit::TestAppContext;

use super::driver::{Driver, open, selector};
use crate::file_dialogs::Stub;

fn open_switcher(app: &mut Driver) {
    app.click("connection-switcher");
}

// `kind` is connect, edit or delete. Connect doubles as disconnect.
fn row_button(app: &mut Driver, kind: &str, id: &str) {
    open_switcher(app);
    app.hover(selector(format!("connection-row-{id}")));
    app.click(selector(format!("{kind}-{id}")));
}

fn names(app: &Driver) -> Vec<String> {
    app.env.bar.list_connections().into_iter().map(|c| c.name).collect()
}

fn selected(app: &mut Driver) -> Option<String> {
    let sidebar = app.sidebar();
    app.cx.update(|_, cx| sidebar.read(cx).selected().map(|c| c.name.clone()))
}

#[gpui_kit::test]
fn the_switcher_button_closes_what_it_opened(cx: &mut TestAppContext) {
    let mut app = open(cx);
    open_switcher(&mut app);
    assert!(app.shown("connection-switcher-menu"));
    open_switcher(&mut app);
    assert!(!app.shown("connection-switcher-menu"), "a second click closes it");
    open_switcher(&mut app);
    assert!(app.shown("connection-switcher-menu"), "and a third opens it again");
}

#[gpui_kit::test]
fn a_new_connection_is_tested_saved_connected_and_disconnected(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("e2e.sqlite");
    std::fs::write(&file, b"").unwrap();
    open_switcher(&mut app);
    app.click("new-connection");
    assert!(app.dialog_open(), "New Connection");
    app.click("conn-name");
    app.type_text("E2E SQLite");
    app.cx.update(|_, cx| cx.set_global(Stub(Some(file.clone()))));
    app.click("browse-sqlite");
    app.click("connection-test");
    app.settle(|cx| {
        cx.update(|_, cx| crate::toast::messages(cx).iter().any(|(_, text)| text == "Connection successful!"))
    });
    app.click("connection-save");
    app.settle_dialog_closed();
    let id = app.env.bar.list_connections().into_iter().find(|c| c.name == "E2E SQLite").expect("saved").id;

    row_button(&mut app, "connect", &id);
    let bar = app.env.bar.clone();
    let target = id.clone();
    app.settle(|_| bar.is_connected(&target));
    assert_eq!(selected(&mut app).as_deref(), Some("E2E SQLite"), "the switcher shows it");
    row_button(&mut app, "connect", &id);
    let target = id.clone();
    app.settle(|_| !bar.is_connected(&target));
}

#[gpui_kit::test]
fn a_saved_connection_is_edited_from_the_switcher(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let id = app.connection.id.clone();
    row_button(&mut app, "edit", &id);
    assert!(app.dialog_open(), "Edit Connection");
    app.click("conn-name");
    app.keys("mod-a");
    app.type_text("Local (edited)");
    app.click("connection-save");
    app.settle_dialog_closed();
    assert_eq!(names(&app), ["Local (edited)"]);
    assert_eq!(selected(&mut app).as_deref(), Some("Local (edited)"));
}

#[gpui_kit::test]
fn a_connection_is_deleted_after_confirming(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let id = app.connection.id.clone();
    row_button(&mut app, "delete", &id);
    assert!(app.dialog_open(), "Delete connection asks first");
    app.confirm_dialog();
    let sidebar = app.sidebar();
    app.settle(|cx| cx.update(|_, cx| sidebar.read(cx).connections().is_empty()));
    assert!(names(&app).is_empty());
    assert_eq!(selected(&mut app), None, "the switcher offers a new connection");
}
