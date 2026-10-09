use std::sync::Arc;

use barsql_db::{Cell, ColumnMeta, ResultSet};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::*;

use super::super::copy::{self, Staged};
use super::super::sort::RowOrder;
use super::{Editing, Grid, GridEvent, SortState, TableOverlay, Width};
use crate::table_edits::{paste_cells, text_cells};

// Rows this close to the bottom ask for the next page.
const LOAD_MORE_ROWS: f32 = 4.;

impl Grid {
    // Turns on table-view mode. Also called to refresh the staged edits.
    pub fn set_overlay(&mut self, overlay: TableOverlay, cx: &mut Context<Self>) {
        self.overlay = Some(overlay);
        cx.notify();
    }

    pub fn set_table_name(&mut self, table: Option<String>) {
        self.table = table;
    }

    pub fn set_dialect(&mut self, dialect: Option<barsql_core::SqlDialect>) {
        self.dialect = dialect;
    }

    pub(super) fn staged(&self) -> Option<Staged> {
        self.overlay.as_ref().map(|overlay| overlay.staged.clone())
    }

    fn editable(&self) -> bool {
        self.overlay.as_ref().is_some_and(|overlay| overlay.editable)
    }

    // A staged value wins over the loaded one.
    pub fn shown(&self, row: usize, column: usize) -> Cell<'_> {
        copy::shown(&self.set, self.overlay.as_ref().map(|overlay| &overlay.staged), row, column)
    }

    pub fn is_deleted(&self, row: usize) -> bool {
        self.overlay.as_ref().is_some_and(|overlay| overlay.deleted.contains(&row))
    }

    // For a replacing page load. Hidden columns and widths survive only if the column names match.
    pub fn reset(&mut self, columns: Arc<[ColumnMeta]>, cx: &mut Context<Self>) {
        let same = columns.len() == self.set.columns.len()
            && columns.iter().zip(self.set.columns.iter()).all(|(a, b)| a.name == b.name);
        let count = columns.len();
        self.set = ResultSet::new(columns);
        self.order = RowOrder::identity(0);
        self.ordered_for = (SortState::default(), 0);
        self.sorting = None;
        self.selection.clear();
        self.editing = None;
        if !same {
            self.hidden.clear();
            self.columns = (0..count).collect();
            self.widths = vec![Width::Ch(0); count];
            self.user_sized.clear();
            self.sampled = None;
            self.selection = Default::default();
        }
        cx.notify();
    }

    // Once loading ends, scrolling near the bottom can ask for more again.
    pub fn set_loading(&mut self, loading: bool) {
        if !loading {
            self.load_armed = true;
        }
    }

    pub fn hidden_names(&self) -> Vec<String> {
        self.hidden.iter().filter_map(|&c| self.set.columns.get(c).map(|meta| meta.name.clone())).collect()
    }

    pub fn set_hidden_names(&mut self, names: &[String], cx: &mut Context<Self>) {
        self.hidden = (0..self.set.columns.len()).filter(|&c| names.contains(&self.set.columns[c].name)).collect();
        self.columns = (0..self.set.columns.len()).filter(|c| !self.hidden.contains(c)).collect();
        cx.notify();
    }

    // Asks for the next page when the last rows scroll into view. The pane decides whether there is one.
    pub(super) fn check_load_more(&mut self, cx: &mut Context<Self>) {
        if self.overlay.is_none() {
            return;
        }
        let position = self.scroll.position().y;
        if position == self.scrolled_to {
            return;
        }
        self.scrolled_to = position;
        let content = self.metrics.row_height * self.order.len() as f32;
        let bottom = position + self.scroll.viewport().size.height;
        if self.load_armed && bottom >= content - self.metrics.row_height * LOAD_MORE_ROWS {
            self.load_armed = false;
            let grid = cx.entity();
            cx.defer(move |cx| grid.update(cx, |_, cx| cx.emit(GridEvent::LoadMore)));
        }
    }

    pub(super) fn table_event(&mut self, event: GridEvent, cx: &mut Context<Self>) {
        if self.overlay.is_none() {
            cx.propagate();
            return;
        }
        cx.emit(event);
    }

    // (display row, result row, result column)
    fn focused_data_cell(&self) -> Option<(usize, usize, usize)> {
        let row = self.selection.focus_row?;
        let display = self.order.display_of(row)?;
        let column = *self.columns.get(usize::try_from(self.selection.focus_col).ok()?)?;
        Some((display, row, column))
    }

    pub(super) fn enter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Enter in the editor lands here before its own commit, so commit now rather than start a new edit.
        if self.editing.is_some() {
            self.commit_edit(window, cx);
            return;
        }
        if self.editable() {
            if let Some((_, row, column)) = self.focused_data_cell() {
                self.selection.clear();
                self.start_edit(row, column, window, cx);
            }
            return;
        }
        self.view_focused(cx);
    }

    // Shift+Enter. In an editable table view the viewer opens as an editor.
    pub(super) fn view_value(&mut self, cx: &mut Context<Self>) {
        match self.focused_data_cell() {
            Some((_, row, column)) if self.editable() => cx.emit(GridEvent::EditInViewer { row, column }),
            _ => self.view_focused(cx),
        }
    }

    pub(super) fn delete_focused_row(&mut self, cx: &mut Context<Self>) {
        match self.selection.focus_row {
            Some(row) if self.editable() && self.editing.is_none() => cx.emit(GridEvent::ToggleDelete(row)),
            _ => cx.propagate(),
        }
    }

    pub(super) fn null_focused_cell(&mut self, cx: &mut Context<Self>) {
        if !self.editable() || self.editing.is_some() {
            cx.propagate();
            return;
        }
        if let Some((_, row, column)) = self.focused_data_cell()
            && !self.is_deleted(row)
        {
            cx.emit(GridEvent::EditCell { row, column, value: None });
        }
    }

    // Pastes at the range's top-left or the focused cell. A copy from any grid pastes back exactly, and other
    // text is split into tab- or comma-separated fields.
    pub(super) fn paste(&mut self, cx: &mut Context<Self>) {
        if !self.editable() {
            cx.propagate();
            return;
        }
        let anchor = match self.selection.range {
            Some(range) => Some((range.r0, range.c0)),
            None => self.focused_data_cell().map(|(display, ..)| (display, self.selection.focus_col as usize)),
        };
        let Some((row, col)) = anchor else { return };
        let item = cx.read_from_clipboard();
        let grid = match item.as_ref().and_then(copy::copied_cells) {
            Some(cells) => cells,
            None => text_cells(&item.and_then(|item| item.text()).unwrap_or_default()),
        };
        let cells: Vec<_> = paste_cells(&grid, row, col, self.order.len(), &self.columns)
            .into_iter()
            .filter_map(|(display, column, value)| Some((self.order.global_at(display)?, column, value)))
            .collect();
        if !cells.is_empty() {
            cx.emit(GridEvent::PasteCells(cells));
        }
    }

    pub(super) fn start_edit(&mut self, row: usize, column: usize, window: &mut Window, cx: &mut Context<Self>) {
        let value = self.shown(row, column).display().unwrap_or_default().to_string();
        let input = cx.new(|cx| InputState::new(window, cx).default_value(value));
        let subscription = cx.subscribe_in(&input, window, |grid, input, event: &InputEvent, window, cx| match event {
            // Its own right-click menu took the focus, and the edit carries on once that closes.
            InputEvent::Blur if input.read(cx).has_selection_focus(window, cx) => {}
            InputEvent::PressEnter { .. } | InputEvent::Blur => grid.commit_edit(window, cx),
            _ => {}
        });
        input.update(cx, |state, cx| {
            let end = state.value().len();
            state.set_selected_range(end..end, cx);
            state.focus(window, cx);
        });
        if let Some(display) = self.order.display_of(row) {
            let col = self.columns.iter().position(|&c| c == column).map_or(0, |p| p as isize);
            self.reveal(display, col);
        }
        self.editing = Some(Editing { row, column, input, _subscription: subscription });
        cx.notify();
    }

    // A blank editor stages NULL. The owner drops the edit if the value matches the original.
    fn commit_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editing) = self.editing.take() else { return };
        let text = editing.input.read(cx).value().to_string();
        let value = (!text.is_empty()).then_some(text);
        window.focus(&self.focus_handle, cx);
        cx.emit(GridEvent::EditCell { row: editing.row, column: editing.column, value });
        cx.notify();
    }

    pub(super) fn cancel_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing.take().is_some() {
            window.focus(&self.focus_handle, cx);
            cx.notify();
        }
    }

    // Relative to the grid, not the window.
    pub(super) fn editor_bounds(&self) -> Option<(Entity<InputState>, Bounds<Pixels>)> {
        let editing = self.editing.as_ref()?;
        let display = self.order.display_of(editing.row)?;
        let col = self.columns.iter().position(|&c| c == editing.column)?;
        let m = &self.metrics;
        let scroll = self.scroll.position();
        let origin = point(
            m.gutter_width + self.col_x[col] - scroll.x,
            m.header_height + m.row_height * display as f32 - scroll.y,
        );
        let size = size(self.col_x[col + 1] - self.col_x[col], m.row_height);
        Some((editing.input.clone(), Bounds::new(origin, size)))
    }
}
