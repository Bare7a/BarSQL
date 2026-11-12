use gpui_kit::component::{ActiveTheme, Theme, ThemeMode};
use gpui_kit::{App, Global, SharedString, Window, WindowAppearance, px};

use crate::i18n::t_with;
use crate::state::{set_setting, setting};
use crate::theme_files::{self, Source, ThemeEntry};
use crate::toast;

const THEME_KEY: &str = "barsql-theme";
pub const DARK_KEY: &str = "barsql-theme-dark";
pub const LIGHT_KEY: &str = "barsql-theme-light";
const ZOOM_KEY: &str = "barsql-ui-zoom-px";
pub const DEFAULT_ZOOM: f32 = 13.;
pub const ZOOM_RANGE: (f32, f32) = (10., 22.);
const EDITOR_FONT_KEY: &str = "barsql-editor-font-size";
pub const DEFAULT_EDITOR_FONT: f32 = 13.;
const EDITOR_FONT_RANGE: (f32, f32) = (10., 24.);

// Set while Select theme… shows a theme that isn't saved.
struct Previewing(bool);

impl Global for Previewing {}

pub fn init(cx: &mut App) {
    fill_slots(cx);
    let mode = resolve(choice(cx), cx);
    Theme::change(mode, None, cx);
    // Focus shows as a tinted border instead of a ring.
    Theme::global_mut(cx).focus_ring = false;
    apply_zoom(zoom(cx), None, cx);
}

// What the View > Theme menu picks. Unset or unknown values mean Match System.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeChoice {
    Dark,
    Light,
    System,
}

pub fn choice(cx: &App) -> ThemeChoice {
    match setting(cx, THEME_KEY).as_deref() {
        Some("dark") => ThemeChoice::Dark,
        Some("light") => ThemeChoice::Light,
        _ => ThemeChoice::System,
    }
}

// Native title bars, menus and dialogs follow the app's theme. A forced appearance hides the system's, so
// System clears it before reading.
fn resolve(choice: ThemeChoice, cx: &App) -> ThemeMode {
    let mode = match choice {
        ThemeChoice::Dark => ThemeMode::Dark,
        ThemeChoice::Light => ThemeMode::Light,
        ThemeChoice::System => {
            cx.set_window_appearance(None);
            return match cx.window_appearance() {
                WindowAppearance::Dark | WindowAppearance::VibrantDark => ThemeMode::Dark,
                WindowAppearance::Light | WindowAppearance::VibrantLight => ThemeMode::Light,
            };
        }
    };
    cx.set_window_appearance(Some(appearance(mode)));
    mode
}

fn appearance(mode: ThemeMode) -> WindowAppearance {
    if mode.is_dark() { WindowAppearance::Dark } else { WindowAppearance::Light }
}

fn pick_key(mode: ThemeMode) -> &'static str {
    if mode.is_dark() { DARK_KEY } else { LIGHT_KEY }
}

// A pick that's gone, or is now of the other mode, shows BarSQL's own but stays saved.
pub fn slot(mode: ThemeMode, cx: &App) -> ThemeEntry {
    let themes = theme_files::themes(cx);
    let picked = setting(cx, pick_key(mode)).and_then(|name| themes.find(&name, mode).cloned());
    picked.unwrap_or_else(|| themes.barsql(mode).clone())
}

fn fill_slots(cx: &mut App) {
    let (dark, light) = (slot(ThemeMode::Dark, cx).config, slot(ThemeMode::Light, cx).config);
    let theme = Theme::global_mut(cx);
    theme.dark_theme = dark;
    theme.light_theme = light;
}

#[derive(Clone, Debug, PartialEq)]
pub enum MenuEntry {
    Theme { name: SharedString, checked: bool },
    Separator,
}

pub fn ordered(mode: ThemeMode, cx: &App) -> Vec<ThemeEntry> {
    fn sorted(mut entries: Vec<&ThemeEntry>) -> Vec<&ThemeEntry> {
        entries.sort_by_key(|entry| entry.config.name.to_lowercase());
        entries
    }
    let themes = theme_files::themes(cx);
    let barsql = themes.barsql(mode);
    let of_mode = |entry: &&ThemeEntry| entry.config.mode == mode && entry.config.name != barsql.config.name;
    let built_in = sorted(themes.all().filter(|entry| entry.source == Source::BuiltIn).filter(of_mode).collect());
    let user = sorted(themes.user().iter().filter(of_mode).collect());
    std::iter::once(barsql).chain(built_in).chain(user).cloned().collect()
}

pub fn menu_entries(mode: ThemeMode, cx: &App) -> Vec<MenuEntry> {
    let showing = slot(mode, cx).config.name.clone();
    let mut entries = Vec::new();
    let mut users = false;
    for entry in ordered(mode, cx) {
        if entry.source != Source::BuiltIn && !users {
            users = true;
            entries.push(MenuEntry::Separator);
        }
        let name = entry.config.name.clone();
        entries.push(MenuEntry::Theme { checked: name == showing, name });
    }
    entries
}

fn named(name: &str, cx: &App) -> Option<ThemeEntry> {
    theme_files::themes(cx).all().find(|entry| entry.config.name == name).cloned()
}

// A name that's gone, say a file deleted while Select theme… was open, still ends any preview.
pub fn pick(name: &str, window: &mut Window, cx: &mut App) {
    let Some(entry) = named(name, cx) else { return end_preview(window, cx) };
    cx.set_global(Previewing(false));
    let mode = entry.config.mode;
    set_setting(cx, pick_key(mode), name);
    if choice(cx) != ThemeChoice::System {
        set_choice(if mode.is_dark() { ThemeChoice::Dark } else { ThemeChoice::Light }, window, cx);
    } else {
        let shown = resolve(ThemeChoice::System, cx);
        apply(shown, window, cx);
        if shown != mode {
            let key = if mode.is_dark() { "theme.laterDark" } else { "theme.laterLight" };
            toast::info(t_with(cx, key, &[("name", name)]), cx);
        }
    }
    theme_files::refresh(window, cx);
}

pub fn preview(name: &str, window: &mut Window, cx: &mut App) {
    if cx.theme().theme_name().as_ref() == name {
        return;
    }
    let Some(entry) = named(name, cx) else { return };
    cx.set_global(Previewing(true));
    let mode = entry.config.mode;
    let theme = Theme::global_mut(cx);
    if mode.is_dark() {
        theme.dark_theme = entry.config;
    } else {
        theme.light_theme = entry.config;
    }
    cx.set_window_appearance(Some(appearance(mode)));
    show(mode, window, cx);
}

pub fn end_preview(window: &mut Window, cx: &mut App) {
    if previewing(cx) {
        cx.set_global(Previewing(false));
        apply(resolve(choice(cx), cx), window, cx);
        theme_files::refresh(window, cx);
    }
}

pub fn previewing(cx: &App) -> bool {
    cx.try_global::<Previewing>().is_some_and(|previewing| previewing.0)
}

pub fn reapply(window: &mut Window, cx: &mut App) {
    if !previewing(cx) {
        apply(resolve(choice(cx), cx), window, cx);
    }
}

pub fn set_choice(choice: ThemeChoice, window: &mut Window, cx: &mut App) {
    let value = match choice {
        ThemeChoice::Dark => "dark",
        ThemeChoice::Light => "light",
        ThemeChoice::System => "system",
    };
    set_setting(cx, THEME_KEY, value);
    apply(resolve(choice, cx), window, cx);
}

// Called when the window's appearance changes. Only Match System follows it, and not mid-preview, which sets
// the appearance itself.
pub fn follow_system(window: &mut Window, cx: &mut App) {
    if choice(cx) == ThemeChoice::System && !previewing(cx) {
        let mode = resolve(ThemeChoice::System, cx);
        if cx.theme().mode != mode {
            apply(mode, window, cx);
        }
    }
}

fn apply(mode: ThemeMode, window: &mut Window, cx: &mut App) {
    fill_slots(cx);
    show(mode, window, cx);
    crate::actions::set_menus(cx);
}

fn show(mode: ThemeMode, window: &mut Window, cx: &mut App) {
    Theme::change(mode, Some(window), cx);
    Theme::global_mut(cx).focus_ring = false;
    apply_zoom(zoom(cx), Some(window), cx);
}

pub fn zoom(cx: &App) -> f32 {
    setting(cx, ZOOM_KEY)
        .and_then(|z| z.parse::<f32>().ok())
        .filter(|z| (ZOOM_RANGE.0..=ZOOM_RANGE.1).contains(z))
        .unwrap_or(DEFAULT_ZOOM)
}

pub fn set_zoom(value: f32, window: &mut Window, cx: &mut App) {
    let value = value.clamp(ZOOM_RANGE.0, ZOOM_RANGE.1);
    set_setting(cx, ZOOM_KEY, &value.to_string());
    apply_zoom(value, Some(window), cx);
}

// Zoom sets the root font size, so rem-based sizes scale with it.
fn apply_zoom(value: f32, window: Option<&mut Window>, cx: &mut App) {
    Theme::global_mut(cx).font_size = px(value);
    if let Some(window) = window {
        window.set_rem_size(px(value));
        window.refresh();
    }
}

// Whole pixels. A stored value out of range is clamped.
pub fn editor_font_size(cx: &App) -> f32 {
    setting(cx, EDITOR_FONT_KEY)
        .and_then(|size| size.trim().parse::<i64>().ok())
        .map_or(DEFAULT_EDITOR_FONT, |size| (size as f32).clamp(EDITOR_FONT_RANGE.0, EDITOR_FONT_RANGE.1))
}

pub fn set_editor_font_size(value: f32, window: &mut Window, cx: &mut App) {
    let value = value.round().clamp(EDITOR_FONT_RANGE.0, EDITOR_FONT_RANGE.1);
    set_setting(cx, EDITOR_FONT_KEY, &value.to_string());
    window.refresh();
}
