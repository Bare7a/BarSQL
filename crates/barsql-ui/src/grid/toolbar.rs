use barsql_io::{EXPORT_FORMATS, ExportFormat};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::{DropdownMenu, PopupMenu, PopupMenuItem};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::scroll::ScrollbarAxis;
use gpui_kit::component::{ActiveTheme, Disableable, Icon, Selectable, Sizable, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::view::Grid;
use super::{copy_format, set_copy_format};
use crate::form::{self, ToolButton};
use crate::i18n::{t, t_with};
use crate::scrollbars::HoverScrollbar as _;
use crate::tokens::{ICON_XS, TEXT_SM};

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

// `extra` goes first among the buttons, like the results' chart toggle. Without `grid_tools`, as under a chart, only
// Export follows it.
pub fn toolbar(
    grid: &Entity<Grid>,
    meta: SharedString,
    extra: Option<AnyElement>,
    grid_tools: bool,
    cx: &mut App,
) -> impl IntoElement {
    let theme = cx.theme();
    let (muted, accent, border) = (theme.muted_foreground, theme.primary, theme.border);
    let state = grid.read(cx);
    let (selected_rows, selected_cols) = state.selection_counts();
    let count = (selected_rows > 0 && selected_cols > 0 && (selected_rows > 1 || selected_cols > 1)).then(|| {
        let (rows, cols) = (selected_rows.to_string(), selected_cols.to_string());
        format!(" · {}", t_with(cx, "tableView.selectionCells", &[("rows", &rows), ("cols", &cols)]))
    });
    let export = grid.clone();
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
                .when(grid_tools, |el| el.children(grid_tools_buttons(grid, cx)))
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

// Fit columns, the column picker, the copy format and Copy.
fn grid_tools_buttons(grid: &Entity<Grid>, cx: &mut App) -> Vec<AnyElement> {
    let format = copy_format(cx);
    let fit = grid.clone();
    let copy = grid.clone();
    let formats = grid.downgrade();
    vec![
        Button::new("fit-columns")
            .tool(Icon::new(Lucide::Columns3), ICON_XS, t(cx, "results.fitColumns"))
            .tooltip(t(cx, "tooltip.fitColumns"))
            .on_click(move |_, _, cx| fit.update(cx, |grid, cx| grid.fit_columns(cx)))
            .into_any_element(),
        column_picker(grid, cx).into_any_element(),
        crate::screenshots::probe(
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
                        menu =
                            menu.item(PopupMenuItem::new(format_label(option, cx)).checked(option == format).on_click(
                                move |_, _, cx| {
                                    set_copy_format(option, cx);
                                    formats.update(cx, |_, cx| cx.notify()).ok();
                                },
                            ));
                    }
                    menu
                }),
            "copy-format",
        )
        .into_any_element(),
        Button::new("copy-results")
            .tool(Icon::new(Lucide::ClipboardCopy), ICON_XS, t(cx, "common.copy"))
            .tooltip(t(cx, "tooltip.copyResults"))
            .on_click(move |_, window, cx| copy.update(cx, |grid, cx| grid.copy(false, window, cx)))
            .into_any_element(),
    ]
}

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

// Hidden columns keep an empty box and a dimmed name.
fn column_row(
    grid: &Entity<Grid>,
    ix: usize,
    name: String,
    type_name: String,
    visible: bool,
    cx: &App,
) -> Stateful<Div> {
    let theme = cx.theme();
    let (on, muted) = (visible.then_some(theme.primary), theme.muted_foreground);
    let grid = grid.clone();
    form::pick_row(format!("column-picker-item-{ix}").into(), on, true, name, type_name, cx)
        .debug_selector(move || format!("column-picker-item-{ix}"))
        .when(!visible, |row| row.text_color(muted))
        .on_click(move |_, _, cx| grid.update(cx, |grid, cx| grid.toggle_column(ix, cx)))
}
