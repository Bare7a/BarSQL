use std::cell::RefCell;
use std::rc::Rc;

use barsql_core::{ColumnInfo, DriverType, ObjectKind, SqlDialect};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{Entity, Modifiers, MouseButton, MouseDownEvent, TestAppContext, VisualTestContext};

use super::{
    SchemaTree, SchemaTreeEvent, column_details, column_icon, column_matches, group_icon, match_ranges, relation_look,
    table_matches, type_label, type_needle,
};
use crate::assets::AppAssets;
use crate::i18n::I18n;
use crate::schema_objects::{Group, group_key};
use crate::state;
use crate::test_support::{Env, in_root, settle};

// In Root, so its menus can open.
fn open<'a>(env: &Env, cx: &'a mut TestAppContext) -> (Entity<SchemaTree>, &'a mut VisualTestContext) {
    let (tree, cx) = in_root(cx, SchemaTree::new);
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

fn inserts(tree: &Entity<SchemaTree>, cx: &mut VisualTestContext) -> Rc<RefCell<Vec<String>>> {
    let inserted = Rc::new(RefCell::new(Vec::new()));
    let seen = inserted.clone();
    cx.update(|_, cx| {
        cx.subscribe(tree, move |_, event: &SchemaTreeEvent, _| {
            if let SchemaTreeEvent::Insert(text) = event {
                seen.borrow_mut().push(text.clone());
            }
        })
        .detach()
    });
    inserted
}

// Picks item `downs` of the options menu with the keyboard. Separators don't count.
fn pick_option(downs: usize, cx: &mut VisualTestContext) {
    cx.update(|window, cx| window.click("schema-options", cx));
    cx.update(|window, cx| window.draw(cx).clear(cx));
    cx.simulate_keystrokes(&format!("{}enter", "down ".repeat(downs + 1)));
    cx.run_until_parked();
}

#[test]
fn search_matches_names_and_column_types() {
    let sqlite = DriverType::Sqlite;
    let column = ColumnInfo { name: "created_at".into(), data_type: "timestamp".into(), ..Default::default() };
    assert!(column_matches(&sqlite, &column, "created"));
    assert!(column_matches(&sqlite, &column, "timest"));
    assert!(!column_matches(&sqlite, &column, "name"));
    assert!(table_matches(&sqlite, "Things", None, "thin"));
    assert!(table_matches(&sqlite, "t", Some(std::slice::from_ref(&column)), "stamp"));
    assert!(!table_matches(&sqlite, "t", Some(&[column]), "zzz"));
}

// The tree shows `varchar(50)`, so searching for what's shown finds it, as does the catalog's own spelling.
fn column(name: &str, data_type: &str) -> ColumnInfo {
    ColumnInfo { name: name.into(), data_type: data_type.into(), ..Default::default() }
}

// A key's icon says it's a key whatever its type, and a column that takes its values from a fixed list says so
// before its type does.
#[test]
fn column_icons_put_keys_first_then_fixed_lists_then_the_type() {
    let pg = Some(SqlDialect::Postgres);
    let statuses = vec!["draft".to_string(), "live".to_string()];
    let key = ColumnInfo { is_primary: true, enum_values: statuses.clone(), ..column("code", "character varying(10)") };
    let link = ColumnInfo { is_foreign: true, ..column("author_id", "uuid") };
    let status = ColumnInfo { enum_values: statuses, ..column("status", "character varying(10)") };
    assert_eq!(column_icon(&key, pg), Lucide::KeyRound);
    assert_eq!(column_icon(&link, pg), Lucide::Link);
    assert_eq!(column_icon(&status, pg), Lucide::List);
    assert_eq!(column_icon(&column("metadata", "jsonb"), pg), Lucide::Braces);
    assert_eq!(column_icon(&column("created_at", "timestamp without time zone"), pg), Lucide::CalendarClock);
    assert_eq!(column_icon(&column("location", "point"), pg), Lucide::Columns3);
}

// An icon the app doesn't ship draws as nothing, so each one the tree can pick has to load.
#[test]
fn every_icon_the_tree_can_draw_is_shipped() {
    use gpui_kit::AssetSource as _;
    let pg = Some(SqlDialect::Postgres);
    let types = ["bigint", "text", "boolean", "date", "time", "timestamp", "jsonb", "uuid", "bytea", "text[]", "point"];
    let mut icons: Vec<Lucide> = types.into_iter().map(|data_type| column_icon(&column("c", data_type), pg)).collect();
    icons.push(column_icon(&ColumnInfo { enum_values: vec!["a".into()], ..column("c", "text") }, pg));
    icons.push(column_icon(&ColumnInfo { is_primary: true, ..column("c", "text") }, pg));
    icons.push(column_icon(&ColumnInfo { is_foreign: true, ..column("c", "text") }, pg));
    icons
        .extend([ObjectKind::Table, ObjectKind::View, ObjectKind::MaterializedView].iter().map(|k| relation_look(k).0));
    icons.extend([Group::Indexes, Group::Constraints, Group::Triggers, Group::Routines].map(group_icon));
    for icon in icons {
        let path = icon.path();
        assert!(AppAssets.load(&path).ok().flatten().is_some(), "{path}");
    }
}

#[test]
fn nullable_types_end_in_a_question_mark_unless_the_type_says_so() {
    let nullable = |data_type: &str| ColumnInfo { is_nullable: true, ..column("c", data_type) };
    assert_eq!(type_label(&nullable("character varying(255)"), Some(SqlDialect::Postgres)), "varchar(255)?");
    assert_eq!(type_label(&column("id", "bigint"), Some(SqlDialect::Postgres)), "bigint");
    assert_eq!(type_label(&nullable("Nullable(String)"), Some(SqlDialect::ClickHouse)), "Nullable(String)");
}

// A materialized view stores rows and a view doesn't, so they can't share an icon or a badge.
#[test]
fn materialized_views_look_apart_from_views() {
    let en = I18n::new("en");
    let (view_icon, view_badge) = relation_look(&ObjectKind::View);
    let (stored_icon, stored_badge) = relation_look(&ObjectKind::MaterializedView);
    assert_ne!(view_icon, stored_icon);
    let (view_badge, stored_badge) = (view_badge.unwrap(), stored_badge.unwrap());
    assert_ne!(en.t(view_badge), en.t(stored_badge));
    assert_ne!(en.t(stored_badge).as_ref(), stored_badge, "the badge has no English label");
    assert_eq!(relation_look(&ObjectKind::Table), (Lucide::Table2, None));
}

// The needle arrives lowercased, so `ID` matches, and every occurrence is marked, not just the first.
#[test]
fn matches_are_marked_wherever_they_fall_in_any_case() {
    assert_eq!(match_ranges("Post_ID", "id"), vec![5..7]);
    assert_eq!(match_ranges("id_paid", "id"), vec![0..2, 5..7]);
    assert!(match_ranges("users", "id").is_empty());
    assert!(match_ranges("users", "").is_empty());
}

// A column found by its name doesn't light up its type too, as `user_id uuid` would for "id". One found by its type,
// like `metadata jsonb` for "json", has the type marked, since that's why it's listed.
#[test]
fn a_type_is_marked_only_when_the_name_did_not_match() {
    assert_eq!(type_needle("user_id", "id"), "");
    assert_eq!(type_needle("metadata", "json"), "json");
}

// `İ` lowercases to two characters, three bytes for two, so a range found in the lowercased name has to be mapped
// back onto the name as written.
#[test]
fn a_match_after_a_letter_that_lowercases_longer_still_lands_on_its_own_bytes() {
    assert_eq!(match_ranges("İd", "d"), vec![2..3]);
    assert_eq!(match_ranges("xİdİ", "i̇d"), vec![1..4]);
}

#[test]
fn search_matches_the_type_as_shown() {
    let column = ColumnInfo { name: "email".into(), data_type: "character varying(50)".into(), ..Default::default() };
    assert!(column_matches(&DriverType::Postgres, &column, "varchar"));
    assert!(column_matches(&DriverType::Postgres, &column, "character var"));
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

// The connection switcher connects on its own, then the sidebar hands the tree the connection it already shows.
#[gpui_kit::test]
fn a_connection_made_elsewhere_loads_when_the_sidebar_shows_it_again(cx: &mut TestAppContext) {
    let env = Env::new(cx);
    // Seeding left the connection open.
    env.runtime.block_on(env.bar.disconnect(&env.connection.id));
    let (tree, cx) = open(&env, cx);
    assert!(ids(&tree, cx).is_empty(), "nothing before connecting");
    env.runtime.block_on(env.bar.connect(&env.connection.id)).unwrap();
    let connection = env.connection.clone();
    tree.update(cx, |tree, cx| tree.set_connection(Some(connection), cx));
    settle(cx, has(&tree, "s:main"));
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
    let inserted = inserts(&tree, cx);
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

#[gpui_kit::test]
fn a_columns_tooltip_spells_out_what_its_row_leaves_out(cx: &mut TestAppContext) {
    let _env = Env::new(cx);
    let author = ColumnInfo {
        name: "author_id".into(),
        data_type: "uuid".into(),
        is_foreign: true,
        foreign_table: "users".into(),
        foreign_column: "id".into(),
        default_val: "gen_random_uuid()".into(),
        ..Default::default()
    };
    let id = ColumnInfo { name: "id".into(), is_primary: true, is_identity: true, ..Default::default() };
    let status = ColumnInfo {
        name: "status".into(),
        is_nullable: true,
        is_computed: true,
        enum_values: vec!["draft".into(), "live".into()],
        ..Default::default()
    };
    let many = ColumnInfo {
        name: "level".into(),
        is_nullable: true,
        enum_values: (1..=14).map(|n| format!("v{n}")).collect(),
        ..Default::default()
    };
    let plain = ColumnInfo { name: "note".into(), is_nullable: true, ..Default::default() };
    cx.update(|cx| {
        assert_eq!(column_details(&author, cx), ["→ users.id · not null · default gen_random_uuid()"]);
        assert_eq!(column_details(&id, cx), ["auto-generated"], "a key is never null, so that goes unsaid");
        assert_eq!(column_details(&status, cx), ["computed", "values: draft, live"]);
        assert_eq!(column_details(&many, cx), ["values: v1, v2, v3, v4, v5, v6, v7, v8, v9, v10, v11, v12, …"]);
        assert!(column_details(&plain, cx).is_empty());
    });
}

#[gpui_kit::test]
fn left_and_right_close_open_and_step_through_the_tree(cx: &mut TestAppContext) {
    let env = Env::new(cx);
    let (tree, cx) = open(&env, cx);
    connect(&tree, cx);
    tree.update_in(cx, |tree, window, cx| tree.focus.focus(window, cx));
    cx.run_until_parked();

    cx.simulate_keystrokes("down down");
    assert_eq!(cursor(&tree, cx).as_deref(), Some("t:main.things"));
    cx.simulate_keystrokes("right");
    settle(cx, has(&tree, "c:main.things.id"));
    assert_eq!(cursor(&tree, cx).as_deref(), Some("t:main.things"), "Right opens the table first");
    cx.simulate_keystrokes("right");
    assert_eq!(cursor(&tree, cx).as_deref(), Some("c:main.things.id"), "then steps onto its first column");
    cx.simulate_keystrokes("right");
    assert_eq!(cursor(&tree, cx).as_deref(), Some("c:main.things.id"), "a column has nothing to open");

    cx.simulate_keystrokes("left");
    assert_eq!(cursor(&tree, cx).as_deref(), Some("t:main.things"), "Left steps up to the parent");
    cx.simulate_keystrokes("left");
    cx.run_until_parked();
    assert!(!ids(&tree, cx).contains(&"c:main.things.id".to_string()), "then closes it");
    cx.simulate_keystrokes("left");
    assert_eq!(cursor(&tree, cx).as_deref(), Some("s:main"));
    cx.simulate_keystrokes("left");
    cx.run_until_parked();
    assert_eq!(ids(&tree, cx), ["s:main"], "and closes the schema");
}

// Other connections' rows stay open.
#[gpui_kit::test]
fn collapse_all_closes_the_tree_and_forgets_only_this_connections_rows(cx: &mut TestAppContext) {
    let env = Env::new(cx);
    cx.update(|cx| state::set_setting(cx, super::SCHEMA_EXPANDED_KEY, "{\"other:main\":true}"));
    let (tree, cx) = open(&env, cx);
    connect(&tree, cx);
    tree.update(cx, |tree, cx| tree.toggle_table("main", "things", cx));
    settle(cx, has(&tree, "c:main.things.id"));

    pick_option(0, cx);
    assert_eq!(ids(&tree, cx), ["s:main"]);
    let schemas = cx.update(|_, cx| state::setting(cx, super::SCHEMA_EXPANDED_KEY));
    assert_eq!(schemas.as_deref(), Some("{\"other:main\":true}"));
    let tables = cx.update(|_, cx| state::setting(cx, super::TABLES_EXPANDED_KEY));
    assert_eq!(tables.as_deref(), Some("{}"));
}

// Without changing what's remembered as open.
#[gpui_kit::test]
fn left_steps_up_from_a_row_the_search_keeps_open(cx: &mut TestAppContext) {
    let env = Env::new(cx);
    let id = env.connection.id.clone();
    env.runtime.block_on(env.bar.execute_query(&id, "CREATE TABLE others (id INTEGER, label TEXT)")).unwrap();
    let (tree, cx) = open(&env, cx);
    connect(&tree, cx);
    tree.update(cx, |tree, cx| tree.set_needle("label".into(), cx));
    settle(cx, has(&tree, "c:main.others.label"));
    let remembered = |cx: &mut VisualTestContext| {
        cx.update(|_, cx| [super::SCHEMA_EXPANDED_KEY, super::TABLES_EXPANDED_KEY].map(|key| state::setting(cx, key)))
    };
    let before = remembered(cx);
    tree.update_in(cx, |tree, window, cx| tree.focus.focus(window, cx));
    cx.run_until_parked();

    cx.simulate_keystrokes("down down left");
    assert_eq!(cursor(&tree, cx).as_deref(), Some("s:main"));
    cx.simulate_keystrokes("left");
    cx.run_until_parked();
    assert_eq!(ids(&tree, cx), ["s:main", "t:main.others", "c:main.others.label"]);
    assert_eq!(remembered(cx), before);
}

#[gpui_kit::test]
fn collapse_all_closes_the_tables_a_closed_schema_hides(cx: &mut TestAppContext) {
    let env = Env::new(cx);
    let (tree, cx) = open(&env, cx);
    connect(&tree, cx);
    tree.update(cx, |tree, cx| tree.toggle_table("main", "things", cx));
    settle(cx, has(&tree, "c:main.things.id"));
    tree.update(cx, |tree, cx| tree.toggle_schema("main", cx));
    cx.run_until_parked();
    assert_eq!(ids(&tree, cx), ["s:main"]);

    pick_option(0, cx);
    tree.update(cx, |tree, cx| tree.toggle_schema("main", cx));
    settle(cx, has(&tree, "t:main.things"));
    assert!(!ids(&tree, cx).contains(&"c:main.things.id".to_string()), "the table stays closed");
}

#[gpui_kit::test]
fn a_click_selects_a_column_and_its_menu_inserts_the_name(cx: &mut TestAppContext) {
    let env = Env::new(cx);
    let (tree, cx) = open(&env, cx);
    connect(&tree, cx);
    tree.update(cx, |tree, cx| tree.toggle_table("main", "things", cx));
    settle(cx, has(&tree, "c:main.things.name"));
    let inserted = inserts(&tree, cx);
    let row = cx.debug_bounds("tree:c:main.things.name").expect("the column's row").center();

    cx.simulate_click(row, Modifiers::none());
    cx.run_until_parked();
    assert_eq!(cursor(&tree, cx).as_deref(), Some("c:main.things.name"));
    assert!(inserted.borrow().is_empty(), "{:?}", inserted.borrow());

    let (button, modifiers) = (MouseButton::Right, Modifiers::none());
    cx.simulate_event(MouseDownEvent { button, position: row, modifiers, click_count: 1, first_mouse: false });
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    cx.simulate_keystrokes("down enter");
    cx.run_until_parked();
    assert_eq!(*inserted.borrow(), ["name"]);
}
