use std::collections::BTreeSet;

use super::range::{CellCoord, CellRange};
use super::sort::RowOrder;

// `rows` and `columns` hold result indices, so a selection survives sorting and hiding columns.
// Cell ranges and `focus_col` are display positions.

// `focus_col` value for the row-number gutter.
pub const GUTTER: isize = -1;

pub struct View<'a> {
    pub order: &'a RowOrder,
    // Result column index of each visible column, in display order.
    pub columns: &'a [usize],
}

impl View<'_> {
    fn rows(&self) -> usize {
        self.order.len()
    }

    fn position_of(&self, column: usize) -> Option<usize> {
        self.columns.iter().position(|&c| c == column)
    }
}

#[derive(Debug, Clone, Default)]
pub struct Selection {
    pub range: Option<CellRange>,
    pub rows: BTreeSet<usize>,
    pub columns: BTreeSet<usize>,
    pub focus_row: Option<usize>,
    pub focus_col: isize,
    row_anchor: Option<usize>,
    column_anchor: Option<usize>,
    anchor: Option<CellCoord>,
    drag: Option<CellCoord>,
    shift_applied: bool,
}

impl Selection {
    pub fn is_empty(&self) -> bool {
        self.range.is_none() && self.rows.is_empty() && self.columns.is_empty()
    }

    pub fn clear(&mut self) {
        self.range = None;
        self.anchor = None;
        self.rows.clear();
        self.columns.clear();
        self.row_anchor = None;
        self.column_anchor = None;
    }

    pub fn focus(&mut self, row: usize, col: isize) {
        self.focus_row = Some(row);
        self.focus_col = col;
    }

    pub fn seed(&mut self, row: usize) {
        self.focus(row, 0);
        self.row_anchor = Some(row);
    }

    pub fn dragging(&self) -> bool {
        self.drag.is_some()
    }

    // A right-click keeps a selection that holds the cell, as in a spreadsheet. `row` and `col` are display
    // positions.
    pub fn covers(&self, row: usize, col: usize, view: &View) -> bool {
        if let Some(range) = &self.range {
            return range.contains(row, col);
        }
        view.order.global_at(row).is_some_and(|global| self.covers_row(global))
            || view.columns.get(col).is_some_and(|&column| self.covers_column(column))
    }

    // Whole selected rows, not a cell range that crosses them.
    pub fn covers_row(&self, global: usize) -> bool {
        self.columns.is_empty() && self.rows.contains(&global)
    }

    pub fn covers_column(&self, column: usize) -> bool {
        self.rows.is_empty() && self.columns.contains(&column)
    }

    fn apply_range(&mut self, anchor: CellCoord, current: CellCoord, view: &View) {
        let range = CellRange::between(anchor, current);
        self.range = Some(range);
        self.columns = (range.c0..=range.c1).filter_map(|c| view.columns.get(c).copied()).collect();
        self.rows = (range.r0..=range.r1).filter_map(|r| view.order.global_at(r)).collect();
    }

    fn apply_row_band(&mut self, range: CellRange, rows: BTreeSet<usize>) {
        self.range = Some(range);
        self.anchor = None;
        self.rows = rows;
        self.columns.clear();
    }

    fn apply_column_band(&mut self, range: CellRange, columns: BTreeSet<usize>) {
        self.range = Some(range);
        self.anchor = None;
        self.columns = columns;
        self.rows.clear();
    }

    pub fn select_all(&mut self, view: &View) {
        if view.rows() == 0 || view.columns.is_empty() {
            return;
        }
        let anchor = CellCoord { row: 0, col: 0 };
        self.anchor = Some(anchor);
        self.apply_range(anchor, CellCoord { row: view.rows() - 1, col: view.columns.len() - 1 }, view);
    }

    // Excel-style. An earlier shift anchor wins, then the focused cell, then the target itself.
    fn shift_anchor(&self, fallback: CellCoord, view: &View) -> CellCoord {
        if let Some(anchor) = self.anchor {
            return anchor;
        }
        let focused = self.focus_row.and_then(|row| view.order.display_of(row));
        match (focused, self.focus_col) {
            (Some(row), col) if col >= 0 => CellCoord { row, col: col as usize },
            _ => fallback,
        }
    }

    // Shift extends from the anchor. Otherwise a drag starts here.
    pub fn press_cell(&mut self, row: usize, col: usize, shift: bool, view: &View) {
        let target = CellCoord { row, col };
        let global = view.order.global_at(row);
        if shift {
            let anchor = self.shift_anchor(target, view);
            if let Some(global) = global {
                self.focus(global, col as isize);
            }
            self.anchor = Some(anchor);
            self.apply_range(anchor, target, view);
            self.shift_applied = true;
            return;
        }
        if let Some(global) = global {
            self.focus(global, col as isize);
        }
        self.anchor = Some(target);
        self.drag = Some(target);
        self.apply_range(target, target, view);
    }

    pub fn drag_to(&mut self, row: usize, col: usize, view: &View) {
        if let Some(anchor) = self.drag {
            self.apply_range(anchor, CellCoord { row, col }, view);
        }
    }

    // A press and release on the same cell is a click, which drops the selection.
    pub fn release(&mut self, at: Option<CellCoord>) {
        let Some(start) = self.drag.take() else {
            self.shift_applied = false;
            return;
        };
        if self.shift_applied {
            self.shift_applied = false;
            return;
        }
        if at == Some(start) {
            self.clear();
        }
    }

    pub fn click_header(&mut self, column: usize, ctrl: bool, shift: bool, view: &View) {
        let Some(position) = view.position_of(column) else { return };
        if self.focus_row.is_some() {
            self.focus_col = position as isize;
        }
        let single = if self.columns.len() == 1 { self.columns.first().copied() } else { None };
        let anchor = single.or(self.column_anchor).unwrap_or(column);
        if shift {
            let mut next = if ctrl { self.columns.clone() } else { BTreeSet::new() };
            match (view.position_of(anchor), Some(position)) {
                (Some(a), Some(b)) => next.extend(view.columns[a.min(b)..=a.max(b)].iter().copied()),
                _ => {
                    next.insert(column);
                }
            }
            let positions: Vec<usize> = next.iter().filter_map(|&c| view.position_of(c)).collect();
            match (positions.iter().min(), positions.iter().max()) {
                (Some(&lo), Some(&hi)) => self.apply_column_band(CellRange::full_cols(lo, hi, view.rows()), next),
                _ => {
                    self.range = None;
                    self.columns = next;
                    self.rows.clear();
                }
            }
            self.column_anchor = Some(column);
            return;
        }
        if ctrl {
            self.range = None;
            self.anchor = None;
            if !self.columns.remove(&column) {
                self.columns.insert(column);
            }
            self.rows.clear();
            self.column_anchor = Some(column);
            return;
        }
        self.apply_column_band(CellRange::full_cols(position, position, view.rows()), BTreeSet::from([column]));
        self.column_anchor = Some(column);
    }

    pub fn click_gutter(&mut self, global: usize, ctrl: bool, shift: bool, view: &View) {
        let display = view.order.display_of(global);
        let focused = self.focus_row.filter(|&row| view.order.display_of(row).is_some());
        let anchor = self.row_anchor.or(focused).unwrap_or(global);
        if shift {
            let mut next = if ctrl { self.rows.clone() } else { BTreeSet::new() };
            match (view.order.display_of(anchor), display) {
                (Some(a), Some(b)) => next.extend((a.min(b)..=a.max(b)).filter_map(|d| view.order.global_at(d))),
                _ => {
                    next.insert(global);
                }
            }
            let positions: Vec<usize> = next.iter().filter_map(|&g| view.order.display_of(g)).collect();
            match (positions.iter().min(), positions.iter().max()) {
                (Some(&lo), Some(&hi)) => self.apply_row_band(CellRange::full_rows(lo, hi, view.columns.len()), next),
                _ => {
                    self.range = None;
                    self.rows = next;
                    self.columns.clear();
                }
            }
            self.row_anchor = Some(global);
            self.focus(global, GUTTER);
            return;
        }
        if ctrl {
            self.range = None;
            self.anchor = None;
            if !self.rows.remove(&global) {
                self.rows.insert(global);
            }
            self.columns.clear();
            self.row_anchor = Some(global);
            self.focus(global, GUTTER);
            return;
        }
        if let Some(display) = display {
            self.apply_row_band(CellRange::full_rows(display, display, view.columns.len()), BTreeSet::from([global]));
        }
        self.row_anchor = Some(global);
        self.focus(global, GUTTER);
    }

    // Shift extends the range from the anchor. A plain move drops the selection.
    pub fn move_focus(&mut self, row: usize, col: isize, shift: bool, view: &View) {
        let Some(global) = view.order.global_at(row) else { return };
        let col = if col < 0 { GUTTER } else { col.min(view.columns.len() as isize - 1).max(0) };
        if shift && col >= 0 {
            let target = CellCoord { row, col: col as usize };
            let anchor = self.shift_anchor(target, view);
            self.anchor = Some(anchor);
            self.apply_range(anchor, target, view);
        } else if !shift && !self.is_empty() {
            self.clear();
        }
        self.focus(global, col);
    }

    // None when the focus is already at the edge.
    pub fn arrow_target(&self, key: Arrow, view: &View) -> Option<(usize, isize)> {
        let row = self.focus_row.and_then(|r| view.order.display_of(r))?;
        let col = self.focus_col;
        let cols = view.columns.len() as isize;
        match key {
            Arrow::Right if col < cols - 1 => Some((row, col + 1)),
            Arrow::Left if col > 0 => Some((row, col - 1)),
            Arrow::Left if col == 0 => Some((row, GUTTER)),
            Arrow::Down if row + 1 < view.rows() => Some((row + 1, col)),
            Arrow::Up if row > 0 => Some((row - 1, col)),
            _ => None,
        }
    }

    // (rows, columns) for the toolbar's selection count.
    pub fn counts(&self, view: &View) -> (usize, usize) {
        if let Some(range) = &self.range {
            return range.dimensions();
        }
        if !self.rows.is_empty() {
            return (self.rows.len(), view.columns.len());
        }
        if !self.columns.is_empty() {
            return (view.rows(), self.columns.len());
        }
        (0, 0)
    }

    // Display rows and column positions to draw as bands. Empty when there's a cell range.
    pub fn bands(&self, view: &View) -> (BTreeSet<usize>, BTreeSet<usize>) {
        if self.range.is_some() {
            return Default::default();
        }
        let rows = self.rows.iter().filter_map(|&g| view.order.display_of(g)).collect();
        let cols = self.columns.iter().filter_map(|&c| view.position_of(c)).collect();
        (rows, cols)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arrow {
    Up,
    Down,
    Left,
    Right,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view<'a>(order: &'a RowOrder, columns: &'a [usize]) -> View<'a> {
        View { order, columns }
    }

    #[test]
    fn a_click_focuses_and_a_drag_selects_a_range() {
        let order = RowOrder::identity(10);
        let columns = [0, 1, 2, 3];
        let v = view(&order, &columns);
        let mut s = Selection::default();
        s.press_cell(2, 1, false, &v);
        s.release(Some(CellCoord { row: 2, col: 1 }));
        assert!(s.is_empty());
        assert_eq!((s.focus_row, s.focus_col), (Some(2), 1));

        s.press_cell(2, 1, false, &v);
        s.drag_to(4, 3, &v);
        s.release(Some(CellCoord { row: 4, col: 3 }));
        assert_eq!(s.range, Some(CellRange { r0: 2, r1: 4, c0: 1, c1: 3 }));
        assert_eq!(s.rows, BTreeSet::from([2, 3, 4]));
        assert_eq!(s.columns, BTreeSet::from([1, 2, 3]));
        assert_eq!(s.counts(&v), (3, 3));
    }

    #[test]
    fn shift_click_extends_from_the_focused_cell() {
        let order = RowOrder::identity(10);
        let columns = [0, 1, 2];
        let v = view(&order, &columns);
        let mut s = Selection::default();
        s.press_cell(1, 0, false, &v);
        s.release(Some(CellCoord { row: 1, col: 0 }));
        s.press_cell(3, 2, true, &v);
        s.release(Some(CellCoord { row: 3, col: 2 }));
        assert_eq!(s.range, Some(CellRange { r0: 1, r1: 3, c0: 0, c1: 2 }), "the release keeps a shift selection");
        s.press_cell(5, 1, true, &v);
        assert_eq!(s.range, Some(CellRange { r0: 1, r1: 5, c0: 0, c1: 1 }), "the anchor stays put");
    }

    #[test]
    fn ranges_over_a_sorted_view_select_the_displayed_rows() {
        let order = RowOrder::sorted(vec![3, 1, 4, 0, 2]);
        let columns = [0];
        let v = view(&order, &columns);
        let mut s = Selection::default();
        s.press_cell(1, 0, false, &v);
        s.drag_to(3, 0, &v);
        assert_eq!(s.rows, BTreeSet::from([1, 4, 0]));
    }

    #[test]
    fn header_clicks_select_ctrl_toggles_and_shift_extends_columns() {
        let order = RowOrder::identity(5);
        let columns = [0, 1, 2, 3];
        let v = view(&order, &columns);
        let mut s = Selection::default();
        s.click_header(1, false, false, &v);
        assert_eq!((s.columns.clone(), s.range), (BTreeSet::from([1]), Some(CellRange::full_cols(1, 1, 5))));
        s.click_header(3, false, true, &v);
        assert_eq!(s.columns, BTreeSet::from([1, 2, 3]));
        s.click_header(2, true, false, &v);
        assert_eq!((s.columns.clone(), s.range), (BTreeSet::from([1, 3]), None));
        assert_eq!(s.counts(&v), (5, 2));
        assert_eq!(s.bands(&v).1, BTreeSet::from([1, 3]));
    }

    #[test]
    fn gutter_clicks_select_rows_and_focus_the_gutter() {
        let order = RowOrder::identity(6);
        let columns = [0, 1];
        let v = view(&order, &columns);
        let mut s = Selection::default();
        s.click_gutter(1, false, false, &v);
        assert_eq!((s.rows.clone(), s.focus_col), (BTreeSet::from([1]), GUTTER));
        s.click_gutter(4, false, true, &v);
        assert_eq!(s.rows, BTreeSet::from([1, 2, 3, 4]));
        assert_eq!(s.range, Some(CellRange::full_rows(1, 4, 2)));
        s.click_gutter(2, true, false, &v);
        assert_eq!(s.rows, BTreeSet::from([1, 3, 4]));
        assert_eq!(s.bands(&v).0, BTreeSet::from([1, 3, 4]));
    }

    #[test]
    fn arrows_move_focus_extend_with_shift_and_stop_at_edges() {
        let order = RowOrder::identity(3);
        let columns = [0, 1];
        let v = view(&order, &columns);
        let mut s = Selection::default();
        s.focus(0, 0);
        assert_eq!(s.arrow_target(Arrow::Up, &v), None);
        assert_eq!(s.arrow_target(Arrow::Left, &v), Some((0, GUTTER)));
        let (row, col) = s.arrow_target(Arrow::Down, &v).unwrap();
        s.move_focus(row, col, true, &v);
        let (row, col) = s.arrow_target(Arrow::Right, &v).unwrap();
        s.move_focus(row, col, true, &v);
        assert_eq!(s.range, Some(CellRange { r0: 0, r1: 1, c0: 0, c1: 1 }));
        assert_eq!(s.arrow_target(Arrow::Right, &v), None);
        let (row, col) = s.arrow_target(Arrow::Up, &v).unwrap();
        s.move_focus(row, col, false, &v);
        assert!(s.is_empty());
        assert_eq!((s.focus_row, s.focus_col), (Some(0), 1));
    }

    #[test]
    fn select_all_covers_the_view() {
        let order = RowOrder::identity(4);
        let columns = [2, 0];
        let v = view(&order, &columns);
        let mut s = Selection::default();
        s.select_all(&v);
        assert_eq!(s.range, Some(CellRange { r0: 0, r1: 3, c0: 0, c1: 1 }));
        assert_eq!(s.columns, BTreeSet::from([0, 2]));
        let mut empty = Selection::default();
        empty.select_all(&view(&RowOrder::identity(0), &columns));
        assert!(empty.is_empty());
    }
}
