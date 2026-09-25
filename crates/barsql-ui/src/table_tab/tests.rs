use std::cell::RefCell;
use std::rc::Rc;

use barsql_app::{EditorTab, TableViewRef};
use barsql_core::Value;
use gpui_kit::component::Root;
use gpui_kit::{AppContext as _, Entity, TestAppContext, VisualTestContext};

use super::{TableTab, TableTabEvent};
use crate::grid::GridEvent;
use crate::query_tab::new_tab_id;
use crate::test_support::{Env, settle};

fn seed(env: &Env, sql: &str) {
    for statement in sql.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        env.runtime.block_on(env.bar.execute_query(&env.connection.id, statement)).unwrap();
    }
}

fn query(env: &Env, sql: &str) -> Vec<Vec<Value>> {
    env.runtime.block_on(env.bar.execute_query(&env.connection.id, sql)).unwrap().rows
}

fn open<'a>(env: &Env, cx: &'a mut TestAppContext, table: &str) -> (Entity<TableTab>, &'a mut VisualTestContext) {
    let tab = EditorTab {
        id: new_tab_id(),
        connection_id: env.connection.id.clone(),
        title: table.into(),
        table_view: Some(TableViewRef { schema: "main".into(), table: table.into(), ..Default::default() }),
        ..Default::default()
    };
    let connection = env.connection.clone();
    let slot = Rc::new(RefCell::new(None));
    let out = slot.clone();
    let window = cx.add_window(move |window, cx| {
        let view = cx.new(|cx| TableTab::new(tab, connection, window, cx));
        *out.borrow_mut() = Some(view.clone());
        Root::new(view, window, cx)
    });
    let cx = VisualTestContext::from_window(window.into(), cx).into_mut();
    let view = slot.borrow_mut().take().unwrap();
    settle(cx, idle(&view));
    (view, cx)
}

fn idle(view: &Entity<TableTab>) -> impl FnMut(&mut VisualTestContext) -> bool + '_ {
    move |cx| cx.update(|_, cx| view.read(cx).loading.is_none())
}

fn rows(view: &Entity<TableTab>, cx: &mut VisualTestContext) -> usize {
    cx.update(|_, cx| view.read(cx).grid.read(cx).set().rows())
}

fn cell(view: &Entity<TableTab>, row: usize, column: usize, cx: &mut VisualTestContext) -> Option<String> {
    cx.update(|_, cx| view.read(cx).grid.read(cx).shown(row, column).display().map(str::to_string))
}

fn grid_event(view: &Entity<TableTab>, event: GridEvent, cx: &mut VisualTestContext) {
    let grid = cx.update(|_, cx| view.read(cx).grid.clone());
    grid.update(cx, |_, cx| cx.emit(event));
    cx.run_until_parked();
}

const PEOPLE: &str = "CREATE TABLE people (id INTEGER PRIMARY KEY, name TEXT, note TEXT);
    INSERT INTO people VALUES (1, 'ann', 'a'), (2, 'bob', NULL), (3, 'cy', 'c')";

#[gpui_kit::test]
fn tables_load_a_page_then_more_on_request(cx: &mut TestAppContext) {
    let env = Env::new(cx);
    let (view, cx) = open(&env, cx, "things");
    assert_eq!(rows(&view, cx), 100);
    assert!(cx.update(|_, cx| view.read(cx).has_more));
    assert!(!cx.update(|_, cx| view.read(cx).editable()), "no primary key, no edits");
    grid_event(&view, GridEvent::LoadMore, cx);
    settle(cx, idle(&view));
    assert_eq!(rows(&view, cx), 200);
    assert_eq!(cell(&view, 150, 0, cx).as_deref(), Some("151"));
}

#[gpui_kit::test]
fn filters_and_sorts_reload_from_the_server(cx: &mut TestAppContext) {
    let env = Env::new(cx);
    let (view, cx) = open(&env, cx, "things");
    view.update_in(cx, |view, window, cx| {
        view.filter.update(cx, |state, cx| state.set_value("id > 2990", window, cx));
        view.apply_filter(window, cx);
    });
    settle(cx, idle(&view));
    assert_eq!(rows(&view, cx), 10);
    grid_event(&view, GridEvent::SortRequested(0), cx);
    settle(cx, idle(&view));
    assert_eq!(cell(&view, 0, 0, cx).as_deref(), Some("2991"), "the first sort of a column is ascending");
    grid_event(&view, GridEvent::SortRequested(0), cx);
    settle(cx, idle(&view));
    assert_eq!(cell(&view, 0, 0, cx).as_deref(), Some("3000"));
    let stored = cx.update(|_, cx| view.read(cx).stored(cx).table_view.unwrap());
    assert_eq!(
        (stored.filter.as_str(), stored.order_by.as_str(), stored.order_dir.as_str()),
        ("id > 2990", "id", "DESC")
    );
}

#[gpui_kit::test]
fn staged_edits_show_then_apply_by_primary_key(cx: &mut TestAppContext) {
    let env = Env::new(cx);
    seed(&env, PEOPLE);
    let (view, cx) = open(&env, cx, "people");
    assert!(cx.update(|_, cx| view.read(cx).editable()));
    grid_event(&view, GridEvent::EditCell { row: 0, column: 1, value: Some("anna".into()) }, cx);
    grid_event(&view, GridEvent::EditCell { row: 1, column: 2, value: Some("b".into()) }, cx);
    assert_eq!(cell(&view, 0, 1, cx).as_deref(), Some("anna"), "the grid shows the staged value");
    assert_eq!(cx.update(|_, cx| view.read(cx).staging.edit_count()), 2);
    assert_eq!(
        query(&env, "SELECT name FROM people WHERE id = 1")[0][0],
        Value::Text("ann".into()),
        "nothing applied yet"
    );
    grid_event(&view, GridEvent::Apply, cx);
    settle(cx, |cx| cx.update(|_, cx| !view.read(cx).applying && view.read(cx).loading.is_none()));
    assert_eq!(
        query(&env, "SELECT name, note FROM people WHERE id IN (1, 2) ORDER BY id"),
        [
            vec![Value::Text("anna".into()), Value::Text("a".into())],
            vec![Value::Text("bob".into()), Value::Text("b".into())],
        ]
    );
    assert!(!cx.update(|_, cx| view.read(cx).staging.has_pending()));
    assert_eq!(cell(&view, 0, 1, cx).as_deref(), Some("anna"), "reloaded from the database");
}

#[gpui_kit::test]
fn deletes_undo_redo_and_apply(cx: &mut TestAppContext) {
    let env = Env::new(cx);
    seed(&env, PEOPLE);
    let (view, cx) = open(&env, cx, "people");
    grid_event(&view, GridEvent::ToggleDelete(2), cx);
    let deleted = |cx: &mut VisualTestContext| cx.update(|_, cx| view.read(cx).grid.read(cx).is_deleted(2));
    assert!(deleted(cx));
    grid_event(&view, GridEvent::Undo, cx);
    assert!(!deleted(cx));
    grid_event(&view, GridEvent::Redo, cx);
    assert!(deleted(cx));
    grid_event(&view, GridEvent::Apply, cx);
    settle(cx, |cx| cx.update(|_, cx| !view.read(cx).applying && view.read(cx).loading.is_none()));
    assert_eq!(query(&env, "SELECT count(*) FROM people")[0][0], Value::Int(2));
    assert_eq!(rows(&view, cx), 2);
}

#[gpui_kit::test]
fn pasted_cells_stage_as_one_step(cx: &mut TestAppContext) {
    let env = Env::new(cx);
    seed(&env, PEOPLE);
    let (view, cx) = open(&env, cx, "people");
    let cells = vec![(0, 2, Some("x".into())), (1, 2, None), (2, 2, Some("z".into()))];
    grid_event(&view, GridEvent::PasteCells(cells), cx);
    assert_eq!([0, 1, 2].map(|row| cell(&view, row, 2, cx)), [Some("x".into()), None, Some("z".into())]);
    assert_eq!(cx.update(|_, cx| view.read(cx).staging.edit_count()), 2, "row 2's NULL was already NULL");
    grid_event(&view, GridEvent::Undo, cx);
    assert_eq!(cx.update(|_, cx| view.read(cx).staging.edit_count()), 0);
}

#[gpui_kit::test]
fn foreign_keys_open_the_referenced_row(cx: &mut TestAppContext) {
    let env = Env::new(cx);
    seed(
        &env,
        "CREATE TABLE authors (id INTEGER PRIMARY KEY, name TEXT);
         CREATE TABLE books (id INTEGER PRIMARY KEY, author_id INTEGER REFERENCES authors(id), editor INTEGER REFERENCES authors);
         INSERT INTO authors VALUES (7, 'ann');
         INSERT INTO books VALUES (1, 7, 7)",
    );
    let (view, cx) = open(&env, cx, "books");
    settle(cx, |cx| cx.update(|_, cx| view.read(cx).foreign.len() == 2));
    let opened = Rc::new(RefCell::new(Vec::new()));
    let sink = opened.clone();
    cx.update(|_, cx| {
        cx.subscribe(&view, move |_, event: &TableTabEvent, _| {
            if let TableTabEvent::OpenTable { table, filter, .. } = event {
                sink.borrow_mut().push((table.clone(), filter.clone()));
            }
        })
        .detach()
    });
    grid_event(&view, GridEvent::OpenForeignKey { row: 0, column: 1 }, cx);
    grid_event(&view, GridEvent::OpenForeignKey { row: 0, column: 2 }, cx);
    settle(cx, |_| opened.borrow().len() == 2);
    assert_eq!(*opened.borrow(), [("authors".to_string(), "id = 7".to_string()), ("authors".into(), "id = 7".into())]);
}

#[gpui_kit::test]
fn the_filter_completes_columns_then_where_keywords(cx: &mut TestAppContext) {
    use futures_util::FutureExt as _;
    use gpui_kit::component::Rope;
    use lsp_types::{CompletionContext, CompletionResponse, CompletionTextEdit, CompletionTriggerKind};

    let env = Env::new(cx);
    seed(&env, "CREATE TABLE odd (id INTEGER PRIMARY KEY, \"user name\" TEXT, note TEXT)");
    let (view, cx) = open(&env, cx, "odd");
    let provider = cx.update(|_, cx| view.read(cx).completion.read(cx).provider()).unwrap();
    let complete = |text: &str, cx: &mut VisualTestContext| {
        let trigger = CompletionContext { trigger_kind: CompletionTriggerKind::INVOKED, trigger_character: None };
        let task = cx.update(|window, cx| provider.completions(&Rope::from(text), text.len(), trigger, window, cx));
        let Ok(CompletionResponse::Array(items)) = task.now_or_never().unwrap() else { panic!("no items") };
        items
            .into_iter()
            .map(|item| match item.text_edit {
                Some(CompletionTextEdit::Edit(edit)) => (item.label, edit.new_text),
                _ => (item.label, String::new()),
            })
            .collect::<Vec<_>>()
    };
    let all = complete("", cx);
    assert_eq!(all[..3].iter().map(|(label, _)| label.as_str()).collect::<Vec<_>>(), ["id", "note", "user name"]);
    assert_eq!(all[2].1, "\"user name\"", "inserted as a quoted identifier");
    assert_eq!(all[3].0, "AND");
    let matches = complete("id > 1 AND no", cx);
    assert_eq!(matches.iter().map(|(label, _)| label.as_str()).collect::<Vec<_>>(), ["note", "NOT", "IS NOT NULL"]);
}

// Checks that the editor keeps the focus the double-click gave it.
#[gpui_kit::test]
fn a_double_clicked_cell_takes_typing_and_enter_stages_it(cx: &mut TestAppContext) {
    let env = Env::new(cx);
    seed(&env, PEOPLE);
    let (view, cx) = open(&env, cx, "people");
    let grid = cx.update(|_, cx| view.read(cx).grid.clone());
    cx.update(|window, cx| window.draw(cx).clear(cx));
    let at = cx.update(|_, cx| grid.read(cx).cell_point(0, 1));
    for click_count in [1, 2] {
        let (button, modifiers) = (gpui_kit::MouseButton::Left, Default::default());
        cx.simulate_event(gpui_kit::MouseDownEvent {
            button,
            position: at,
            modifiers,
            click_count,
            first_mouse: false,
        });
        cx.simulate_event(gpui_kit::MouseUpEvent { button, position: at, modifiers, click_count });
    }
    cx.update(|window, cx| window.draw(cx).clear(cx));
    cx.simulate_keystrokes(if cfg!(target_os = "macos") { "cmd-a" } else { "ctrl-a" });
    cx.simulate_input("anna");
    cx.update(|window, cx| window.draw(cx).clear(cx));
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(cx.update(|_, cx| view.read(cx).staging.edit_count()), 1);
    assert_eq!(cell(&view, 0, 1, cx).as_deref(), Some("anna"));
}

#[gpui_kit::test]
fn the_filter_placeholder_follows_the_language(cx: &mut TestAppContext) {
    let env = Env::new(cx);
    seed(&env, PEOPLE);
    let (view, cx) = open(&env, cx, "people");
    let placeholder = |cx: &mut VisualTestContext| {
        cx.update(|_, cx| view.read(cx).filter.read(cx).presentation().placeholder().to_string())
    };
    let english = placeholder(cx);
    cx.update(|_, cx| cx.set_global(crate::i18n::I18n::new("de")));
    cx.run_until_parked();
    let german = cx.update(|_, cx| crate::i18n::t(cx, "tableView.filterPlaceholder").to_string());
    assert_ne!(english, german);
    assert_eq!(placeholder(cx), german);
}
