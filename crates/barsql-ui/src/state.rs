use std::future::Future;

use barsql_app::BarApp;
use gpui_kit::{App, Global, Task};

use crate::i18n::{I18n, t};
use crate::toast;

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

// Network futures run on tokio, never on GPUI's executors. A panic there ends the task empty, with a toast.
pub fn spawn<T: Send + 'static>(cx: &App, work: impl Future<Output = T> + Send + 'static) -> Task<Option<T>> {
    let handle = cx.global::<AppState>().bar.runtime().spawn(work);
    cx.spawn(async move |cx| match handle.await {
        Ok(value) => Some(value),
        Err(error) => {
            if error.is_panic() {
                cx.update(|cx| toast::error(t(cx, "errors.bug"), cx));
            }
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use gpui_kit::TestAppContext;

    use crate::test_support::Env;
    use crate::toast::{self, ToastKind};

    async fn boom() {
        panic!("boom")
    }

    #[gpui_kit::test]
    fn a_task_that_panics_shows_an_error_toast(cx: &mut TestAppContext) {
        let _env = Env::new(cx);
        let _task = cx.update(|cx| super::spawn(cx, boom()));
        let mut toasts = Vec::new();
        for _ in 0..1000 {
            cx.run_until_parked();
            toasts = cx.update(|cx| toast::messages(cx));
            if !toasts.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let said = "BarSQL hit a bug. The details are in crash.log in the data folder.";
        assert_eq!(toasts, [(ToastKind::Error, said.to_string())]);
    }
}
