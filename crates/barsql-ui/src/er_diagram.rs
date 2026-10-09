use std::collections::HashMap;
use std::rc::Rc;

use barsql_core::ColumnInfo;
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::scroll::ScrollbarAxis;
use gpui_kit::component::{ActiveTheme, Icon, StyledExt, WindowExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::i18n::{t, t_with};
use crate::modal::{self, Modal};
use crate::schema;
use crate::scrollbars::HoverScrollbar as _;
use crate::tokens::{ICON_XS, RADIUS, TEXT_SM, TEXT_XS};

// Sizes in rems of the 13px root.
const BOX_W: f32 = 16.923;
const HEADER_H: f32 = 2.154;
const ROW_H: f32 = 1.692;
const GAP_X: f32 = 6.154;
const GAP_Y: f32 = 1.846;
const MARGIN: f32 = 1.538;
// A column of boxes wraps past this height.
const MAX_STACK: f32 = 61.538;
// Rows a box shows before a "more" row.
const MAX_ROWS: usize = 14;
// Tables a diagram loads.
pub const MAX_TABLES: usize = 80;

pub struct ErTable {
    pub name: String,
    pub columns: Vec<ColumnInfo>,
}

impl ErTable {
    fn shown(&self) -> usize {
        self.columns.len().min(MAX_ROWS)
    }

    fn hidden(&self) -> usize {
        self.columns.len() - self.shown()
    }

    fn height(&self) -> f32 {
        HEADER_H + ROW_H * (self.shown() + usize::from(self.hidden() > 0)) as f32
    }

    // Where a column's row is, or None when it's past the shown rows.
    fn row(&self, column: &str) -> Option<usize> {
        let lower = column.to_lowercase();
        self.columns.iter().take(MAX_ROWS).position(|c| c.name.to_lowercase() == lower)
    }
}

// A foreign key from a column's row to the referenced table, at its column's row when shown. None is the header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edge {
    pub from: (usize, Option<usize>),
    pub to: (usize, Option<usize>),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placed {
    pub x: f32,
    pub y: f32,
    pub h: f32,
}

pub struct Layout {
    pub boxes: Vec<Placed>,
    pub edges: Vec<Edge>,
    pub width: f32,
    pub height: f32,
}

// SQLite leaves the column empty when the key references the primary key.
fn edges(tables: &[ErTable]) -> Vec<Edge> {
    let by_name: HashMap<String, usize> =
        tables.iter().enumerate().map(|(ix, t)| (t.name.to_lowercase(), ix)).collect();
    let mut out = Vec::new();
    for (from, table) in tables.iter().enumerate() {
        for column in table.columns.iter().filter(|c| c.is_foreign && !c.foreign_table.is_empty()) {
            let Some(&to) = by_name.get(&column.foreign_table.to_lowercase()) else { continue };
            let target = &tables[to];
            let to_row = match column.foreign_column.is_empty() {
                true => target.columns.iter().take(MAX_ROWS).position(|c| c.is_primary),
                false => target.row(&column.foreign_column),
            };
            out.push(Edge { from: (from, table.row(&column.name)), to: (to, to_row) });
        }
    }
    out
}

// Referenced tables stand left of the ones that reference them: a table's column is one past its deepest
// reference. Tables with no keys either way come last. A column taller than MAX_STACK wraps.
pub fn layout(tables: &[ErTable]) -> Layout {
    let edges = edges(tables);
    let n = tables.len();
    let mut refs: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut linked = vec![false; n];
    for edge in edges.iter().filter(|e| e.from.0 != e.to.0) {
        refs[edge.from.0].push(edge.to.0);
        linked[edge.from.0] = true;
        linked[edge.to.0] = true;
    }
    let mut depth: Vec<Option<usize>> = vec![None; n];
    fn visit(ix: usize, refs: &[Vec<usize>], depth: &mut [Option<usize>], path: &mut Vec<usize>) -> usize {
        if let Some(d) = depth[ix] {
            return d;
        }
        // A cycle stops at the table it came back to.
        if path.contains(&ix) {
            return 0;
        }
        path.push(ix);
        let d = refs[ix].iter().map(|&to| visit(to, refs, depth, path) + 1).max().unwrap_or(0);
        path.pop();
        depth[ix] = Some(d);
        d
    }
    let deepest = (0..n).filter(|&ix| linked[ix]).map(|ix| visit(ix, &refs, &mut depth, &mut Vec::new())).max();
    let isolated = deepest.map_or(0, |d| d + 1);
    let mut groups: Vec<Vec<usize>> = vec![Vec::new(); isolated + 1];
    for ix in 0..n {
        let group = if linked[ix] { depth[ix].unwrap_or(0) } else { isolated };
        groups[group].push(ix);
    }
    for group in &mut groups {
        group.sort_by_key(|&ix| tables[ix].name.to_lowercase());
    }
    let mut boxes = vec![Placed { x: 0., y: 0., h: 0. }; n];
    let (mut x, mut width, mut height) = (MARGIN, 0f32, 0f32);
    for group in groups.iter().filter(|g| !g.is_empty()) {
        let mut y = MARGIN;
        for &ix in group {
            let h = tables[ix].height();
            if y > MARGIN && y + h > MAX_STACK {
                x += BOX_W + GAP_X;
                y = MARGIN;
            }
            boxes[ix] = Placed { x, y, h };
            y += h + GAP_Y;
            height = height.max(y - GAP_Y + MARGIN);
        }
        width = x + BOX_W + MARGIN;
        x += BOX_W + GAP_X;
    }
    Layout { boxes, edges, width, height }
}

enum State {
    Loading,
    Ready(Rc<Vec<ErTable>>, Rc<Layout>),
    Empty,
    Failed(String),
}

type OnOpen = Rc<dyn Fn(String, &mut Window, &mut App)>;

pub struct ErDiagram {
    connection_id: String,
    schema: String,
    state: State,
    total: usize,
    scroll: ScrollHandle,
    on_open: OnOpen,
    _load: Task<()>,
}

impl ErDiagram {
    fn new(connection_id: String, schema: String, on_open: OnOpen, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            connection_id,
            schema,
            state: State::Loading,
            total: 0,
            scroll: ScrollHandle::new(),
            on_open,
            _load: Task::ready(()),
        };
        this.load(cx);
        this
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let load = schema::load_schema_columns(&self.connection_id, &self.schema, MAX_TABLES, cx);
        self._load = cx.spawn(async move |this, cx| {
            let loaded = load.await;
            let _ = this.update(cx, |this, cx| {
                this.state = match loaded {
                    Err(error) => State::Failed(error),
                    Ok((_, tables)) if tables.is_empty() => State::Empty,
                    Ok((total, tables)) => {
                        this.total = total;
                        let tables: Vec<ErTable> =
                            tables.into_iter().map(|(name, columns)| ErTable { name, columns }).collect();
                        let layout = Rc::new(layout(&tables));
                        State::Ready(Rc::new(tables), layout)
                    }
                };
                cx.notify();
            });
        });
    }

    fn table_box(&self, ix: usize, table: &ErTable, at: Placed, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let (muted, border) = (theme.muted_foreground, theme.border);
        let rows = table.columns.iter().take(MAX_ROWS).map(|column| {
            let key = match (column.is_primary, column.is_foreign) {
                (true, _) => Some(Icon::new(Lucide::KeyRound).size(ICON_XS).text_color(theme.warning)),
                (false, true) => Some(Icon::new(Lucide::Link).size(ICON_XS).text_color(theme.primary)),
                _ => None,
            };
            h_flex()
                .h(rems(ROW_H))
                .flex_none()
                .px(rems(0.615))
                .gap(rems(0.462))
                .child(div().flex_none().w(ICON_XS).children(key))
                .child(div().flex_1().min_w_0().truncate().child(column.name.clone()))
                .child(
                    div()
                        .flex_none()
                        .max_w(rems(6.154))
                        .truncate()
                        .text_size(TEXT_XS)
                        .text_color(muted)
                        .child(column.data_type.to_lowercase()),
                )
        });
        let hidden = table.hidden();
        let name = table.name.clone();
        let on_open = self.on_open.clone();
        v_flex()
            .id(("er-table", ix))
            .debug_selector(move || format!("er-table-{ix}"))
            .absolute()
            .left(rems(at.x))
            .top(rems(at.y))
            .w(rems(BOX_W))
            .h(rems(at.h))
            .overflow_hidden()
            .rounded(RADIUS)
            .border_1()
            .border_color(border)
            .bg(theme.background)
            .text_size(TEXT_SM)
            .cursor_pointer()
            .hover(|style| style.border_color(theme.primary))
            .on_click(move |_, window, cx| {
                window.close_dialog(cx);
                on_open(name.clone(), window, cx);
            })
            .child(
                h_flex()
                    .h(rems(HEADER_H))
                    .flex_none()
                    .px(rems(0.615))
                    .gap(rems(0.462))
                    .bg(theme.secondary)
                    .border_b_1()
                    .border_color(border)
                    .child(Icon::new(Lucide::Table2).size(ICON_XS).text_color(muted))
                    .child(div().min_w_0().truncate().font_semibold().child(table.name.clone())),
            )
            .children(rows)
            .when(hidden > 0, |el| {
                el.child(h_flex().h(rems(ROW_H)).px(rems(0.615)).text_size(TEXT_XS).text_color(muted).child(t_with(
                    cx,
                    "erDiagram.more",
                    &[("count", &hidden.to_string())],
                )))
            })
    }
}

// From the side facing the other box, with horizontal tangents. Both ends in the same column loop out on the right.
fn paint_edges(layout: &Layout, bounds: Bounds<Pixels>, color: Hsla, window: &mut Window) {
    let rem = window.rem_size();
    let at = |x: f32, y: f32| point(bounds.left() + rem * x, bounds.top() + rem * y);
    let row_y = |placed: Placed, row: Option<usize>| match row {
        Some(row) => placed.y + HEADER_H + ROW_H * (row as f32 + 0.5),
        None => placed.y + HEADER_H / 2.,
    };
    for edge in &layout.edges {
        let (from, to) = (layout.boxes[edge.from.0], layout.boxes[edge.to.0]);
        let (y1, y2) = (row_y(from, edge.from.1), row_y(to, edge.to.1));
        let (x1, x2, bend) = if to.x + BOX_W <= from.x {
            (from.x, to.x + BOX_W, -1.)
        } else if to.x >= from.x + BOX_W {
            (from.x + BOX_W, to.x, 1.)
        } else {
            (from.x + BOX_W, to.x + BOX_W, 0.)
        };
        let reach = ((x2 - x1).abs() / 2.).max(2.308);
        let (c1, c2) = match bend {
            0. => (x1 + reach, x2 + reach),
            _ => (x1 + reach * bend, x2 - reach * bend),
        };
        let mut path = PathBuilder::stroke(px(1.5));
        path.move_to(at(x1, y1));
        path.cubic_bezier_to(at(x2, y2), at(c1, y1), at(c2, y2));
        if let Ok(path) = path.build() {
            window.paint_path(path, color);
        }
        let dot = |center: Point<Pixels>| Bounds::centered_at(center, size(px(6.), px(6.)));
        window.paint_quad(fill(dot(at(x2, y2)), color).corner_radii(px(3.)));
    }
}

impl Render for ErDiagram {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let (tables, layout) = match &self.state {
            State::Ready(tables, layout) => (tables.clone(), layout.clone()),
            state => {
                let message = match state {
                    State::Failed(error) => error.clone().into(),
                    State::Empty => t(cx, "erDiagram.empty"),
                    _ => t(cx, "erDiagram.loading"),
                };
                return div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(theme.muted_foreground)
                    .child(message)
                    .into_any_element();
            }
        };
        let boxes: Vec<_> =
            tables.iter().enumerate().map(|(ix, table)| self.table_box(ix, table, layout.boxes[ix], cx)).collect();
        let color = theme.muted_foreground.opacity(0.7);
        let edges = layout.clone();
        let content = div()
            .relative()
            .w(rems(layout.width))
            .h(rems(layout.height))
            .child(
                canvas(|_, _, _| {}, move |bounds, _, window, _| paint_edges(&edges, bounds, color, window))
                    .absolute()
                    .inset_0(),
            )
            .children(boxes);
        v_flex()
            .size_full()
            .when(self.total > MAX_TABLES, |el| {
                el.child(
                    div()
                        .flex_none()
                        .px(rems(1.231))
                        .py(rems(0.462))
                        .text_size(TEXT_XS)
                        .text_color(theme.muted_foreground)
                        .child(t_with(
                            cx,
                            "erDiagram.truncated",
                            &[("shown", &MAX_TABLES.to_string()), ("total", &self.total.to_string())],
                        )),
                )
            })
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .id("er-diagram")
                            .debug_selector(|| "er-diagram".into())
                            .size_full()
                            .overflow_scroll()
                            .track_scroll(&self.scroll)
                            .child(content),
                    )
                    .hover_scrollbar(&self.scroll, ScrollbarAxis::Both),
            )
            .into_any_element()
    }
}

// Clicking a table closes the diagram and hands its name to `on_open`.
pub fn open(
    connection_id: String,
    schema: String,
    window: &mut Window,
    cx: &mut App,
    on_open: impl Fn(String, &mut Window, &mut App) + 'static,
) {
    let title = t_with(cx, "erDiagram.title", &[("schema", &schema)]);
    let view = cx.new(|cx| ErDiagram::new(connection_id, schema, Rc::new(on_open), cx));
    window.open_dialog(cx, move |dialog, window, cx| {
        let height = window.viewport_size().height * 0.85;
        Modal::new("er-diagram", title.clone()).size(modal::Size::Rem(110.)).height(height).build(
            dialog,
            view.clone(),
            window,
            cx,
        )
    });
}

#[cfg(test)]
mod tests {
    use barsql_core::ColumnInfo;

    use super::{ErTable, layout};

    fn column(name: &str, primary: bool, references: Option<&str>) -> ColumnInfo {
        ColumnInfo {
            name: name.into(),
            data_type: "integer".into(),
            is_primary: primary,
            is_foreign: references.is_some(),
            foreign_table: references.unwrap_or_default().into(),
            ..Default::default()
        }
    }

    fn table(name: &str, columns: Vec<ColumnInfo>) -> ErTable {
        ErTable { name: name.into(), columns }
    }

    #[test]
    fn referenced_tables_stand_left_and_loose_ones_last() {
        let tables = [
            table("orders", vec![column("id", true, None), column("user_id", false, Some("users"))]),
            table("users", vec![column("id", true, None)]),
            table("items", vec![column("order_id", false, Some("orders"))]),
            table("notes", vec![column("body", false, None)]),
        ];
        let layout = layout(&tables);
        let x = |ix: usize| layout.boxes[ix].x;
        assert!(x(1) < x(0) && x(0) < x(2), "users, then orders, then items");
        assert!(x(3) > x(2), "a table with no keys comes last");
        assert_eq!(layout.edges.len(), 2);
        assert_eq!(layout.edges[0].to, (1, Some(0)), "SQLite's empty column means the primary key");
    }

    #[test]
    fn a_cycle_still_lays_out() {
        let tables =
            [table("a", vec![column("b_id", false, Some("b"))]), table("b", vec![column("a_id", false, Some("a"))])];
        let layout = layout(&tables);
        assert_ne!(layout.boxes[0].x, layout.boxes[1].x);
    }
}
