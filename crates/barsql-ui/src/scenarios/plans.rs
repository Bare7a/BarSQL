// SQLite only. The Postgres cases (analyze, formats) run in the e2e engine scenarios.
use gpui_kit::TestAppContext;

use super::driver::{Driver, open, selector};
use crate::i18n::t;

const SEED: &str = "CREATE TABLE plan_t (id INTEGER PRIMARY KEY, name VARCHAR(50));
    INSERT INTO plan_t (id, name) VALUES (1, 'Alice'), (2, 'Bob')";

fn explain(app: &mut Driver, sql: &str) {
    app.set_sql(sql);
    app.click("explain");
    app.wait_idle();
}

fn rows(app: &mut Driver) -> Vec<(String, String, f64)> {
    let plan = app.plan().expect("the plan view");
    app.cx.update(|_, cx| plan.read(cx).rows())
}

#[gpui_kit::test]
fn explain_shows_an_estimated_plan_with_sqlites_note(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.seed(SEED);
    app.connect();
    explain(&mut app, "SELECT * FROM plan_t WHERE id = 1;");
    let plan = app.plan().expect("the plan view");
    let (analyzed, notes) = app.cx.update(|_, cx| (plan.read(cx).analyzed(), plan.read(cx).notes()));
    assert!(!analyzed, "the badge says Estimated");
    assert_eq!(notes, ["noMetrics"]);
    assert!(rows(&mut app).iter().all(|(_, label, _)| !label.is_empty()));
}

#[gpui_kit::test]
fn the_selected_node_shows_its_details_and_the_raw_plan(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.seed(SEED);
    app.connect();
    explain(&mut app, "SELECT * FROM plan_t WHERE id = 1;");
    app.click("plan-row-0");
    assert!(app.shown("plan-details"));
    app.click("plan-raw");
    assert!(app.shown("plan-raw-text"));
    let plan = app.plan().unwrap();
    let raw = app.cx.update(|_, cx| plan.read(cx).raw_text()).expect("the raw plan");
    assert!(raw.contains("plan_t"), "{raw}");
}

#[gpui_kit::test]
fn a_nested_plan_collapses_and_expands(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.seed(SEED);
    app.connect();
    explain(&mut app, "SELECT * FROM plan_t WHERE id IN (SELECT id FROM plan_t WHERE name = 'Bob');");
    let before = rows(&mut app);
    let parent = before
        .iter()
        .map(|(key, ..)| key.clone())
        .find(|key| before.iter().any(|(other, ..)| other.starts_with(&format!("{key}."))))
        .expect("a node with children");
    let twisty = selector(format!("plan-twisty-{parent}"));
    app.click(twisty);
    let collapsed = rows(&mut app);
    assert!(collapsed.len() < before.len());
    assert!(collapsed.iter().all(|(key, ..)| !key.starts_with(&format!("{parent}."))));
    app.click(twisty);
    assert_eq!(rows(&mut app).len(), before.len());
}

#[gpui_kit::test]
fn running_a_query_replaces_the_plan_with_results(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.seed(SEED);
    app.connect();
    explain(&mut app, "SELECT id FROM plan_t WHERE id = 1;");
    assert!(app.plan().is_some() && app.grid().is_none());
    app.dispatch(crate::actions::RunAll);
    app.wait_idle();
    assert!(app.plan().is_none());
    assert_eq!(app.cell(0, 0).as_deref(), Some("1"));
}

#[gpui_kit::test]
fn a_failed_explain_reports_the_error_instead_of_a_plan(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.connect();
    explain(&mut app, "SELECT * FROM definitely_missing_table_e2e;");
    assert!(app.status_is_error());
    assert!(app.plan().is_none());
    assert!(app.error().is_some());
}

#[gpui_kit::test]
fn a_typed_explain_query_plan_shows_the_plan_viewer(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.seed(SEED);
    app.connect();
    app.run("EXPLAIN QUERY PLAN SELECT * FROM plan_t WHERE id = 1;");
    assert!(app.grid().is_none());
    assert!(!rows(&mut app).is_empty());
}

#[gpui_kit::test]
fn a_script_mixes_grids_and_plans_across_result_tabs(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.seed(SEED);
    app.connect();
    app.run("SELECT * FROM plan_t; EXPLAIN QUERY PLAN SELECT * FROM plan_t; SELECT COUNT(*) FROM plan_t;");
    assert_eq!(app.result_tabs(), 3);
    app.click("result-tab-0");
    assert!(app.grid().is_some() && app.plan().is_none());
    app.click("result-tab-1");
    assert!(app.plan().is_some() && app.grid().is_none());
    assert!(!rows(&mut app).is_empty());
    app.click("result-tab-2");
    assert!(app.grid().is_some() && app.plan().is_none());
}

#[gpui_kit::test]
fn sqlite_offers_no_measured_plan(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.seed(SEED);
    app.connect();
    explain(&mut app, "SELECT * FROM plan_t WHERE id = 1;");
    let plan = app.plan().unwrap();
    assert!(app.cx.update(|_, cx| plan.read(cx).metrics()).is_empty());
    let note = app.cx.update(|_, cx| t(cx, "results.planNote.noMetrics"));
    assert!(note.contains("SQLite"), "{note}");
    app.keys("mod-shift-a");
    app.wait_idle();
    let plan = app.plan().unwrap();
    assert!(!app.cx.update(|_, cx| plan.read(cx).analyzed()), "the shortcut keeps the estimate");
}

#[gpui_kit::test]
fn a_bare_sqlite_explain_keeps_its_bytecode_rows(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.seed(SEED);
    app.connect();
    app.run("EXPLAIN SELECT * FROM plan_t;");
    assert!(app.plan().is_none());
    assert!(app.cell(0, 0).is_some(), "the opcode rows");
}

#[gpui_kit::test]
fn a_plan_rows_menu_collapses_and_expands_the_whole_tree(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.seed(SEED);
    app.connect();
    explain(&mut app, "SELECT * FROM plan_t WHERE id IN (SELECT id FROM plan_t WHERE name = 'Bob');");
    let before = rows(&mut app).len();
    // Collapse node, Expand all, then Collapse all.
    app.context_menu("plan-row-0", 2);
    let collapsed = rows(&mut app);
    assert!(collapsed.len() < before && collapsed.iter().all(|(key, ..)| !key.contains('.')), "only the top nodes");
    // Expand node, then Expand all.
    app.context_menu("plan-row-0", 1);
    assert_eq!(rows(&mut app).len(), before);
}
