use std::rc::Rc;

use gpui_kit::component::{Theme, ThemeMode, ThemeSet};
use gpui_kit::{App, Window, WindowAppearance, px};

use crate::state::{set_setting, setting};

const THEMES: &str = include_str!("../themes/barsql.json");
const THEME_KEY: &str = "barsql-theme";
const ZOOM_KEY: &str = "barsql-ui-zoom-px";
pub const DEFAULT_ZOOM: f32 = 13.;
pub const ZOOM_RANGE: (f32, f32) = (10., 22.);
const EDITOR_FONT_KEY: &str = "barsql-editor-font-size";
pub const DEFAULT_EDITOR_FONT: f32 = 13.;
const EDITOR_FONT_RANGE: (f32, f32) = (10., 24.);

pub fn init(cx: &mut App) {
    let set: ThemeSet = serde_json::from_str(THEMES).expect("theme json");
    let theme = Theme::global_mut(cx);
    for config in set.themes {
        if config.mode.is_dark() {
            theme.dark_theme = Rc::new(config);
        } else {
            theme.light_theme = Rc::new(config);
        }
    }
    let mode = saved_mode(cx);
    Theme::change(mode, None, cx);
    // Focus shows as a tinted border instead of a ring.
    Theme::global_mut(cx).focus_ring = false;
    set_native_appearance(mode, cx);
    apply_zoom(zoom(cx), None, cx);
}

// Native title bars, menus and dialogs follow the app's theme, not the system's.
fn set_native_appearance(mode: ThemeMode, cx: &App) {
    cx.set_window_appearance(Some(if mode.is_dark() { WindowAppearance::Dark } else { WindowAppearance::Light }));
}

pub fn saved_mode(cx: &App) -> ThemeMode {
    if setting(cx, THEME_KEY).as_deref() == Some("light") { ThemeMode::Light } else { ThemeMode::Dark }
}

pub fn toggle(window: &mut Window, cx: &mut App) {
    let mode = if Theme::global(cx).is_dark() { ThemeMode::Light } else { ThemeMode::Dark };
    set_setting(cx, THEME_KEY, if mode.is_dark() { "dark" } else { "light" });
    Theme::change(mode, Some(window), cx);
    Theme::global_mut(cx).focus_ring = false;
    set_native_appearance(mode, cx);
    apply_zoom(zoom(cx), Some(window), cx);
    crate::actions::set_menus(cx);
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
