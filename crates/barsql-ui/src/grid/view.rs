use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use barsql_db::{ColumnMeta, ResultChunk, ResultSet};
use barsql_io::ExportFormat;
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::ActiveTheme;
use gpui_kit::component::input::{Copy, Input, InputState, Paste, Redo, SelectAll, Undo};
use gpui_kit::component::menu::{ContextMenuExt, PopupMenu, PopupMenuItem};
use gpui_kit::component::scroll::Scrollbar;
use gpui_kit::*;

use super::copy::{self, CopyTarget, Staged};
use super::layout::{SAMPLE_ROWS, auto_width_ch, row_height};
use super::range::{self, CellCoord};
use super::scroll::GridScroll;
use super::selection::{Arrow, GUTTER, Selection, View};
use super::sort::{RowOrder, SortState, sort_order};
use crate::i18n::t;
use crate::state::{set_setting, setting};
use crate::toast;

const CONTEXT: &str = "Grid";
const FORMAT_KEY: &str = "barsql-export-format";
const MIN_COLUMN_PX: f32 = 40.;
const RESIZE_HANDLE_PX: f32 = 5.;
// Longer values are cut before layout. The cell viewer still shows them whole.
const MAX_CELL_CHARS: usize = 1000;
// Larger results sort off the main thread, keeping the previous order until the new one is ready.
const SYNC_SORT_ROWS: usize = 20_000;

actions!(
    grid,
    [
        MoveUp,
        MoveDown,
        MoveLeft,
        MoveRight,
        SelectUp,
        SelectDown,
        SelectLeft,
        SelectRight,
        PageUp,
        PageDown,
        MoveToTop,
        MoveToBottom,
        ClearSelection,
        ViewCell,
        ViewCellValue,
        ExportResults,
        ToggleDeleteRow,
        SetNull,
        ApplyChanges,
        RefreshData,
    ]
);

#[cfg(target_os = "macos")]
const MOD: &str = "cmd";
#[cfg(not(target_os = "macos"))]
const MOD: &str = "ctrl";

pub fn init(cx: &mut App) {
    let context = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("up", MoveUp, context),
        KeyBinding::new("down", MoveDown, context),
        KeyBinding::new("left", MoveLeft, context),
        KeyBinding::new("right", MoveRight, context),
        KeyBinding::new("shift-up", SelectUp, context),
        KeyBinding::new("shift-down", SelectDown, context),
        KeyBinding::new("shift-left", SelectLeft, context),
        KeyBinding::new("shift-right", SelectRight, context),
        KeyBinding::new("pageup", PageUp, context),
        KeyBinding::new("pagedown", PageDown, context),
        KeyBinding::new("home", MoveToTop, context),
        KeyBinding::new("end", MoveToBottom, context),
        KeyBinding::new("escape", ClearSelection, context),
        KeyBinding::new("enter", ViewCell, context),
        KeyBinding::new("shift-enter", ViewCellValue, context),
        KeyBinding::new(&format!("{MOD}-c"), Copy, context),
        KeyBinding::new(&format!("{MOD}-a"), SelectAll, context),
        KeyBinding::new("delete", ToggleDeleteRow, context),
        KeyBinding::new("backspace", ToggleDeleteRow, context),
        KeyBinding::new(&format!("{MOD}-delete"), SetNull, context),
        KeyBinding::new(&format!("{MOD}-backspace"), SetNull, context),
        KeyBinding::new(&format!("{MOD}-v"), Paste, context),
        KeyBinding::new(&format!("{MOD}-z"), Undo, context),
        KeyBinding::new(&format!("{MOD}-shift-z"), Redo, context),
        KeyBinding::new(&format!("{MOD}-y"), Redo, context),
        KeyBinding::new(&format!("{MOD}-s"), ApplyChanges, context),
        KeyBinding::new(&format!("{MOD}-r"), RefreshData, context),
        KeyBinding::new("f5", RefreshData, context),
    ]);
}

// One setting shared by every grid.
pub fn copy_format(cx: &App) -> ExportFormat {
    setting(cx, FORMAT_KEY).and_then(|id| ExportFormat::parse(&id)).unwrap_or(ExportFormat::Csv)
}

pub fn set_copy_format(format: ExportFormat, cx: &mut App) {
    set_setting(cx, FORMAT_KEY, format.id());
}

// Rows and columns are result indices in display order.
pub struct ExportSource {
    pub set: ResultSet,
    pub staged: Option<Staged>,
    pub table: Option<String>,
    pub order: Vec<usize>,
    pub selected_rows: Vec<usize>,
    pub visible: Vec<usize>,
    pub selected_columns: Vec<usize>,
}

// For the JSON row panel, with only the visible columns.
#[derive(Clone)]
pub struct RowRef {
    pub set: ResultSet,
    pub row: usize,
    pub columns: Vec<usize>,
}

pub enum GridEvent {
    // Result row and column.
    ViewCell { row: usize, column: usize },
    Export,
    // Also sent when the visible columns change.
    FocusedRowChanged,
    HiddenChanged,
    // Table views only. The server sorts, pages load on demand and the owner stages edits.
    SortRequested(usize),
    LoadMore,
    EditCell { row: usize, column: usize, value: Option<String> },
    EditInViewer { row: usize, column: usize },
    PasteCells(Vec<(usize, usize, Option<String>)>),
    ToggleDelete(usize),
    OpenForeignKey { row: usize, column: usize },
    Undo,
    Redo,
    Apply,
    Refresh,
}

// Rows are result indices and `sort` is the server's (column, desc).
#[derive(Clone, Default)]
pub struct TableOverlay {
    pub staged: Staged,
    pub deleted: HashSet<usize>,
    pub sort: Option<(usize, bool)>,
    // Column -> referenced table.
    pub foreign: HashMap<usize, String>,
    pub editable: bool,
}

struct Editing {
    row: usize,
    column: usize,
    input: Entity<InputState>,
    _subscription: Subscription,
}

// Auto-fit widths are in `ch` of the grid font so they follow the zoom. Dragged widths stay fixed.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Width {
    Ch(usize),
    Px(Pixels),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Press {
    Cells,
    Resize { column: usize, start_x: Pixels, start_width: Pixels },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Zone {
    Title,
    Sort,
    Resize,
}

// Rows are display rows and columns are display positions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    Corner,
    Header { col: usize, zone: Zone },
    Gutter { row: usize },
    Cell { row: usize, col: usize },
    // Foreign-key jump button in a table view cell.
    Jump { row: usize, col: usize },
}

// All in px at the current zoom.
#[derive(Debug, Clone, Copy, Default)]
struct Metrics {
    font_size: Pixels,
    line_height: Pixels,
    row_height: Pixels,
    header_height: Pixels,
    gutter_width: Pixels,
    cell_pad_x: Pixels,
    cell_pad_y: Pixels,
    gutter_pad_x: Pixels,
    chevron: Pixels,
    gap: Pixels,
    icon: Pixels,
    radius: Pixels,
    jump_inset: Pixels,
    ch: Pixels,
}

impl Metrics {
    fn new(font: &Font, window: &Window) -> Self {
        let rem = f32::from(window.rem_size());
        let r = |value: f32| px((rem * value).round());
        let font_size = px(rem * 0.923);
        let text_system = window.text_system();
        let ch = text_system.ch_advance(text_system.resolve_font(font), font_size).unwrap_or(px(rem * 0.6));
        Self {
            font_size,
            line_height: r(1.231),
            row_height: px(row_height(rem)),
            header_height: px((rem * (0.462 * 2. + 1.385)).round() + 1.),
            gutter_width: r(4.),
            cell_pad_x: r(0.769),
            cell_pad_y: r(0.308),
            gutter_pad_x: r(0.615),
            chevron: r(1.385),
            gap: r(0.385),
            icon: r(0.923),
            radius: r(0.308),
            jump_inset: r(0.231),
            ch,
        }
    }
}

// Custom-painted. Only the visible cells are laid out.
pub struct Grid {
    set: ResultSet,
    table: Option<String>,
    focus_handle: FocusHandle,
    scroll: GridScroll,
    bounds: Rc<RefCell<Bounds<Pixels>>>,
    metrics: Metrics,
    sort: SortState,
    order: RowOrder,
    // Sort and row count that `order` was built for, and the ones a background sort is working on.
    ordered_for: (SortState, usize),
    sorting: Option<((SortState, usize), Task<()>)>,
    hidden: BTreeSet<usize>,
    columns: Vec<usize>,
    widths: Vec<Width>,
    sampled: Option<usize>,
    user_sized: BTreeSet<usize>,
    col_x: Vec<Pixels>,
    selection: Selection,
    press: Option<Press>,
    hover: Option<Target>,
    published: (Option<usize>, Vec<usize>),
    pub(super) picker_open: bool,
    overlay: Option<TableOverlay>,
    editing: Option<Editing>,
    load_armed: bool,
    scrolled_to: Pixels,
}

impl EventEmitter<GridEvent> for Grid {}

impl Focusable for Grid {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Grid {
    pub fn new(columns: Arc<[ColumnMeta]>, cx: &mut Context<Self>) -> Self {
        let count = columns.len();
        Self {
            set: ResultSet::new(columns),
            table: None,
            focus_handle: cx.focus_handle(),
            scroll: GridScroll::default(),
            bounds: Rc::default(),
            metrics: Metrics::default(),
            sort: SortState::default(),
            order: RowOrder::identity(0),
            ordered_for: (SortState::default(), 0),
            sorting: None,
            hidden: BTreeSet::new(),
            columns: (0..count).collect(),
            widths: vec![Width::Ch(0); count],
            sampled: None,
            user_sized: BTreeSet::new(),
            col_x: Vec::new(),
            selection: Selection::default(),
            press: None,
            hover: None,
            published: (None, Vec::new()),
            picker_open: false,
            overlay: None,
            editing: None,
            load_armed: true,
            scrolled_to: px(0.),
        }
    }

    pub fn focused_row(&self) -> Option<RowRef> {
        let row = self.selection.focus_row.filter(|&row| row < self.set.rows())?;
        Some(RowRef { set: self.set.clone(), row, columns: self.columns.clone() })
    }

    fn publish(&mut self, cx: &mut Context<Self>) {
        if self.published.0 != self.selection.focus_row || self.published.1 != self.columns {
            self.published = (self.selection.focus_row, self.columns.clone());
            cx.emit(GridEvent::FocusedRowChanged);
        }
    }

    pub fn set(&self) -> &ResultSet {
        &self.set
    }

    pub fn push(&mut self, chunk: Arc<ResultChunk>, cx: &mut Context<Self>) {
        self.set.push(chunk);
        self.sync_order(cx);
        if self.selection.focus_row.is_none_or(|row| row >= self.set.rows())
            && let Some(first) = self.order.global_at(0)
        {
            self.selection.seed(first);
        }
        self.publish(cx);
        cx.notify();
    }

    fn sync_order(&mut self, cx: &mut Context<Self>) {
        let wanted = (self.sort, self.set.rows());
        if self.ordered_for == wanted || self.sorting.as_ref().is_some_and(|(target, _)| *target == wanted) {
            return;
        }
        let Some(column) = self.sort.column else {
            self.order = RowOrder::identity(wanted.1);
            self.ordered_for = wanted;
            self.sorting = None;
            return;
        };
        let dir = self.sort.dir();
        if wanted.1 <= SYNC_SORT_ROWS {
            self.order = RowOrder::sorted(sort_order(&self.set, column, dir));
            self.ordered_for = wanted;
            self.sorting = None;
            return;
        }
        let set = self.set.clone();
        let task = cx.spawn(async move |this, cx| {
            let order = cx.background_spawn(async move { sort_order(&set, column, dir) }).await;
            this.update(cx, |grid, cx| {
                grid.sorting = None;
                if (grid.sort, grid.set.rows()) == wanted {
                    grid.order = RowOrder::sorted(order);
                    grid.ordered_for = wanted;
                }
                cx.notify();
            })
            .ok();
        });
        self.sorting = Some((wanted, task));
    }

    pub fn toggle_sort(&mut self, column: usize, cx: &mut Context<Self>) {
        self.sort.toggle(column);
        self.sync_order(cx);
        cx.notify();
    }

    pub fn column_names(&self) -> impl Iterator<Item = &str> {
        self.set.columns.iter().map(|c| c.name.as_str())
    }

    pub fn is_hidden(&self, column: usize) -> bool {
        self.hidden.contains(&column)
    }

    pub fn hidden_count(&self) -> usize {
        self.hidden.len()
    }

    pub fn toggle_column(&mut self, column: usize, cx: &mut Context<Self>) {
        if !self.hidden.remove(&column) {
            self.hidden.insert(column);
        }
        self.update_columns(cx);
    }

    pub fn show_all_columns(&mut self, cx: &mut Context<Self>) {
        self.hidden.clear();
        self.update_columns(cx);
    }

    pub fn hide_all_columns(&mut self, cx: &mut Context<Self>) {
        self.hidden = (0..self.set.columns.len()).collect();
        self.update_columns(cx);
    }

    fn update_columns(&mut self, cx: &mut Context<Self>) {
        self.columns = (0..self.set.columns.len()).filter(|c| !self.hidden.contains(c)).collect();
        let last = self.columns.len() as isize - 1;
        if self.selection.focus_col > last {
            self.selection.focus_col = last.max(0);
        }
        self.publish(cx);
        cx.emit(GridEvent::HiddenChanged);
        cx.notify();
    }

    // Forgets dragged widths. The next layout re-fits every column.
    pub fn fit_columns(&mut self, cx: &mut Context<Self>) {
        self.user_sized.clear();
        self.sampled = None;
        cx.notify();
    }

    // Widths come from the first SAMPLE_ROWS display rows. More rows re-fit only the undragged columns.
    fn sync_widths(&mut self) {
        let want = self.set.rows().min(SAMPLE_ROWS);
        if self.sampled.is_some_and(|sampled| sampled >= want) {
            return;
        }
        let keep_dragged = self.sampled.is_some();
        for column in 0..self.set.columns.len() {
            if keep_dragged && self.user_sized.contains(&column) {
                continue;
            }
            let sample = (0..want).filter_map(|d| self.order.global_at(d)).map(|row| self.set.display(row, column));
            self.widths[column] = Width::Ch(auto_width_ch(&self.set.columns[column].name, sample));
        }
        self.sampled = Some(want);
    }

    fn width_px(&self, column: usize) -> Pixels {
        match self.widths[column] {
            Width::Ch(ch) => (self.metrics.ch * ch as f32).round(),
            Width::Px(width) => width,
        }
    }

    fn sync_layout(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.sync_order(cx);
        self.sync_widths();
        self.metrics = Metrics::new(&grid_font(cx), window);
        let mut x = px(0.);
        self.col_x.clear();
        self.col_x.push(x);
        for &column in &self.columns {
            x += self.width_px(column);
            self.col_x.push(x);
        }
    }

    fn view(&self) -> View<'_> {
        View { order: &self.order, columns: &self.columns }
    }

    // (rows, columns)
    pub fn selection_counts(&self) -> (usize, usize) {
        self.selection.counts(&self.view())
    }

    pub fn clear_selection(&mut self, cx: &mut Context<Self>) {
        self.selection.clear();
        cx.notify();
    }

    // Result row and column. None when the gutter has the focus.
    pub fn focused_cell(&self) -> Option<(usize, usize)> {
        let row = self.selection.focus_row?;
        let column = *self.columns.get(usize::try_from(self.selection.focus_col).ok()?)?;
        Some((row, column))
    }

    pub fn export_source(&self) -> ExportSource {
        let order = (0..self.order.len()).filter_map(|display| self.order.global_at(display)).collect();
        let mut selected_rows: Vec<usize> = self.selection.rows.iter().copied().collect();
        selected_rows.sort_by_key(|&row| self.order.display_of(row));
        ExportSource {
            set: self.set.clone(),
            staged: self.staged(),
            table: self.table.clone(),
            order,
            selected_rows,
            visible: self.columns.clone(),
            selected_columns: self.columns.iter().copied().filter(|c| self.selection.columns.contains(c)).collect(),
        }
    }

    pub fn copy_target(&self) -> CopyTarget {
        copy::resolve(&self.selection, &self.view())
    }

    // With `single_cell`, a lone focused cell copies just its value.
    pub fn copy(&mut self, single_cell: bool, window: &mut Window, cx: &mut Context<Self>) {
        let staged = self.staged();
        if single_cell && let Some((row, column)) = copy::single_cell(&self.selection, &self.view()) {
            let text = copy::cell_text(&self.set, staged.as_ref(), row, column);
            let target = CopyTarget { columns: vec![column], rows: vec![row] };
            cx.write_to_clipboard(copy::clipboard_item(text, None, &self.set, staged.as_ref(), &target));
            toast::success(t(cx, "toast.copiedCell"), cx);
            return;
        }
        let (set, target, table) = (self.set.clone(), self.copy_target(), self.table.clone());
        let format = copy_format(cx);
        let item = cx.background_spawn(async move {
            let text = copy::export(&set, staged.as_ref(), format, table.as_deref(), &target);
            copy::clipboard_item(text, Some(format), &set, staged.as_ref(), &target)
        });
        cx.spawn_in(window, async move |this, cx| {
            let item = item.await;
            this.update(cx, |_, cx| {
                cx.write_to_clipboard(item);
                toast::success(t(cx, "toast.copiedClipboard"), cx);
            })
            .ok();
        })
        .detach();
    }

    fn visible_rows(&self) -> usize {
        let height = self.scroll.viewport().size.height;
        ((height / self.metrics.row_height).floor() as usize).max(1)
    }

    fn reveal(&mut self, row: usize, col: isize) {
        let size = self.scroll.viewport().size;
        let mut position = self.scroll.position();
        let top = self.metrics.row_height * row as f32;
        let bottom = top + self.metrics.row_height;
        if top < position.y {
            position.y = top;
        } else if bottom > position.y + size.height {
            position.y = bottom - size.height;
        }
        if let Ok(col) = usize::try_from(col)
            && let (Some(&left), Some(&right)) = (self.col_x.get(col), self.col_x.get(col + 1))
        {
            if left < position.x {
                position.x = left;
            } else if right > position.x + size.width {
                position.x = (right - size.width).min(left);
            }
        }
        self.scroll.scroll_to(position);
    }

    fn arrow(&mut self, arrow: Arrow, extend: bool, cx: &mut Context<Self>) {
        if self.editing.is_some() {
            return;
        }
        let view = View { order: &self.order, columns: &self.columns };
        let Some((row, col)) = self.selection.arrow_target(arrow, &view) else { return };
        self.selection.move_focus(row, col, extend, &view);
        self.reveal(row, col);
        self.publish(cx);
        cx.notify();
    }

    fn jump(&mut self, rows: isize, cx: &mut Context<Self>) {
        if self.editing.is_some() {
            return;
        }
        let view = View { order: &self.order, columns: &self.columns };
        let Some(current) = self.selection.focus_row.and_then(|row| view.order.display_of(row)) else { return };
        let last = view.order.len().saturating_sub(1) as isize;
        let row = (current as isize + rows).clamp(0, last) as usize;
        let col = self.selection.focus_col;
        self.selection.move_focus(row, col, false, &view);
        self.reveal(row, col);
        self.publish(cx);
        cx.notify();
    }

    fn escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing.is_some() {
            self.cancel_edit(window, cx);
            return;
        }
        if self.selection.rows.is_empty() && self.selection.columns.is_empty() {
            cx.propagate();
            return;
        }
        self.clear_selection(cx);
    }

    fn select_all(&mut self, cx: &mut Context<Self>) {
        let view = View { order: &self.order, columns: &self.columns };
        self.selection.select_all(&view);
        cx.notify();
    }

    // Enter and double-click open the viewer on a lone focused cell.
    fn view_focused(&mut self, cx: &mut Context<Self>) {
        if let Some((row, column)) = copy::single_cell(&self.selection, &self.view()) {
            cx.emit(GridEvent::ViewCell { row, column });
        }
    }

    fn column_at(&self, x: Pixels) -> Option<usize> {
        if x < px(0.) {
            return None;
        }
        let ix = self.col_x.partition_point(|&left| left <= x);
        (ix >= 1 && ix < self.col_x.len()).then(|| ix - 1)
    }

    fn hit(&self, position: Point<Pixels>) -> Option<Target> {
        let bounds = *self.bounds.borrow();
        if !bounds.contains(&position) {
            return None;
        }
        let m = &self.metrics;
        let local = position - bounds.origin;
        let scroll = self.scroll.position();
        let in_header = local.y < m.header_height;
        let in_gutter = local.x < m.gutter_width;
        let col = (!in_gutter).then(|| self.column_at(local.x - m.gutter_width + scroll.x)).flatten();
        let row = (!in_header)
            .then(|| {
                let row = ((local.y - m.header_height + scroll.y) / m.row_height).floor() as usize;
                (row < self.order.len()).then_some(row)
            })
            .flatten();
        match (in_header, in_gutter) {
            (true, true) => Some(Target::Corner),
            (true, false) => {
                let col = col?;
                let x = local.x - m.gutter_width + scroll.x - self.col_x[col];
                let width = self.col_x[col + 1] - self.col_x[col];
                let chevron_left = width - m.cell_pad_x - m.chevron;
                let chevron_top = (m.header_height - m.chevron) / 2.;
                let zone = if x >= width - px(RESIZE_HANDLE_PX) {
                    Zone::Resize
                } else if x >= chevron_left
                    && x < chevron_left + m.chevron
                    && local.y >= chevron_top
                    && local.y < chevron_top + m.chevron
                {
                    Zone::Sort
                } else {
                    Zone::Title
                };
                Some(Target::Header { col, zone })
            }
            (false, true) => row.map(|row| Target::Gutter { row }),
            (false, false) => {
                let (row, col) = (row?, col?);
                let on_jump = self
                    .overlay
                    .as_ref()
                    .is_some_and(|overlay| overlay.foreign.contains_key(&self.columns[col]))
                    && self.order.global_at(row).is_some_and(|global| !self.shown(global, self.columns[col]).is_null())
                    && self.jump_box(row, col).contains(&position);
                Some(if on_jump { Target::Jump { row, col } } else { Target::Cell { row, col } })
            }
        }
    }

    // Foreign-key button at the cell's right edge, in window coordinates.
    fn jump_box(&self, row: usize, col: usize) -> Bounds<Pixels> {
        let bounds = *self.bounds.borrow();
        let m = &self.metrics;
        let scroll = self.scroll.position();
        let right = bounds.left() + m.gutter_width + self.col_x[col + 1] - scroll.x - px(1.) - m.jump_inset;
        let top = bounds.top() + m.header_height + m.row_height * row as f32 - scroll.y;
        Bounds::new(
            point(right - m.chevron, top + (m.row_height - px(1.) - m.chevron) / 2.),
            size(m.chevron, m.chevron),
        )
    }

    // Clamps to the nearest cell so a drag can leave the body.
    fn nearest_cell(&self, position: Point<Pixels>) -> Option<CellCoord> {
        let rows = self.order.len();
        if rows == 0 || self.columns.is_empty() {
            return None;
        }
        let bounds = *self.bounds.borrow();
        let m = &self.metrics;
        let scroll = self.scroll.position();
        let local = position - bounds.origin;
        let y = local.y.clamp(m.header_height, bounds.size.height - px(1.)) - m.header_height + scroll.y;
        let x = local.x.clamp(m.gutter_width, bounds.size.width - px(1.)) - m.gutter_width + scroll.x;
        let row = ((y / m.row_height).floor() as usize).min(rows - 1);
        let col = self.column_at(x).unwrap_or(self.columns.len() - 1);
        Some(CellCoord { row, col })
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        // GPUI's own click-to-focus runs after this and would take focus back from a cell editor.
        window.focus(&self.focus_handle, cx);
        window.prevent_default();
        let Some(target) = self.hit(event.position) else { return };
        let ctrl = event.modifiers.control || event.modifiers.platform;
        let shift = event.modifiers.shift;
        let view = View { order: &self.order, columns: &self.columns };
        let editable = self.overlay.as_ref().is_some_and(|overlay| overlay.editable);
        match target {
            Target::Corner => return,
            Target::Jump { row, col } => {
                if let Some(global) = view.order.global_at(row) {
                    cx.emit(GridEvent::OpenForeignKey { row: global, column: self.columns[col] });
                }
                return;
            }
            Target::Cell { row, col } => {
                self.selection.press_cell(row, col, shift, &view);
                self.press = Some(Press::Cells);
                if event.click_count >= 2 && !shift {
                    self.selection.release(Some(CellCoord { row, col }));
                    self.press = None;
                    match self.order.global_at(row) {
                        Some(global) if editable => {
                            self.selection.clear();
                            self.start_edit(global, self.columns[col], window, cx);
                        }
                        _ => self.view_focused(cx),
                    }
                }
            }
            Target::Gutter { row } => {
                if let Some(global) = view.order.global_at(row) {
                    self.selection.click_gutter(global, ctrl, shift, &view);
                    if event.click_count >= 2 && editable {
                        cx.emit(GridEvent::ToggleDelete(global));
                    }
                }
            }
            Target::Header { col, zone: Zone::Resize } => {
                let column = self.columns[col];
                self.press =
                    Some(Press::Resize { column, start_x: event.position.x, start_width: self.width_px(column) });
            }
            Target::Header { col, zone: Zone::Sort } => {
                let column = self.columns[col];
                if self.overlay.is_some() {
                    self.selection.clear();
                    cx.emit(GridEvent::SortRequested(column));
                } else {
                    self.toggle_sort(column, cx);
                }
            }
            Target::Header { col, zone: Zone::Title } => {
                self.selection.click_header(self.columns[col], ctrl, shift, &view);
            }
        }
        self.publish(cx);
        cx.notify();
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, hovered: bool, cx: &mut Context<Self>) {
        match self.press {
            Some(Press::Cells) if self.selection.dragging() => {
                if let Some(at) = self.nearest_cell(event.position) {
                    let view = View { order: &self.order, columns: &self.columns };
                    let before = self.selection.range;
                    self.selection.drag_to(at.row, at.col, &view);
                    if self.selection.range != before {
                        cx.notify();
                    }
                }
            }
            Some(Press::Resize { column, start_x, start_width }) => {
                let width = (start_width + (event.position.x - start_x)).max(px(MIN_COLUMN_PX)).round();
                if self.widths[column] != Width::Px(width) {
                    self.widths[column] = Width::Px(width);
                    self.user_sized.insert(column);
                    cx.notify();
                }
            }
            _ => {
                let hover = if hovered { self.hit(event.position) } else { None };
                if hover != self.hover {
                    self.hover = hover;
                    cx.notify();
                }
            }
        }
    }

    fn mouse_up(&mut self, event: &MouseUpEvent, cx: &mut Context<Self>) {
        match self.press.take() {
            Some(Press::Cells) => {
                let at = match self.hit(event.position) {
                    Some(Target::Cell { row, col }) => Some(CellCoord { row, col }),
                    _ => None,
                };
                self.selection.release(at);
                cx.notify();
            }
            Some(Press::Resize { .. }) => cx.notify(),
            None => {}
        }
    }

    // Each wheel event scrolls one axis only. Shift scrolls sideways.
    fn scroll_wheel(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let delta = event.delta.pixel_delta(self.metrics.row_height);
        let max = self.scroll.max();
        let (can_x, can_y) = (max.x > px(1.), max.y > px(1.));
        let (ax, ay) = (delta.x.abs(), delta.y.abs());
        let shift = event.modifiers.shift;
        let vertical = can_y && !shift && ay > ax && (ax == px(0.) || ay > ax * 1.5);
        let mut position = self.scroll.position();
        if vertical || !can_x {
            position.y -= delta.y;
        } else {
            let sideways = if shift {
                if delta.y != px(0.) { delta.y } else { delta.x }
            } else if ax > px(0.) {
                delta.x
            } else {
                delta.y
            };
            let before = position.x;
            position.x = (position.x - sideways).clamp(px(0.), max.x);
            if position.x == before {
                position.y -= delta.y;
            }
        }
        if self.scroll.scroll_to(position) {
            cx.notify();
        }
        cx.stop_propagation();
    }

    fn menu(grid: &WeakEntity<Self>, menu: PopupMenu, cx: &mut Context<PopupMenu>) -> PopupMenu {
        let Some(this) = grid.upgrade() else { return menu };
        if this.read(cx).overlay.is_some() {
            return Self::table_menu(this, menu, cx);
        }
        let (has_selection, can_view) = {
            let grid = this.read(cx);
            (!grid.selection.rows.is_empty() || !grid.selection.columns.is_empty(), grid.focused_cell().is_some())
        };
        let target = this.clone();
        let menu = menu
            .item(PopupMenuItem::new(t(cx, "common.copy")).on_click({
                let target = target.clone();
                move |_, window, cx| target.update(cx, |grid, cx| grid.copy(true, window, cx))
            }))
            .item(PopupMenuItem::new(t(cx, "results.contextExportAs")).on_click({
                let target = target.clone();
                move |_, _, cx| target.update(cx, |_, cx| cx.emit(GridEvent::Export))
            }))
            .separator()
            .item(PopupMenuItem::new(t(cx, "results.contextClearSelection")).disabled(!has_selection).on_click({
                let target = target.clone();
                move |_, _, cx| target.update(cx, |grid, cx| grid.clear_selection(cx))
            }));
        if !can_view {
            return menu;
        }
        menu.item(PopupMenuItem::new(t(cx, "results.contextViewCell")).on_click(move |_, _, cx| {
            target.update(cx, |grid, cx| {
                if let Some((row, column)) = grid.focused_cell() {
                    cx.emit(GridEvent::ViewCell { row, column });
                }
            })
        }))
    }
}

fn grid_font(cx: &App) -> Font {
    font(cx.theme().mono_font_family.clone())
}

impl Render for Grid {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_layout(window, cx);
        self.check_load_more(cx);
        let entity = cx.entity();
        let weak = entity.downgrade();
        let m = self.metrics;
        // Editor fills the cell inside its borders, with a 2px ring outside.
        let editor = self.editor_bounds().map(|(input, bounds)| {
            let theme = cx.theme();
            let ring = px(2.);
            div()
                .absolute()
                .left(bounds.left() - ring)
                .top(bounds.top() - ring)
                .w(bounds.size.width - px(1.) + ring * 2.)
                .h(bounds.size.height - px(1.) + ring * 2.)
                .border_2()
                .border_color(theme.primary)
                .bg(theme.background)
                .child(
                    Input::new(&input)
                        .context_menu(crate::context_menu::input(&input))
                        .appearance(false)
                        .size_full()
                        .px(m.cell_pad_x)
                        .py_0()
                        .text_size(m.font_size)
                        .font_family(theme.mono_font_family.clone()),
                )
        });
        div()
            .id("grid")
            .key_context(CONTEXT)
            .track_focus(&self.focus_handle)
            .size_full()
            .overflow_hidden()
            .on_action(cx.listener(|this, _: &MoveUp, _, cx| this.arrow(Arrow::Up, false, cx)))
            .on_action(cx.listener(|this, _: &MoveDown, _, cx| this.arrow(Arrow::Down, false, cx)))
            .on_action(cx.listener(|this, _: &MoveLeft, _, cx| this.arrow(Arrow::Left, false, cx)))
            .on_action(cx.listener(|this, _: &MoveRight, _, cx| this.arrow(Arrow::Right, false, cx)))
            .on_action(cx.listener(|this, _: &SelectUp, _, cx| this.arrow(Arrow::Up, true, cx)))
            .on_action(cx.listener(|this, _: &SelectDown, _, cx| this.arrow(Arrow::Down, true, cx)))
            .on_action(cx.listener(|this, _: &SelectLeft, _, cx| this.arrow(Arrow::Left, true, cx)))
            .on_action(cx.listener(|this, _: &SelectRight, _, cx| this.arrow(Arrow::Right, true, cx)))
            .on_action(cx.listener(|this, _: &PageUp, _, cx| this.jump(-(this.visible_rows() as isize), cx)))
            .on_action(cx.listener(|this, _: &PageDown, _, cx| this.jump(this.visible_rows() as isize, cx)))
            .on_action(cx.listener(|this, _: &MoveToTop, _, cx| this.jump(isize::MIN / 2, cx)))
            .on_action(cx.listener(|this, _: &MoveToBottom, _, cx| this.jump(isize::MAX / 2, cx)))
            .on_action(cx.listener(|this, _: &ClearSelection, window, cx| this.escape(window, cx)))
            .on_action(cx.listener(|this, _: &ViewCell, window, cx| this.enter(window, cx)))
            .on_action(cx.listener(|this, _: &ViewCellValue, _, cx| this.view_value(cx)))
            .on_action(cx.listener(|this, _: &ToggleDeleteRow, _, cx| this.delete_focused_row(cx)))
            .on_action(cx.listener(|this, _: &SetNull, _, cx| this.null_focused_cell(cx)))
            .on_action(cx.listener(|this, _: &Paste, _, cx| this.paste(cx)))
            .on_action(cx.listener(|this, _: &Undo, _, cx| this.table_event(GridEvent::Undo, cx)))
            .on_action(cx.listener(|this, _: &Redo, _, cx| this.table_event(GridEvent::Redo, cx)))
            .on_action(cx.listener(|this, _: &ApplyChanges, _, cx| this.table_event(GridEvent::Apply, cx)))
            .on_action(cx.listener(|this, _: &RefreshData, _, cx| this.table_event(GridEvent::Refresh, cx)))
            .on_action(cx.listener(|_, _: &ExportResults, _, cx| cx.emit(GridEvent::Export)))
            .on_action(cx.listener(|this, _: &Copy, window, cx| this.copy(true, window, cx)))
            .on_action(cx.listener(|this, _: &SelectAll, _, cx| this.select_all(cx)))
            .on_scroll_wheel(cx.listener(Self::scroll_wheel))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::right_down))
            .child(
                canvas(
                    {
                        let entity = entity.clone();
                        move |bounds, window, cx| prepaint(&entity, bounds, window, cx)
                    },
                    move |bounds, frame, window, cx| paint(&entity, bounds, frame, window, cx),
                )
                .size_full(),
            )
            .child(
                div()
                    .absolute()
                    .top(m.header_height)
                    .right_0()
                    .bottom_0()
                    .w(Scrollbar::width())
                    .child(Scrollbar::vertical(&self.scroll).viewport_from_layout()),
            )
            .child(
                div()
                    .absolute()
                    .left(m.gutter_width)
                    .right_0()
                    .bottom_0()
                    .h(Scrollbar::width())
                    .child(Scrollbar::horizontal(&self.scroll).viewport_from_layout()),
            )
            .children(editor)
            .context_menu(move |menu, _, cx| Grid::menu(&weak, menu, cx))
    }
}

struct Colors {
    background: Hsla,
    head: Hsla,
    hover: Hsla,
    border: Hsla,
    text: Hsla,
    muted: Hsla,
    accent: Hsla,
    focused_row: Hsla,
    focused_cell: Hsla,
    selected: Hsla,
    selected_hover: Hsla,
    head_selected: Hsla,
    handle: Hsla,
    staged: Hsla,
    staged_border: Hsla,
    deleted: Hsla,
}

// Mixes `amount` of `a` into `b`, in sRGB.
fn mix(a: Hsla, b: Hsla, amount: f32) -> Hsla {
    let (a, b) = (a.to_rgb(), b.to_rgb());
    let lerp = |x: f32, y: f32| x * amount + y * (1. - amount);
    Rgba { r: lerp(a.r, b.r), g: lerp(a.g, b.g), b: lerp(a.b, b.b), a: lerp(a.a, b.a) }.into()
}

impl Colors {
    fn new(cx: &App) -> Self {
        let theme = cx.theme();
        let accent = theme.primary;
        // Cells sit on the results pane, so tints mix against the base background.
        let background = theme.background;
        Self {
            background: theme.table,
            head: theme.table_head,
            hover: theme.table_hover,
            border: theme.border,
            text: theme.foreground,
            muted: theme.muted_foreground,
            accent,
            focused_row: mix(accent, background, 0.06),
            focused_cell: mix(accent, background, 0.14),
            selected: mix(accent, background, 0.22),
            selected_hover: mix(accent, background, 0.28),
            head_selected: mix(accent, theme.table_head, 0.22),
            handle: accent.opacity(0.5),
            staged: mix(theme.warning, background, 0.22),
            staged_border: theme.warning.opacity(0.55),
            deleted: theme.danger.opacity(0.22),
        }
    }
}

struct Layer {
    mask: Bounds<Pixels>,
    quads: Vec<PaintQuad>,
    texts: Vec<(ShapedLine, Point<Pixels>)>,
    icons: Vec<(Bounds<Pixels>, SharedString, Hsla)>,
}

impl Layer {
    fn new(mask: Bounds<Pixels>) -> Self {
        Self { mask, quads: Vec::new(), texts: Vec::new(), icons: Vec::new() }
    }
}

pub struct Frame {
    layers: Vec<Layer>,
    hitbox: Hitbox,
    pointer: Vec<Hitbox>,
    resize: Vec<Hitbox>,
    line_height: Pixels,
    resizing: bool,
}

// Runs of whitespace collapse into one space and are dropped at the ends.
fn display_text(text: &str) -> Cow<'_, str> {
    let text = match text.char_indices().nth(MAX_CELL_CHARS) {
        Some((ix, _)) => &text[..ix],
        None => text,
    };
    let collapsible = |c: char| matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0c');
    let clean = !text.contains(['\t', '\n', '\r', '\x0c'])
        && !text.contains("  ")
        && !text.starts_with(' ')
        && !text.ends_with(' ');
    if clean {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    for word in text.split(collapsible).filter(|w| !w.is_empty()) {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    Cow::Owned(out)
}

struct Shaper {
    font: Font,
    size: Pixels,
    wrapper: LineWrapperHandle,
}

impl Shaper {
    fn new(font: Font, size: Pixels, window: &Window) -> Self {
        let wrapper = window.text_system().line_wrapper(font.clone(), size);
        Self { font, size, wrapper }
    }

    // Cut with an ellipsis to fit `width`.
    fn shape(&mut self, text: &str, color: Hsla, width: Pixels, window: &Window) -> ShapedLine {
        let text: SharedString = match self.wrapper.should_truncate_line(text, width, "…", TruncateFrom::End) {
            Some(ix) => format!("{}…", &text[..ix]).into(),
            None => text.to_string().into(),
        };
        let run = TextRun {
            len: text.len(),
            font: self.font.clone(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        window.text_system().shape_line(text, self.size, &[run], None)
    }
}

fn prepaint(entity: &Entity<Grid>, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) -> Frame {
    let grid = entity.read(cx);
    let m = grid.metrics;
    *grid.bounds.borrow_mut() = bounds;
    let body = Bounds::from_corners(
        point(bounds.left() + m.gutter_width, bounds.top() + m.header_height),
        bounds.bottom_right(),
    );
    let total_width = grid.col_x.last().copied().unwrap_or_default();
    let rows = grid.order.len();
    grid.scroll.set_layout(body, size(total_width, m.row_height * rows as f32));
    let scroll = grid.scroll.position();
    let colors = Colors::new(cx);
    let base = grid_font(cx);
    let mut plain = Shaper::new(base.clone(), m.font_size, window);
    let mut italic = Shaper::new(Font { style: FontStyle::Italic, ..base.clone() }, m.font_size, window);
    let mut bold = Shaper::new(Font { weight: FontWeight::SEMIBOLD, ..base }, m.font_size, window);

    let first_row = ((scroll.y / m.row_height).floor() as usize).min(rows);
    let last_row = (((scroll.y + body.size.height) / m.row_height).ceil() as usize).min(rows);
    let first_col = grid.column_at(scroll.x).unwrap_or(grid.columns.len());
    let end_col = grid.col_x.partition_point(|&left| left < scroll.x + body.size.width).min(grid.columns.len());
    let visible_cols = first_col..end_col.max(first_col);
    let row_top = |row: usize| body.top() + m.row_height * row as f32 - scroll.y;
    let col_left = |col: usize| body.left() + grid.col_x[col] - scroll.x;
    let columns_right = body.left() + total_width - scroll.x;
    let rows_bottom = row_top(rows);

    let view = grid.view();
    let overlay = grid.overlay.as_ref();
    let selection = &grid.selection;
    let has_selection = !selection.is_empty();
    let (band_rows, band_cols) = selection.bands(&view);
    let focused = grid.focus_handle.is_focused(window);
    let focus_display = selection.focus_row.and_then(|row| grid.order.display_of(row));
    let hover_row = match grid.hover {
        Some(Target::Cell { row, .. } | Target::Gutter { row }) => Some(row),
        _ => None,
    };
    let null = t(cx, "common.null");

    let mut background = Layer::new(bounds);
    background.quads.push(fill(bounds, colors.background));

    let mut cells = Layer::new(body);
    for row in first_row..last_row {
        let Some(global) = grid.order.global_at(row) else { continue };
        let top = row_top(row);
        let focused_row = focus_display == Some(row) && !has_selection;
        let row_fill = if focused_row {
            Some(colors.focused_row)
        } else if hover_row == Some(row) {
            Some(colors.hover)
        } else {
            None
        };
        let row_band = Bounds::from_corners(point(body.left(), top), point(columns_right, top + m.row_height));
        if let Some(color) = row_fill {
            cells.quads.push(fill(row_band, color));
        }
        let deleted = grid.is_deleted(global);
        if deleted {
            cells.quads.push(fill(row_band, colors.deleted));
        }
        for col in visible_cols.clone() {
            let column = grid.columns[col];
            let left = col_left(col);
            let width = grid.col_x[col + 1] - grid.col_x[col];
            let cell = Bounds::new(point(left, top), size(width, m.row_height));
            let edges =
                range::highlight(row, col, selection.range.as_ref(), &band_rows, &band_cols, rows, grid.columns.len());
            let focus_here = focus_display == Some(row) && selection.focus_col == col as isize;
            let staged = overlay.is_some_and(|overlay| overlay.staged.contains_key(&(global, column)));
            if edges.is_some() {
                let color = if hover_row == Some(row) { colors.selected_hover } else { colors.selected };
                cells.quads.push(fill(cell, color));
            } else if staged {
                cells.quads.push(fill(cell, colors.staged));
            } else if focus_here && !has_selection {
                cells.quads.push(fill(cell, colors.focused_cell));
            }
            let value = grid.shown(global, column);
            let jump = overlay.is_some_and(|overlay| overlay.foreign.contains_key(&column))
                && !value.is_null()
                && matches!(grid.hover, Some(Target::Cell { row: r, col: c } | Target::Jump { row: r, col: c }) if r == row && c == col);
            let text_width = width - m.cell_pad_x * 2. - if jump { m.chevron } else { px(0.) };
            let origin = point(left + m.cell_pad_x, top + m.cell_pad_y);
            let line = match value.display() {
                Some(text) => plain.shape(&display_text(text), colors.text, text_width, window),
                None => italic.shape(&null, colors.muted, text_width, window),
            };
            cells.texts.push((line, origin));
            let inner = Bounds::new(cell.origin, size(width - px(1.), m.row_height - px(1.)));
            if staged && edges.is_none() {
                cells.quads.push(quad(
                    inner,
                    px(0.),
                    transparent_black(),
                    px(1.),
                    colors.staged_border,
                    BorderStyle::Solid,
                ));
            }
            if jump {
                let button = grid.jump_box(row, col);
                let hot = matches!(grid.hover, Some(Target::Jump { .. }));
                let (border, background, color) = if hot {
                    (colors.accent, colors.hover, colors.accent)
                } else {
                    (colors.border, colors.head, colors.muted)
                };
                cells.quads.push(quad(button, m.radius, background, px(1.), border, BorderStyle::Solid));
                let icon = Bounds::new(button.center() - point(m.icon / 2., m.icon / 2.), size(m.icon, m.icon));
                cells.icons.push((icon, Lucide::ExternalLink.path(), color));
            }
            if let Some(edges) = edges {
                let side = |on: bool| if on { px(2.) } else { px(0.) };
                let widths = Edges {
                    top: side(edges.top),
                    right: side(edges.right),
                    bottom: side(edges.bottom),
                    left: side(edges.left),
                };
                cells.quads.push(quad(inner, px(0.), transparent_black(), widths, colors.accent, BorderStyle::Solid));
            }
            if focus_here && focused {
                cells.quads.push(quad(cell, px(0.), transparent_black(), px(2.), colors.accent, BorderStyle::Solid));
            } else if focus_here && !has_selection && !staged {
                cells.quads.push(quad(inner, px(0.), transparent_black(), px(1.), colors.accent, BorderStyle::Solid));
            }
        }
        cells.quads.push(fill(
            Bounds::from_corners(
                point(body.left(), top + m.row_height - px(1.)),
                point(columns_right, top + m.row_height),
            ),
            colors.border,
        ));
    }
    for col in visible_cols.clone() {
        let right = col_left(col + 1);
        cells.quads.push(fill(
            Bounds::from_corners(point(right - px(1.), body.top()), point(right, rows_bottom.min(body.bottom()))),
            colors.border,
        ));
    }

    let gutter_mask = Bounds::from_corners(
        point(bounds.left(), bounds.top() + m.header_height),
        point(bounds.left() + m.gutter_width, bounds.bottom()),
    );
    let mut gutter = Layer::new(gutter_mask);
    for row in first_row..last_row {
        let top = row_top(row);
        let cell = Bounds::new(point(bounds.left(), top), size(m.gutter_width, m.row_height));
        let focused_row = focus_display == Some(row) && !has_selection;
        let focus_here = focused_row && selection.focus_col == GUTTER;
        let hovered_cell = grid.hover == Some(Target::Gutter { row });
        let color = if focus_here {
            colors.focused_cell
        } else if focused_row {
            colors.focused_row
        } else if hover_row == Some(row) {
            colors.hover
        } else {
            colors.head
        };
        gutter.quads.push(fill(cell, color));
        if grid.order.global_at(row).is_some_and(|global| grid.is_deleted(global)) {
            gutter.quads.push(fill(cell, colors.deleted));
        }
        let text_color = if hovered_cell { colors.text } else { colors.muted };
        let line = plain.shape(&(row + 1).to_string(), text_color, m.gutter_width - m.gutter_pad_x * 2., window);
        let x = cell.right() - px(1.) - m.gutter_pad_x - line.width;
        gutter.texts.push((line, point(x, top + m.cell_pad_y)));
        gutter.quads.push(fill(
            Bounds::from_corners(point(cell.left(), cell.bottom() - px(1.)), cell.bottom_right()),
            colors.border,
        ));
        let inner = Bounds::new(cell.origin, size(m.gutter_width - px(1.), m.row_height - px(1.)));
        if focus_display == Some(row) && selection.focus_col == GUTTER && focused {
            gutter.quads.push(quad(cell, px(0.), transparent_black(), px(2.), colors.accent, BorderStyle::Solid));
        } else if focus_here {
            gutter.quads.push(quad(inner, px(0.), transparent_black(), px(1.), colors.accent, BorderStyle::Solid));
        }
    }
    let gutter_bottom = rows_bottom.min(bounds.bottom());
    gutter.quads.push(fill(
        Bounds::from_corners(
            point(bounds.left() + m.gutter_width - px(1.), bounds.top() + m.header_height),
            point(bounds.left() + m.gutter_width, gutter_bottom),
        ),
        colors.border,
    ));

    let header_mask = Bounds::from_corners(
        point(bounds.left() + m.gutter_width, bounds.top()),
        point(bounds.right(), bounds.top() + m.header_height),
    );
    let mut header = Layer::new(header_mask);
    let mut resize = Vec::new();
    for col in visible_cols {
        let column = grid.columns[col];
        let left = col_left(col);
        let width = grid.col_x[col + 1] - grid.col_x[col];
        let cell = Bounds::new(point(left, bounds.top()), size(width, m.header_height));
        let selected = selection.columns.contains(&column);
        let hover = match grid.hover {
            Some(Target::Header { col: hovered, zone }) if hovered == col => Some(zone),
            _ => None,
        };
        let color = if selected {
            colors.head_selected
        } else if hover.is_some() {
            colors.hover
        } else {
            colors.head
        };
        header.quads.push(fill(cell, color));
        let chevron = Bounds::new(
            point(cell.right() - m.cell_pad_x - m.chevron, cell.top() + (m.header_height - m.chevron) / 2.),
            size(m.chevron, m.chevron),
        );
        let title_width = chevron.left() - m.gap - (left + m.cell_pad_x);
        let title_color = if selected { colors.accent } else { colors.text };
        let line = bold.shape(&display_text(&grid.set.columns[column].name), title_color, title_width, window);
        let title_top = cell.top() + (m.header_height - px(1.) - m.line_height) / 2.;
        header.texts.push((line, point(left + m.cell_pad_x, title_top)));
        let (sort_column, desc) = match overlay {
            Some(overlay) => (overlay.sort.map(|(c, _)| c), overlay.sort.is_some_and(|(_, desc)| desc)),
            None => (grid.sort.column, grid.sort.desc),
        };
        let sorted = sort_column == Some(column);
        if hover == Some(Zone::Sort) {
            header.quads.push(quad(chevron, m.radius, colors.hover, px(0.), transparent_black(), BorderStyle::Solid));
        }
        let icon = match (sorted, desc) {
            (true, false) => Lucide::ChevronUp,
            (true, true) => Lucide::ChevronDown,
            (false, _) => Lucide::ChevronsUpDown,
        };
        let icon_color = if sorted {
            colors.accent
        } else if hover == Some(Zone::Sort) {
            colors.text
        } else {
            colors.muted
        };
        let icon_bounds = Bounds::new(chevron.center() - point(m.icon / 2., m.icon / 2.), size(m.icon, m.icon));
        header.icons.push((icon_bounds, icon.path(), icon_color));
        if selected {
            header.quads.push(fill(
                Bounds::from_corners(
                    point(cell.left(), cell.bottom() - px(1.) - px(2.)),
                    point(cell.right(), cell.bottom() - px(1.)),
                ),
                colors.accent,
            ));
        }
        header.quads.push(fill(
            Bounds::from_corners(point(cell.right() - px(1.), cell.top()), cell.bottom_right()),
            colors.border,
        ));
        let handle = Bounds::from_corners(point(cell.right() - px(RESIZE_HANDLE_PX), cell.top()), cell.bottom_right());
        let resizing = matches!(grid.press, Some(Press::Resize { column: c, .. }) if c == column);
        if hover == Some(Zone::Resize) || resizing {
            header.quads.push(fill(handle, colors.handle));
        }
        resize.push(handle.intersect(&header_mask));
    }
    header.quads.push(fill(
        Bounds::from_corners(
            point(header_mask.left(), header_mask.bottom() - px(1.)),
            point(columns_right.min(bounds.right()), header_mask.bottom()),
        ),
        colors.border,
    ));

    let corner_bounds = Bounds::new(bounds.origin, size(m.gutter_width, m.header_height));
    let mut corner = Layer::new(corner_bounds);
    corner.quads.push(fill(corner_bounds, colors.head));
    corner.quads.push(quad(
        corner_bounds,
        px(0.),
        transparent_black(),
        Edges { top: px(0.), right: px(1.), bottom: px(1.), left: px(0.) },
        colors.border,
        BorderStyle::Solid,
    ));

    let resizing = matches!(grid.press, Some(Press::Resize { .. }));
    let line_height = m.line_height;
    let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
    let pointer = [
        header_mask,
        gutter_mask.intersect(&Bounds::from_corners(gutter_mask.origin, point(gutter_mask.right(), gutter_bottom))),
    ]
    .into_iter()
    .map(|area| window.insert_hitbox(area, HitboxBehavior::Normal))
    .collect();
    let resize = resize.into_iter().map(|area| window.insert_hitbox(area, HitboxBehavior::Normal)).collect();
    Frame { layers: vec![background, cells, gutter, header, corner], hitbox, pointer, resize, line_height, resizing }
}

fn paint(entity: &Entity<Grid>, _: Bounds<Pixels>, frame: Frame, window: &mut Window, cx: &mut App) {
    let Frame { layers, hitbox, pointer, resize, line_height, resizing } = frame;
    for layer in layers {
        window.with_content_mask(Some(ContentMask { bounds: layer.mask }), |window| {
            for quad in layer.quads {
                window.paint_quad(quad);
            }
            for (line, origin) in layer.texts {
                line.paint(origin, line_height, TextAlign::Left, None, window, cx).ok();
            }
            for (bounds, path, color) in layer.icons {
                window.paint_svg(bounds, path, None, TransformationMatrix::unit(), color, cx).ok();
            }
        });
    }
    for area in &pointer {
        window.set_cursor_style(CursorStyle::PointingHand, area);
    }
    for area in &resize {
        window.set_cursor_style(CursorStyle::ResizeLeftRight, area);
    }
    if resizing {
        window.set_window_cursor_style(CursorStyle::ResizeLeftRight);
    }
    window.on_mouse_event({
        let entity = entity.clone();
        move |event: &MouseMoveEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble {
                let hovered = hitbox.is_hovered(window);
                entity.update(cx, |grid, cx| grid.mouse_move(event, hovered, cx));
            }
        }
    });
    window.on_mouse_event({
        let entity = entity.clone();
        move |event: &MouseUpEvent, phase, _, cx| {
            if phase == DispatchPhase::Bubble && event.button == MouseButton::Left {
                entity.update(cx, |grid, cx| grid.mouse_up(event, cx));
            }
        }
    });
}

mod table;

#[cfg(any(test, feature = "snapshot"))]
impl Grid {
    pub(crate) fn rows_shown(&self) -> usize {
        self.order.len()
    }

    pub(crate) fn cell_point(&self, row: usize, col: usize) -> Point<Pixels> {
        let (bounds, m, scroll) = (*self.bounds.borrow(), &self.metrics, self.scroll.position());
        point(
            bounds.left() + m.gutter_width + (self.col_x[col] + self.col_x[col + 1]) / 2. - scroll.x,
            bounds.top() + m.header_height + m.row_height * (row as f32 + 0.5) - scroll.y,
        )
    }
}

// For README screenshots. row and col are display positions, and the scroll stops at the edges.
#[cfg(feature = "snapshot")]
impl Grid {
    pub(crate) fn scroll_to_cell(&mut self, row: usize, col: usize, cx: &mut Context<Self>) {
        let x = self.col_x.get(col).copied().unwrap_or_default();
        self.scroll.scroll_to(point(x, self.metrics.row_height * row as f32));
        cx.notify();
    }
}

#[cfg(test)]
impl Grid {
    pub(crate) fn text_at(&self, row: usize, col: usize) -> Option<String> {
        let global = self.order.global_at(row)?;
        self.shown(global, *self.columns.get(col)?).display().map(str::to_string)
    }

    pub(crate) fn visible_names(&self) -> Vec<String> {
        self.columns.iter().map(|&c| self.set.columns[c].name.clone()).collect()
    }

    pub(crate) fn sorted_by(&self) -> Option<(String, bool)> {
        self.sort.column.map(|c| (self.set.columns[c].name.clone(), self.sort.desc))
    }

    pub(crate) fn gutter_point(&self, row: usize) -> Point<Pixels> {
        let (bounds, m, scroll) = (*self.bounds.borrow(), &self.metrics, self.scroll.position());
        point(
            bounds.left() + m.gutter_width / 2.,
            bounds.top() + m.header_height + m.row_height * (row as f32 + 0.5) - scroll.y,
        )
    }

    pub(crate) fn chevron_point(&self, col: usize) -> Point<Pixels> {
        let (bounds, m, scroll) = (*self.bounds.borrow(), &self.metrics, self.scroll.position());
        point(
            bounds.left() + m.gutter_width + self.col_x[col + 1] - m.cell_pad_x - m.chevron / 2. - scroll.x,
            bounds.top() + m.header_height / 2.,
        )
    }

    pub(crate) fn jump_point(&self, row: usize, col: usize) -> Point<Pixels> {
        self.jump_box(row, col).center()
    }

    // Only a non-NULL foreign-key cell has a jump button.
    pub(crate) fn jumps_at(&self, row: usize, col: usize) -> bool {
        matches!(self.hit(self.jump_point(row, col)), Some(Target::Jump { .. }))
    }

    pub(crate) fn focused_display(&self) -> Option<(usize, usize)> {
        let row = self.order.display_of(self.selection.focus_row?)?;
        Some((row, usize::try_from(self.selection.focus_col).ok()?))
    }
}

#[cfg(test)]
mod tests;
