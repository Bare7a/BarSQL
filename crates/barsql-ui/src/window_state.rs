use std::time::Duration;

use gpui_kit::{App, Bounds, Pixels, Window, WindowBounds, point, px, size};
use serde::{Deserialize, Serialize};

use crate::state::setting;

pub const KEY: &str = "barsql-window-state";
pub const DEBOUNCE: Duration = Duration::from_millis(500);
pub const MIN_WIDTH: i64 = 800;
pub const MIN_HEIGHT: i64 = 600;

const NORMAL: &str = "normal";
const MAXIMISED: &str = "maximised";
const FULLSCREEN: &str = "fullscreen";

// x and y are relative to the work area of the window's display.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct WindowState {
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub width: i64,
    #[serde(default)]
    pub height: i64,
    #[serde(default)]
    pub x: i64,
    #[serde(default)]
    pub y: i64,
}

pub fn parse(raw: &str) -> Option<WindowState> {
    if raw.is_empty() {
        return None;
    }
    serde_json::from_str(raw).ok()
}

pub fn load(cx: &App) -> Option<WindowState> {
    parse(&setting(cx, KEY)?)
}

// Saved bounds on the primary display, else 80% of it, centred. The first run starts maximised.
pub fn initial_bounds(saved: Option<&WindowState>, cx: &App) -> WindowBounds {
    let display = cx.primary_display();
    let default = display.as_ref().map_or(size(px(1280.), px(720.)), |display| {
        let screen = display.bounds().size;
        let scaled = |side: Pixels| px((side.as_f32() as i64 * 80 / 100) as f32);
        size(scaled(screen.width), scaled(screen.height))
    });
    let restorable = saved.filter(|s| s.width >= MIN_WIDTH && s.height >= MIN_HEIGHT);
    let bounds = match (restorable, &display) {
        (Some(s), Some(display)) => Bounds::new(
            display.visible_bounds().origin + point(px(s.x as f32), px(s.y as f32)),
            size(px(s.width as f32), px(s.height as f32)),
        ),
        (Some(s), None) => Bounds::centered(None, size(px(s.width as f32), px(s.height as f32)), cx),
        (None, _) => Bounds::centered(None, default, cx),
    };
    match saved.map(|s| s.mode.as_str()) {
        None | Some(MAXIMISED) => WindowBounds::Maximized(bounds),
        Some(FULLSCREEN) => WindowBounds::Fullscreen(bounds),
        Some(_) => WindowBounds::Windowed(bounds),
    }
}

// Maximised and fullscreen windows keep their last normal bounds.
pub fn capture(last: &WindowState, window: &Window, cx: &App) -> WindowState {
    let mode = if window.is_fullscreen() {
        FULLSCREEN
    } else if window.is_maximized() {
        MAXIMISED
    } else {
        NORMAL
    };
    if mode != NORMAL {
        return WindowState { mode: mode.into(), ..last.clone() };
    }
    let bounds = window.bounds();
    let work = window.display(cx).map(|display| display.visible_bounds().origin).unwrap_or_default();
    normal(last, bounds, work)
}

fn normal(last: &WindowState, bounds: Bounds<Pixels>, work: gpui_kit::Point<Pixels>) -> WindowState {
    let round = |value: Pixels| value.as_f32().round() as i64;
    let (width, height) = (round(bounds.size.width), round(bounds.size.height));
    if width <= 0 || height <= 0 {
        return last.clone();
    }
    WindowState {
        mode: NORMAL.into(),
        width,
        height,
        x: round(bounds.origin.x - work.x),
        y: round(bounds.origin.y - work.y),
    }
}

pub fn stored_width(cx: &App, key: &str, default: f32, min: f32, max: f32) -> Pixels {
    let width = setting(cx, key)
        .and_then(|raw| raw.trim().parse::<f64>().ok())
        .filter(|width| width.is_finite())
        .map_or(default, |width| (width as f32).clamp(min, max));
    px(width)
}

#[cfg(test)]
mod tests {
    use gpui_kit::{Bounds, point, px, size};

    use super::{WindowState, normal, parse};

    #[test]
    fn reads_a_stored_record() {
        let saved = parse(r#"{"mode":"normal","width":1280,"height":720,"x":40,"y":25}"#).unwrap();
        assert_eq!(saved, WindowState { mode: "normal".into(), width: 1280, height: 720, x: 40, y: 25 });
        assert_eq!(
            serde_json::to_string(&saved).unwrap(),
            r#"{"mode":"normal","width":1280,"height":720,"x":40,"y":25}"#
        );
        assert_eq!(parse(r#"{"mode":"maximised"}"#).unwrap().width, 0);
        assert_eq!(parse(""), None);
        assert_eq!(parse("{"), None);
        assert_eq!(parse(r#"{"mode":"normal","width":1280.5}"#), None, "integer fields reject fractions");
    }

    #[test]
    fn normal_bounds_are_kept_relative_to_the_work_area() {
        let last = WindowState { mode: "maximised".into(), width: 1000, height: 700, x: 5, y: 6 };
        let bounds = Bounds::new(point(px(100.4), px(60.)), size(px(1200.), px(800.)));
        let captured = normal(&last, bounds, point(px(0.), px(25.)));
        assert_eq!(captured, WindowState { mode: "normal".into(), width: 1200, height: 800, x: 100, y: 35 });
        let empty = Bounds::new(point(px(0.), px(0.)), size(px(0.), px(0.)));
        assert_eq!(normal(&last, empty, point(px(0.), px(0.))), last, "a zero size keeps the last record");
    }
}
