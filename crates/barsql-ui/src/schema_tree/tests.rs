use std::cell::RefCell;
use std::rc::Rc;

use barsql_core::ColumnInfo;
use gpui_kit::{Entity, TestAppContext, VisualTestContext};

use super::{SchemaTree, SchemaTreeEvent, column_matches, table_matches};
use crate::schema_objects::{Group, group_key};
use crate::state;
use crate::test_support::{Env, settle};

fn open<'a>(env: &Env, cx: &'a mut TestAppContext) -> (Entity<SchemaTree>, &'a mut VisualTestContext) {
    let (tree, cx) = cx.add_window_view(SchemaTree::new);
    let connection = env.connection.clone();
    tree.update(cx, |tree, cx| tree.set_connection(Some(connection), cx));
    (tree, cx)
}

fn ids(tree: &Entity<SchemaTree>, cx: &mut VisualTestContext) -> Vec<String> {
    cx.update(|_, cx| tree.read(cx).rows.iter().map(|row| row.id.to_string()).collect())
}

fn has(tree: &Entity<SchemaTree>, id: &str) -> impl FnMut(&mut VisualTestContext) -> bool {
    let (tree, id) = (tree.clone(), id.to_string());
    move |cx| ids(&tree, cx).contains(&id)
}

fn connect(tree: &Entity<SchemaTree>, cx: &mut VisualTestContext) {
    tree.update(cx, |tree, cx| tree.connect(cx));
    settle(cx, has(tree, "s:main"));
}

#[test]
fn search_matches_names_and_column_types() {
    let column = ColumnInfo { name: "created_at".into(), data_type: "timestamp".into(), ..Default::default() };
    assert!(column_matches(&column, "created"));
    assert!(column_matches(&column, "timest"));
    assert!(!column_matches(&column, "name"));
    assert!(table_matches("Things", None, "thin"));
    assert!(table_matches("t", Some(std::slice::from_ref(&column)), "stamp"));
    assert!(!table_matches("t", Some(&[column]), "zzz"));
}

#[gpui_kit::test]
fn connecting_loads_and_expands_the_schema_then_tables_open_lazily(cx: &mut TestAppContext) {
    let env = Env::new(cx);
    let id = env.connection.id.clone();
    env.runtime.block_on(env.bar.execute_query(&id, "CREATE INDEX things_name ON things(name)")).unwrap();
    let (tree, cx) = open(&env, cx);
    assert!(ids(&tree, cx).is_empty(), "nothing before connecting");
    let events = Rc::new(RefCell::new(Vec::new()));
    let seen = events.clone();
    cx.update(|_, cx| {
        cx.subscribe(&tree, move |_, event: &SchemaTreeEvent, _| {
            if let SchemaTreeEvent::Connected(id) = event {
                seen.borrow_mut().push(id.clone());
            }
        })
        .detach()
    });
    connect(&tree, cx);
    assert_eq!(events.borrow().as_slice(), std::slice::from_ref(&id));
    assert_eq!(ids(&tree, cx)[..2], ["s:main", "t:main.things"], "the loaded schema opens");

    tree.update(cx, |tree, cx| tree.toggle_table("main", "things", cx));
    settle(cx, has(&tree, "c:main.things.name"));
    assert!(ids(&tree, cx).contains(&"c:main.things.id".to_string()));
    let stored = cx.update(|_, cx| state::setting(cx, super::TABLES_EXPANDED_KEY)).unwrap();
    assert_eq!(stored, format!("{{\"{id}:main:things\":true}}"));

    let key = group_key(&id, "main", "things", Group::Indexes);
    tree.update(cx, |tree, cx| {
        tree.toggle_group(key.clone(), Group::Indexes, "main".into(), Some("things".into()), cx)
    });
    settle(cx, has(&tree, &format!("o:{key}:things_name")));
}

#[gpui_kit::test]
fn searching_filters_tables_by_column_and_counts_them(cx: &mut TestAppContext) {
    let env = Env::new(cx);
    let id = env.connection.id.clone();
    env.runtime.block_on(env.bar.execute_query(&id, "CREATE TABLE others (id INTEGER, label TEXT)")).unwrap();
    let (tree, cx) = open(&env, cx);
    connect(&tree, cx);
    tree.update(cx, |tree, cx| tree.set_needle("label".into(), cx));
    settle(cx, has(&tree, "c:main.others.label"));
    let rows = ids(&tree, cx);
    assert_eq!(rows, ["s:main", "t:main.others", "c:main.others.label"], "only the matching column, no groups");
    let count = cx.update(|_, cx| match &tree.read(cx).rows[0].kind {
        super::RowKind::Schema { count, .. } => count.clone(),
        _ => None,
    });
    assert_eq!(count.as_deref(), Some("1/2"));

    tree.update(cx, |tree, cx| tree.set_needle("nothing matches this".into(), cx));
    settle(cx, |cx| ids(&tree, cx).is_empty());
}

#[gpui_kit::test]
fn refresh_reloads_and_forgets_loaded_groups(cx: &mut TestAppContext) {
    let env = Env::new(cx);
    let id = env.connection.id.clone();
    let (tree, cx) = open(&env, cx);
    connect(&tree, cx);
    let key = crate::schema_objects::routines_key(&id, "main");
    tree.update(cx, |tree, cx| tree.toggle_group(key.clone(), Group::Routines, "main".into(), None, cx));
    settle(cx, has(&tree, &format!("n:{key}")));
    env.runtime.block_on(env.bar.execute_query(&id, "CREATE TABLE later (id INTEGER)")).unwrap();
    tree.update(cx, |tree, cx| tree.refresh(cx));
    settle(cx, has(&tree, "t:main.later"));
    assert!(!ids(&tree, cx).contains(&format!("n:{key}")), "groups start collapsed again");
}

fn cursor(tree: &Entity<SchemaTree>, cx: &mut VisualTestContext) -> Option<String> {
    cx.update(|_, cx| tree.read(cx).cursor.as_ref().map(|id| id.to_string()))
}

#[gpui_kit::test]
fn the_keyboard_walks_and_opens_the_tree(cx: &mut TestAppContext) {
    let env = Env::new(cx);
    let (tree, cx) = open(&env, cx);
    connect(&tree, cx);
    let inserted = Rc::new(RefCell::new(Vec::new()));
    let seen = inserted.clone();
    cx.update(|_, cx| {
        cx.subscribe(&tree, move |_, event: &SchemaTreeEvent, _| {
            if let SchemaTreeEvent::Insert(text) = event {
                seen.borrow_mut().push(text.clone());
            }
        })
        .detach()
    });
    tree.update_in(cx, |tree, window, cx| tree.focus.focus(window, cx));
    cx.run_until_parked();

    cx.simulate_keystrokes("down down");
    assert_eq!(cursor(&tree, cx).as_deref(), Some("t:main.things"));
    cx.simulate_keystrokes("enter");
    settle(cx, has(&tree, "c:main.things.name"));
    cx.simulate_keystrokes("down down");
    assert_eq!(cursor(&tree, cx).as_deref(), Some("c:main.things.name"));
    cx.simulate_keystrokes("space");
    cx.run_until_parked();
    assert_eq!(*inserted.borrow(), ["name"]);

    cx.simulate_keystrokes("home");
    assert_eq!(cursor(&tree, cx).as_deref(), Some("s:main"));
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(ids(&tree, cx), ["s:main"], "Enter closes the open schema");
    cx.simulate_keystrokes("end up");
    assert_eq!(cursor(&tree, cx).as_deref(), Some("s:main"), "one row left");
}
