use std::future::Future;

use barsql_app::BarApp;
use gpui_kit::{App, AppContext as _, Global, Task};

use crate::i18n::I18n;

pub struct AppState {
    pub bar: BarApp,
}

impl Global for AppState {}

pub fn init(bar: BarApp, cx: &mut App) {
    let lang = bar.settings().get("barsql-language").cloned().unwrap_or_default();
    cx.set_global(I18n::new(&lang));
    cx.set_global(AppState { bar });
}

pub fn bar(cx: &App) -> BarApp {
    cx.global::<AppState>().bar.clone()
}

pub fn setting(cx: &App, key: &str) -> Option<String> {
    cx.global::<AppState>().bar.settings().get(key).cloned()
}

pub fn set_setting(cx: &App, key: &str, value: &str) {
    let _ = cx.global::<AppState>().bar.set_setting(key, value);
}

// Stored as "1" or "0".
pub fn setting_bool(cx: &App, key: &str, default: bool) -> bool {
    setting(cx, key).map_or(default, |value| value == "1")
}

pub fn set_setting_bool(cx: &App, key: &str, value: bool) {
    set_setting(cx, key, if value { "1" } else { "0" });
}

// Unreadable JSON counts as unset.
pub fn setting_json<T: serde::de::DeserializeOwned + Default>(cx: &App, key: &str) -> T {
    setting(cx, key).and_then(|raw| serde_json::from_str(&raw).ok()).unwrap_or_default()
}

pub fn set_setting_json<T: serde::Serialize>(cx: &App, key: &str, value: &T) {
    if let Ok(raw) = serde_json::to_string(value) {
        set_setting(cx, key, &raw);
    }
}

// Network futures run on tokio, never on GPUI's executors.
pub fn spawn<T: Send + 'static>(cx: &App, work: impl Future<Output = T> + Send + 'static) -> Task<Option<T>> {
    let handle = cx.global::<AppState>().bar.runtime().spawn(work);
    cx.background_spawn(async move { handle.await.ok() })
}
