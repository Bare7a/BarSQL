use gpui_kit::TestAppContext;
use gpui_kit::component::ActiveTheme as _;

use super::driver::{Driver, open};
use crate::actions::{ThemeDark, ThemeLight, ThemeSystem, ToggleJsonPanel, ToggleSidebar};
use crate::theme::{ThemeChoice, choice};

#[gpui_kit::test]
fn the_shell_opens_with_the_sidebar_on_its_schema_panel(cx: &mut TestAppContext) {
    let mut app = open(cx);
    assert!(app.shown("sidebar"));
    assert!(app.shown("sidebar.schema"));
    assert!(!app.shown("json-panel"));
}

// View menu items just dispatch these actions.
#[gpui_kit::test]
fn the_view_menu_hides_and_shows_the_sidebar(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.dispatch(ToggleSidebar);
    assert!(!app.shown("sidebar"));
    app.dispatch(ToggleSidebar);
    assert!(app.shown("sidebar"));
}

#[gpui_kit::test]
fn the_view_menu_opens_and_closes_the_json_viewer(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.dispatch(ToggleJsonPanel);
    assert!(app.shown("json-panel"));
    app.dispatch(ToggleJsonPanel);
    assert!(!app.shown("json-panel"));
}

// The test platform reports a light system appearance.
#[gpui_kit::test]
fn the_theme_follows_the_system_when_asked(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let theme = |app: &mut super::driver::Driver| app.cx.update(|_, cx| (choice(cx), cx.theme().mode.is_dark()));
    assert_eq!(theme(&mut app), (ThemeChoice::Dark, true));
    app.dispatch(ThemeSystem);
    assert_eq!(theme(&mut app), (ThemeChoice::System, false));
    app.dispatch(ThemeDark);
    assert_eq!(theme(&mut app), (ThemeChoice::Dark, true));
    app.dispatch(ThemeLight);
    assert_eq!(theme(&mut app), (ThemeChoice::Light, false));
}

// The palette runs the picked command where it was opened from.
#[gpui_kit::test]
fn the_command_palette_runs_the_picked_command(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.connect();
    app.set_sql("SELECT 7 AS n;");
    app.keys("mod-shift-p");
    assert!(app.dialog_open());
    app.type_text("run all");
    app.keys("enter");
    app.wait_idle();
    assert!(!app.dialog_open());
    assert_eq!(app.cell(0, 0).as_deref(), Some("7"));
}

// Editor commands show from the editor, and the transaction ones follow the tab's state.
#[gpui_kit::test]
fn the_command_palette_lists_what_the_focus_can_run(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.connect();
    let labels = |app: &mut Driver| {
        let palette = app.workspace.update_in(app.cx, |ws, window, cx| ws.open_command_palette(window, cx));
        let palette = palette.expect("the palette opens");
        app.cx.run_until_parked();
        let labels = app.cx.update(|_, cx| palette.read(cx).labels());
        app.keys("escape");
        labels
    };
    let has = |labels: &[String], label: &str| labels.iter().any(|l| l == label);
    let idle = labels(&mut app);
    assert!(has(&idle, "Run all") && has(&idle, "Toggle sidebar") && has(&idle, "Begin transaction"), "{idle:?}");
    assert!(!has(&idle, "Commit transaction"));
    app.click("begin-txn");
    app.wait_idle();
    let open = labels(&mut app);
    assert!(has(&open, "Commit transaction") && has(&open, "Roll back transaction"), "{open:?}");
    assert!(!has(&open, "Begin transaction"));
}
