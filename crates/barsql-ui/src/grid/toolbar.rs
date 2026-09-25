use barsql_io::{EXPORT_FORMATS, ExportFormat};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::Button;
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::menu::{DropdownMenu, PopupMenu, PopupMenuItem};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::{ActiveTheme, Icon, Selectable, Sizable, h_flex, v_flex};
use gpui_kit::*;

use super::view::Grid;
use super::{copy_format, set_copy_format};
use crate::form::ToolButton;
use crate::i18n::t;
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

pub fn toolbar(grid: &Entity<Grid>, meta: SharedString, cx: &mut App) -> impl IntoElement {
    let theme = cx.theme();
    let (muted, accent, border) = (theme.muted_foreground, theme.primary, theme.border);
    let state = grid.read(cx);
    let (selected_rows, selected_cols) = state.selection_counts();
    let count = (selected_rows > 0 && selected_cols > 0 && (selected_rows > 1 || selected_cols > 1))
        .then(|| format!(" · {selected_rows} × {selected_cols} selected"));
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

fn column_picker(grid: &Entity<Grid>, cx: &mut App) -> impl IntoElement {
    let state = grid.read(cx);
    let total = state.set().columns.len();
    let visible = total - state.hidden_count();
    let open = state.picker_open;
    let label = format!("{} ({visible}/{total})", t(cx, "common.columns"));
    let this = grid.downgrade();
    let grid = grid.clone();
    Popover::new("column-picker")
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
        .content(move |_, _, cx| {
            let state = grid.read(cx);
            let names: Vec<(usize, String, bool)> = state
                .column_names()
                .enumerate()
                .map(|(ix, name)| (ix, name.to_string(), !state.is_hidden(ix)))
                .collect();
            let (show, hide) = (grid.clone(), grid.clone());
            v_flex()
                .gap_2()
                .max_h(px(320.))
                .min_w(px(200.))
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("columns-show-all")
                                .tool_label(t(cx, "common.showAll"))
                                .on_click(move |_, _, cx| show.update(cx, |grid, cx| grid.show_all_columns(cx))),
                        )
                        .child(
                            Button::new("columns-hide-all")
                                .tool_label(t(cx, "common.hideAll"))
                                .on_click(move |_, _, cx| hide.update(cx, |grid, cx| grid.hide_all_columns(cx))),
                        ),
                )
                .child(v_flex().id("column-picker-list").gap_1().overflow_y_scroll().children(names.into_iter().map(
                    |(ix, name, checked)| {
                        let grid = grid.clone();
                        Checkbox::new(("column-picker-item", ix))
                            .debug_selector(move || format!("column-picker-item-{ix}"))
                            .label(name)
                            .checked(checked)
                            .on_click(move |_, _, cx| grid.update(cx, |grid, cx| grid.toggle_column(ix, cx)))
                    },
                )))
        })
}
