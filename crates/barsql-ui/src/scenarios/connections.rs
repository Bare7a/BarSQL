// Drag-and-drop reordering is tested in connections_drag_into_order_and_folders.
use barsql_core::{ConnectionConfig, DriverType};
use gpui_kit::component::scroll::Scrollbar;
use gpui_kit::{Pixels, TestAppContext, px, size};

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

// Adds 40 connections, in a window short enough for the switcher's list to scroll.
fn many_connections(app: &mut Driver) {
    for n in 0..40 {
        let config = ConnectionConfig {
            name: format!("Conn {n:02}"),
            driver: DriverType::Sqlite,
            file_path: format!("/tmp/barsql-conn-{n}.db"),
            ..Default::default()
        };
        app.env.runtime.block_on(app.env.bar.save_connection(config)).unwrap();
    }
    app.reopen();
    app.cx.simulate_resize(size(px(1200.), px(560.)));
}

// How far the first row's Delete ends from the list's right side, with the switcher open.
fn delete_inset(app: &mut Driver) -> Pixels {
    let first = app.env.bar.list_connections()[0].id.clone();
    app.hover(selector(format!("connection-row-{first}")));
    let list = app.bounds("connection-list").expect("the list is drawn");
    let delete = app.bounds(selector(format!("delete-{first}"))).expect("hovering shows the buttons");
    list.right() - delete.right()
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

#[gpui_kit::test]
fn a_long_switcher_keeps_its_toolbar_and_scrolls_the_list(cx: &mut TestAppContext) {
    let mut app = open(cx);
    many_connections(&mut app);
    open_switcher(&mut app);
    let viewport = app.cx.update(|window, _| window.viewport_size());
    let menu = app.bounds("connection-switcher-menu").expect("the switcher is open");
    assert!(menu.size.height <= viewport.height * 0.6 + px(0.5), "{menu:?} fits under the cap for {viewport:?}");
    let toolbar = app.bounds("new-connection").expect("the toolbar is drawn");
    let list = app.bounds("connection-list").expect("the list is drawn");
    assert!(menu.contains(&toolbar.origin) && list.bottom() <= menu.bottom(), "the list shrinks to the menu");
    let first = app.env.bar.list_connections()[0].id.clone();
    app.scrolls_by_its_bar("connection-list", selector(format!("connection-row-{first}")));
    assert_eq!(app.bounds("new-connection"), Some(toolbar), "the toolbar stays put");
}

// A shown bar takes clicks on its track, so once the list can scroll the row buttons move clear of it.
#[gpui_kit::test]
fn row_buttons_keep_clear_of_the_switcher_bar(cx: &mut TestAppContext) {
    let mut app = open(cx);
    open_switcher(&mut app);
    assert!(delete_inset(&mut app) < Scrollbar::width(), "a short list keeps them at its edge");
    open_switcher(&mut app);
    many_connections(&mut app);
    open_switcher(&mut app);
    assert!(delete_inset(&mut app) >= Scrollbar::width() - px(0.5), "a long list moves them off its bar");
}
