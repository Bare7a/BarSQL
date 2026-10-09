use std::rc::Rc;

use gpui_kit::{Action, App, DummyKeyboardMapper, Global, KeyBinding, KeyBindingContextPredicate, Keystroke};
use serde::{Deserialize, Serialize};

use crate::actions::*;
use crate::i18n::t;
use crate::state::{set_setting, setting};

// In a Binding, `ctrl` means Cmd on macOS.
pub const OVERRIDES_KEY: &str = "barsql-shortcut-overrides";
const EDITOR_CONTEXT: &str = "QueryEditor > Input";

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Binding {
    pub key: String,
    #[serde(default)]
    pub ctrl: bool,
    #[serde(default)]
    pub shift: bool,
    #[serde(default)]
    pub alt: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    Query,
    Tabs,
    View,
}

impl Category {
    pub fn key(self) -> &'static str {
        match self {
            Self::Query => "query",
            Self::Tabs => "tabs",
            Self::View => "view",
        }
    }
}

// Editor shortcuts only act in the SQL editor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Global,
    Editor,
}

pub struct ShortcutDef {
    pub id: &'static str,
    pub category: Category,
    pub scope: Scope,
    key: &'static str,
    ctrl: bool,
    shift: bool,
    alt: bool,
}

impl ShortcutDef {
    pub fn default_binding(&self) -> Binding {
        Binding { key: self.key.into(), ctrl: self.ctrl, shift: self.shift, alt: self.alt }
    }

    fn action(&self) -> Box<dyn Action> {
        match self.id {
            "quickSearch" => Box::new(QuickSearch),
            "commandPalette" => Box::new(CommandPalette),
            "runSelection" => Box::new(RunSelection),
            "runAll" => Box::new(RunAll),
            "explainQuery" => Box::new(ExplainQuery),
            "explainAnalyze" => Box::new(ExplainAnalyze),
            "saveQuery" => Box::new(SaveQuery),
            "renameSavedQuery" => Box::new(RenameSavedQuery),
            "newTab" => Box::new(NewTab),
            "closeTab" => Box::new(CloseTab),
            "reopenClosedTab" => Box::new(ReopenClosedTab),
            "nextTab" => Box::new(NextTab),
            "prevTab" => Box::new(PrevTab),
            "toggleSidebar" => Box::new(ToggleSidebar),
            "toggleJsonPanel" => Box::new(ToggleJsonPanel),
            "zoomIn" => Box::new(ZoomIn),
            "zoomOut" => Box::new(ZoomOut),
            "resetZoom" => Box::new(ResetZoom),
            "increaseEditorFontSize" => Box::new(IncreaseEditorFontSize),
            "decreaseEditorFontSize" => Box::new(DecreaseEditorFontSize),
            _ => Box::new(ToggleFullscreen),
        }
    }

    pub fn label(&self, cx: &App) -> gpui_kit::SharedString {
        t(cx, &format!("shortcuts.items.{}", self.id))
    }
}

const fn def(
    id: &'static str,
    category: Category,
    scope: Scope,
    key: &'static str,
    (ctrl, shift, alt): (bool, bool, bool),
) -> ShortcutDef {
    ShortcutDef { id, category, scope, key, ctrl, shift, alt }
}

const CTRL: (bool, bool, bool) = (true, false, false);
const CTRL_SHIFT: (bool, bool, bool) = (true, true, false);
const NONE: (bool, bool, bool) = (false, false, false);

const QUERY: Category = Category::Query;
const TABS: Category = Category::Tabs;
const VIEW: Category = Category::View;
const GLOBAL: Scope = Scope::Global;
const EDITOR: Scope = Scope::Editor;

pub const SHORTCUTS: [ShortcutDef; 21] = [
    def("quickSearch", TABS, GLOBAL, "p", CTRL),
    def("commandPalette", VIEW, GLOBAL, "p", CTRL_SHIFT),
    def("runSelection", QUERY, EDITOR, "Enter", CTRL),
    def("runAll", QUERY, EDITOR, "Enter", CTRL_SHIFT),
    def("explainQuery", QUERY, EDITOR, "e", CTRL_SHIFT),
    // Not Ctrl+Alt, since Windows reports AltGr as Ctrl+Alt and German AltGr+E types the euro sign.
    def("explainAnalyze", QUERY, EDITOR, "a", CTRL_SHIFT),
    def("saveQuery", QUERY, EDITOR, "s", CTRL),
    def("renameSavedQuery", QUERY, EDITOR, "F2", NONE),
    def("newTab", TABS, GLOBAL, "t", CTRL),
    def("closeTab", TABS, GLOBAL, "w", CTRL),
    def("reopenClosedTab", TABS, GLOBAL, "t", CTRL_SHIFT),
    def("nextTab", TABS, GLOBAL, "Tab", CTRL),
    def("prevTab", TABS, GLOBAL, "Tab", CTRL_SHIFT),
    def("toggleSidebar", VIEW, GLOBAL, "b", CTRL),
    def("toggleJsonPanel", VIEW, GLOBAL, "j", CTRL),
    def("zoomIn", VIEW, GLOBAL, "=", CTRL),
    def("zoomOut", VIEW, GLOBAL, "-", CTRL),
    def("resetZoom", VIEW, GLOBAL, "0", CTRL),
    def("increaseEditorFontSize", VIEW, EDITOR, ".", CTRL_SHIFT),
    def("decreaseEditorFontSize", VIEW, EDITOR, ",", CTRL_SHIFT),
    def("toggleFullscreen", VIEW, GLOBAL, "F11", NONE),
];

pub fn find(id: &str) -> Option<&'static ShortcutDef> {
    SHORTCUTS.iter().find(|def| def.id == id)
}

type Overrides = serde_json::Map<String, serde_json::Value>;

fn read_overrides(cx: &App) -> Overrides {
    setting(cx, OVERRIDES_KEY)
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .and_then(|value| match value {
            serde_json::Value::Object(map) => Some(map),
            _ => None,
        })
        .unwrap_or_default()
}

fn write_overrides(overrides: &Overrides, cx: &mut App) {
    set_setting(cx, OVERRIDES_KEY, &serde_json::Value::Object(overrides.clone()).to_string());
    apply(cx);
}

pub fn effective(def: &ShortcutDef, cx: &App) -> Binding {
    effective_in(def, &read_overrides(cx))
}

fn effective_in(def: &ShortcutDef, overrides: &Overrides) -> Binding {
    overrides
        .get(def.id)
        .and_then(|value| serde_json::from_value::<Binding>(value.clone()).ok())
        .filter(|binding| !binding.key.is_empty())
        .unwrap_or_else(|| def.default_binding())
}

pub fn set_binding(id: &str, binding: &Binding, cx: &mut App) {
    let mut overrides = read_overrides(cx);
    overrides.insert(id.to_string(), serde_json::to_value(binding).unwrap_or_default());
    write_overrides(&overrides, cx);
}

pub fn reset_binding(id: &str, cx: &mut App) {
    let mut overrides = read_overrides(cx);
    overrides.shift_remove(id);
    write_overrides(&overrides, cx);
}

pub fn reset_all(cx: &mut App) {
    write_overrides(&Overrides::new(), cx);
}

pub fn binding_key(binding: &Binding) -> String {
    let flag = |on: bool| if on { "1" } else { "0" };
    format!("{}:{}:{}:{}", flag(binding.ctrl), flag(binding.shift), flag(binding.alt), binding.key.to_lowercase())
}

pub fn find_conflict(id: &str, binding: &Binding, cx: &App) -> Option<&'static ShortcutDef> {
    let overrides = read_overrides(cx);
    let key = binding_key(binding);
    SHORTCUTS.iter().find(|def| def.id != id && binding_key(&effective_in(def, &overrides)) == key)
}

fn key_label(key: &str) -> String {
    match key {
        " " => "Space".into(),
        "-" => "\u{2212}".into(),
        _ if key.chars().count() == 1 => key.to_uppercase(),
        _ => key.into(),
    }
}

// Option, Shift, Cmd glyphs in that order on macOS, Ctrl+Alt+Shift+P elsewhere.
pub fn format_binding(binding: &Binding, mac: bool) -> String {
    let key = key_label(&binding.key);
    if mac {
        let mut label = String::new();
        for (on, glyph) in [(binding.alt, "⌥"), (binding.shift, "⇧"), (binding.ctrl, "⌘")] {
            if on {
                label.push_str(glyph);
            }
        }
        return label + &key;
    }
    let mut parts: Vec<&str> = Vec::new();
    for (on, name) in [(binding.ctrl, "Ctrl"), (binding.alt, "Alt"), (binding.shift, "Shift")] {
        if on {
            parts.push(name);
        }
    }
    parts.push(&key);
    parts.join("+")
}

pub fn format(binding: &Binding) -> String {
    format_binding(binding, cfg!(target_os = "macos"))
}

// US layout unshifted/shifted pairs. macOS and Linux report Shift+. as `>`.
const SHIFTED: [(char, char); 21] = [
    ('1', '!'),
    ('2', '@'),
    ('3', '#'),
    ('4', '$'),
    ('5', '%'),
    ('6', '^'),
    ('7', '&'),
    ('8', '*'),
    ('9', '('),
    ('0', ')'),
    ('-', '_'),
    ('=', '+'),
    ('[', '{'),
    (']', '}'),
    ('\\', '|'),
    (';', ':'),
    ('\'', '"'),
    (',', '<'),
    ('.', '>'),
    ('/', '?'),
    ('`', '~'),
];

fn shifted(key: &str) -> Option<String> {
    let mut chars = key.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else { return None };
    SHIFTED.iter().find(|(plain, _)| *plain == c).map(|(_, shifted)| shifted.to_string())
}

fn unshifted(key: &str) -> Option<String> {
    let mut chars = key.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else { return None };
    SHIFTED.iter().find(|(_, shifted)| *shifted == c).map(|(plain, _)| plain.to_string())
}

// Maps stored key names like Enter, ArrowUp or F5 to GPUI's.
fn gpui_key(key: &str) -> Option<String> {
    let named = match key {
        "Enter" => "enter",
        "Tab" => "tab",
        " " => "space",
        "Escape" => "escape",
        "Backspace" => "backspace",
        "Delete" => "delete",
        "Insert" => "insert",
        "ArrowUp" => "up",
        "ArrowDown" => "down",
        "ArrowLeft" => "left",
        "ArrowRight" => "right",
        "Home" => "home",
        "End" => "end",
        "PageUp" => "pageup",
        "PageDown" => "pagedown",
        _ => "",
    };
    if !named.is_empty() {
        return Some(named.into());
    }
    if let Some(n) = key.strip_prefix('F').filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())) {
        return Some(format!("f{n}"));
    }
    (key.chars().count() == 1 && !key.chars().any(char::is_whitespace)).then(|| key.to_lowercase())
}

fn stored_key(key: &str) -> String {
    match key {
        "enter" => "Enter".into(),
        "tab" => "Tab".into(),
        "space" => " ".into(),
        "escape" => "Escape".into(),
        "backspace" => "Backspace".into(),
        "delete" => "Delete".into(),
        "insert" => "Insert".into(),
        "up" => "ArrowUp".into(),
        "down" => "ArrowDown".into(),
        "left" => "ArrowLeft".into(),
        "right" => "ArrowRight".into(),
        "home" => "Home".into(),
        "end" => "End".into(),
        "pageup" => "PageUp".into(),
        "pagedown" => "PageDown".into(),
        f if f.len() > 1 && f.starts_with('f') && f[1..].bytes().all(|b| b.is_ascii_digit()) => f.to_uppercase(),
        other => other.to_lowercase(),
    }
}

const MODIFIER_KEYS: [&str; 6] = ["shift", "control", "alt", "platform", "function", "cmd"];

// A shifted symbol is stored as its unshifted key plus Shift.
pub fn binding_from_keystroke(keystroke: &Keystroke) -> Option<Binding> {
    if keystroke.key.is_empty() || MODIFIER_KEYS.contains(&keystroke.key.as_str()) {
        return None;
    }
    let modifiers = keystroke.modifiers;
    let mut binding = Binding {
        key: stored_key(&keystroke.key),
        ctrl: modifiers.control || modifiers.platform,
        shift: modifiers.shift,
        alt: modifiers.alt,
    };
    if let Some(plain) = unshifted(&binding.key) {
        binding.key = plain;
        binding.shift = true;
    }
    Some(binding)
}

// Canonical keystroke last so menus and tooltips show it. A shifted symbol also matches its shifted character,
// `=` also matches `+` for the numpad and layouts with a + key, and on macOS `ctrl` also matches Ctrl.
pub fn keystrokes(binding: &Binding) -> Vec<String> {
    let Some(key) = gpui_key(&binding.key) else { return Vec::new() };
    let mut keys: Vec<(String, bool)> = Vec::new();
    match key.as_str() {
        "=" if !binding.shift => keys.push(("+".into(), false)),
        "+" if !binding.shift => keys.push(("=".into(), false)),
        _ => {}
    }
    if binding.shift
        && let Some(shifted) = shifted(&key)
    {
        keys.push((shifted, false));
    }
    keys.push((key, binding.shift));
    let mut prefixes: Vec<&str> = Vec::new();
    if binding.ctrl {
        prefixes.push("ctrl-");
        if cfg!(target_os = "macos") {
            prefixes.push("cmd-");
        }
    } else {
        prefixes.push("");
    }
    let alt = if binding.alt { "alt-" } else { "" };
    let mut out = Vec::new();
    for prefix in prefixes {
        for (key, shift) in &keys {
            let shift = if *shift { "shift-" } else { "" };
            out.push(format!("{prefix}{alt}{shift}{key}"));
        }
    }
    out
}

fn key_bindings(def: &ShortcutDef, binding: &Binding) -> Vec<KeyBinding> {
    let context = match def.scope {
        Scope::Global => None,
        Scope::Editor => KeyBindingContextPredicate::parse(EDITOR_CONTEXT).ok().map(Rc::new),
    };
    keystrokes(binding)
        .iter()
        .filter_map(|keys| {
            KeyBinding::load(keys, def.action(), context.clone(), false, None, &DummyKeyboardMapper).ok()
        })
        .collect()
}

// Every binding but the remappable ones. Captured in init, which must run after all other bindings.
struct BaseKeymap(Vec<KeyBinding>);

impl Global for BaseKeymap {}

pub fn init(cx: &mut App) {
    let base = cx.key_bindings().borrow().bindings().cloned().collect();
    cx.set_global(BaseKeymap(base));
    apply(cx);
}

pub fn apply(cx: &mut App) {
    let base = cx.try_global::<BaseKeymap>().map(|base| base.0.clone()).unwrap_or_default();
    let overrides = read_overrides(cx);
    let shortcuts: Vec<KeyBinding> =
        SHORTCUTS.iter().flat_map(|def| key_bindings(def, &effective_in(def, &overrides))).collect();
    cx.clear_key_bindings();
    cx.bind_keys(base);
    cx.bind_keys(shortcuts);
    set_menus(cx);
}

#[cfg(test)]
mod tests;
