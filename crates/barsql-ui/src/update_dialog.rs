use std::io;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use barsql_app::update::{self, InstallEvent, Release, Stage, Staged};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::progress::Progress;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::text::TextView;
use gpui_kit::component::{ActiveTheme, Icon, StyledExt, WindowExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::form;
use crate::i18n::{t, t_with};
use crate::modal::{self, Modal};
use crate::scrollbars::ScrollbarsOnHover as _;
use crate::spinner::Spinner;
use crate::toast::{self, ToastAction, ToastKind};
use crate::tokens::{ICON_MD, ICON_SM, RADIUS};

const STARTUP_DELAY: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Transfer {
    pub written: u64,
    pub total: u64,
    pub rate: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum UpdateState {
    Checking,
    UpToDate,
    Available(Release),
    Downloading(Release, Option<Transfer>),
    Verifying(Release),
    Installing(Release),
    Ready(Release, Staged),
    Failed { stage: Stage, message: String, release: Option<Release> },
}

impl UpdateState {
    fn release(&self) -> Option<&Release> {
        match self {
            Self::Available(release)
            | Self::Downloading(release, _)
            | Self::Verifying(release)
            | Self::Installing(release)
            | Self::Ready(release, _) => Some(release),
            Self::Failed { release, .. } => release.as_ref(),
            _ => None,
        }
    }

    fn installing(&self) -> bool {
        matches!(self, Self::Downloading(..) | Self::Verifying(_) | Self::Installing(_) | Self::Ready(..))
    }
}

// Lasts for the session only.
#[derive(Default)]
struct Skipped(Option<String>);

impl Global for Skipped {}

fn is_skipped(version: &str, cx: &App) -> bool {
    cx.try_global::<Skipped>().and_then(|skipped| skipped.0.as_deref()) == Some(version)
}

// Keeps the dialog alive past its window, so Close leaves a download running and Check for Updates
// reopens it where it left off.
struct Flow(Entity<UpdateDialog>);

impl Global for Flow {}

// BARSQL_UPDATE_API points the check at a test server.
fn api_base() -> String {
    std::env::var("BARSQL_UPDATE_API").ok().filter(|base| !base.is_empty()).unwrap_or(update::GITHUB_API.into())
}

fn check(base: String, cx: &App) -> Task<Result<Option<Release>, String>> {
    cx.background_spawn(async move {
        let kind = update::install().kind;
        update::check(&base, barsql_app::VERSION, kind, update::platform(), update::arch())
    })
}

// B to GB, one decimal above bytes, ties rounded up.
fn format_bytes(n: u64) -> String {
    if n == 0 {
        return String::new();
    }
    let units = ["B", "KB", "MB", "GB"];
    let i = ((n as f64).ln() / 1024f64.ln()).floor().min(3.) as usize;
    let value = n as f64 / 1024f64.powi(i as i32);
    if i == 0 { format!("{value:.0} {}", units[i]) } else { format!("{:.1} {}", (value * 10.).round() / 10., units[i]) }
}

// The last argument is the macOS password prompt.
type Apply = Rc<dyn Fn(&Release, &Path, &str) -> io::Result<()>>;

pub struct UpdateDialog {
    base: String,
    state: UpdateState,
    task: Task<()>,
    stop: Arc<AtomicBool>,
    apply: Apply,
    notes_scroll: ScrollHandle,
}

impl Drop for UpdateDialog {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl UpdateDialog {
    fn new(base: String, state: Option<UpdateState>, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            base,
            state: UpdateState::Checking,
            task: Task::ready(()),
            stop: Arc::default(),
            apply: Rc::new(|release, staged, prompt| {
                update::spawn_helper(update::install(), staged, &release.version, prompt)
            }),
            notes_scroll: ScrollHandle::new(),
        };
        match state {
            Some(state) => this.state = state,
            None => this.start(cx),
        }
        this
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        self.state = UpdateState::Checking;
        cx.notify();
        let task = check(self.base.clone(), cx);
        self.task = cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.state = match result {
                    Ok(Some(release)) if !is_skipped(&release.version, cx) => UpdateState::Available(release),
                    Ok(_) => UpdateState::UpToDate,
                    Err(message) => UpdateState::Failed { stage: Stage::Check, message, release: None },
                };
                cx.notify();
            });
        });
    }

    fn retry(&mut self, cx: &mut Context<Self>) {
        match &self.state {
            UpdateState::Failed { release: Some(_), .. } => self.install(cx),
            _ => self.start(cx),
        }
    }

    fn install(&mut self, cx: &mut Context<Self>) {
        let Some(release) = self.state.release().cloned() else { return };
        self.stop.store(true, Ordering::Relaxed);
        let stop = Arc::new(AtomicBool::new(false));
        self.stop = stop.clone();
        self.state = UpdateState::Downloading(release.clone(), None);
        cx.notify();
        let (events, received) = async_channel::unbounded();
        let job = cx.background_spawn({
            let release = release.clone();
            async move {
                update::download_and_stage(&release, &stop, |event| {
                    let _ = events.try_send(event);
                })
            }
        });
        self.task = cx.spawn(async move |this, cx| {
            while let Ok(event) = received.recv().await {
                let state = match event {
                    InstallEvent::Downloading { written, total, rate } => {
                        UpdateState::Downloading(release.clone(), Some(Transfer { written, total, rate }))
                    }
                    InstallEvent::Verifying => UpdateState::Verifying(release.clone()),
                    InstallEvent::Installing => UpdateState::Installing(release.clone()),
                };
                if this.update(cx, |this, cx| this.set_state(state, cx)).is_err() {
                    return;
                }
            }
            let state = match job.await {
                Ok(staged) => UpdateState::Ready(release, staged),
                Err(error) => {
                    UpdateState::Failed { stage: error.stage, message: error.message, release: Some(release) }
                }
            };
            let _ = this.update(cx, |this, cx| this.set_state(state, cx));
        });
    }

    fn set_state(&mut self, state: UpdateState, cx: &mut Context<Self>) {
        self.state = state;
        cx.notify();
    }

    // The helper waits for this process to exit, then puts the staged version in place.
    fn restart(&mut self, cx: &mut Context<Self>) {
        let UpdateState::Ready(release, staged) = &self.state else { return };
        match (self.apply)(release, &staged.path, &t(cx, "update.adminPrompt")) {
            Ok(()) => cx.quit(),
            Err(error) => {
                let _ = std::fs::remove_dir_all(&staged.dir);
                let message = format!("spawn helper: {error}");
                let state = UpdateState::Failed { stage: Stage::Install, message, release: Some(release.clone()) };
                self.set_state(state, cx);
            }
        }
    }

    // Later checks this session treat the release as no update.
    fn skip(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let UpdateState::Available(release) = &self.state {
            cx.set_global(Skipped(Some(release.version.clone())));
        }
        window.close_dialog(cx);
    }

    fn footer(&self, cx: &mut Context<Self>) -> Option<Div> {
        let close =
            Button::new("update-close").label(t(cx, "common.close")).on_click(|_, window, cx| window.close_dialog(cx));
        let primary = |id: &'static str, label: &str| Button::new(id).primary().label(label.to_string());
        let footer = modal::footer(cx);
        match &self.state {
            UpdateState::Checking | UpdateState::Verifying(_) | UpdateState::Installing(_) => None,
            UpdateState::UpToDate | UpdateState::Downloading(..) => Some(footer.child(close)),
            UpdateState::Failed { .. } => Some(footer.child(close).child(
                primary("update-retry", &t(cx, "update.retry")).on_click(cx.listener(|this, _, _, cx| this.retry(cx))),
            )),
            UpdateState::Ready(..) => Some(
                footer.child(close).child(
                    primary("update-restart", &t(cx, "update.restart"))
                        .on_click(cx.listener(|this, _, _, cx| this.restart(cx))),
                ),
            ),
            UpdateState::Available(_) => Some(
                footer
                    .justify_between()
                    .child(
                        h_flex()
                            .gap(rems(0.615))
                            .child(
                                Button::new("update-skip")
                                    .ghost()
                                    .label(t(cx, "update.skip"))
                                    .on_click(cx.listener(|this, _, window, cx| this.skip(window, cx))),
                            )
                            .child(
                                Button::new("update-remind")
                                    .ghost()
                                    .label(t(cx, "update.remind"))
                                    .on_click(|_, window, cx| window.close_dialog(cx)),
                            ),
                    )
                    .child(
                        h_flex().gap(rems(0.615)).child(close).child(
                            primary("update-install", &t(cx, "update.install"))
                                .on_click(cx.listener(|this, _, _, cx| this.install(cx))),
                        ),
                    ),
            ),
        }
    }

    fn title(&self, cx: &App) -> SharedString {
        t(
            cx,
            match &self.state {
                UpdateState::Checking => "update.checking",
                UpdateState::Available(_) => "update.available",
                UpdateState::Downloading(..) => "update.downloading",
                UpdateState::Verifying(_) => "update.verifying",
                UpdateState::Installing(_) => "update.installing",
                UpdateState::Ready(..) => "update.ready",
                UpdateState::UpToDate => "update.upToDate",
                UpdateState::Failed { .. } => "update.failed",
            },
        )
    }

    fn transfer(&self, transfer: Option<Transfer>, cx: &App) -> AnyElement {
        let theme = cx.theme();
        let (label, rate, percent) = match transfer {
            None => (t(cx, "update.starting"), String::new(), None),
            Some(Transfer { written, total, rate }) => {
                let rate = if rate == 0 { String::new() } else { format!("{}/s", format_bytes(rate)) };
                if total > 0 {
                    let percent = (written as f64 / total as f64 * 100.).round();
                    let (written, total) = (format_bytes(written), format_bytes(total));
                    let label = t_with(
                        cx,
                        "update.progress",
                        &[("percent", &percent.to_string()), ("written", &written), ("total", &total)],
                    );
                    (label, rate, Some(percent as f32))
                } else {
                    (t_with(cx, "update.downloaded", &[("written", &format_bytes(written))]), rate, None)
                }
            }
        };
        v_flex()
            .gap(rems(0.462))
            .p(rems(0.769))
            .rounded(RADIUS)
            .border_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .justify_between()
                    .child(div().font_medium().child(label))
                    .child(div().text_color(theme.muted_foreground).child(rate)),
            )
            .child(match percent {
                Some(percent) => Progress::new("update-progress").value(percent),
                None => Progress::new("update-progress").loading(true),
            })
            .into_any_element()
    }
}

impl Render for UpdateDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let current = barsql_app::VERSION;
        let (icon, color) = match &self.state {
            UpdateState::Verifying(_) => (Lucide::CircleCheck, theme.primary),
            UpdateState::Ready(..) | UpdateState::UpToDate => (Lucide::CircleCheck, theme.success),
            UpdateState::Failed { .. } => (Lucide::TriangleAlert, theme.danger),
            _ => (Lucide::Download, theme.primary),
        };
        let subtitle = match &self.state {
            UpdateState::UpToDate => Some(format!("v{current}")),
            UpdateState::Failed { .. } => None,
            state => state.release().map(|release| {
                let size = format_bytes(release.asset.size);
                let size = if size.is_empty() { size } else { format!(" · {size}") };
                format!("{current} → v{}{size}", release.version)
            }),
        };
        let spinner = |text: SharedString| {
            h_flex()
                .gap(rems(0.615))
                .text_color(theme.muted_foreground)
                .child(Spinner::new(ICON_SM))
                .child(text)
                .into_any_element()
        };
        let body = match &self.state {
            UpdateState::Checking => Some(spinner(t(cx, "update.contacting"))),
            UpdateState::Verifying(_) => Some(spinner(t(cx, "update.checksum"))),
            UpdateState::Installing(_) => Some(spinner(t(cx, "update.unpacking"))),
            UpdateState::Downloading(_, transfer) => Some(self.transfer(*transfer, cx)),
            UpdateState::Available(release) | UpdateState::Ready(release, _) if !release.notes.trim().is_empty() => {
                let notes = div()
                    .id("update-notes")
                    .max_h(rems(18.))
                    .overflow_y_scroll()
                    .track_scroll(&self.notes_scroll)
                    .p(rems(0.769))
                    .rounded(RADIUS)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.background)
                    .child(TextView::markdown("update-notes-text", release.notes.clone()));
                Some(
                    div()
                        .relative()
                        .child(notes)
                        .vertical_scrollbar(&self.notes_scroll)
                        .scrollbars_on_hover()
                        .into_any_element(),
                )
            }
            UpdateState::Failed { stage, message, .. } => {
                let stage = t(cx, &format!("update.stage.{}", stage.key()));
                let text = t_with(cx, "update.during", &[("stage", &stage), ("message", message)]);
                Some(form::error(text, cx).into_any_element())
            }
            _ => None,
        };
        v_flex()
            .child(
                modal::body()
                    .when_some(subtitle, |el, subtitle| {
                        el.child(
                            h_flex()
                                .gap(rems(0.615))
                                .child(Icon::new(icon).size(ICON_MD).text_color(color))
                                .child(div().text_color(theme.muted_foreground).child(subtitle)),
                        )
                    })
                    .children(body),
            )
            .children(self.footer(cx))
    }
}

pub fn open(window: &mut Window, cx: &mut App) -> Entity<UpdateDialog> {
    open_in(api_base(), None, window, cx)
}

// A running or staged install reopens as it is. Passing a state skips the check, for screenshots.
pub fn open_in(base: String, state: Option<UpdateState>, window: &mut Window, cx: &mut App) -> Entity<UpdateDialog> {
    let running = cx
        .try_global::<Flow>()
        .map(|flow| flow.0.clone())
        .filter(|view| state.is_none() && view.read(cx).state.installing());
    let view = running.unwrap_or_else(|| cx.new(|cx| UpdateDialog::new(base, state, cx)));
    cx.set_global(Flow(view.clone()));
    let dialog = view.clone();
    window.open_dialog(cx, move |frame, window, cx| {
        let title = dialog.read(cx).title(cx);
        Modal::new("update", title).build(frame, dialog.clone(), window, cx)
    });
    view
}

pub fn check_on_startup(cx: &mut App) {
    check_after(api_base(), STARTUP_DELAY, cx);
}

fn check_after(base: String, delay: Duration, cx: &mut App) {
    cx.spawn(async move |cx| {
        cx.background_executor().timer(delay).await;
        let task = cx.update(|cx| check(base, cx));
        let Ok(Some(release)) = task.await else { return };
        cx.update(|cx| {
            if is_skipped(&release.version, cx) {
                return;
            }
            let message = t_with(cx, "toast.updateAvailable", &[("version", &release.version)]);
            let action = ToastAction {
                label: t(cx, "toast.update"),
                on_click: Rc::new(|window, cx| {
                    open(window, cx);
                }),
            };
            toast::push(message, ToastKind::Info, Some(Duration::ZERO), Some(action), cx);
        });
    })
    .detach();
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod format_tests {
    use super::format_bytes;

    #[test]
    fn sizes_format_with_one_decimal_above_bytes() {
        let cases = [
            (0, ""),
            (512, "512 B"),
            (1024, "1.0 KB"),
            (1280, "1.3 KB"),
            (1536, "1.5 KB"),
            (12_582_912, "12.0 MB"),
            (5 << 40, "5120.0 GB"),
        ];
        for (bytes, text) in cases {
            assert_eq!(format_bytes(bytes), text, "{bytes}");
        }
    }
}
