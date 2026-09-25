use gpui_kit::TestAppContext;

use super::driver::open;
use crate::actions::{ToggleJsonPanel, ToggleSidebar};

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
