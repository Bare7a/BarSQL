use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::menu::{DropdownMenu, PopupMenu};
use gpui_kit::component::{ActiveTheme, Icon, IconName, Sizable, StyledExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::tokens::{
    DIMMED, ICON_2XS, ICON_MD, ICON_SM, ICON_XL, ICON_XS, RADIUS, RADIUS_SM, TEXT_2XS, TEXT_BASE, TEXT_SM, TEXT_XS,
    TINT, TINT_BORDER,
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
        .py_0()
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

// A select's menu is as wide as the select, or wider for a longer item. GPUI Kit builds the menu while rendering
// the frame it opens in, before this frame's layout, so the width is the one measured a frame earlier and kept
// in state keyed by the select.
pub trait SelectMenu {
    fn select_menu(
        self,
        window: &mut Window,
        cx: &mut App,
        items: impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static,
    ) -> Div;
}

impl SelectMenu for Button {
    fn select_menu(
        mut self,
        window: &mut Window,
        cx: &mut App,
        items: impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static,
    ) -> Div {
        let id = self.interactivity().element_id.clone().unwrap_or_else(|| "select".into());
        let key = ElementId::NamedChild(Arc::new(id), "menu-width".into());
        let width = window.use_keyed_state(key, cx, |_, _| Rc::new(Cell::new(Pixels::ZERO))).read(cx).clone();
        let measured = width.clone();
        // GPUI Kit's own minimum, which a smaller min_w would replace.
        let floor = window.rem_size() * 8.;
        div()
            .relative()
            .child(self.dropdown_menu(move |menu, window, cx| items(menu, window, cx).min_w(width.get().max(floor))))
            .child(
                canvas(move |bounds, _, _| measured.set(bounds.size.width), |_, _, _, _| {})
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full(),
            )
    }
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

// A 24px row of a column list, which toggles as a whole: a check box filled with the colour when on, the name, and
// the type faintly after it. The name takes the row's text colour. A row that isn't `enabled` dims.
pub fn pick_row(
    id: SharedString,
    on: Option<Hsla>,
    enabled: bool,
    name: impl Into<SharedString>,
    type_name: impl Into<SharedString>,
    cx: &App,
) -> Stateful<Div> {
    let theme = cx.theme();
    let (hover, check, muted) = (theme.accent, theme.primary_foreground, theme.muted_foreground);
    let group = SharedString::from(format!("{id}-row"));
    let type_name: SharedString = type_name.into();
    let mark =
        div().flex_none().size(rems(1.077)).flex().items_center().justify_center().rounded(RADIUS_SM).border_1().map(
            |el| match on {
                Some(color) => {
                    el.bg(color).border_color(color).child(Icon::new(IconName::Check).size(ICON_2XS).text_color(check))
                }
                None => el
                    .border_color(muted.opacity(TINT_BORDER))
                    .when(enabled, |el| el.group_hover(group.clone(), |style| style.border_color(muted))),
            },
        );
    h_flex()
        .id(ElementId::Name(id))
        .group(group)
        .h(rems(1.846))
        .px(rems(0.462))
        .gap(rems(0.615))
        .rounded(RADIUS_SM)
        .map(|el| match enabled {
            true => el.cursor_pointer().hover(|style| style.bg(hover)),
            false => el.opacity(DIMMED),
        })
        .child(mark)
        .child(div().flex_1().min_w_0().truncate().child(name.into()))
        .when(!type_name.is_empty(), |el| {
            el.child(
                div().flex_none().max_w(rems(9.231)).truncate().text_size(TEXT_XS).text_color(muted).child(type_name),
            )
        })
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

#[cfg(test)]
mod tests {
    use gpui_kit::component::menu::PopupMenuItem;
    use gpui_kit::{
        Context, InteractiveElement as _, IntoElement, Modifiers, ParentElement as _, Render, Styled as _,
        TestAppContext, Window, div, px,
    };

    use super::SelectMenu;
    use crate::test_support::Env;

    struct WideSelect;

    impl Render for WideSelect {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div().w(px(400.)).child(super::select("wide", "Pick", cx).debug_selector(|| "wide".into()).select_menu(
                window,
                cx,
                |menu, _, _| {
                    menu.item(PopupMenuItem::element(|_, _| div().debug_selector(|| "item".into()).w_full().h(px(8.))))
                },
            ))
        }
    }

    #[gpui_kit::test]
    fn a_select_menu_is_as_wide_as_the_select(cx: &mut TestAppContext) {
        let _env = Env::new(cx);
        let (_, cx) = cx.add_window_view(|_, _| WideSelect);
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let select = cx.debug_bounds("wide").expect("the select is drawn");
        cx.simulate_click(select.center(), Modifiers::none());
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let item = cx.debug_bounds("item").expect("the menu is open");
        // The item sits inside the menu's padding, so it's a little narrower than the menu.
        assert!(item.size.width > select.size.width * 0.9, "item {item:?} in a menu under the select {select:?}");
    }
}
