use barsql_app::{EditorSession, EditorTab};
use barsql_core::{ConnectionConfig, DriverType, SavedQuery, TableInfo};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::{Root, WindowExt};
use gpui_kit::{AppContext as _, Entity, TestAppContext, VisualTestContext, px, size};

use super::{Candidates, Kind, SchemaTables, TabEntry, Target, rank};
use crate::actions::QuickSearch;
use crate::test_support::{Env, assert_dialog_holds_its_row, dialog_row, settle};
use crate::workspace::Workspace;
use crate::{saved_queries, schema};

fn connection(id: &str, name: &str, database: &str) -> ConnectionConfig {
    ConnectionConfig {
        id: id.into(),
        name: name.into(),
        driver: DriverType::Postgres,
        database: database.into(),
        color: "#22c55e".into(),
        ..Default::default()
    }
}

fn tab(id: &str, title: &str, connection_id: &str) -> TabEntry {
    TabEntry {
        id: id.into(),
        title: title.into(),
        connection_id: connection_id.into(),
        icon: Lucide::File,
        table: None,
        saved_query_id: String::new(),
    }
}

fn tables(connection_id: &str, schema: &str, names: &[&str]) -> SchemaTables {
    SchemaTables {
        connection_id: connection_id.into(),
        schema: schema.into(),
        tables: names
            .iter()
            .map(|name| TableInfo { schema: schema.into(), name: name.to_string(), kind: "table".into() })
            .collect(),
    }
}

fn saved(id: &str, name: &str, connection_id: &str) -> SavedQuery {
    SavedQuery { id: id.into(), name: name.into(), connection_id: connection_id.into(), ..Default::default() }
}

fn labels(query: &str, candidates: &Candidates) -> Vec<(Kind, String)> {
    rank(query, candidates).into_iter().map(|item| (item.kind, item.label)).collect()
}

#[test]
fn an_empty_query_lists_the_tabs_then_the_connections() {
    let candidates = Candidates {
        tabs: vec![tab("a", "Query 1 (Forum)", "pg"), tab("b", "posts", "gone")],
        connections: vec![connection("pg", "Forum", "forum_db"), connection("lite", "Local", "")],
        tables: vec![tables("pg", "public", &["posts"])],
        saved: vec![saved("s", "Top posts", "pg")],
    };
    let items = rank("  ", &candidates);
    let kinds: Vec<Kind> = items.iter().map(|item| item.kind).collect();
    assert_eq!(kinds, [Kind::Tab, Kind::Tab, Kind::Connection, Kind::Connection]);
    let details: Vec<Option<&str>> = items.iter().map(|item| item.detail.as_deref()).collect();
    assert_eq!(details, [Some("Forum"), None, Some("postgres · forum_db"), Some("postgres")]);
    assert_eq!(items[1].color, None, "a tab of a missing connection is muted");
    assert!(items.iter().all(|item| item.ranges.is_empty()));
}

#[test]
fn tables_and_saved_queries_already_open_are_left_out() {
    let table_tab = TabEntry { table: Some(("public".into(), "posts".into())), ..tab("t", "posts", "pg") };
    let linked = TabEntry { saved_query_id: "s1".into(), ..tab("l", "Renamed", "pg") };
    let candidates = Candidates {
        tabs: vec![table_tab, linked, tab("u", "Post stats", "pg")],
        connections: vec![connection("pg", "Forum", "")],
        tables: vec![tables("pg", "public", &["posts", "post_tags"]), tables("pg", "archive", &["posts"])],
        saved: vec![
            saved("s1", "Posts by day", "pg"),
            saved("s2", "Post stats", ""),
            saved("s3", "Posts per user", "pg"),
        ],
    };
    let found = labels("post", &candidates);
    assert!(found.contains(&(Kind::Table, "post_tags".into())));
    assert_eq!(found.iter().filter(|(kind, label)| *kind == Kind::Table && label == "posts").count(), 1);
    let saved: Vec<&str> =
        found.iter().filter(|(kind, _)| *kind == Kind::Saved).map(|(_, label)| label.as_str()).collect();
    assert_eq!(saved, ["Posts per user"]);
    let archive =
        rank("post", &candidates).into_iter().find(|item| item.kind == Kind::Table && item.label == "posts").unwrap();
    assert_eq!(
        archive.target,
        Target::Table { connection_id: "pg".into(), schema: "archive".into(), table: "posts".into() }
    );
    assert_eq!(archive.detail.as_deref(), Some("Forum"));
}

#[test]
fn categories_break_ties_and_every_match_is_listed() {
    let names: Vec<String> = (10..22).map(|n| format!("users_{n}")).collect();
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    let candidates = Candidates {
        tabs: vec![tab("a", "users", "pg")],
        connections: vec![connection("pg", "users", "")],
        tables: vec![tables("pg", "public", &["users"]), tables("pg", "extra", &names)],
        saved: vec![saved("s", "users", "other")],
    };
    let items = rank("users", &candidates);
    assert_eq!(items.len(), 16, "the tab, 13 tables, the saved query and the connection");
    let mut kinds: Vec<Kind> = items.iter().map(|item| item.kind).collect();
    kinds.dedup();
    assert_eq!(kinds, [Kind::Tab, Kind::Table, Kind::Saved, Kind::Connection], "each kind together, best first");
    assert_eq!(items[0].ranges.first(), Some(&(0..5)));
    assert_eq!(
        (items[1].label.as_str(), items[2].label.as_str()),
        ("users", "users_10"),
        "equal scores keep their order"
    );
}

#[test]
fn tables_match_their_own_name_only() {
    let candidates = Candidates {
        tabs: vec![tab("t", "Query 1", "pg")],
        connections: vec![connection("pg", "Warehouse", "")],
        tables: vec![tables("pg", "sales", &["orders"]), tables("pg", "public", &["monthly_sales"])],
        saved: vec![saved("s", "Top orders", "pg")],
    };
    assert_eq!(labels("sales", &candidates), [(Kind::Table, "monthly_sales".into())], "not by schema");
    assert!(labels("public", &candidates).is_empty());
    let by_connection = [
        (Kind::Connection, "Warehouse".to_string()),
        (Kind::Tab, "Query 1".to_string()),
        (Kind::Saved, "Top orders".to_string()),
    ];
    assert_eq!(labels("wareh", &candidates), by_connection, "tabs and saved queries still match their connection");
}

fn open(cx: &mut TestAppContext) -> (Entity<Workspace>, &mut VisualTestContext) {
    let mut workspace = None;
    let window = cx.add_window(|window, cx| {
        let view = cx.new(|cx| Workspace::new(window, cx));
        workspace = Some(view.clone());
        Root::new(view, window, cx)
    });
    (workspace.unwrap(), VisualTestContext::from_window(window.into(), cx).into_mut())
}

fn session(env: &Env) {
    let tab = EditorTab {
        id: "first".into(),
        connection_id: env.connection.id.clone(),
        title: "first".into(),
        sql: "SELECT 1".into(),
        ..Default::default()
    };
    env.bar.save_editor_session(EditorSession { tabs: vec![tab], active_tab: "first".into() }).unwrap();
}

fn dialog_open(cx: &mut VisualTestContext) -> bool {
    cx.update(|window, cx| window.has_active_dialog(cx))
}

fn search(cx: &mut VisualTestContext, query: &str) {
    cx.dispatch_action(QuickSearch);
    cx.run_until_parked();
    assert!(dialog_open(cx));
    cx.simulate_input(query);
    cx.run_until_parked();
}

#[gpui_kit::test]
fn typing_a_table_name_and_enter_opens_its_table_view(cx: &mut TestAppContext) {
    let env = Env::new(cx);
    session(&env);
    let id = env.connection.id.clone();
    let (workspace, cx) = open(cx);
    cx.update(|_, cx| schema::ensure_loaded(&id, DriverType::Sqlite, cx));
    settle(cx, |cx| cx.update(|_, cx| schema::get(cx, &id).is_some_and(|entry| entry.tables("main").is_some())));

    search(cx, "thin");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(!dialog_open(cx));
    let (count, title) = cx.update(|_, cx| {
        let workspace = workspace.read(cx);
        (workspace.tab_count(), workspace.active_title(cx))
    });
    assert_eq!((count, title.as_str()), (2, "things"));

    let dialog = workspace.update_in(cx, |ws, window, cx| ws.open_quick_search(window, cx)).unwrap();
    cx.run_until_parked();
    cx.simulate_input("thin");
    cx.run_until_parked();
    let items: Vec<(Kind, String)> =
        cx.update(|_, cx| dialog.read(cx).items.iter().map(|item| (item.kind, item.label.clone())).collect());
    assert_eq!(items, [(Kind::Tab, "things".to_string())], "the open table view is offered as its tab only");
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(!dialog_open(cx));
    assert_eq!(cx.update(|_, cx| workspace.read(cx).tab_count()), 2);

    search(cx, "local");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let title = cx.update(|_, cx| workspace.read(cx).active_title(cx));
    assert_eq!(title, "Query 2 · Local", "numbered among the plain query tabs, not the table view");
}

// An open Structure tab doesn't stand in for its table, so the table's data stays on offer.
#[gpui_kit::test]
fn a_tables_open_structure_leaves_its_data_on_offer(cx: &mut TestAppContext) {
    let env = Env::new(cx);
    session(&env);
    let (id, connection) = (env.connection.id.clone(), env.connection.clone());
    let (workspace, cx) = open(cx);
    cx.update(|_, cx| schema::ensure_loaded(&id, DriverType::Sqlite, cx));
    settle(cx, |cx| cx.update(|_, cx| schema::get(cx, &id).is_some_and(|entry| entry.tables("main").is_some())));
    workspace.update_in(cx, |ws, window, cx| {
        ws.open_structure(connection, "main".into(), Some("things".into()), window, cx)
    });
    cx.run_until_parked();

    let dialog = workspace.update_in(cx, |ws, window, cx| ws.open_quick_search(window, cx)).unwrap();
    cx.run_until_parked();
    cx.simulate_input("thin");
    cx.run_until_parked();
    let items: Vec<(Kind, String)> =
        cx.update(|_, cx| dialog.read(cx).items.iter().map(|item| (item.kind, item.label.clone())).collect());
    assert_eq!(items, [(Kind::Tab, "things".to_string()), (Kind::Table, "things".to_string())]);
    cx.simulate_keystrokes("down enter");
    cx.run_until_parked();
    assert!(cx.update(|_, cx| workspace.read(cx).table_tab().is_some()), "the data view opens");
}

#[gpui_kit::test]
fn arrows_pick_a_connection_and_its_new_tab_takes_the_keyboard(cx: &mut TestAppContext) {
    let env = Env::new(cx);
    session(&env);
    let (workspace, cx) = open(cx);

    search(cx, "");
    cx.simulate_keystrokes("down down up down");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(!dialog_open(cx));
    let (count, title) = cx.update(|_, cx| {
        let workspace = workspace.read(cx);
        (workspace.tab_count(), workspace.active_title(cx))
    });
    assert_eq!((count, title.as_str()), (2, "Query 2 · Local"));

    cx.simulate_input("SELECT 9");
    cx.run_until_parked();
    let sql = cx.update(|_, cx| workspace.read(cx).active_sql(cx));
    assert_eq!(sql.as_deref(), Some("SELECT 9"));
}

#[gpui_kit::test]
fn the_dialog_keeps_its_place_when_nothing_matches(cx: &mut TestAppContext) {
    let env = Env::new(cx);
    session(&env);
    let (workspace, cx) = open(cx);
    let dialog = workspace.update_in(cx, |ws, window, cx| ws.open_quick_search(window, cx)).unwrap();
    for scale in [1., 1.25, 1.5, 1.75, 2.] {
        cx.simulate_scale_factor_change(scale);
        let rows = ["", "nothing like this"].map(|query| {
            cx.update(|window, cx| super::type_query(&dialog, query, window, cx));
            dialog_row(scale, cx)
        });
        assert_dialog_holds_its_row(5.5, rows, scale, cx);
    }
}

#[gpui_kit::test]
fn saved_queries_open_from_the_palette(cx: &mut TestAppContext) {
    let env = Env::new(cx);
    session(&env);
    env.bar
        .save_saved_query(SavedQuery {
            name: "Heavy things".into(),
            connection_id: env.connection.id.clone(),
            sql: "SELECT * FROM things".into(),
            ..Default::default()
        })
        .unwrap();
    let (workspace, cx) = open(cx);
    cx.update(|_, cx| saved_queries::refresh(cx));

    search(cx, "heavy");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let (title, sql) = cx.update(|_, cx| {
        let workspace = workspace.read(cx);
        (workspace.active_title(cx), workspace.active_sql(cx))
    });
    assert_eq!((title.as_str(), sql.as_deref()), ("Heavy things", Some("SELECT * FROM things")));
}

fn save_queries(env: &Env, count: usize) {
    for n in 0..count {
        let query = SavedQuery {
            name: format!("Query {n:03}"),
            connection_id: env.connection.id.clone(),
            sql: format!("SELECT {n}"),
            ..Default::default()
        };
        env.bar.save_saved_query(query).unwrap();
    }
}

fn redraw(cx: &mut VisualTestContext) {
    cx.update(|window, cx| window.draw(cx).clear(cx));
}

// Every match is listed, yet only the rows on screen are built, so a long list costs what a short one does.
#[gpui_kit::test]
fn a_long_list_builds_only_the_rows_on_screen(cx: &mut TestAppContext) {
    let env = Env::new(cx);
    session(&env);
    save_queries(&env, 60);
    let (workspace, cx) = open(cx);
    cx.update(|_, cx| saved_queries::refresh(cx));
    cx.simulate_resize(size(px(1200.), px(500.)));
    let dialog = workspace.update_in(cx, |ws, window, cx| ws.open_quick_search(window, cx)).unwrap();
    cx.run_until_parked();
    cx.simulate_input("query");
    cx.run_until_parked();
    assert_eq!(cx.update(|_, cx| dialog.read(cx).items.len()), 60);
    assert!(cx.debug_bounds("quick-search-row-0").is_some(), "the best match is on screen");
    assert!(cx.debug_bounds("quick-search-row-15").is_none(), "a match below isn't built");

    let down = ["down"; 15].join(" ");
    cx.simulate_keystrokes(&down);
    redraw(cx);
    let list = cx.debug_bounds("quick-search-list").expect("the list is drawn");
    let row = cx.debug_bounds("quick-search-row-15").expect("Down scrolls to the picked match");
    assert!(list.top() <= row.top() && row.bottom() <= list.bottom(), "{row:?} shows inside {list:?}");

    cx.simulate_keystrokes(&["up"; 15].join(" "));
    redraw(cx);
    let first = cx.debug_bounds("quick-search-row-0").expect("Up scrolls back to the first match");
    assert!(first.top() > list.top(), "its heading shows above it");

    cx.simulate_keystrokes(&down);
    let picked = cx.update(|_, cx| dialog.read(cx).items[15].label.clone());
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(cx.update(|_, cx| workspace.read(cx).active_title(cx)), picked);
}

// Many matches fill 80% of the window, whatever its height. Fewer leave the list no taller than its rows.
#[gpui_kit::test]
fn the_dialog_grows_to_most_of_the_window(cx: &mut TestAppContext) {
    let env = Env::new(cx);
    session(&env);
    save_queries(&env, 60);
    let (workspace, cx) = open(cx);
    cx.update(|_, cx| saved_queries::refresh(cx));
    let dialog = workspace.update_in(cx, |ws, window, cx| ws.open_quick_search(window, cx)).unwrap();
    cx.update(|window, cx| super::type_query(&dialog, "query", window, cx));
    for height in [900., 600.] {
        cx.simulate_resize(size(px(1200.), px(height)));
        redraw(cx);
        let shown = cx.debug_bounds("quick-search").expect("the dialog is drawn");
        let tall = shown.size.height;
        assert!((tall - px(height * 0.8)).abs() <= px(0.5), "{tall:?} tall in a {height}px window");
        let legend = cx.debug_bounds("quick-search-legend").expect("the legend is drawn");
        assert!((legend.bottom() - shown.bottom()).abs() <= px(0.5), "the list scrolls, the legend stays in view");
    }

    cx.update(|window, cx| super::type_query(&dialog, "", window, cx));
    redraw(cx);
    let list = cx.debug_bounds("quick-search-list").expect("the list is drawn");
    let last = cx.debug_bounds("quick-search-row-1").expect("the connection, after the tab");
    let past = list.bottom() - last.bottom();
    assert!(past.abs() <= px(0.5), "the list runs {past:?} past its last row");
    assert_eq!((last.left(), last.right()), (list.left(), list.right()), "a row and its highlight span the list");
}
