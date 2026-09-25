use gpui_kit::component::menu::AppMenuBar;
use gpui_kit::component::{ActiveTheme, Icon, IconName, Sizable, TITLE_BAR_HEIGHT, TitleBar, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::assets::LOGO;
use crate::tokens::TEXT_SM;

// macOS has traffic lights and the system menu bar, so its bar only shows the title.
// README screenshots draw the Windows bar on macOS too, where GPUI Kit leaves out the window controls.
pub fn render(menu_bar: Option<&Entity<AppMenuBar>>, title: SharedString, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    let mac_screenshot = cfg!(target_os = "macos") && menu_bar.is_some();
    let content = match menu_bar {
        Some(menu_bar) => h_flex()
            .size_full()
            .gap_1()
            .child(img(LOGO).size(px(16.)).flex_none())
            .child(menu_bar.clone())
            .child(div().flex_1())
            .when(mac_screenshot, |bar| bar.child(window_controls(cx))),
        None => h_flex()
            .size_full()
            .justify_center()
            .pr(px(80.))
            .child(div().text_size(TEXT_SM).text_color(theme.muted_foreground).child(title)),
    };
    TitleBar::new().when(mac_screenshot, |bar| bar.pl(px(12.))).child(content)
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
