use std::path::PathBuf;
use std::time::Duration;

use barsql_app::{EditorSession, EditorTab, TableViewRef};
use barsql_core::SavedQuery;
use gpui_kit::component::{Root, WindowExt};
use gpui_kit::{
    AppContext as _, Entity, ExternalPaths, FileDropEvent, Modifiers, MouseButton, TestAppContext, VisualTestContext,
    point, px, size,
};

use super::Workspace;
use crate::query_tab::QueryTab;
use crate::saved_queries;
use crate::test_support::Env;
use crate::window_state;

fn tab(id: &str, connection_id: &str, sql: &str) -> EditorTab {
    EditorTab {
        id: id.into(),
        connection_id: connection_id.into(),
        title: id.into(),
        sql: sql.into(),
        ..Default::default()
    }
}

// Wrapped in Root like the app, so dialogs can open.
fn open(cx: &mut TestAppContext) -> (Entity<Workspace>, &mut VisualTestContext) {
    let mut workspace = None;
    let window = cx.add_window(|window, cx| {
        let view = cx.new(|cx| Workspace::new(window, cx));
        workspace = Some(view.clone());
        Root::new(view, window, cx)
    });
    (workspace.unwrap(), VisualTestContext::from_window(window.into(), cx).into_mut())
}

fn active(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> Entity<QueryTab> {
    cx.update(|_, cx| workspace.read(cx).active_query().cloned().unwrap())
}

fn modifier() -> &'static str {
    if cfg!(target_os = "macos") { "cmd" } else { "ctrl" }
}

fn end_of_text() -> &'static str {
    if cfg!(target_os = "macos") { "cmd-down" } else { "ctrl-end" }
}

fn ids(session: &EditorSession) -> Vec<&str> {
    session.tabs.iter().map(|t| t.id.as_str()).collect()
}

#[gpui_kit::test]
fn table_views_restore_and_orphaned_tabs_stay_in_the_session(cx: &mut TestAppContext) {
    let env = Env::new(cx);
    let id = env.connection.id.clone();
    let table_view = EditorTab {
        table_view: Some(TableViewRef { schema: "main".into(), table: "things".into(), ..Default::default() }),
        ..tab("view", &id, "")
    };
    let session = EditorSession {
        tabs: vec![tab("first", &id, "SELECT 1"), table_view, tab("orphan", "gone", "SELECT 2")],
        active_tab: "first".into(),
    };
    env.bar.save_editor_session(session).unwrap();
    let (workspace, cx) = open(cx);
    assert_eq!(cx.update(|_, cx| workspace.read(cx).tabs.len()), 2);
    assert!(cx.update(|_, cx| matches!(workspace.read(cx).tabs[1].view, super::TabView::Table(_))));

    let connection = env.connection.clone();
    workspace.update_in(cx, |ws, window, cx| ws.open_query_tab(connection, window, cx));
    let saved = env.bar.editor_session();
    let new_id = cx.update(|_, cx| workspace.read(cx).tabs[2].view.id(cx));
    assert_eq!(ids(&saved), ["first", "view", "orphan", new_id.as_str()]);
    assert_eq!(saved.active_tab, new_id);
    assert!(saved.tabs[1].table_view.is_some());
    assert_eq!(saved.tabs[3].color, env.connection.color);
}

#[gpui_kit::test]
fn a_session_that_opens_on_a_table_view_fills_the_sidebar_tree(cx: &mut TestAppContext) {
    let env = Env::new(cx);
    let id = env.connection.id.clone();
    // Seeding left the connection open, and the app starts with none.
    env.runtime.block_on(env.bar.disconnect(&id));
    let table_view = EditorTab {
        table_view: Some(TableViewRef { schema: "main".into(), table: "things".into(), ..Default::default() }),
        ..tab("view", &id, "")
    };
    env.bar.save_editor_session(EditorSession { tabs: vec![table_view], active_tab: "view".into() }).unwrap();
    let (workspace, cx) = open(cx);
    let tree = cx.update(|_, cx| workspace.read(cx).sidebar().read(cx).panels().0);
    crate::test_support::settle(cx, |cx| {
        cx.update(|_, cx| tree.read(cx).listed().iter().any(|(row, _, _)| row == "s:main"))
    });
}

#[gpui_kit::test]
fn edits_are_saved_after_a_pause_and_closing_saves_at_once(cx: &mut TestAppContext) {
    let env = Env::new(cx);
    let id = env.connection.id.clone();
    let session =
        EditorSession { tabs: vec![tab("a", &id, "SELECT 1"), tab("b", &id, "SELECT 2")], active_tab: "b".into() };
    env.bar.save_editor_session(session).unwrap();
    let (workspace, cx) = open(cx);

    cx.simulate_keystrokes(end_of_text());
    cx.simulate_input(" + 1");
    cx.run_until_parked();
    assert_eq!(env.bar.editor_session().tabs[1].sql, "SELECT 2", "not before the debounce");
    cx.executor().advance_clock(Duration::from_secs(1));
    cx.run_until_parked();
    assert_eq!(env.bar.editor_session().tabs[1].sql, "SELECT 2 + 1");

    workspace.update_in(cx, |ws, window, cx| ws.close_tab(1, window, cx));
    let saved = env.bar.editor_session();
    assert_eq!((ids(&saved), saved.active_tab.as_str()), (vec!["a"], "a"));

    workspace.update_in(cx, |ws, window, cx| ws.reopen_closed_tab(window, cx));
    let saved = env.bar.editor_session();
    assert_eq!(ids(&saved), ["a", "b"]);
    assert_eq!(saved.tabs[1].sql, "SELECT 2 + 1");
}

#[gpui_kit::test]
fn saved_queries_open_linked_tabs_that_track_changes(cx: &mut TestAppContext) {
    let env = Env::new(cx);
    let query = SavedQuery {
        name: "Top things".into(),
        connection_id: env.connection.id.clone(),
        sql: "SELECT 1".into(),
        ..Default::default()
    };
    let saved = env.bar.save_saved_query(query).unwrap();
    let (workspace, cx) = open(cx);
    cx.update(|_, cx| saved_queries::refresh(cx));
    workspace.update_in(cx, |ws, window, cx| ws.open_saved(saved.clone(), window, cx));
    let tab = active(&workspace, cx);
    let (title, linked, dirty) = cx.update(|_, cx| {
        (tab.read(cx).title.to_string(), tab.read(cx).saved_query_id().to_string(), tab.read(cx).is_dirty(cx))
    });
    assert_eq!((title.as_str(), linked.as_str(), dirty), ("Top things", saved.id.as_str(), false));

    cx.simulate_keystrokes(end_of_text());
    cx.simulate_input(" + 1");
    assert!(cx.update(|_, cx| tab.read(cx).is_dirty(cx)));
    cx.simulate_keystrokes(&format!("{}-s", modifier()));
    assert!(!cx.update(|_, cx| tab.read(cx).is_dirty(cx)));
    assert_eq!(env.bar.list_saved_queries("")[0].sql, "SELECT 1 + 1");
    assert_eq!(cx.update(|_, cx| saved_queries::list(cx)[0].sql.clone()), "SELECT 1 + 1");

    workspace.update_in(cx, |ws, window, cx| ws.open_saved(saved.clone(), window, cx));
    assert_eq!(cx.update(|_, cx| workspace.read(cx).tabs.len()), 1, "the linked tab is reused");

    cx.simulate_input(" -- more");
    workspace.update_in(cx, |ws, window, cx| ws.request_close(0, window, cx));
    assert!(cx.update(|window, cx| window.has_active_dialog(cx)), "unsaved changes ask first");
    assert_eq!(cx.update(|_, cx| workspace.read(cx).tabs.len()), 1);
}

#[gpui_kit::test]
fn history_sql_lands_in_the_plain_query_tab(cx: &mut TestAppContext) {
    let env = Env::new(cx);
    let (workspace, cx) = open(cx);
    let connection = env.connection.clone();
    workspace.update_in(cx, |ws, window, cx| ws.open_sql(connection.clone(), "SELECT 1".into(), window, cx));
    workspace.update_in(cx, |ws, window, cx| ws.open_sql(connection.clone(), "SELECT 2".into(), window, cx));
    let (count, title, sql) = cx.update(|_, cx| {
        let ws = workspace.read(cx);
        let tab = ws.active_query().unwrap().read(cx);
        (ws.tabs.len(), tab.title.to_string(), tab.sql(cx).to_string())
    });
    assert_eq!((count, title.as_str(), sql.as_str()), (1, "Query 1", "SELECT 2"));
    // Once edited, the tab keeps its text and the next entry gets a tab of its own.
    let tab = cx.update(|_, cx| workspace.read(cx).active_query().cloned().unwrap());
    tab.update_in(cx, |tab, window, cx| tab.set_sql("SELECT 2 -- mine".into(), window, cx));
    workspace.update_in(cx, |ws, window, cx| ws.open_sql(connection.clone(), "SELECT 3".into(), window, cx));
    let (count, kept) = cx.update(|_, cx| (workspace.read(cx).tabs.len(), tab.read(cx).sql(cx).to_string()));
    assert_eq!((count, kept.as_str()), (2, "SELECT 2 -- mine"));
}

#[gpui_kit::test]
fn panel_widths_come_back_clamped_and_drags_store_them(cx: &mut TestAppContext) {
    let env = Env::new(cx);
    env.bar.set_setting("barsql-sidebar-w", "700").unwrap();
    env.bar.set_setting("barsql-json-w", "wide").unwrap();
    let (workspace, cx) = open(cx);
    let widths = cx.update(|_, cx| {
        let workspace = workspace.read(cx);
        (workspace.sidebar_width, workspace.json_width)
    });
    assert_eq!(widths, (px(520.), px(320.)));

    workspace.update(cx, |workspace, cx| {
        workspace.json_open = true;
        workspace.panels_resized(&[px(301.4), px(600.), px(250.6)], cx);
    });
    let settings = env.bar.settings();
    assert_eq!(settings.get("barsql-sidebar-w").map(String::as_str), Some("301"));
    assert_eq!(settings.get("barsql-json-w").map(String::as_str), Some("251"));
}

#[gpui_kit::test]
fn window_moves_and_resizes_are_saved_after_a_pause(cx: &mut TestAppContext) {
    let env = Env::new(cx);
    let (workspace, cx) = open(cx);
    workspace.update_in(cx, |workspace, window, cx| workspace.track_window(None, window, cx));
    cx.simulate_resize(size(px(1000.), px(700.)));
    cx.run_until_parked();
    assert_eq!(env.bar.settings().get(window_state::KEY), None, "not before the pause");

    cx.executor().advance_clock(Duration::from_millis(500));
    cx.run_until_parked();
    let saved = env.bar.settings().get(window_state::KEY).and_then(|raw| window_state::parse(raw)).unwrap();
    assert_eq!((saved.mode.as_str(), saved.width, saved.height), ("normal", 1000, 700));
}

fn dialog_open(cx: &mut VisualTestContext) -> bool {
    cx.update(|window, cx| window.has_active_dialog(cx))
}

// Second launches and Finder opens both go through open_sqlite.
#[gpui_kit::test]
fn a_sqlite_file_from_the_os_opens_a_filled_in_connection(cx: &mut TestAppContext) {
    let env = Env::new(cx);
    let (_workspace, cx) = open(cx);
    cx.run_until_parked();
    assert!(!dialog_open(cx));
    env.bar.open_sqlite("/tmp/notes.sqlite");
    crate::test_support::settle(cx, dialog_open);
}

#[gpui_kit::test]
fn dropping_files_opens_the_first_sqlite_one(cx: &mut TestAppContext) {
    let _env = Env::new(cx);
    let (_workspace, cx) = open(cx);
    cx.run_until_parked();
    let drop = |cx: &mut VisualTestContext, paths: &[&str]| {
        let position = point(px(400.), px(300.));
        let paths = ExternalPaths(paths.iter().map(PathBuf::from).collect());
        cx.simulate_event(FileDropEvent::Entered { position, paths });
        cx.simulate_event(FileDropEvent::Pending { position });
        cx.simulate_event(FileDropEvent::Submit { position });
        cx.run_until_parked();
    };
    drop(cx, &["/tmp/readme.txt"]);
    assert!(!dialog_open(cx), "no SQLite file, no dialog");
    drop(cx, &["/tmp/readme.txt", "/tmp/shop.db"]);
    assert!(dialog_open(cx));
}

fn checked(menus: &[gpui_kit::OwnedMenu], name: &str) -> Option<bool> {
    menus.iter().find_map(|menu| {
        menu.items.iter().find_map(|item| match item {
            gpui_kit::OwnedMenuItem::Action { name: item_name, checked, .. } if item_name == name => Some(*checked),
            gpui_kit::OwnedMenuItem::Submenu(submenu) => checked(std::slice::from_ref(submenu), name),
            _ => None,
        })
    })
}

#[gpui_kit::test]
fn the_menus_tick_what_is_on(cx: &mut TestAppContext) {
    let _env = Env::new(cx);
    let (workspace, cx) = open(cx);
    let menus = |cx: &mut VisualTestContext| cx.update(|_, cx| cx.get_menus().unwrap_or_default());
    let current = menus(cx);
    assert_eq!(checked(&current, "Dark theme"), Some(true));
    assert_eq!(checked(&current, "English"), Some(true));
    assert_eq!(checked(&current, "Toggle sidebar"), Some(true));
    assert_eq!(checked(&current, "Toggle JSON viewer"), Some(false));

    // Windows and Linux draw these in the title bar from GPUI Kit's copy.
    let drawn = |cx: &mut VisualTestContext| {
        cx.update(|_, cx| gpui_kit::component::GlobalState::global(cx).app_menus().to_vec())
    };
    let names: Vec<String> = drawn(cx).iter().map(|menu| menu.name.to_string()).collect();
    for name in ["File", "Edit", "View", "Help"] {
        assert!(names.iter().any(|menu| menu == name), "{name} in {names:?}");
    }

    workspace.update_in(cx, |_, window, cx| window.dispatch_action(Box::new(crate::actions::ToggleSidebar), cx));
    cx.run_until_parked();
    assert_eq!(checked(&menus(cx), "Toggle sidebar"), Some(false));
    assert_eq!(checked(&drawn(cx), "Toggle sidebar"), Some(false));
    if cfg!(target_os = "macos") {
        assert!(menus(cx).iter().any(|menu| menu.name.as_ref() == "Window"), "AppKit's Window menu");
    }
}

#[gpui_kit::test]
fn tabs_drag_into_a_new_order(cx: &mut TestAppContext) {
    let env = Env::new(cx);
    let id = env.connection.id.clone();
    let session = EditorSession {
        tabs: vec![tab("a", &id, "SELECT 1"), tab("b", &id, "SELECT 2"), tab("c", &id, "SELECT 3")],
        active_tab: "b".into(),
    };
    env.bar.save_editor_session(session).unwrap();
    let (_workspace, cx) = open(cx);
    cx.run_until_parked();
    let drag = |cx: &mut VisualTestContext, from: &'static str, to: &'static str| {
        let (start, end) = (cx.debug_bounds(from).unwrap().center(), cx.debug_bounds(to).unwrap().center());
        cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(start + point(px(8.), px(0.)), MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(end, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_up(end, MouseButton::Left, Modifiers::default());
        cx.run_until_parked();
    };
    drag(cx, "editor-tab-0", "editor-tab-2");
    let saved = env.bar.editor_session();
    assert_eq!((ids(&saved), saved.active_tab.as_str()), (vec!["b", "c", "a"], "b"));
    drag(cx, "editor-tab-2", "editor-tab-0");
    assert_eq!(ids(&env.bar.editor_session()), ["a", "b", "c"]);
}

#[gpui_kit::test]
fn a_closed_table_view_reopens_with_its_filter_and_sort(cx: &mut TestAppContext) {
    let env = Env::new(cx);
    let (workspace, cx) = open(cx);
    let connection = env.connection.clone();
    workspace.update_in(cx, |ws, window, cx| {
        ws.open_table(connection, "main".into(), "things".into(), Some("id > 2990".into()), window, cx)
    });
    let table = cx.update(|_, cx| match &workspace.read(cx).tabs.last().unwrap().view {
        super::TabView::Table(table) => table.clone(),
        super::TabView::Query(_) => panic!("a table view"),
    });
    crate::test_support::settle(cx, |cx| cx.update(|_, cx| table.read(cx).is_loaded()));
    table.update_in(cx, |table, window, cx| table.sort_by(1, window, cx));
    crate::test_support::settle(cx, |cx| cx.update(|_, cx| table.read(cx).is_loaded()));
    let before = cx.update(|_, cx| table.read(cx).stored(cx).table_view.unwrap());
    assert_eq!((before.filter.as_str(), before.order_by.as_str()), ("id > 2990", "name"));

    let last = cx.update(|_, cx| workspace.read(cx).tab_count() - 1);
    workspace.update_in(cx, |ws, window, cx| ws.close_tab(last, window, cx));
    workspace.update_in(cx, |ws, window, cx| ws.reopen_closed_tab(window, cx));
    let reopened = cx.update(|_, cx| workspace.read(cx).tabs.last().unwrap().view.stored(cx).table_view.unwrap());
    assert_eq!(reopened, before);
}
