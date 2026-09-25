use std::rc::Rc;
use std::time::Duration;

use crate::tokens::{RADIUS, TEXT_LG, TEXT_SM};
use gpui_kit::component::ActiveTheme;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

// Over five, the oldest is dropped. A zero duration never expires.
const MAX_TOASTS: usize = 5;
const DEFAULT_DURATION: Duration = Duration::from_millis(3500);
const ERROR_DURATION: Duration = Duration::from_millis(7000);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Success,
    Error,
}

type OnAction = Rc<dyn Fn(&mut Window, &mut App)>;

#[derive(Clone)]
pub struct ToastAction {
    pub label: SharedString,
    pub on_click: OnAction,
}

struct Toast {
    id: u64,
    message: SharedString,
    kind: ToastKind,
    action: Option<ToastAction>,
    _expiry: Task<()>,
}

#[derive(Default)]
pub struct Toasts {
    items: Vec<Toast>,
    next_id: u64,
}

impl Global for Toasts {}

pub fn success(message: impl Into<SharedString>, cx: &mut App) {
    push(message, ToastKind::Success, None, None, cx);
}

pub fn error(message: impl Into<SharedString>, cx: &mut App) {
    push(message, ToastKind::Error, None, None, cx);
}

pub fn info(message: impl Into<SharedString>, cx: &mut App) {
    push(message, ToastKind::Info, None, None, cx);
}

pub fn error_in(context: impl AsRef<str>, message: impl AsRef<str>, cx: &mut App) {
    error(format!("{}: {}", context.as_ref(), message.as_ref()), cx);
}

pub fn push(
    message: impl Into<SharedString>,
    kind: ToastKind,
    duration: Option<Duration>,
    action: Option<ToastAction>,
    cx: &mut App,
) {
    let message: SharedString = message.into();
    let message: SharedString = message.trim().to_string().into();
    if message.is_empty() {
        return;
    }
    let duration = duration.unwrap_or(if kind == ToastKind::Error { ERROR_DURATION } else { DEFAULT_DURATION });
    let toasts = cx.default_global::<Toasts>();
    toasts.next_id += 1;
    let id = toasts.next_id;
    let expiry = if duration.is_zero() {
        Task::ready(())
    } else {
        cx.spawn(async move |cx| {
            cx.background_executor().timer(duration).await;
            cx.update(|cx| dismiss(id, cx));
        })
    };
    cx.update_global::<Toasts, _>(|toasts, _| {
        toasts.items.push(Toast { id, message, kind, action, _expiry: expiry });
        let overflow = toasts.items.len().saturating_sub(MAX_TOASTS);
        toasts.items.drain(..overflow);
    });
}

pub fn dismiss(id: u64, cx: &mut App) {
    if cx.has_global::<Toasts>() {
        cx.update_global::<Toasts, _>(|toasts, _| toasts.items.retain(|toast| toast.id != id));
    }
}

#[cfg(test)]
pub fn messages(cx: &App) -> Vec<(ToastKind, String)> {
    cx.try_global::<Toasts>()
        .map(|toasts| toasts.items.iter().map(|toast| (toast.kind, toast.message.to_string())).collect())
        .unwrap_or_default()
}

// Newest on top.
pub fn render(window: &Window, cx: &App) -> Option<AnyElement> {
    let toasts = cx.try_global::<Toasts>().filter(|toasts| !toasts.items.is_empty())?;
    let theme = cx.theme();
    let rem = window.rem_size();
    let max_width = (rem * 27.692).min(window.viewport_size().width - px(32.));
    let items = toasts.items.iter().map(|toast| {
        let id = toast.id;
        let stripe = match toast.kind {
            ToastKind::Info => theme.primary,
            ToastKind::Success => theme.success,
            ToastKind::Error => theme.danger,
        };
        h_row()
            .id(("toast", id as usize))
            .gap(rems(0.769))
            .pl(rem * 0.923 + px(2.))
            .pr(rems(0.923))
            .py(rems(0.769))
            .bg(theme.popover)
            .text_color(theme.foreground)
            .border_1()
            .border_color(theme.border)
            .rounded(RADIUS)
            .shadow_lg()
            .occlude()
            .child(div().w(px(3.)).h_full().absolute().left_0().top_0().bottom_0().rounded_l(RADIUS).bg(stripe))
            .child(div().flex_1().min_w_0().child(toast.message.clone()))
            .when_some(toast.action.clone(), |el, action| {
                el.child(
                    div()
                        .id(("toast-action", id as usize))
                        .flex_none()
                        .px(rems(0.615))
                        .py(rems(0.231))
                        .rounded(RADIUS)
                        .bg(theme.primary)
                        .text_color(theme.primary_foreground)
                        .text_size(TEXT_SM)
                        .cursor_pointer()
                        .child(action.label.clone())
                        .on_click(move |_, window, cx| {
                            (action.on_click)(window, cx);
                            dismiss(id, cx);
                        }),
                )
            })
            .child(
                div()
                    .id(("toast-dismiss", id as usize))
                    .flex_none()
                    .size(rems(1.692))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(RADIUS)
                    .text_size(TEXT_LG)
                    .text_color(theme.muted_foreground)
                    .cursor_pointer()
                    .hover(|style| style.bg(theme.accent).text_color(theme.foreground))
                    .child("×")
                    .on_click(move |_, _, cx| dismiss(id, cx)),
            )
    });
    Some(
        div()
            .absolute()
            .bottom(px(32.))
            .left(px(16.))
            .max_w(max_width)
            .flex()
            .flex_col_reverse()
            .gap(rems(0.615))
            .children(items)
            .into_any_element(),
    )
}

fn h_row() -> Div {
    div().relative().flex().flex_row().items_center()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use gpui_kit::TestAppContext;

    use super::{ToastKind, error, messages, push, success};

    #[gpui_kit::test]
    fn five_at_most_and_errors_linger_longer(cx: &mut TestAppContext) {
        cx.update(|cx| {
            for n in 1..=6 {
                push(format!("  note {n} "), ToastKind::Info, None, None, cx);
            }
            push("   ", ToastKind::Info, None, None, cx);
        });
        let shown: Vec<String> = cx.update(|cx| messages(cx)).into_iter().map(|(_, m)| m).collect();
        assert_eq!(shown, ["note 2", "note 3", "note 4", "note 5", "note 6"], "trimmed, blank skipped, oldest dropped");

        cx.update(|cx| {
            super::dismiss(u64::MAX, cx);
            error("broken", cx);
            success("saved", cx);
            push("sticky", ToastKind::Info, Some(Duration::ZERO), None, cx);
        });
        cx.executor().advance_clock(Duration::from_millis(3500));
        cx.run_until_parked();
        let shown = cx.update(|cx| messages(cx));
        assert_eq!(shown, [(ToastKind::Error, "broken".to_string()), (ToastKind::Info, "sticky".to_string())]);
        cx.executor().advance_clock(Duration::from_millis(3500));
        cx.run_until_parked();
        assert_eq!(cx.update(|cx| messages(cx)), [(ToastKind::Info, "sticky".to_string())]);
    }
}
