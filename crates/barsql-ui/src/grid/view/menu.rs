use barsql_io::EXPORT_FORMATS;
use gpui_kit::component::input::{Copy, SelectAll};
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::*;

use super::super::range::CellCoord;
use super::super::selection::View;
use super::{ClearSelection, Grid, GridEvent, SetNull, Target, ToggleDeleteRow, ViewCell, ViewCellValue};
use crate::grid::format_label;
use crate::i18n::{t, t_count, t_with};
use crate::toast;

type Handler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

fn on(grid: &Entity<Grid>, f: impl Fn(&mut Grid, &mut Window, &mut Context<Grid>) + 'static) -> Handler {
    let grid = grid.clone();
    Box::new(move |_, window, cx| grid.update(cx, |grid, cx| f(grid, window, cx)))
}

fn emit(grid: &Entity<Grid>, event: impl Fn() -> GridEvent + 'static) -> Handler {
    on(grid, move |_, _, cx| cx.emit(event()))
}

fn item(label: impl Into<SharedString>, handler: Handler) -> PopupMenuItem {
    PopupMenuItem::new(label).on_click(handler)
}

impl Grid {
    // A right-click selects what it lands on unless the selection already holds it, as in a spreadsheet, so the menu
    // acts on what was clicked.
    pub(super) fn right_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus_handle, cx);
        let target = self.hit(event.position);
        self.menu_target = target;
        let view = View { order: &self.order, columns: &self.columns };
        match target {
            Some(Target::Cell { row, col } | Target::Jump { row, col }) if !self.selection.covers(row, col, &view) => {
                // As a left click does: the cell takes the focus and any selection goes.
                self.selection.press_cell(row, col, false, &view);
                self.selection.release(Some(CellCoord { row, col }));
            }
            Some(Target::Gutter { row }) => {
                let Some(global) = view.order.global_at(row).filter(|&g| !self.selection.covers_row(g)) else { return };
                self.selection.click_gutter(global, false, false, &view);
            }
            Some(Target::Header { col, .. }) if !self.selection.covers_column(self.columns[col]) => {
                self.selection.click_header(self.columns[col], false, false, &view);
            }
            _ => return,
        }
        self.publish(cx);
        cx.notify();
    }

    pub(super) fn menu(
        grid: &WeakEntity<Self>,
        menu: PopupMenu,
        window: &mut Window,
        cx: &mut Context<PopupMenu>,
    ) -> PopupMenu {
        let Some(grid) = grid.upgrade() else { return menu };
        // Shortcuts show as the grid has them.
        let menu = menu.action_context(grid.read(cx).focus_handle.clone());
        match grid.read(cx).menu_target {
            Some(Target::Header { col, .. }) => Self::column_menu(&grid, col, menu, window, cx),
            Some(Target::Gutter { .. }) => Self::rows_menu(&grid, menu, window, cx),
            _ => Self::cell_menu(&grid, menu, window, cx),
        }
    }

    // Copy, Copy as and Export, on whatever the selection or the focused cell is.
    fn copy_items(grid: &Entity<Self>, menu: PopupMenu, window: &mut Window, cx: &mut Context<PopupMenu>) -> PopupMenu {
        let formats = grid.clone();
        menu.item(
            item(t(cx, "common.copy"), on(grid, |grid, window, cx| grid.copy(true, window, cx))).action(Box::new(Copy)),
        )
        .submenu(t(cx, "grid.copyAs"), window, cx, move |menu, _, cx| {
            EXPORT_FORMATS.into_iter().fold(menu, |menu, format| {
                menu.item(item(
                    format_label(format, cx),
                    on(&formats, move |grid, window, cx| grid.copy_as(format, window, cx)),
                ))
            })
        })
        .item(item(t(cx, "results.contextExportAs"), emit(grid, || GridEvent::Export)))
    }

    fn selection_items(grid: &Entity<Self>, menu: PopupMenu, cx: &App) -> PopupMenu {
        let empty = grid.read(cx).selection.is_empty();
        menu.separator()
            .item(
                item(t(cx, "grid.selectAll"), on(grid, |grid, _, cx| grid.select_all(cx))).action(Box::new(SelectAll)),
            )
            .item(
                item(t(cx, "results.contextClearSelection"), on(grid, |grid, _, cx| grid.clear_selection(cx)))
                    .action(Box::new(ClearSelection))
                    .disabled(empty),
            )
    }

    fn cell_menu(grid: &Entity<Self>, menu: PopupMenu, window: &mut Window, cx: &mut Context<PopupMenu>) -> PopupMenu {
        let menu = Self::copy_items(grid, menu, window, cx).separator();
        let this = grid.read(cx);
        let Some((row, column)) = this.focused_cell() else { return Self::selection_items(grid, menu, cx) };
        let Some(overlay) = this.overlay.clone() else {
            let view = item(t(cx, "results.contextViewCell"), emit(grid, move || GridEvent::ViewCell { row, column }));
            return Self::selection_items(grid, menu.item(view.action(Box::new(ViewCell))), cx);
        };
        let null = this.shown(row, column).is_null();
        let deleted = this.is_deleted(row);
        let menu = if overlay.editable {
            menu.item(
                item(t(cx, "tableView.contextEdit"), emit(grid, move || GridEvent::EditInViewer { row, column }))
                    .action(Box::new(ViewCellValue))
                    .disabled(deleted),
            )
        } else {
            let view = item(t(cx, "results.contextViewCell"), emit(grid, move || GridEvent::ViewCell { row, column }));
            menu.item(view.action(Box::new(ViewCell)))
        };
        let menu = match overlay.foreign.get(&column) {
            Some(table) => menu.item(
                item(
                    t_with(cx, "tableView.viewForeignKey", &[("table", table)]),
                    emit(grid, move || GridEvent::OpenForeignKey { row, column }),
                )
                .disabled(null),
            ),
            None => menu,
        };
        let menu =
            menu.item(item(t(cx, "grid.filterByValue"), emit(grid, move || GridEvent::FilterBy { row, column })));
        if !overlay.editable {
            return Self::selection_items(grid, menu, cx);
        }
        let staged = overlay.staged.contains_key(&(row, column));
        let restore = on(grid, move |grid, _, cx| {
            let value = grid.set.display(row, column).map(str::to_string);
            cx.emit(GridEvent::EditCell { row, column, value });
        });
        let delete_key = if deleted { "tableView.undeleteRow" } else { "tableView.deleteRow" };
        let menu = menu
            .separator()
            .item(
                item(
                    t(cx, "tableView.contextSetNull"),
                    emit(grid, move || GridEvent::EditCell { row, column, value: None }),
                )
                .action(Box::new(SetNull))
                .disabled(null || deleted),
            )
            .item(item(t(cx, "tableView.contextRestore"), restore).disabled(!staged))
            .separator()
            .item(
                item(t(cx, delete_key), emit(grid, move || GridEvent::ToggleDelete(row)))
                    .action(Box::new(ToggleDeleteRow)),
            );
        Self::selection_items(grid, menu, cx)
    }

    // On the row numbers: the selected rows.
    fn rows_menu(grid: &Entity<Self>, menu: PopupMenu, window: &mut Window, cx: &mut Context<PopupMenu>) -> PopupMenu {
        let menu = Self::copy_items(grid, menu, window, cx);
        let this = grid.read(cx);
        let editable = this.overlay.as_ref().is_some_and(|overlay| overlay.editable);
        let mut rows: Vec<usize> = this.selection.rows.iter().copied().collect();
        if !editable || rows.is_empty() {
            return Self::selection_items(grid, menu, cx);
        }
        rows.sort_by_key(|&row| this.order.display_of(row));
        let unmark = rows.iter().all(|&row| this.is_deleted(row));
        let key = if unmark { "tableView.unmarkRows" } else { "tableView.markRows" };
        let label = t_count(cx, key, rows.len() as i64, &[]);
        let menu = menu.separator().item(
            item(label, emit(grid, move || GridEvent::DeleteRows(rows.clone()))).action(Box::new(ToggleDeleteRow)),
        );
        Self::selection_items(grid, menu, cx)
    }

    // On a header: sorting, copying and showing that column.
    fn column_menu(
        grid: &Entity<Self>,
        col: usize,
        menu: PopupMenu,
        window: &mut Window,
        cx: &mut Context<PopupMenu>,
    ) -> PopupMenu {
        let this = grid.read(cx);
        let Some(&column) = this.columns.get(col) else { return menu };
        let name = this.set.columns[column].name.clone();
        let sorted = match &this.overlay {
            Some(overlay) => overlay.sort,
            None => this.sort.column.map(|c| (c, this.sort.desc)),
        };
        let any_hidden = this.hidden_count() > 0;
        let sort = |grid: &Entity<Self>, to: Option<(usize, bool)>| {
            on(grid, move |grid, _, cx| match grid.overlay.is_some() {
                true => cx.emit(GridEvent::SortTo(to)),
                false => grid.sort_to(to, cx),
            })
        };
        let menu = menu
            .item(
                item(t(cx, "grid.sortAscending"), sort(grid, Some((column, false))))
                    .checked(sorted == Some((column, false))),
            )
            .item(
                item(t(cx, "grid.sortDescending"), sort(grid, Some((column, true))))
                    .checked(sorted == Some((column, true))),
            )
            .item(item(t(cx, "grid.clearSort"), sort(grid, None)).disabled(sorted.is_none()))
            .separator();
        let menu = Self::copy_items(grid, menu, window, cx)
            .item(PopupMenuItem::new(t(cx, "grid.copyColumnName")).on_click(move |_, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(name.clone()));
                toast::success(t(cx, "toast.copiedClipboard"), cx);
            }))
            .separator()
            .item(item(t(cx, "grid.hideColumn"), on(grid, move |grid, _, cx| grid.toggle_column(column, cx))))
            .item(
                item(t(cx, "grid.showAllColumns"), on(grid, |grid, _, cx| grid.show_all_columns(cx)))
                    .disabled(!any_hidden),
            )
            .item(item(t(cx, "results.fitColumns"), on(grid, |grid, _, cx| grid.fit_columns(cx))));
        Self::selection_items(grid, menu, cx)
    }
}
