use barsql_app::app_info;
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{ActiveTheme, Icon, Sizable, StyledExt, WindowExt, h_flex, v_flex};
use gpui_kit::*;

use crate::assets::LOGO;
use crate::form::ToolButton;
use crate::i18n::{t, t_with};
use crate::modal::{self, Modal};
use crate::tokens::{ICON_XS, TEXT_LG, TEXT_SM, TEXT_XS};
use crate::{toast, update_dialog};

const COPYRIGHT_YEAR: &str = "2026";
const LICENSE: &str = "GPL-3.0";
// What BarSQL is made with, each with where to read about it.
const CREDITS: [(&str, &str); 6] = [
    ("Rust", "https://www.rust-lang.org"),
    ("GPUI", "https://www.gpui.rs"),
    ("GPUI Kit", "https://github.com/longbridge/gpui-kit"),
    ("Inter", "https://rsms.me/inter/"),
    ("Fira Code", "https://github.com/tonsky/FiraCode"),
    ("Lucide", "https://lucide.dev"),
];

// As a bug report wants it, like "macOS arm64".
pub(crate) fn platform() -> String {
    let os = match std::env::consts::OS {
        "macos" => "macOS",
        "windows" => "Windows",
        "linux" => "Linux",
        other => other,
    };
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        other => other,
    };
    format!("{os} {arch}")
}

fn link(id: impl Into<ElementId>, text: impl Into<SharedString>, url: String, cx: &App) -> Stateful<Div> {
    div()
        .id(id)
        .text_color(cx.theme().primary)
        .cursor_pointer()
        .hover(|style| style.underline())
        .child(text.into())
        .on_click(move |_, _, cx| cx.open_url(&url))
}

fn link_button(id: &'static str, icon: Lucide, label: impl Into<SharedString>, url: String) -> Button {
    Button::new(id)
        .debug_selector(move || id.into())
        .tool(Icon::new(icon), ICON_XS, label)
        .on_click(move |_, _, cx| cx.open_url(&url))
}

// The logo, name and version, a line to copy for bug reports, where to find out more, then the license and credits.
pub fn open(window: &mut Window, cx: &mut App) {
    let info = app_info();
    window.open_dialog(cx, move |dialog, window, cx| {
        let muted = cx.theme().muted_foreground;
        let platform = platform();
        let copied = format!("{} {} ({platform})", info.name, info.version);
        let version = t_with(cx, "about.versionLine", &[("version", &info.version), ("platform", &platform)]);
        let hero = v_flex()
            .items_center()
            .gap(rems(0.308))
            .child(img(LOGO).size(rems(4.923)).mb(rems(0.462)))
            .child(div().text_size(TEXT_LG).font_semibold().child(info.name.clone()))
            .child(
                h_flex()
                    .gap(rems(0.308))
                    .text_size(TEXT_SM)
                    .text_color(muted)
                    .child(div().debug_selector(|| "about-version".into()).child(version))
                    .child(
                        Button::new("about-copy-version")
                            .debug_selector(|| "about-copy-version".into())
                            .ghost()
                            .xsmall()
                            .icon(Icon::new(Lucide::Copy))
                            .tooltip(t(cx, "about.copyVersion"))
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(copied.clone()));
                                toast::success(t(cx, "toast.copiedClipboard"), cx);
                            }),
                    ),
            )
            .child(div().mt(rems(0.308)).text_center().text_color(muted).child(t(cx, "about.tagline")));
        let repository = info.repository.clone();
        let links = h_flex()
            .flex_wrap()
            .justify_center()
            .gap(rems(0.462))
            .child(link_button("about-website", Lucide::Globe, t(cx, "about.website"), info.website.clone()))
            .child(link_button("about-github", Lucide::Github, "GitHub", repository.clone()))
            .child(link_button(
                "about-release-notes",
                Lucide::NotebookText,
                t(cx, "about.releaseNotes"),
                format!("{repository}/releases/tag/v{}", info.version),
            ))
            .child(link_button(
                "about-report-issue",
                Lucide::Bug,
                t(cx, "about.reportIssue"),
                format!("{repository}/issues/new/choose"),
            ));
        let credits = CREDITS.iter().enumerate().flat_map(|(ix, (name, url))| {
            let dot = (ix > 0).then(|| div().child("·").into_any_element());
            dot.into_iter().chain([link(("about-credit", ix), *name, url.to_string(), cx).into_any_element()])
        });
        let legal = v_flex()
            .items_center()
            .gap(rems(0.308))
            .text_size(TEXT_XS)
            .text_color(muted)
            .child(
                h_flex()
                    .gap(rems(0.308))
                    .child(format!("© {COPYRIGHT_YEAR}"))
                    .child(link("about-author", info.author.clone(), format!("mailto:{}", info.email), cx))
                    .child("·")
                    .child(link(
                        "about-license",
                        t_with(cx, "about.license", &[("name", LICENSE)]),
                        format!("{repository}/blob/master/LICENSE"),
                        cx,
                    )),
            )
            .child(
                h_flex().flex_wrap().justify_center().gap(rems(0.308)).child(t(cx, "about.madeWith")).children(credits),
            );
        let content = v_flex()
            .child(modal::body().items_center().gap(rems(1.231)).pt(rems(1.846)).child(hero).child(links).child(legal))
            .child(
                modal::footer(cx)
                    .child(Button::new("about-check-updates").large().label(t(cx, "about.checkUpdates")).on_click(
                        |_, window, cx| {
                            update_dialog::open(window, cx);
                        },
                    ))
                    .child(
                        Button::new("about-close")
                            .large()
                            .primary()
                            .label(t(cx, "common.close"))
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    ),
            );
        Modal::new("about", t_with(cx, "about.title", &[("name", &info.name)])).build(dialog, content, window, cx)
    });
}
