use gpui_kit::base::resize_handle;
use gpui_kit::component::button::Button;
use gpui_kit::component::input::Editor;
use gpui_kit::component::{ActiveTheme, Icon, IconName, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use barsql_sql::structure::Problem;
use gpui_kit::component::scroll::ScrollbarAxis;

use super::{StructureTab, section_id};
use crate::form::{self, ToolButton as _};
use crate::i18n::{t, t_count};
use crate::scrollbars::{HoverScrollbar as _, ScrollbarsOnHover as _};
use crate::theme;
use crate::tokens::{ICON_SM, TEXT_SM, TEXT_XS};

// The script's height in rems: as it opens, its least, and the least of the rows that stays above it.
const HEIGHT: (f32, f32, f32) = (9.231, 3.077, 6.154);
const LABEL_ROW: f32 = 2.308;

// Shared by every Structure tab and kept for the session only, like the query results' split.
struct PreviewHeight(f32);

impl Global for PreviewHeight {}

#[derive(Clone)]
pub(super) struct PreviewDrag;

impl Render for PreviewDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

fn height(cx: &App) -> f32 {
    cx.try_global::<PreviewHeight>().map_or(HEIGHT.0, |height| height.0)
}

// `event.bounds` holds the rows and the preview, so the script takes what's below the pointer.
pub(super) fn resize(event: &DragMoveEvent<PreviewDrag>, window: &Window, cx: &mut App) {
    let (bounds, rem) = (event.bounds, window.rem_size());
    let wanted = (bounds.bottom() - event.event.position.y) / rem - LABEL_ROW;
    let most = (bounds.size.height / rem - LABEL_ROW - HEIGHT.2).max(HEIGHT.1);
    cx.set_global(PreviewHeight(wanted.clamp(HEIGHT.1, most)));
}

fn problems(
    tab: &StructureTab,
    lines: Vec<(Problem, SharedString)>,
    cx: &mut Context<StructureTab>,
) -> impl IntoElement {
    let list = v_flex()
        .id("structure-problems")
        .size_full()
        .overflow_y_scroll()
        .track_scroll(&tab.problems_scroll)
        .px(rems(0.923))
        .py(rems(0.462))
        .gap(rems(0.308))
        .text_size(TEXT_SM)
        .text_color(cx.theme().danger)
        .children(lines.into_iter().enumerate().map(|(ix, (problem, line))| {
            let id = format!("structure:problem:{}:{}", section_id(problem.section), problem.row);
            div()
                .id(("structure-problem", ix))
                .debug_selector(move || id.clone())
                .cursor_pointer()
                .hover(|style| style.underline())
                .child(line)
                .on_click(cx.listener(move |tab, _, window, cx| tab.show_problem(&problem, window, cx)))
        }));
    div().relative().size_full().child(list).hover_scrollbar(&tab.problems_scroll, ScrollbarAxis::Vertical)
}

// The script as it would run, or what keeps it from running. It folds away, or its edge drags, to give the rows room.
pub(super) fn panel(tab: &StructureTab, cx: &mut Context<StructureTab>) -> impl IntoElement {
    let theme = cx.theme().clone();
    let open = tab.preview_open;
    let least = rems(LABEL_ROW + height(cx));
    let lines = tab.problem_lines(cx);
    let count = (!lines.is_empty())
        .then(|| form::badge(t_count(cx, "structure.problems", lines.len() as i64, &[]), theme.danger));
    let toggle = Button::new("structure-preview-toggle")
        .debug_selector(|| "structure-preview-toggle".into())
        .tool_icon(Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronUp }), ICON_SM)
        .on_click(cx.listener(|tab, _, _, cx| {
            tab.preview_open = !tab.preview_open;
            cx.notify();
        }));
    v_flex()
        .debug_selector(|| "structure-preview-panel".into())
        .relative()
        // Open, it takes the room the rows leave, and no less than the height its edge was dragged to.
        .map(|panel| if open { panel.flex_1().min_h(least) } else { panel.flex_none() })
        .border_t_1()
        .border_color(theme.border)
        .child(
            h_flex()
                .h(rems(LABEL_ROW))
                .justify_between()
                .px(rems(0.923))
                .text_size(TEXT_XS)
                .text_color(theme.muted_foreground)
                .child(h_flex().gap(rems(0.462)).child(t(cx, "structure.preview")).children(count))
                .child(toggle),
        )
        .when(open, |panel| {
            let body = match lines.is_empty() {
                true => Editor::new(&tab.preview)
                    .readonly(true)
                    .size_full()
                    .bordered(false)
                    .text_size(px(theme::editor_font_size(cx)))
                    .scrollbars_on_hover()
                    .into_any_element(),
                false => problems(tab, lines, cx).into_any_element(),
            };
            let area = div().flex_1().min_h_0().overflow_hidden().debug_selector(|| "structure-preview".into());
            panel.child(area.child(body)).child(
                resize_handle::<PreviewDrag, PreviewDrag>("structure-preview-splitter", Axis::Vertical).on_drag(
                    PreviewDrag,
                    |drag, _, _, cx| {
                        cx.stop_propagation();
                        cx.new(|_| drag.clone())
                    },
                ),
            )
        })
}
