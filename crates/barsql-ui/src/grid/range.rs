use std::collections::BTreeSet;

// All coordinates here are display positions, not result-set indices.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellCoord {
    pub row: usize,
    pub col: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellRange {
    pub r0: usize,
    pub r1: usize,
    pub c0: usize,
    pub c1: usize,
}

impl CellRange {
    pub fn between(anchor: CellCoord, current: CellCoord) -> Self {
        Self {
            r0: anchor.row.min(current.row),
            r1: anchor.row.max(current.row),
            c0: anchor.col.min(current.col),
            c1: anchor.col.max(current.col),
        }
    }

    pub fn full_rows(r0: usize, r1: usize, col_count: usize) -> Self {
        Self { r0: r0.min(r1), r1: r0.max(r1), c0: 0, c1: col_count.saturating_sub(1) }
    }

    pub fn full_cols(c0: usize, c1: usize, row_count: usize) -> Self {
        Self { r0: 0, r1: row_count.saturating_sub(1), c0: c0.min(c1), c1: c0.max(c1) }
    }

    pub fn contains(&self, row: usize, col: usize) -> bool {
        (self.r0..=self.r1).contains(&row) && (self.c0..=self.c1).contains(&col)
    }

    // (rows, columns)
    pub fn dimensions(&self) -> (usize, usize) {
        (self.r1 - self.r0 + 1, self.c1 - self.c0 + 1)
    }
}

// Sides of a selected cell that sit on the selection outline.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Edges {
    pub top: bool,
    pub bottom: bool,
    pub left: bool,
    pub right: bool,
}

pub fn range_edges(row: usize, col: usize, range: &CellRange) -> Option<Edges> {
    range.contains(row, col).then_some(Edges {
        top: row == range.r0,
        bottom: row == range.r1,
        left: col == range.c0,
        right: col == range.c1,
    })
}

// Adjacent selected rows merge into one band with no lines between them.
pub fn selected_rows_edges(row: usize, col: usize, rows: &BTreeSet<usize>, col_count: usize) -> Option<Edges> {
    rows.contains(&row).then(|| Edges {
        top: row == 0 || !rows.contains(&(row - 1)),
        bottom: !rows.contains(&(row + 1)),
        left: col == 0,
        right: col + 1 == col_count,
    })
}

fn selected_cols_edges(row: usize, col: usize, cols: &BTreeSet<usize>, row_count: usize) -> Option<Edges> {
    cols.contains(&col).then(|| Edges {
        top: row == 0,
        bottom: row + 1 == row_count,
        left: col == 0 || !cols.contains(&(col - 1)),
        right: !cols.contains(&(col + 1)),
    })
}

// A cell range wins over row and column bands.
pub fn highlight(
    row: usize,
    col: usize,
    range: Option<&CellRange>,
    rows: &BTreeSet<usize>,
    cols: &BTreeSet<usize>,
    row_count: usize,
    col_count: usize,
) -> Option<Edges> {
    if let Some(range) = range {
        return range_edges(row, col, range);
    }
    if !rows.is_empty() {
        return selected_rows_edges(row, col, rows, col_count);
    }
    if !cols.is_empty() {
        return selected_cols_edges(row, col, cols, row_count);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_order_their_corners_whichever_way_they_were_dragged() {
        let range = CellRange::between(CellCoord { row: 5, col: 2 }, CellCoord { row: 1, col: 0 });
        assert_eq!(range, CellRange { r0: 1, r1: 5, c0: 0, c1: 2 });
    }

    #[test]
    fn only_perimeter_cells_get_edges() {
        let range = CellRange { r0: 0, r1: 1, c0: 0, c1: 1 };
        let top_left = range_edges(0, 0, &range).unwrap();
        assert!(top_left.top && top_left.left && !top_left.bottom && !top_left.right);
        assert!(range_edges(1, 1, &range).unwrap().bottom);
        assert!(range.contains(0, 1));
        let top_right = range_edges(0, 1, &range).unwrap();
        assert!(top_right.right && !top_right.left);
        assert_eq!(range_edges(2, 0, &range), None);
    }

    #[test]
    fn dimensions_count_rows_and_columns() {
        assert_eq!(CellRange { r0: 5, r1: 13, c0: 1, c1: 3 }.dimensions(), (9, 3));
    }

    #[test]
    fn row_and_column_selections_span_the_other_axis() {
        assert_eq!(CellRange::full_rows(2, 4, 5), CellRange { r0: 2, r1: 4, c0: 0, c1: 4 });
        assert_eq!(CellRange::full_cols(1, 2, 10), CellRange { r0: 0, r1: 9, c0: 1, c1: 2 });
    }

    #[test]
    fn selected_rows_draw_one_band() {
        let rows = BTreeSet::from([1, 2]);
        let first = selected_rows_edges(1, 0, &rows, 4).unwrap();
        assert!(first.top && !first.bottom);
        let second = selected_rows_edges(2, 0, &rows, 4).unwrap();
        assert!(second.bottom && !second.top);
    }

    #[test]
    fn a_cell_range_outranks_row_and_column_bands() {
        let range = CellRange { r0: 0, r1: 0, c0: 0, c1: 1 };
        let edges = highlight(0, 0, Some(&range), &BTreeSet::from([1]), &BTreeSet::from([2]), 5, 4);
        assert_eq!(edges, range_edges(0, 0, &range));
    }
}
