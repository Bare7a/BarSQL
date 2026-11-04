use std::rc::Rc;

use gpui_kit::component::menu::AppMenuBar;
use gpui_kit::component::{ActiveTheme, Icon, IconName, Sizable, TITLE_BAR_HEIGHT, TitleBar, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::assets::{MARK_DARK, MARK_LIGHT, vector_img};
use crate::tokens::TEXT_SM;

// macOS has traffic lights and the system menu bar, so its bar only shows the title.
// README screenshots draw the Windows bar on macOS too, where GPUI Kit leaves out the window controls.
// On Linux, and in fullscreen, the close button calls `on_close` instead of removing the window.
// Elsewhere closing goes through on_window_should_close.
// With the menu bar, 9px instead of GPUI Kit's 12px centres the mark over the connection switcher's icon below it. In
// fullscreen GPUI Kit pads the bar's content by another 0.75rem, which the menu bar's row takes back.
pub fn render(
    menu_bar: Option<&Entity<AppMenuBar>>,
    title: SharedString,
    on_close: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    window: &Window,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();
    let mac_screenshot = cfg!(target_os = "macos") && menu_bar.is_some();
    let content = match menu_bar {
        Some(menu_bar) => h_flex()
            .debug_selector(|| "title-bar-menus".into())
            .size_full()
            .gap_1()
            .when(window.is_fullscreen(), |bar| bar.ml(rems(-0.75)))
            .child(vector_img(if theme.is_dark() { MARK_DARK } else { MARK_LIGHT }, px(16.)).flex_none())
            .child(menu_bar.clone())
            .child(div().flex_1())
            .when(mac_screenshot, |bar| bar.child(window_controls(cx))),
        None => h_flex()
            .size_full()
            .justify_center()
            .pr(px(80.))
            .child(div().text_size(TEXT_SM).text_color(theme.muted_foreground).child(title)),
    };
    let on_close = Rc::new(on_close);
    let bar = TitleBar::new()
        .on_close_window({
            let on_close = on_close.clone();
            move |event, window, cx| on_close(event, window, cx)
        })
        .when(menu_bar.is_some(), |bar| bar.pl(px(9.)))
        .child(content);
    let fullscreen = window.is_fullscreen() && draws_window_controls(window);
    div().flex_shrink_0().child(bar).when(fullscreen, |bar| bar.child(fullscreen_controls(window, on_close)))
}

fn draws_window_controls(window: &Window) -> bool {
    cfg!(target_os = "windows")
        || (cfg!(target_os = "linux") && matches!(window.window_decorations(), Decorations::Client { .. }))
}

// In fullscreen GPUI stops hit testing the window controls on Windows, which leaves GPUI Kit's buttons without a click
// handler, and on Linux its maximise can't leave fullscreen. These sit over its buttons and take their clicks.
fn fullscreen_controls(
    window: &Window,
    on_close: Rc<impl Fn(&ClickEvent, &mut Window, &mut App) + 'static>,
) -> impl IntoElement {
    let supported = window.window_controls();
    let control = |id: &'static str| div().id(id).debug_selector(move || id.into()).w(TITLE_BAR_HEIGHT).h_full();
    h_flex()
        .absolute()
        .top_0()
        .right_0()
        .h(TITLE_BAR_HEIGHT)
        .when(supported.minimize, |controls| {
            controls.child(control("fullscreen-minimize").on_click(|_, window, cx| {
                cx.stop_propagation();
                window.minimize_window();
            }))
        })
        .when(supported.maximize, |controls| {
            controls.child(control("fullscreen-exit").on_click(|_, window, cx| {
                cx.stop_propagation();
                window.toggle_fullscreen();
            }))
        })
        .child(control("fullscreen-close").on_click(move |event, window, cx| {
            cx.stop_propagation();
            on_close(event, window, cx);
        }))
}

fn window_controls(cx: &App) -> impl IntoElement {
    let color = cx.theme().foreground;
    let control = |icon: IconName| {
        div()
            .w(TITLE_BAR_HEIGHT)
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .child(Icon::new(icon).small().text_color(color))
    };
    h_flex()
        .h_full()
        .flex_none()
        .children([IconName::WindowMinimize, IconName::WindowMaximize, IconName::WindowClose].map(control))
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use gpui_kit::component::menu::AppMenuBar;
    use gpui_kit::{
        App, ClickEvent, Context, Entity, IntoElement, Modifiers, ParentElement as _, Render, Styled as _,
        TestAppContext, VisualTestContext, Window, div,
    };

    use crate::test_support::Env;

    struct Bar(Rc<Cell<bool>>);

    impl Render for Bar {
        fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let closed = self.0.clone();
            let on_close = move |_: &ClickEvent, _: &mut Window, _: &mut App| closed.set(true);
            div().size_full().child(super::fullscreen_controls(window, Rc::new(on_close)))
        }
    }

    fn click(selector: &'static str, cx: &mut VisualTestContext) {
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let at = cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} is not drawn")).center();
        cx.simulate_click(at, Modifiers::none());
        cx.run_until_parked();
    }

    #[gpui_kit::test]
    fn in_fullscreen_maximise_leaves_it_and_close_asks_the_workspace(cx: &mut TestAppContext) {
        let _env = Env::new(cx);
        let closed = Rc::new(Cell::new(false));
        let (_, cx) = cx.add_window_view(|_, _| Bar(closed.clone()));
        cx.update(|window, _| window.toggle_fullscreen());
        click("fullscreen-exit", cx);
        assert!(!cx.update(|window, _| window.is_fullscreen()));
        click("fullscreen-close", cx);
        assert!(closed.get());
    }

    struct Menus(Entity<AppMenuBar>);

    impl Render for Menus {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            super::render(Some(&self.0), "BarSQL".into(), |_, _, _| {}, window, cx)
        }
    }

    #[gpui_kit::test]
    fn the_menu_bar_keeps_its_place_in_fullscreen(cx: &mut TestAppContext) {
        let _env = Env::new(cx);
        let (_, cx) = cx.add_window_view(|_, cx| Menus(AppMenuBar::new(cx)));
        let left = |cx: &mut VisualTestContext| {
            cx.update(|window, cx| window.draw(cx).clear(cx));
            cx.debug_bounds("title-bar-menus").expect("the menu bar is drawn").left()
        };
        let windowed = left(cx);
        cx.update(|window, _| window.toggle_fullscreen());
        assert_eq!(left(cx), windowed);
    }
}
