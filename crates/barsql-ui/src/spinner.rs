use std::sync::LazyLock;
use std::time::Instant;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::Icon;
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
