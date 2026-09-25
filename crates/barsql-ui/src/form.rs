use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{ActiveTheme, Icon, IconName, Sizable, StyledExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::tokens::{
    DIMMED, ICON_MD, ICON_SM, ICON_XL, ICON_XS, RADIUS, RADIUS_SM, TEXT_2XS, TEXT_BASE, TEXT_SM, TEXT_XS, TINT,
    TINT_BORDER,
};

// 30px tall with body-size text.
pub fn input(state: &Entity<InputState>, window: &Window, cx: &App) -> Input {
    field(state, rems(1.), rems(0.769), window, cx)
}

pub fn filter_input(state: &Entity<InputState>, window: &Window, cx: &App) -> Input {
    field(state, rems(0.923), rems(0.615), window, cx)
}

// Focus thickens the border to 2px, so the padding drops 1px to keep the text still.
fn field(state: &Entity<InputState>, text: Rems, padding: Rems, window: &Window, cx: &App) -> Input {
    let theme = cx.theme();
    let focused = state.read(cx).focus_handle(cx).is_focused(window);
    let input = Input::new(state)
        .focus_bordered(false)
        .bg(theme.background)
        .text_size(text)
        .context_menu(crate::context_menu::input(state));
    let input = match focused {
        true => input.border_2().border_color(theme.ring).px(padding.to_pixels(window.rem_size()) - px(1.)),
        false => input.px(padding),
    };
    Styled::h(input, rems(2.308))
}

pub fn filter_bar(field: impl IntoElement, cx: &App) -> Div {
    h_flex()
        .flex_none()
        .gap(rems(0.615))
        .p(rems(0.615))
        .child(Icon::new(IconName::Search).size(ICON_SM).text_color(cx.theme().muted_foreground))
        .child(div().flex_1().min_w_0().child(field))
}

pub fn filter_button(id: impl Into<ElementId>, icon: impl Into<Icon>) -> Button {
    Button::new(id).small().size(rems(2.308)).p_0().child(icon.into().size(ICON_XS))
}

// 22px tall with an 11px label. GPUI Kit's small button is 19.5px and shrinks its icon to 11.4px, so the icon
// and label go in as children, sized here.
pub trait ToolButton {
    fn tool(self, icon: impl Into<Icon>, icon_size: Rems, label: impl Into<SharedString>) -> Self;
    fn tool_icon(self, icon: impl Into<Icon>, icon_size: Rems) -> Self;
    fn tool_label(self, label: impl Into<SharedString>) -> Self;
}

impl ToolButton for Button {
    fn tool(self, icon: impl Into<Icon>, icon_size: Rems, label: impl Into<SharedString>) -> Self {
        tool_frame(self)
            .child(h_flex().gap(rems(0.308)).text_size(TEXT_XS).child(icon.into().size(icon_size)).child(label.into()))
    }

    fn tool_icon(self, icon: impl Into<Icon>, icon_size: Rems) -> Self {
        tool_frame(self).child(icon.into().size(icon_size))
    }

    fn tool_label(self, label: impl Into<SharedString>) -> Self {
        tool_frame(self).child(div().text_size(TEXT_XS).child(label.into()))
    }
}

fn tool_frame(button: Button) -> Button {
    button.small().h(rems(1.692)).px(rems(0.615))
}

pub fn label(text: impl Into<SharedString>, cx: &App) -> Div {
    div().mb(rems(0.308)).text_size(TEXT_XS).text_color(cx.theme().muted_foreground).child(text.into())
}

pub fn hint(text: impl Into<SharedString>, cx: &App) -> Div {
    div().mt(rems(0.462)).text_size(TEXT_XS).text_color(cx.theme().muted_foreground).child(text.into())
}

pub fn group(label_text: impl Into<SharedString>, control: impl IntoElement, cx: &App) -> Div {
    v_flex().min_w_0().child(label(label_text, cx)).child(control)
}

pub fn row(left: impl IntoElement, right: impl IntoElement) -> Div {
    h_flex()
        .items_start()
        .gap(rems(0.769))
        .child(div().flex_1().min_w_0().child(left))
        .child(div().flex_1().min_w_0().child(right))
}

pub fn toggle_group() -> Div {
    h_flex().gap(rems(0.308))
}

pub fn toggle(id: impl Into<ElementId>, label: impl Into<SharedString>, active: bool) -> Button {
    Button::new(id).small().flex_1().h(rems(1.692)).label(label).when(active, |button| button.primary())
}

pub fn progress(label: impl Into<SharedString>, fraction: f32, cx: &App) -> Div {
    let theme = cx.theme();
    v_flex()
        .child(div().mt(rems(0.308)).text_size(TEXT_SM).text_color(theme.muted_foreground).child(label.into()))
        .child(
            div()
                .mt(rems(0.462))
                .h(rems(0.25))
                .w_full()
                .rounded_full()
                .bg(theme.secondary_hover)
                .child(div().h_full().w(relative(fraction.clamp(0., 1.))).rounded_full().bg(theme.primary)),
        )
}

pub fn section(cx: &App) -> Div {
    v_flex().gap(rems(0.923)).pt(rems(0.923)).border_t_1().border_color(cx.theme().border)
}

pub fn select(id: impl Into<ElementId>, label: impl Into<SharedString>, cx: &App) -> Button {
    let theme = cx.theme();
    Button::new(id).outline().w_full().h(rems(2.308)).pl(rems(0.769)).pr(rems(0.615)).bg(theme.background).child(
        h_flex()
            .w_full()
            .justify_between()
            .gap(rems(0.462))
            .text_size(TEXT_BASE)
            .text_color(theme.foreground)
            .child(div().min_w_0().truncate().child(label.into()))
            .child(Icon::new(IconName::ChevronDown).size(ICON_MD).text_color(theme.muted_foreground)),
    )
}

pub fn select_small(id: impl Into<ElementId>, label: impl Into<SharedString>, cx: &App) -> Button {
    let theme = cx.theme();
    Button::new(id)
        .outline()
        .h(rems(1.846))
        .min_w(rems(5.385))
        .pl(rems(0.615))
        .pr(rems(0.462))
        .bg(theme.background)
        .child(
            h_flex()
                .gap(rems(0.462))
                .text_size(TEXT_XS)
                .text_color(theme.foreground)
                .child(label.into())
                .child(Icon::new(IconName::ChevronDown).size(ICON_XS).text_color(theme.muted_foreground)),
        )
}

// 18px box. The caller flips the value on click.
pub fn checkbox(
    id: impl Into<SharedString>,
    checked: bool,
    label: impl Into<SharedString>,
    disabled: bool,
    cx: &App,
) -> Stateful<Div> {
    let theme = cx.theme();
    let id: SharedString = id.into();
    let group = SharedString::from(format!("{id}-box"));
    let border = theme.muted_foreground.opacity(TINT_BORDER);
    let hover = theme.muted_foreground;
    h_flex()
        .id(ElementId::Name(id))
        .group(group.clone())
        .gap(rems(0.615))
        .when(disabled, |el| el.opacity(DIMMED))
        .when(!disabled, |el| el.cursor_pointer())
        .child(
            div()
                .flex_none()
                .size(rems(1.385))
                .flex()
                .items_center()
                .justify_center()
                .rounded(RADIUS_SM)
                .border_1()
                .map(|el| match checked {
                    true => el
                        .bg(theme.primary)
                        .border_color(theme.primary)
                        .child(Icon::new(IconName::Check).size(ICON_XS).text_color(theme.primary_foreground)),
                    false => el
                        .bg(theme.secondary)
                        .border_color(border)
                        .when(!disabled, |el| el.group_hover(group, |style| style.border_color(hover))),
                }),
        )
        .child(div().flex_1().min_w_0().text_size(TEXT_BASE).font_medium().child(label.into()))
}

pub fn alert(color: Hsla) -> Div {
    div()
        .py(rems(0.615))
        .px(rems(0.769))
        .rounded(RADIUS)
        .border_1()
        .border_color(color.opacity(TINT_BORDER))
        .bg(color.opacity(TINT))
        .text_size(TEXT_SM)
}

pub fn error(text: impl Into<SharedString>, cx: &App) -> Div {
    let danger = cx.theme().danger;
    alert(danger).text_color(danger).child(text.into())
}

pub fn badge(text: impl Into<SharedString>, color: Hsla) -> Div {
    div()
        .flex_none()
        .px(rems(0.385))
        .rounded(RADIUS_SM)
        .text_size(TEXT_2XS)
        .font_semibold()
        .text_color(color)
        .bg(color.opacity(TINT))
        .child(text.into())
}

pub fn caption(text: impl Into<SharedString>, cx: &App) -> Div {
    let text: SharedString = text.into();
    div().text_size(TEXT_XS).font_semibold().text_color(cx.theme().muted_foreground).child(text.to_uppercase())
}

pub fn empty_state(icon: Option<Icon>, text: impl Into<SharedString>, cx: &App) -> Div {
    v_flex()
        .p_4()
        .gap_2()
        .items_center()
        .text_center()
        .text_size(TEXT_BASE)
        .text_color(cx.theme().muted_foreground)
        .when_some(icon, |el, icon| el.child(icon.size(ICON_XL)))
        .child(text.into())
}
