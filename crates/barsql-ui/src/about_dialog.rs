use barsql_app::app_info;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{ActiveTheme, WindowExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::form;
use crate::i18n::{t, t_with};
use crate::modal::{self, Modal};
use crate::tokens::TEXT_SM;
use crate::update_dialog;

fn link(id: &'static str, text: String, url: String, cx: &App) -> impl IntoElement + use<> {
    div()
        .id(id)
        .text_color(cx.theme().primary)
        .cursor_pointer()
        .hover(|style| style.underline())
        .child(text)
        .on_click(move |_, _, cx| cx.open_url(&url))
}

fn entry(label: SharedString, value: impl IntoElement, cx: &App) -> Div {
    v_flex().child(form::caption(label, cx).mb(rems(0.154))).child(value)
}

pub fn open(window: &mut Window, cx: &mut App) {
    let info = app_info();
    window.open_dialog(cx, move |dialog, window, cx| {
        let muted = cx.theme().muted_foreground;
        let author = h_flex().gap_1().child(info.author.clone()).when(!info.email.is_empty(), |el| {
            el.child(link("about-email", info.email.clone(), format!("mailto:{}", info.email), cx))
        });
        let content = v_flex()
            .child(
                modal::body()
                    .gap(rems(1.077))
                    .child(modal::description(info.description.clone(), cx))
                    .child(
                        v_flex()
                            .gap(rems(0.769))
                            .child(entry(t(cx, "about.version"), info.version.clone(), cx))
                            .child(entry(t(cx, "about.createdBy"), author, cx))
                            .child(entry(
                                t(cx, "about.website"),
                                link("about-website", info.website.clone(), info.website.clone(), cx),
                                cx,
                            ))
                            .child(entry(
                                t(cx, "about.repository"),
                                link("about-repository", info.repository.clone(), info.repository.clone(), cx),
                                cx,
                            )),
                    )
                    .child(div().text_size(TEXT_SM).text_color(muted).child(t(cx, "about.stack"))),
            )
            .child(
                modal::footer(cx)
                    .child(Button::new("about-check-updates").label(t(cx, "about.checkUpdates")).on_click(
                        |_, window, cx| {
                            update_dialog::open(window, cx);
                        },
                    ))
                    .child(
                        Button::new("about-close")
                            .primary()
                            .label(t(cx, "common.close"))
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    ),
            );
        Modal::new("about", t_with(cx, "about.title", &[("name", &info.name)])).build(dialog, content, window, cx)
    });
}
