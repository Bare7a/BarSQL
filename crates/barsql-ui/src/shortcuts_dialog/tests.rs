use gpui_kit::component::Root;
use gpui_kit::{AppContext as _, Context, Entity, IntoElement, Render, TestAppContext, VisualTestContext, Window, div};

use super::{ShortcutsDialog, open};
use crate::shortcuts::{self, Binding};
use crate::test_support::Env;

struct Blank;

impl Render for Blank {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

fn dialog(cx: &mut TestAppContext) -> (Entity<ShortcutsDialog>, &mut VisualTestContext) {
    let window = cx.add_window(|window, cx| {
        let view = cx.new(|_| Blank);
        Root::new(view, window, cx)
    });
    let cx = VisualTestContext::from_window(window.into(), cx).into_mut();
    let dialog = cx.update(open);
    cx.run_until_parked();
    (dialog, cx)
}

fn quick_search(cx: &mut VisualTestContext) -> Binding {
    cx.update(|_, cx| shortcuts::effective(shortcuts::find("quickSearch").unwrap(), cx))
}

fn modifier() -> &'static str {
    if cfg!(target_os = "macos") { "cmd" } else { "ctrl" }
}

#[gpui_kit::test]
fn a_recorded_combination_replaces_the_binding(cx: &mut TestAppContext) {
    let _env = Env::new(cx);
    let (dialog, cx) = dialog(cx);
    dialog.update(cx, |dialog, cx| dialog.record("quickSearch", cx));
    cx.simulate_keystrokes("shift");
    assert!(cx.update(|_, cx| dialog.read(cx).is_recording()), "modifiers alone keep listening");
    cx.simulate_keystrokes(&format!("{}-k", modifier()));
    cx.run_until_parked();
    assert!(!cx.update(|_, cx| dialog.read(cx).is_recording()));
    assert_eq!(quick_search(cx), Binding { key: "k".into(), ctrl: true, ..Default::default() });
}

#[gpui_kit::test]
fn a_taken_combination_is_refused_and_escape_cancels(cx: &mut TestAppContext) {
    let _env = Env::new(cx);
    let (dialog, cx) = dialog(cx);
    dialog.update(cx, |dialog, cx| dialog.record("quickSearch", cx));
    cx.simulate_keystrokes(&format!("{}-t", modifier()));
    cx.run_until_parked();
    let (recording, error) = cx.update(|_, cx| (dialog.read(cx).is_recording(), dialog.read(cx).error.clone()));
    assert!(recording, "still listening after a conflict");
    assert_eq!(error.as_deref(), Some("Already used by “New query tab”. Choose a different combination."));

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(!cx.update(|_, cx| dialog.read(cx).is_recording()));
    assert!(cx.update(gpui_kit::component::WindowExt::has_active_dialog), "Esc cancelled only the recording");
    assert_eq!(quick_search(cx), Binding { key: "p".into(), ctrl: true, ..Default::default() });
}
