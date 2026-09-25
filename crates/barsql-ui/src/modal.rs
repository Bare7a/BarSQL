use std::collections::HashMap;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::dialog::Dialog;
use gpui_kit::component::scroll::Scrollbar;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme, StyledExt, WindowExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::i18n::t;
use crate::tokens::{ICON_MD, RADIUS, RADIUS_LG, TEXT_BASE, TEXT_MD, TEXT_XS};

#[derive(Clone, Copy)]
pub enum Size {
    Sm,
    Md,
    Lg,
    Xl,
    Rem(f32),
}

impl Size {
    fn width(self, window: &Window) -> Pixels {
        let (width, share) = match self {
            Self::Sm => (32., 0.9),
            Self::Md => (36.923, 0.9),
            Self::Lg => (49.231, 0.92),
            Self::Xl => (67.692, 0.94),
            Self::Rem(width) => (width, 0.9),
        };
        (window.rem_size() * width).min(window.viewport_size().width * share)
    }
}

#[derive(Default)]
struct Heights(HashMap<SharedString, Pixels>);

impl Global for Heights {}

pub struct Modal {
    id: SharedString,
    title: SharedString,
    extra: Option<SharedString>,
    size: Size,
    height: Option<Pixels>,
    closable: bool,
    danger: bool,
}

impl Modal {
    pub fn new(id: impl Into<SharedString>, title: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            extra: None,
            size: Size::Md,
            height: None,
            closable: true,
            danger: false,
        }
    }

    // Muted text after the title.
    pub fn extra(mut self, extra: impl Into<SharedString>) -> Self {
        self.extra = Some(extra.into());
        self
    }

    // Still capped at 90% of the window. The content fills it.
    pub fn height(mut self, height: Pixels) -> Self {
        self.height = Some(height);
        self
    }

    pub fn size(mut self, size: Size) -> Self {
        self.size = size;
        self
    }

    pub fn closable(mut self, closable: bool) -> Self {
        self.closable = closable;
        self
    }

    pub fn danger(mut self, danger: bool) -> Self {
        self.danger = danger;
        self
    }

    pub fn build(self, dialog: Dialog, content: impl IntoElement, window: &mut Window, cx: &mut App) -> Dialog {
        let theme = cx.theme().clone();
        let viewport = window.viewport_size();
        let max_height = (viewport.height * 0.9).floor();
        // Centre on the last measured height. Until there is one, sit a tenth of the way down.
        let top = match cx.try_global::<Heights>().and_then(|heights| heights.0.get(&self.id).copied()) {
            Some(height) => ((viewport.height - height.min(max_height)) / 2.).max(Pixels::ZERO).round(),
            None => viewport.height / 10.,
        };
        let id = self.id.clone();
        let close_group = SharedString::from(format!("{}-close", self.id));
        let header = h_flex()
            .flex_none()
            .justify_between()
            .gap(rems(0.923))
            .px(rems(1.231))
            .py(rems(1.077))
            .border_b_1()
            .border_color(theme.border)
            .when(self.danger, |el| el.text_color(theme.danger))
            .child(
                h_flex()
                    .min_w_0()
                    .child(div().min_w_0().truncate().font_semibold().text_size(TEXT_MD).child(self.title))
                    .when_some(self.extra, |el, extra| {
                        el.child(
                            div()
                                .flex_none()
                                .ml(rems(0.769))
                                .text_size(TEXT_XS)
                                .text_color(theme.muted_foreground)
                                .child(extra),
                        )
                    }),
            )
            .when(self.closable, |el| {
                let label = t(cx, "common.close");
                el.child(
                    div()
                        .id("modal-close")
                        .group(close_group.clone())
                        .debug_selector(|| "modal-close".into())
                        .flex_none()
                        .p(rems(0.231))
                        .rounded(RADIUS)
                        .cursor_pointer()
                        .hover(|style| style.bg(theme.secondary_hover))
                        .child(
                            svg()
                                .path(Lucide::X.path())
                                .size(ICON_MD)
                                .text_color(theme.muted_foreground)
                                .group_hover(close_group, |style| style.text_color(theme.foreground)),
                        )
                        .tooltip(move |window, cx| Tooltip::new(label.clone()).build(window, cx))
                        .on_click(|_, window, cx| window.close_dialog(cx)),
                )
            });
        dialog
            .close_button(false)
            .w(self.size.width(window))
            .margin_top(top)
            .p_0()
            .bg(theme.sidebar)
            .rounded(RADIUS_LG)
            .child(
                v_flex()
                    .relative()
                    .max_h(max_height)
                    .when_some(self.height, |el, height| el.h(height.min(max_height)))
                    .child(
                        canvas(
                            move |bounds, window, cx| {
                                let height = bounds.size.height;
                                let heights = &mut cx.default_global::<Heights>().0;
                                if heights.get(&id).is_none_or(|known| (*known - height).abs() > px(0.5)) {
                                    heights.insert(id, height);
                                    window.on_next_frame(|window, _| window.refresh());
                                }
                            },
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .size_full(),
                    )
                    .child(header)
                    .child(content),
            )
    }
}

pub fn body() -> Div {
    v_flex().p(rems(1.231)).gap(rems(0.923))
}

// Header and footer stay put. A plain scroll container, because GPUI Kit's scrollable wrapper adds no height
// to a column that sizes to its content.
pub fn scroll_body(handle: &ScrollHandle, body: Div) -> Div {
    div()
        .relative()
        .flex()
        .flex_col()
        .flex_auto()
        .min_h_0()
        .child(div().id("modal-scroll").flex_auto().min_h_0().overflow_y_scroll().track_scroll(handle).child(body))
        .child(
            div()
                .absolute()
                .top_0()
                .right_0()
                .bottom_0()
                .w(Scrollbar::width())
                .child(Scrollbar::vertical(handle).viewport_from_layout()),
        )
}

// Shrinks to the frame so the body can scroll.
pub fn scroll_content() -> Div {
    v_flex().flex_auto().min_h_0()
}

pub fn footer(cx: &App) -> Div {
    h_flex()
        .flex_none()
        .justify_end()
        .gap(rems(0.615))
        .px(rems(1.231))
        .py(rems(0.923))
        .border_t_1()
        .border_color(cx.theme().border)
}

pub fn description(text: impl Into<SharedString>, cx: &App) -> Div {
    div().text_size(TEXT_BASE).text_color(cx.theme().muted_foreground).child(text.into())
}

pub fn detail(id: impl Into<ElementId>, text: impl Into<SharedString>, cx: &App) -> Stateful<Div> {
    let theme = cx.theme();
    div()
        .id(id)
        .max_h(rems(12.308))
        .overflow_y_scroll()
        .py(rems(0.769))
        .px(rems(0.923))
        .rounded(RADIUS)
        .border_1()
        .border_color(theme.border)
        .bg(theme.background)
        .font_family(theme.mono_font_family.clone())
        .text_size(TEXT_XS)
        .text_color(theme.muted_foreground)
        .child(text.into())
}
