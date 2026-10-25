use std::sync::LazyLock;
use std::time::Instant;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::{ActiveTheme, Icon};
use gpui_kit::*;

// One turn a second. Reduced motion is on app-wide and freezes GPUI Kit's spinner, so this one keeps its own clock.
#[derive(IntoElement)]
pub struct Spinner {
    size: Rems,
}

impl Spinner {
    pub fn new(size: Rems) -> Self {
        Self { size }
    }
}

impl RenderOnce for Spinner {
    fn render(self, window: &mut Window, _: &mut App) -> impl IntoElement {
        static EPOCH: LazyLock<Instant> = LazyLock::new(Instant::now);
        window.request_animation_frame();
        let turn = EPOCH.elapsed().as_secs_f32().fract();
        Icon::new(Lucide::LoaderCircle).size(self.size).transform(Transformation::rotate(percentage(turn)))
    }
}

// An indeterminate progress line along the top of a relative parent. On its own clock, like the spinner.
#[derive(IntoElement)]
pub struct ProgressLine;

impl RenderOnce for ProgressLine {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        static EPOCH: LazyLock<Instant> = LazyLock::new(Instant::now);
        window.request_animation_frame();
        let along = (EPOCH.elapsed().as_secs_f32() / 1.2).fract();
        let accent = cx.theme().primary;
        div()
            .debug_selector(|| "run-progress".into())
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .h(px(2.))
            .overflow_hidden()
            .bg(accent.opacity(0.15))
            .child(div().absolute().top_0().bottom_0().left(relative(1.3 * along - 0.3)).w(relative(0.3)).bg(accent))
    }
}
