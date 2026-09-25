use barsql_app::EditorTab;
use gpui_kit::{Entity, Modifiers, MouseButton, TestAppContext, VisualTestContext, point, px};

use super::{QueryTab, TxnState, char_offset, new_tab_id, results_share, word_range};
use crate::results::ResultStatus;
use crate::schema;
use crate::test_support::{Env, settle};

fn open<'a>(env: &Env, cx: &'a mut TestAppContext, sql: &str) -> (Entity<QueryTab>, &'a mut VisualTestContext) {
    let tab =
        EditorTab { id: new_tab_id(), connection_id: env.connection.id.clone(), sql: sql.into(), ..Default::default() };
    let connection = env.connection.clone();
    let (tab, cx) = cx.add_window_view(|window, cx| QueryTab::new(tab, connection, window, cx));
    cx.update(|window, cx| tab.update(cx, |tab, cx| tab.focus_editor(window, cx)));
    (tab, cx)
}

#[test]
fn positions_count_characters_not_utf16_units() {
    let stmt = "SELECT 'héllo 🌍' AS v, nope";
    let offset = char_offset(stmt, 24).unwrap();
    assert_eq!(&stmt[offset..], "nope");
    assert_eq!(char_offset("SELECT 1 WHERE", 15), Some(14));
    assert_eq!(char_offset("SELECT", 8), None);
    assert_eq!(char_offset("SELECT", 0), None);
}

#[test]
fn the_squiggle_covers_the_word_at_the_position() {
    let text = "SELECT nope, x FROM t";
    assert_eq!(&text[word_range(text, 7)], "nope");
    assert_eq!(&text[word_range(text, 11)], "nope");
    assert_eq!(&text[word_range(text, 12)], " ");
    assert_eq!(word_range(text, text.len()), 20..21);
    assert_eq!(word_range("a; ", 3), 3..3);
}

fn idle(tab: &Entity<QueryTab>) -> impl FnMut(&mut VisualTestContext) -> bool + '_ {
    move |cx| cx.update(|_, cx| tab.read(cx).running.is_none())
}

fn status(tab: &Entity<QueryTab>, cx: &mut VisualTestContext) -> (usize, ResultStatus) {
    cx.update(|_, cx| {
        let results = tab.read(cx).results.read(cx);
        (results.result_count(), results.status(cx))
    })
}

fn text(tab: &Entity<QueryTab>, cx: &mut VisualTestContext) -> String {
    cx.update(|_, cx| tab.read(cx).sql(cx).to_string())
}

fn set_text(tab: &Entity<QueryTab>, sql: &str, cx: &mut VisualTestContext) {
    cx.update(|window, cx| {
        let editor = tab.read(cx).editor.clone();
        editor.update(cx, |state, cx| state.set_value(sql.to_string(), window, cx));
    });
}

fn modifier() -> &'static str {
    if cfg!(target_os = "macos") { "cmd" } else { "ctrl" }
}

#[gpui_kit::test]
fn run_all_streams_every_result_set(cx: &mut TestAppContext) {
    let env = Env::new(cx);
    let (tab, cx) = open(&env, cx, "SELECT * FROM things;\nSELECT count(*) FROM things;");
    tab.update_in(cx, |tab, window, cx| tab.run_all(window, cx));
    settle(cx, idle(&tab));
    let (sets, first) = status(&tab, cx);
    assert_eq!(sets, 2);
    assert!(matches!(first, ResultStatus::Rows { count: 3000, .. }), "{:?}", matches!(first, ResultStatus::Empty));
    let label = cx.update(|_, cx| tab.read(cx).status(cx).text);
    assert!(label.starts_with("3000 row(s)"), "{label}");
}

#[gpui_kit::test]
fn a_failing_statement_shows_its_error(cx: &mut TestAppContext) {
    let env = Env::new(cx);
    let (tab, cx) = open(&env, cx, "SELECT nope FROM things");
    tab.update_in(cx, |tab, window, cx| tab.run_all(window, cx));
    settle(cx, idle(&tab));
    let (_, status) = status(&tab, cx);
    let ResultStatus::Error(message) = status else { panic!("expected an error") };
    assert!(message.contains("nope"), "{message}");
    assert!(cx.update(|_, cx| tab.read(cx).status(cx).error));
}

#[gpui_kit::test]
fn lone_transaction_statements_drive_the_tab_transaction(cx: &mut TestAppContext) {
    let env = Env::new(cx);
    let (tab, cx) = open(&env, cx, "BEGIN;");
    tab.update_in(cx, |tab, window, cx| tab.run_all(window, cx));
    settle(cx, |cx| cx.update(|_, cx| tab.read(cx).txn == TxnState::Active));
    let tab_id = cx.update(|_, cx| tab.read(cx).id.clone());
    assert!(env.bar.transaction_status(&tab_id));
    let (_, status) = status(&tab, cx);
    assert!(matches!(status, ResultStatus::Affected { count: 0, .. }));

    set_text(&tab, "DELETE FROM things WHERE id > 10", cx);
    tab.update_in(cx, |tab, window, cx| tab.run_all(window, cx));
    settle(cx, idle(&tab));
    set_text(&tab, "ROLLBACK", cx);
    tab.update_in(cx, |tab, window, cx| tab.run_all(window, cx));
    settle(cx, |cx| cx.update(|_, cx| tab.read(cx).txn == TxnState::Idle));
    assert!(!env.bar.transaction_status(&tab_id));
    let count = env.runtime.block_on(env.bar.execute_query(&env.connection.id, "SELECT count(*) FROM things")).unwrap();
    assert_eq!(count.rows[0][0], barsql_core::Value::Int(3000), "the rollback kept every row");
}

#[gpui_kit::test]
fn mod_enter_runs_the_selection_without_typing_a_newline(cx: &mut TestAppContext) {
    let env = Env::new(cx);
    let (tab, cx) = open(&env, cx, "SELECT id FROM things WHERE id <= 5");
    let before = text(&tab, cx);
    cx.simulate_keystrokes(&format!("{}-enter", modifier()));
    cx.run_until_parked();
    assert_eq!(text(&tab, cx), before, "no newline and no run without a selection");
    assert_eq!(status(&tab, cx).0, 0);

    cx.simulate_keystrokes(&format!("{m}-a {m}-enter", m = modifier()));
    settle(cx, |cx| status(&tab, cx).0 == 1 && cx.update(|_, cx| tab.read(cx).running.is_none()));
    assert_eq!(text(&tab, cx), before);
    assert!(matches!(status(&tab, cx).1, ResultStatus::Rows { count: 5, .. }));
}

#[gpui_kit::test]
fn completion_offers_tables_then_their_columns(cx: &mut TestAppContext) {
    let env = Env::new(cx);
    let (tab, cx) = open(&env, cx, "");
    let connection_id = env.connection.id.clone();
    settle(cx, |cx| cx.update(|_, cx| schema::catalog(cx, &connection_id).is_some()));
    let labels = |cx: &mut VisualTestContext| cx.update(|_, cx| tab.read(cx).completion.read(cx).labels(cx));
    cx.simulate_input("SELECT * FROM th");
    settle(cx, |cx| labels(cx).iter().any(|l| l == "things"));

    cx.simulate_input("ings WHERE na");
    settle(cx, |cx| labels(cx).iter().any(|l| l == "name"));
}

#[gpui_kit::test]
fn unknown_tables_are_flagged_once_the_schema_loads(cx: &mut TestAppContext) {
    let env = Env::new(cx);
    let (tab, cx) = open(&env, cx, "SELECT * FROM things;\nSELECT * FROM nothing_here;");
    let connection_id = env.connection.id.clone();
    settle(cx, |cx| cx.update(|_, cx| schema::catalog(cx, &connection_id).is_some()));
    cx.executor().advance_clock(super::DIAGNOSTICS_DEBOUNCE);
    cx.run_until_parked();
    let messages = cx.update(|_, cx| {
        let editor = tab.read(cx).editor.clone();
        editor.update(cx, |state, _| {
            state.diagnostics_mut().map(|set| set.iter().map(|d| d.message.to_string()).collect::<Vec<_>>())
        })
    });
    assert_eq!(messages, Some(vec!["Unknown table \"nothing_here\"".to_string()]));
}

#[gpui_kit::test]
fn jumping_to_an_error_finds_its_statement_after_the_run(cx: &mut TestAppContext) {
    let env = Env::new(cx);
    let sql = "SELECT 1;\n\nSELECT 'héllo 🌍', nope FROM things;";
    let (tab, cx) = open(&env, cx, sql);
    tab.update_in(cx, |tab, window, cx| tab.run_all(window, cx));
    settle(cx, idle(&tab));
    let statement = "SELECT 'héllo 🌍', nope FROM things;";
    tab.update_in(cx, |tab, window, cx| tab.jump_to_error(statement, 19, "no such column", window, cx));
    let (cursor, error) = cx.update(|_, cx| {
        let tab = tab.read(cx);
        (tab.editor.read(cx).cursor(), tab.query_error.clone())
    });
    assert_eq!(&sql[cursor..cursor + 4], "nope");
    assert_eq!(error.map(|(range, _)| &sql[range]), Some("nope"));
}

#[gpui_kit::test]
fn format_rewrites_the_text_and_undo_brings_it_back(cx: &mut TestAppContext) {
    let env = Env::new(cx);
    let (tab, cx) = open(&env, cx, "select id from things where id = 1");
    cx.dispatch_action(crate::actions::FormatQuery);
    assert_eq!(text(&tab, cx), "SELECT\n  id\nFROM\n  things\nWHERE\n  id = 1");
    cx.simulate_keystrokes(&format!("{}-z", modifier()));
    assert_eq!(text(&tab, cx), "select id from things where id = 1");
}

#[gpui_kit::test]
fn a_run_publishes_the_focused_row_for_the_json_panel(cx: &mut TestAppContext) {
    let env = Env::new(cx);
    let (tab, cx) = open(&env, cx, "SELECT id, name FROM things ORDER BY id LIMIT 3");
    let published = std::rc::Rc::new(std::cell::Cell::new(0));
    let counter = published.clone();
    cx.update(|_, cx| {
        cx.subscribe(&tab, move |_, event: &super::QueryTabEvent, _| {
            if matches!(event, super::QueryTabEvent::FocusedRowChanged) {
                counter.set(counter.get() + 1);
            }
        })
        .detach()
    });
    tab.update_in(cx, |tab, window, cx| tab.run_all(window, cx));
    settle(cx, idle(&tab));
    assert!(published.get() > 0);
    let row = cx.update(|_, cx| tab.read(cx).focused_row(cx)).expect("the first row is focused");
    assert_eq!((row.row, row.columns), (0, vec![0, 1]));
    assert_eq!(row.set.display(0, 1), Some("thing 1"));
}

// The splitter line is 1px but grabs 4px either side, over both the editor and the results.
#[gpui_kit::test]
fn the_results_splitter_drags_between_fifteen_and_seventy_percent(cx: &mut TestAppContext) {
    let env = Env::new(cx);
    let (_tab, cx) = open(&env, cx, "SELECT 1");
    cx.run_until_parked();
    assert_eq!(cx.update(|_, cx| results_share(cx)), 40.);
    let drag = |cx: &mut VisualTestContext, from: f32, y: f32| {
        let pane = cx.debug_bounds("results-pane").expect("the results are drawn");
        let start = point(pane.center().x, pane.top() + px(from));
        cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(start + point(px(0.), px(-5.)), MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(point(start.x, px(y)), MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_up(point(start.x, px(y)), MouseButton::Left, Modifiers::default());
        cx.run_until_parked();
        cx.update(|_, cx| results_share(cx))
    };
    let height = cx.update(|window, _| window.bounds().size.height);
    let half = drag(cx, 0., (height / 2.).as_f32());
    assert!((half - 50.).abs() < 1., "{half}");
    assert_eq!(drag(cx, -3., 0.), 70.);
    assert_eq!(drag(cx, 3., height.as_f32()), 15.);
}

#[gpui_kit::test]
fn the_run_glyph_runs_its_statement(cx: &mut TestAppContext) {
    let env = Env::new(cx);
    let (tab, cx) = open(&env, cx, "SELECT 1 AS one;\nSELECT 2 AS two;");
    cx.run_until_parked();
    let (top, line_height) = cx.update(|_, cx| {
        let state = tab.read(cx).editor.read(cx);
        let start = "SELECT 1 AS one;\n".len();
        (state.range_to_bounds(&(start..start)).unwrap().top(), state.line_height().unwrap())
    });
    cx.simulate_click(point(px(7.), top + line_height / 2.), Modifiers::default());
    settle(cx, idle(&tab));
    let ran = cx.update(|_, cx| tab.read(cx).run_origin.as_ref().map(|origin| origin.text.clone()));
    assert_eq!(ran.as_deref(), Some("SELECT 2 AS two;"));
    assert_eq!(status(&tab, cx).0, 1);
}

#[gpui_kit::test]
fn result_tabs_count_rows_affected_rows_or_nothing(cx: &mut TestAppContext) {
    let env = Env::new(cx);
    let (tab, cx) = open(
        &env,
        cx,
        "SELECT 1 AS a UNION ALL SELECT 2;\nUPDATE things SET name = name WHERE id <= 3;\nSELECT nope FROM nowhere;",
    );
    cx.dispatch_action(crate::actions::RunAll);
    settle(cx, idle(&tab));
    let counts = cx.update(|_, cx| tab.read(cx).results.read(cx).tab_counts(cx));
    assert_eq!(counts, [Some(2), Some(3), None]);
}
