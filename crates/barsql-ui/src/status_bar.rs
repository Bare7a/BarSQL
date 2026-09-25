use crate::tokens::{ICON_XS, TEXT_XS};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::{ActiveTheme, Icon, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

pub struct StatusBarState {
    pub connection: Option<(SharedString, bool)>,
    pub read_only: bool,
    pub status: SharedString,
    pub error: bool,
}

pub fn render(state: StatusBarState, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    let left = match state.connection {
        Some((label, connected)) => h_flex()
            .gap_1p5()
            .child(div().size(px(8.)).rounded_full().bg(if connected { theme.success } else { theme.muted_foreground }))
            .child(label)
            .when(state.read_only, |el| {
                el.child(div().text_color(theme.warning).child(crate::i18n::t(cx, "app.readOnly")))
            }),
        None => h_flex().child(crate::i18n::t(cx, "app.statusNoConnection")),
    };
    h_flex()
        .h(px(22.))
        .flex_none()
        .px_3()
        .justify_between()
        .text_size(TEXT_XS)
        .text_color(theme.muted_foreground)
        .bg(theme.status_bar)
        .border_t_1()
        .border_color(theme.status_bar_border)
        .child(left)
        .child(
            h_flex()
                .min_w_0()
                .gap_1()
                .when(state.error, |el| {
                    el.text_color(theme.danger).child(Icon::new(Lucide::CircleAlert).size(ICON_XS).flex_none())
                })
                .child(div().truncate().child(state.status)),
        )
}
