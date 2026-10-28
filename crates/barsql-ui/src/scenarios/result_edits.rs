use barsql_core::{DriverType, Value};
use barsql_sql::lang::TxnControl;
use gpui_kit::component::WindowExt as _;
use gpui_kit::{Entity, Modifiers, TestAppContext};

use super::driver::{Driver, open};
use crate::actions::CloseTab;
use crate::grid::Grid;
use crate::query_tab::TxnState;

const PEOPLE: &str = "CREATE TABLE e2e_qe (id INTEGER PRIMARY KEY, name VARCHAR(50));
    INSERT INTO e2e_qe VALUES (1, 'Alice'), (2, 'Bob')";

// Runs `sql` and waits until its result turns out to be editable.
fn editable<'a>(cx: &'a mut TestAppContext, sql: &str) -> Driver<'a> {
    let mut app = open(cx);
    app.seed(PEOPLE);
    app.connect();
    app.run(sql);
    let results = app.results();
    app.settle(|cx| cx.update(|_, cx| results.read(cx).editing_table().is_some()));
    app
}

fn grid(app: &mut Driver) -> Entity<Grid> {
    app.grid().expect("a grid")
}

fn edit_cell(app: &mut Driver, row: usize, col: usize, text: &str) {
    let grid = grid(app);
    let at = app.grid_point(&grid, |grid| grid.cell_point(row, col));
    app.double_click_at(at);
    app.keys("mod-a");
    app.type_text(text);
    app.keys("enter");
}

fn pending(app: &mut Driver) -> usize {
    let results = app.results();
    app.cx.update(|_, cx| results.read(cx).pending_edits())
}

fn apply(app: &mut Driver) {
    app.click("result-apply");
    let results = app.results();
    app.settle(|cx| cx.update(|_, cx| results.read(cx).pending_edits() == 0));
}

fn click_cell(app: &mut Driver, row: usize, col: usize) {
    let grid = grid(app);
    let at = app.grid_point(&grid, |grid| grid.cell_point(row, col));
    app.click_at(at, Modifiers::none());
}

#[gpui_kit::test]
fn an_edited_query_result_saves_to_its_table(cx: &mut TestAppContext) {
    let mut app = editable(cx, "SELECT id, name AS n, upper(name) AS shout FROM e2e_qe ORDER BY id");
    let results = app.results();
    assert_eq!(app.cx.update(|_, cx| results.read(cx).editing_table()), Some("e2e_qe".into()));
    edit_cell(&mut app, 0, 1, "Alicia");
    assert_eq!((app.cell(0, 1).as_deref(), pending(&mut app)), (Some("Alicia"), 1));
    click_cell(&mut app, 1, 2);
    app.keys("mod-backspace");
    assert_eq!((app.cell(1, 2).as_deref(), pending(&mut app)), (Some("BOB"), 1), "an expression isn't edited");
    apply(&mut app);
    assert_eq!(pending(&mut app), 0);
    assert_eq!(app.cell(0, 1).as_deref(), Some("Alicia"), "the grid keeps what was written");
    assert_eq!(app.cell(0, 2).as_deref(), Some("ALICE"), "until the query runs again");
    assert_eq!(app.query("SELECT name FROM e2e_qe WHERE id = 1")[0][0], Value::Text("Alicia".into()));
}

#[gpui_kit::test]
fn a_row_marked_for_delete_leaves_the_result_and_the_table(cx: &mut TestAppContext) {
    let mut app = editable(cx, "SELECT * FROM e2e_qe ORDER BY id");
    click_cell(&mut app, 0, 1);
    app.keys("delete");
    assert_eq!(pending(&mut app), 1);
    apply(&mut app);
    let grid = grid(&mut app);
    assert_eq!((app.cell(0, 1).as_deref(), app.cx.update(|_, cx| grid.read(cx).rows_shown())), (Some("Bob"), 1));
    assert_eq!(app.query("SELECT count(*) FROM e2e_qe")[0][0], Value::Int(1));
}

#[gpui_kit::test]
fn running_again_asks_before_dropping_result_edits(cx: &mut TestAppContext) {
    let mut app = editable(cx, "SELECT * FROM e2e_qe ORDER BY id");
    edit_cell(&mut app, 0, 1, "Alicia");
    app.run("SELECT * FROM e2e_qe ORDER BY id");
    assert!(app.dialog_open(), "the run waits for an answer");
    assert_eq!(app.asked().0, "Discard unsaved changes?");
    app.confirm_dialog();
    app.wait_idle();
    assert_eq!((app.cell(0, 1).as_deref(), pending(&mut app)), (Some("Alice"), 0));
    assert_eq!(app.query("SELECT name FROM e2e_qe WHERE id = 1")[0][0], Value::Text("Alice".into()));
}

#[gpui_kit::test]
fn edits_made_in_an_open_transaction_roll_back_with_it(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.seed(PEOPLE);
    let tab = app.connect();
    app.run("BEGIN");
    app.run("SELECT * FROM e2e_qe ORDER BY id");
    let results = app.results();
    app.settle(|cx| cx.update(|_, cx| results.read(cx).editing_table().is_some()));
    edit_cell(&mut app, 0, 1, "Alicia");
    apply(&mut app);
    assert_eq!(app.cell(0, 1).as_deref(), Some("Alicia"));
    assert_eq!(app.cx.update(|_, cx| tab.read(cx).txn_state()), TxnState::Active, "the transaction stays open");
    app.run("ROLLBACK");
    assert_eq!(app.query("SELECT name FROM e2e_qe WHERE id = 1")[0][0], Value::Text("Alice".into()));
}

// The Dock's Quit can't read the tabs, so it goes by what the workspace last noted.
#[cfg(target_os = "macos")]
#[gpui_kit::test]
fn the_docks_quit_asks_before_dropping_result_edits(cx: &mut TestAppContext) {
    use crate::system_quit::{Reply, should_terminate};

    let mut app = editable(cx, "SELECT * FROM e2e_qe ORDER BY id");
    let (bar, system) = (app.env.bar.clone(), app.cx.update(|_, cx| cx.to_async()));
    assert_eq!(should_terminate(&bar, &system), Reply::Now);
    edit_cell(&mut app, 0, 1, "Alicia");
    assert_eq!(should_terminate(&bar, &system), Reply::Cancel);
    app.cx.run_until_parked();
    assert_eq!(app.asked().0, "Discard unapplied changes?");
    app.keys("escape");
    apply(&mut app);
    assert_eq!(should_terminate(&bar, &system), Reply::Now, "nothing left to lose");

    edit_cell(&mut app, 0, 1, "Ally");
    app.dispatch(CloseTab);
    app.confirm_dialog();
    assert_eq!(should_terminate(&bar, &system), Reply::Now, "the tab that held them is gone");
}

// With edits staged, a toolbar button's failure shows in a dialog, so the results keep them.
#[gpui_kit::test]
fn a_failed_toolbar_commit_keeps_result_edits(cx: &mut TestAppContext) {
    let mut app = editable(cx, "SELECT * FROM e2e_qe ORDER BY id");
    edit_cell(&mut app, 0, 1, "Alicia");
    let tab = app.tab();
    app.cx.update(|window, cx| tab.update(cx, |tab, cx| tab.transaction(TxnControl::Commit, false, window, cx)));
    app.settle(|cx| cx.update(|window, cx| window.has_active_dialog(cx)));
    assert_eq!((app.cell(0, 1).as_deref(), pending(&mut app)), (Some("Alicia"), 1));
}

// The tab keeps its driver, but its results can't keep edits meant for a server the connection no longer names.
#[gpui_kit::test]
fn a_connection_moved_to_another_driver_drops_result_edits(cx: &mut TestAppContext) {
    let mut app = editable(cx, "SELECT * FROM e2e_qe ORDER BY id");
    edit_cell(&mut app, 0, 1, "Alicia");
    let tab = app.tab();
    let moved = app.cx.update(|_, cx| tab.read(cx).connection.clone());
    let moved = barsql_core::ConnectionConfig { driver: DriverType::Postgres, host: "elsewhere".into(), ..moved };
    app.cx.update(|_, cx| tab.update(cx, |tab, cx| tab.set_connection(moved, cx)));
    let results = app.results();
    assert_eq!(app.cx.update(|_, cx| results.read(cx).editing_table()), None);
    assert_eq!(pending(&mut app), 0);
}

#[gpui_kit::test]
fn closing_a_tab_with_result_edits_asks_first(cx: &mut TestAppContext) {
    let mut app = editable(cx, "SELECT * FROM e2e_qe ORDER BY id");
    let tabs = app.titles().len();
    edit_cell(&mut app, 0, 1, "Alicia");
    app.dispatch(CloseTab);
    assert!(app.dialog_open());
    assert_eq!(app.asked().0, "Discard unapplied changes?");
    app.confirm_dialog();
    assert_eq!(app.titles().len(), tabs - 1);
    assert_eq!(app.query("SELECT name FROM e2e_qe WHERE id = 1")[0][0], Value::Text("Alice".into()));
}
