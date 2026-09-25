use std::collections::HashMap;

use gpui_kit::{Keystroke, TestAppContext};

use super::{
    Binding, SHORTCUTS, binding_from_keystroke, binding_key, effective, find, find_conflict, format_binding,
    keystrokes, reset_all, reset_binding, set_binding,
};
use crate::actions::{QuickSearch, ZoomIn};
use crate::test_support::Env;

fn binding(key: &str, ctrl: bool, shift: bool, alt: bool) -> Binding {
    Binding { key: key.into(), ctrl, shift, alt }
}

fn recorded(keystroke: &str) -> Option<Binding> {
    binding_from_keystroke(&Keystroke::parse(keystroke).unwrap())
}

#[test]
fn default_bindings_are_distinct_and_never_ctrl_alt() {
    let mut seen: HashMap<String, &str> = HashMap::new();
    for def in &SHORTCUTS {
        let key = binding_key(&def.default_binding());
        assert!(seen.insert(key, def.id).is_none(), "{} shares its default binding", def.id);
        let default = def.default_binding();
        assert!(!(default.ctrl && default.alt), "{} defaults to a Ctrl+Alt chord (AltGr on Windows)", def.id);
    }
    assert_eq!(SHORTCUTS.len(), 20);
}

#[test]
fn conflict_identity_keeps_distinct_keys_apart() {
    assert_ne!(binding_key(&binding("-", true, false, false)), binding_key(&binding("=", true, false, false)));
    assert_eq!(binding_key(&binding("P", true, false, false)), "1:0:0:p");
}

#[test]
fn labels_follow_the_platform_convention() {
    let cases = [
        (binding("p", true, false, false), "⌘P", "Ctrl+P"),
        (binding("-", true, false, false), "⌘\u{2212}", "Ctrl+\u{2212}"),
        (binding("Enter", true, true, false), "⇧⌘Enter", "Ctrl+Shift+Enter"),
        (binding("a", true, true, true), "⌥⇧⌘A", "Ctrl+Alt+Shift+A"),
        (binding("F2", false, false, false), "F2", "F2"),
        (binding(" ", false, true, false), "⇧Space", "Shift+Space"),
    ];
    for (binding, mac, other) in cases {
        assert_eq!(format_binding(&binding, true), mac);
        assert_eq!(format_binding(&binding, false), other);
    }
}

#[test]
fn a_binding_answers_to_its_platform_keystrokes() {
    let mac = cfg!(target_os = "macos");
    let with_cmd = |keys: &[&str]| -> Vec<String> {
        let ctrl = keys.iter().map(|k| format!("ctrl-{k}"));
        if mac { ctrl.chain(keys.iter().map(|k| format!("cmd-{k}"))).collect() } else { ctrl.collect() }
    };
    assert_eq!(keystrokes(&binding("p", true, false, false)), with_cmd(&["p"]));
    assert_eq!(keystrokes(&binding(".", true, true, false)), with_cmd(&[">", "shift-."]));
    assert_eq!(keystrokes(&binding("=", true, false, false)), with_cmd(&["+", "="]));
    assert_eq!(keystrokes(&binding("Enter", true, true, false)), with_cmd(&["shift-enter"]));
    assert_eq!(keystrokes(&binding("F11", false, false, false)), ["f11"]);
    assert_eq!(keystrokes(&binding("ArrowUp", false, false, true)), ["alt-up"]);
    assert!(keystrokes(&binding("Nonsense", true, false, false)).is_empty());
}

#[test]
fn recorded_keys_are_stored_with_their_named_keys() {
    assert_eq!(recorded("cmd-shift-t"), Some(binding("t", true, true, false)));
    assert_eq!(recorded("ctrl-shift-t"), Some(binding("t", true, true, false)));
    assert_eq!(recorded("cmd->"), Some(binding(".", true, true, false)), "a folded shift comes back");
    assert_eq!(recorded("ctrl-shift-."), Some(binding(".", true, true, false)));
    assert_eq!(recorded("ctrl-enter"), Some(binding("Enter", true, false, false)));
    assert_eq!(recorded("f5"), Some(binding("F5", false, false, false)));
    assert_eq!(recorded("alt-up"), Some(binding("ArrowUp", false, false, true)));
    assert_eq!(recorded("shift"), None);
}

#[gpui_kit::test]
fn overrides_rebuild_the_keymap_and_resets_restore_the_defaults(cx: &mut TestAppContext) {
    let env = Env::new(cx);
    let quick = find("quickSearch").unwrap();
    let bound = |cx: &mut TestAppContext| -> Vec<String> {
        cx.update(|cx| {
            let keymap = cx.key_bindings();
            let keymap = keymap.borrow();
            keymap.bindings_for_action(&QuickSearch).map(|b| b.keystrokes()[0].inner().unparse()).collect()
        })
    };
    assert!(bound(cx).iter().any(|k| k.ends_with("-p")));

    cx.update(|cx| set_binding("quickSearch", &binding("k", true, false, false), cx));
    let keys = bound(cx);
    assert!(keys.iter().all(|k| k.ends_with("-k")), "{keys:?}");
    let stored = env.bar.settings().get(super::OVERRIDES_KEY).cloned().unwrap();
    assert_eq!(stored, r#"{"quickSearch":{"key":"k","ctrl":true,"shift":false,"alt":false}}"#);
    assert_eq!(cx.update(|cx| effective(quick, cx)), binding("k", true, false, false));

    let conflict = cx.update(|cx| find_conflict("zoomIn", &binding("K", true, false, false), cx).map(|d| d.id));
    assert_eq!(conflict, Some("quickSearch"));
    assert_eq!(cx.update(|cx| find_conflict("quickSearch", &binding("k", true, false, false), cx)).map(|d| d.id), None);

    cx.update(|cx| reset_binding("quickSearch", cx));
    assert!(bound(cx).iter().any(|k| k.ends_with("-p")));
    cx.update(|cx| set_binding("zoomIn", &binding("i", true, false, false), cx));
    cx.update(reset_all);
    assert_eq!(env.bar.settings().get(super::OVERRIDES_KEY).map(String::as_str), Some("{}"));
    let zoom = cx.update(|cx| cx.key_bindings().borrow().bindings_for_action(&ZoomIn).count());
    assert!(zoom >= 2, "= and its + alias");
}
