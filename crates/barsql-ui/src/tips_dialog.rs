use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{ActiveTheme, WindowExt, h_flex, v_flex};
use gpui_kit::*;

use crate::form;
use crate::i18n::t;
use crate::modal::{self, Modal};
use crate::shortcuts::{self, Binding};
use crate::tokens::{RADIUS_SM, TEXT_BASE, TEXT_XS};

fn keys(id: &str, cx: &App) -> String {
    shortcuts::find(id).map(|def| shortcuts::format(&shortcuts::effective(def, cx))).unwrap_or_default()
}

fn tip(keys: Option<String>, text: SharedString, cx: &App) -> impl IntoElement + use<> {
    let theme = cx.theme();
    h_flex()
        .items_start()
        .gap(rems(0.769))
        .py(rems(0.385))
        .text_size(TEXT_BASE)
        .children(keys.map(|keys| {
            div()
                .flex_none()
                .min_w(rems(4.5 * 0.846))
                .px(rems(0.615))
                .py(rems(0.154))
                .rounded(RADIUS_SM)
                .border_1()
                .border_color(theme.border)
                .bg(theme.background)
                .font_family(theme.mono_font_family.clone())
                .text_size(TEXT_XS)
                .text_center()
                .child(keys)
        }))
        .child(div().flex_1().min_w_0().child(text))
}

fn section(title: SharedString, tips: Vec<AnyElement>, cx: &App) -> impl IntoElement + use<> {
    v_flex().child(form::caption(title, cx).mb(rems(0.615))).children(tips)
}

// Covers what the shortcuts dialog doesn't list, with the current bindings.
pub fn open(window: &mut Window, cx: &mut App) {
    let scroll = ScrollHandle::new();
    window.open_dialog(cx, move |dialog, window, cx| {
        let tip = |keys: Option<String>, key: &str, cx: &App| tip(keys, t(cx, key), cx).into_any_element();
        let copy = shortcuts::format(&Binding { key: "c".into(), ctrl: true, ..Default::default() });
        let escape = shortcuts::format(&Binding { key: "Escape".into(), ..Default::default() });
        let sections = v_flex()
            .gap(rems(1.385))
            .child(section(
                t(cx, "tips.editorTitle"),
                vec![
                    tip(Some(keys("runSelection", cx)), "tips.editorRunSelection", cx),
                    tip(Some(keys("runAll", cx)), "tips.editorRunAll", cx),
                    tip(Some(keys("saveQuery", cx)), "tips.editorSave", cx),
                    tip(None, "tips.editorGutter", cx),
                    tip(None, "tips.editorContext", cx),
                ],
                cx,
            ))
            .child(section(
                t(cx, "tips.resultsTitle"),
                vec![
                    tip(Some(copy), "tips.resultsCopy", cx),
                    tip(None, "tips.resultsSort", cx),
                    tip(None, "tips.resultsRow", cx),
                    tip(None, "tips.resultsCtrlClick", cx),
                    tip(None, "tips.resultsDoubleClick", cx),
                    tip(None, "tips.resultsContext", cx),
                    tip(Some(escape), "tips.resultsEsc", cx),
                ],
                cx,
            ))
            .child(section(
                t(cx, "tips.schemaTitle"),
                vec![tip(None, "tips.schemaClick", cx), tip(None, "tips.schemaDblClick", cx)],
                cx,
            ))
            .child(section(
                t(cx, "tips.jsonTitle"),
                vec![tip(Some(keys("toggleJsonPanel", cx)), "tips.jsonToggle", cx)],
                cx,
            ));
        let content = modal::scroll_content()
            .child(modal::scroll_body(
                &scroll,
                modal::body().child(modal::description(t(cx, "tips.intro"), cx)).child(sections),
            ))
            .child(
                modal::footer(cx).child(
                    Button::new("tips-close")
                        .primary()
                        .label(t(cx, "common.close"))
                        .on_click(|_, window, cx| window.close_dialog(cx)),
                ),
            );
        Modal::new("tips", t(cx, "tips.title")).build(dialog, content, window, cx)
    });
}
