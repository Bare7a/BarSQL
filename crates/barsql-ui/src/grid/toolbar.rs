use barsql_io::{EXPORT_FORMATS, ExportFormat};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::{DropdownMenu, PopupMenu, PopupMenuItem};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::scroll::ScrollbarAxis;
use gpui_kit::component::{ActiveTheme, Disableable, Icon, IconName, Selectable, Sizable, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::view::Grid;
use super::{copy_format, set_copy_format};
use crate::form::ToolButton;
use crate::i18n::{t, t_with};
use crate::scrollbars::HoverScrollbar as _;
use crate::tokens::{ICON_2XS, ICON_XS, RADIUS_SM, TEXT_SM, TEXT_XS, TINT_BORDER};

pub fn format_label(format: ExportFormat, cx: &App) -> SharedString {
    let key = match format {
        ExportFormat::Text => "export.formatText",
        ExportFormat::Csv => "export.formatCsv",
        ExportFormat::Json => "export.formatJson",
        ExportFormat::Markdown => "export.formatMarkdown",
        ExportFormat::Sql => "export.formatSql",
    };
    t(cx, key)
}

// `extra` goes first among the buttons, like the results' chart toggle.
pub fn toolbar(grid: &Entity<Grid>, meta: SharedString, extra: Option<AnyElement>, cx: &mut App) -> impl IntoElement {
    let theme = cx.theme();
    let (muted, accent, border) = (theme.muted_foreground, theme.primary, theme.border);
    let state = grid.read(cx);
    let (selected_rows, selected_cols) = state.selection_counts();
    let count = (selected_rows > 0 && selected_cols > 0 && (selected_rows > 1 || selected_cols > 1)).then(|| {
        let (rows, cols) = (selected_rows.to_string(), selected_cols.to_string());
        format!(" · {}", t_with(cx, "tableView.selectionCells", &[("rows", &rows), ("cols", &cols)]))
    });
    let format = copy_format(cx);
    let fit = grid.clone();
    let copy = grid.clone();
    let export = grid.clone();
    let formats = grid.downgrade();
    h_flex()
        .flex_none()
        .px(rems(0.769))
        .py(rems(0.462))
        .gap(rems(0.615))
        .justify_between()
        .border_b_1()
        .border_color(border)
        .text_size(TEXT_SM)
        .text_color(muted)
        .child(
            h_flex()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .child(meta)
                .children(count.map(|count| div().text_color(accent).child(count))),
        )
        .child(
            h_flex()
                .flex_none()
                .gap(rems(0.462))
                .children(extra)
                .child(
                    Button::new("fit-columns")
                        .tool(Icon::new(Lucide::Columns3), ICON_XS, t(cx, "results.fitColumns"))
                        .tooltip(t(cx, "tooltip.fitColumns"))
                        .on_click(move |_, _, cx| fit.update(cx, |grid, cx| grid.fit_columns(cx))),
                )
                .child(column_picker(grid, cx))
                .child(crate::screenshots::probe(
                    Button::new("copy-format")
                        .small()
                        .h(rems(1.692))
                        .px(rems(0.615))
                        .label(format_label(format, cx))
                        .dropdown_caret(true)
                        .tooltip(t(cx, "tooltip.copyFormat"))
                        .dropdown_menu(move |mut menu: PopupMenu, _, cx| {
                            for option in EXPORT_FORMATS {
                                let formats = formats.clone();
                                menu = menu.item(
                                    PopupMenuItem::new(format_label(option, cx)).checked(option == format).on_click(
                                        move |_, _, cx| {
                                            set_copy_format(option, cx);
                                            formats.update(cx, |_, cx| cx.notify()).ok();
                                        },
                                    ),
                                );
                            }
                            menu
                        }),
                    "copy-format",
                ))
                .child(
                    Button::new("copy-results")
                        .tool(Icon::new(Lucide::ClipboardCopy), ICON_XS, t(cx, "common.copy"))
                        .tooltip(t(cx, "tooltip.copyResults"))
                        .on_click(move |_, window, cx| copy.update(cx, |grid, cx| grid.copy(false, window, cx))),
                )
                .child(
                    Button::new("export-results")
                        .debug_selector(|| "export-results".into())
                        .tool(Icon::new(Lucide::Download), ICON_XS, t(cx, "results.export"))
                        .tooltip(t(cx, "tooltip.exportOptions"))
                        .on_click(move |_, window, cx| {
                            let source = export.read(cx).export_source();
                            crate::export_dialog::open(source, window, cx);
                        }),
                ),
        )
}

// 24px column picker rows.
const ROW_HEIGHT: f32 = 1.846;

fn column_picker(grid: &Entity<Grid>, cx: &mut App) -> impl IntoElement {
    let state = grid.read(cx);
    let total = state.set().columns.len();
    let visible = total - state.hidden_count();
    let open = state.picker_open;
    let label = format!("{} ({visible}/{total})", t(cx, "common.columns"));
    let this = grid.downgrade();
    let grid = grid.clone();
    Popover::new("column-picker")
        .p_0()
        .trigger(
            Button::new("column-picker-button")
                .debug_selector(|| "column-picker-button".into())
                .tool(Icon::new(Lucide::Columns3), ICON_XS, label)
                .selected(open)
                .tooltip(t(cx, "tooltip.showHideColumns")),
        )
        .open(open)
        .on_open_change(move |open, _, cx| {
            this.update(cx, |grid, cx| {
                grid.picker_open = *open;
                cx.notify();
            })
            .ok();
        })
        .content(move |_, window, cx| {
            let state = grid.read(cx);
            let names: Vec<(usize, String, String, bool)> = state
                .set()
                .columns
                .iter()
                .enumerate()
                .map(|(ix, meta)| (ix, meta.name.clone(), meta.type_name.to_lowercase(), !state.is_hidden(ix)))
                .collect();
            let (hidden, total, scroll) = (state.hidden_count(), names.len(), state.picker_scroll.clone());
            // About 13 rows, or half the window when that is shorter.
            let list_height = (window.viewport_size().height * 0.5).min(window.rem_size() * 24.615);
            let (show, hide) = (grid.clone(), grid.clone());
            let border = cx.theme().border;
            let rows: Vec<Stateful<Div>> = names
                .into_iter()
                .map(|(ix, name, type_name, visible)| column_row(&grid, ix, name, type_name, visible, cx))
                .collect();
            v_flex()
                .min_w(rems(15.385))
                .max_w(rems(24.615))
                .text_size(TEXT_SM)
                // Two equal halves with a rule between them.
                .child(
                    h_flex()
                        .p(rems(0.308))
                        .border_b_1()
                        .border_color(border)
                        .child(
                            Button::new("columns-show-all")
                                .debug_selector(|| "columns-show-all".into())
                                .ghost()
                                .flex_1()
                                .disabled(hidden == 0)
                                .tool(Icon::new(Lucide::Eye), ICON_XS, t(cx, "common.showAll"))
                                .on_click(move |_, _, cx| show.update(cx, |grid, cx| grid.show_all_columns(cx))),
                        )
                        .child(div().flex_none().w(px(1.)).h(rems(1.231)).mx(rems(0.308)).bg(border))
                        .child(
                            Button::new("columns-hide-all")
                                .debug_selector(|| "columns-hide-all".into())
                                .ghost()
                                .flex_1()
                                .disabled(hidden == total)
                                .tool(Icon::new(Lucide::EyeOff), ICON_XS, t(cx, "common.hideAll"))
                                .on_click(move |_, _, cx| hide.update(cx, |grid, cx| grid.hide_all_columns(cx))),
                        ),
                )
                .child(
                    div()
                        .relative()
                        .child(
                            v_flex()
                                .id("column-picker-list")
                                .max_h(list_height)
                                .overflow_y_scroll()
                                .track_scroll(&scroll)
                                .p(rems(0.308))
                                .children(rows),
                        )
                        .hover_scrollbar(&scroll, ScrollbarAxis::Vertical),
                )
        })
}

// A whole row toggles its column. Hidden columns keep an empty box and a dimmed name. The type follows faintly.
fn column_row(
    grid: &Entity<Grid>,
    ix: usize,
    name: String,
    type_name: String,
    visible: bool,
    cx: &App,
) -> Stateful<Div> {
    let theme = cx.theme();
    let (hover, primary, check, muted) =
        (theme.accent, theme.primary, theme.primary_foreground, theme.muted_foreground);
    let text = if visible { theme.foreground } else { muted };
    let group = SharedString::from(format!("column-picker-row-{ix}"));
    let grid = grid.clone();
    h_flex()
        .id(("column-picker-item", ix))
        .debug_selector(move || format!("column-picker-item-{ix}"))
        .group(group.clone())
        .h(rems(ROW_HEIGHT))
        .px(rems(0.462))
        .gap(rems(0.615))
        .rounded(RADIUS_SM)
        .cursor_pointer()
        .hover(|style| style.bg(hover))
        .on_click(move |_, _, cx| grid.update(cx, |grid, cx| grid.toggle_column(ix, cx)))
        .child(
            div()
                .flex_none()
                .size(rems(1.077))
                .flex()
                .items_center()
                .justify_center()
                .rounded(RADIUS_SM)
                .border_1()
                .map(|el| match visible {
                    true => el
                        .bg(primary)
                        .border_color(primary)
                        .child(Icon::new(IconName::Check).size(ICON_2XS).text_color(check)),
                    false => el
                        .border_color(muted.opacity(TINT_BORDER))
                        .group_hover(group, |style| style.border_color(muted)),
                }),
        )
        .child(div().flex_1().min_w_0().truncate().text_color(text).child(name))
        .when(!type_name.is_empty(), |el| {
            el.child(
                div().flex_none().max_w(rems(9.231)).truncate().text_size(TEXT_XS).text_color(muted).child(type_name),
            )
        })
}
