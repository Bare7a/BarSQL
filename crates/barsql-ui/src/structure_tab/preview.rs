use gpui_kit::component::button::Button;
use gpui_kit::component::input::Editor;
use gpui_kit::component::{ActiveTheme, Icon, IconName, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::StructureTab;
use crate::form::ToolButton as _;
use crate::i18n::t;
use crate::scrollbars::ScrollbarsOnHover as _;
use crate::theme;
use crate::tokens::{ICON_SM, TEXT_XS};

// The plan's script as it would run, read-only. It folds away to give the rows the room.
pub(super) fn panel(tab: &StructureTab, cx: &mut Context<StructureTab>) -> impl IntoElement {
    let theme = cx.theme().clone();
    let open = tab.preview_open;
    let toggle = Button::new("structure-preview-toggle")
        .debug_selector(|| "structure-preview-toggle".into())
        .tool_icon(Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronUp }), ICON_SM)
        .on_click(cx.listener(|tab, _, _, cx| {
            tab.preview_open = !tab.preview_open;
            cx.notify();
        }));
    v_flex()
        .flex_none()
        .border_t_1()
        .border_color(theme.border)
        .child(
            h_flex()
                .justify_between()
                .px(rems(0.923))
                .py(rems(0.308))
                .text_size(TEXT_XS)
                .text_color(theme.muted_foreground)
                .child(t(cx, "structure.preview"))
                .child(toggle),
        )
        .when(open, |panel| {
            panel.child(
                div().h(rems(9.231)).debug_selector(|| "structure-preview".into()).child(
                    Editor::new(&tab.preview)
                        .readonly(true)
                        .size_full()
                        .bordered(false)
                        .text_size(px(theme::editor_font_size(cx)))
                        .scrollbars_on_hover(),
                ),
            )
        })
}
