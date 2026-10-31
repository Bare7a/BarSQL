use std::ops::Range;
use std::rc::Rc;

use barsql_app::{AlgebraRef, EditorTab};
use barsql_core::{ConnectionConfig, SqlDialect};
use barsql_sql::{AlgebraError, AlgebraNode, Column, Item, TypeFamily, algebra, columns, type_family};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::scroll::ScrollbarAxis;
use gpui_kit::component::{ActiveTheme, Disableable, Icon, IconName, StyledExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::er_diagram::zoom;
use crate::form::ToolButton;
use crate::i18n::{t, t_with};
use crate::schema;
use crate::scrollbars::HoverScrollbar as _;
use crate::toast;
use crate::tokens::{ICON_XS, RADIUS, TEXT_2XS, TEXT_SM, TEXT_XS};

// Sizes are rems of the 13px root, like the ER diagram's.
const NODE_W: f32 = 18.462;
const HEADER_H: f32 = 2.154;
const GAP_X: f32 = 2.308;
const GAP_Y: f32 = 2.846;
const MARGIN: f32 = 1.538;
const BODY_PAD: f32 = 0.385;
// What a body has room for across: the node less its padding.
const INNER_W: f32 = NODE_W - 1.231;
const PILL_H: f32 = 1.462;
const PILL_GAP: f32 = 0.308;
const MAX_PILL_ROWS: usize = 4;
const LINE_H: f32 = 1.385;
const LINE_GAP: f32 = 0.231;
// Room for the AND that leads a condition.
const AND_W: f32 = 2.;
// About how many characters of code fit across a body.
const CHARS_PER_LINE: usize = 32;
// A press that moves further pans, and doesn't count as a click.
const PAN_SLOP: Pixels = px(4.);
// What the preview lists, with the rest counted.
const PREVIEW_COLUMNS: usize = 8;
const PREVIEW_ROWS: usize = 3;
const PREVIEW_COL_W: f32 = 8.462;
const PREVIEW_MIN_W: f32 = 20.;
const PREVIEW_PAD: f32 = 0.769;

// The graph says which part of the query an operator came from, and the workspace takes it from there.
pub enum AlgebraEvent {
    // `range` is where the operator sits in `sql`, the text the graph was drawn from, which began at `offset` in the
    // query tab `source`. None clears what an earlier pick marked.
    Reveal { source: String, offset: usize, sql: String, range: Option<Range<usize>>, color: Hsla },
}

// What a node shows under its header.
enum Body {
    None,
    // The items of a projection, grouping or sort, a row of pills at a time.
    Pills(Vec<Vec<String>>),
    // An expression as code, wrapping to a second line at most.
    Lines(Vec<String>),
    // The conditions of a selection, one a row, each one on a single line.
    Conditions(Vec<String>),
}

struct Flat {
    node: AlgebraNode,
    body: Body,
}

// A join's kind, if it says one, and its condition.
fn join_parts(detail: &str) -> (Option<&str>, &str) {
    let (first, rest) = detail.split_once(' ').unwrap_or((detail, ""));
    let (kind, rest) = match ["LEFT", "RIGHT", "FULL", "CROSS", "SEMI", "ANTI"].contains(&first) {
        true => (Some(first), rest),
        false => (None, detail),
    };
    (kind, rest.strip_prefix("ON ").unwrap_or(rest))
}

fn body_of(node: &AlgebraNode) -> Body {
    let lines = |text: &str| if text.is_empty() { Body::None } else { Body::Lines(vec![text.to_string()]) };
    match node.symbol {
        "" => Body::None,
        "π" | "γ" | "τ" if !node.items.is_empty() => Body::Pills(pill_rows(&node.items)),
        "ρ" | "limit" if !node.detail.is_empty() => Body::Pills(vec![vec![node.detail.clone()]]),
        "σ" if node.items.len() > 1 => Body::Conditions(node.items.iter().map(|item| item.expr.clone()).collect()),
        "⋈" => lines(join_parts(&node.detail).1),
        _ => lines(&node.detail),
    }
}

fn line_count(text: &str) -> usize {
    text.chars().count().div_ceil(CHARS_PER_LINE).clamp(1, 2)
}

// Where each operator goes: leaves side by side along the bottom, every parent above and between its children.
struct Layout {
    // In the order of a walk from the root. The first is the root.
    flat: Vec<Flat>,
    // Left, top and height.
    boxes: Vec<(f32, f32, f32)>,
    // Parent and child.
    edges: Vec<(usize, usize)>,
    width: f32,
    height: f32,
}

// A guess at a pill's width, for filling rows before anything is drawn.
fn pill_width(label: &str) -> f32 {
    (label.chars().count() as f32 * 0.58 + 1.4).min(INNER_W)
}

fn row_width(row: &[String]) -> f32 {
    row.iter().map(|label| pill_width(label)).sum::<f32>() + PILL_GAP * row.len().saturating_sub(1) as f32
}

// The items in rows that fit the node. Past the last row, a "+N" stands for the rest.
fn pill_rows(items: &[Item]) -> Vec<Vec<String>> {
    let mut rows: Vec<Vec<String>> = vec![];
    for item in items {
        let fits = rows.last().is_some_and(|row| row_width(row) + PILL_GAP + pill_width(&item.label) <= INNER_W);
        match (fits, rows.len() == MAX_PILL_ROWS) {
            (true, _) => rows.last_mut().expect("a row fits it").push(item.label.clone()),
            (false, true) => break,
            (false, false) => rows.push(vec![item.label.clone()]),
        }
    }
    let shown = |rows: &[Vec<String>]| rows.iter().map(Vec::len).sum::<usize>();
    if shown(&rows) < items.len() {
        // Room for the count in the last row, wide enough for the biggest there could be.
        let room = pill_width(&format!("+{}", items.len()));
        while let Some(row) = rows.last_mut() {
            if row.len() <= 1 || row_width(row) + PILL_GAP + room <= INNER_W {
                break;
            }
            row.pop();
        }
        let more = format!("+{}", items.len() - shown(&rows));
        if let Some(row) = rows.last_mut() {
            row.push(more);
        }
    }
    rows
}

fn height_of(body: &Body) -> f32 {
    let rows = |count: usize, each: f32, gap: f32| {
        HEADER_H + count as f32 * each + count.saturating_sub(1) as f32 * gap + 2. * BODY_PAD
    };
    match body {
        Body::None => HEADER_H,
        Body::Pills(pills) => rows(pills.len(), PILL_H, PILL_GAP),
        Body::Conditions(list) => rows(list.len(), LINE_H, LINE_GAP),
        Body::Lines(lines) => {
            let count: usize = lines.iter().map(|text| line_count(text)).sum();
            rows(count, LINE_H, LINE_GAP)
        }
    }
}

fn arrange(root: &AlgebraNode) -> Layout {
    fn place(node: &AlgebraNode, depth: usize, next: &mut f32, out: &mut Layout, depths: &mut Vec<usize>) -> usize {
        let ix = out.flat.len();
        let body = body_of(node);
        let height = height_of(&body);
        out.flat.push(Flat { node: node.clone(), body });
        out.boxes.push((0., 0., height));
        depths.push(depth);
        let children: Vec<usize> =
            node.children.iter().map(|child| place(child, depth + 1, next, out, depths)).collect();
        let x = match (children.first(), children.last()) {
            (Some(&first), Some(&last)) => (out.boxes[first].0 + out.boxes[last].0) / 2.,
            _ => {
                *next += NODE_W + GAP_X;
                *next - NODE_W - GAP_X
            }
        };
        out.boxes[ix].0 = x;
        out.edges.extend(children.into_iter().map(|child| (ix, child)));
        ix
    }
    let mut out = Layout { flat: vec![], boxes: vec![], edges: vec![], width: 0., height: 0. };
    let mut depths = vec![];
    place(root, 0, &mut 0., &mut out, &mut depths);
    // A row of the graph is as tall as its tallest box.
    let mut tall = vec![0f32; depths.iter().max().map_or(0, |d| d + 1)];
    for (ix, &depth) in depths.iter().enumerate() {
        tall[depth] = tall[depth].max(out.boxes[ix].2);
    }
    let mut tops = vec![];
    let mut y = MARGIN;
    for height in &tall {
        tops.push(y);
        y += height + GAP_Y;
    }
    for (ix, &depth) in depths.iter().enumerate() {
        out.boxes[ix].0 += MARGIN;
        out.boxes[ix].1 = tops[depth];
    }
    out.width = out.boxes.iter().fold(0f32, |w, (x, ..)| w.max(*x)) + NODE_W + MARGIN;
    out.height = out.boxes.iter().fold(0f32, |h, (_, y, height)| h.max(y + height)) + MARGIN;
    out
}

// A line from each operator up to the one that reads it, with an arrowhead where it arrives.
fn paint_edges(layout: &Layout, color: Hsla, bounds: Bounds<Pixels>, window: &mut Window) {
    let rem = window.rem_size();
    let at = |x: f32, y: f32| point(bounds.left() + rem * x, bounds.top() + rem * y);
    for &(parent, child) in &layout.edges {
        let (from, to) = (layout.boxes[child], layout.boxes[parent]);
        let (x1, y1, x2, y2) = (from.0 + NODE_W / 2., from.1, to.0 + NODE_W / 2., to.1 + to.2);
        let reach = (y1 - y2) / 2.;
        let mut path = PathBuilder::stroke(px(1.5));
        path.move_to(at(x1, y1));
        path.cubic_bezier_to(at(x2, y2), at(x1, y1 - reach), at(x2, y2 + reach));
        if let Ok(path) = path.build() {
            window.paint_path(path, color);
        }
        let mut head = PathBuilder::fill();
        head.move_to(at(x2, y2));
        head.line_to(at(x2 - 0.346, y2 + 0.692));
        head.line_to(at(x2 + 0.346, y2 + 0.692));
        head.close();
        if let Ok(head) = head.build() {
            window.paint_path(head, color);
        }
    }
}

// The name an operator goes by in the locale.
fn op(symbol: &str) -> &'static str {
    match symbol {
        "σ" => "select",
        "π" => "project",
        "⋈" => "join",
        "×" => "cross",
        "γ" => "group",
        "τ" => "sort",
        "limit" => "limit",
        "δ" => "distinct",
        "∪" => "union",
        "∩" => "intersect",
        "−" => "except",
        "ρ" => "rename",
        _ => "table",
    }
}

// A symbol takes its colour from the theme, and nothing else is coloured until it's picked. `base` is the theme's
// warning, primary, success and muted colours.
fn tone(symbol: &str, base: [Hsla; 4]) -> Hsla {
    match op(symbol) {
        "select" => base[0],
        "project" => base[1],
        "join" | "cross" => base[2],
        _ => base[3],
    }
}

fn palette(cx: &App) -> [Hsla; 4] {
    let theme = cx.theme();
    [theme.warning, theme.primary, theme.success, theme.muted_foreground]
}

// The schema and name, as loaded, of the table `name` (as written, maybe quoted or qualified) stands for.
fn find_table(cx: &App, connection_id: &str, name: &str) -> Option<(String, String)> {
    let trim = |part: &str| part.trim_matches(['"', '`', '[', ']']).to_string();
    let parts: Vec<String> = name.split('.').map(trim).collect();
    let (table, schema) = (parts.last()?, (parts.len() > 1).then(|| parts[parts.len() - 2].clone()));
    let loaded = schema::get(cx, connection_id)?;
    loaded.loaded_tables().into_iter().find_map(|(name, tables)| {
        if schema.as_ref().is_some_and(|wanted| !wanted.eq_ignore_ascii_case(name)) {
            return None;
        }
        let found = tables.iter().find(|t| t.name.eq_ignore_ascii_case(table))?;
        Some((name.to_string(), found.name.clone()))
    })
}

fn table_columns(cx: &App, connection_id: &str, name: &str) -> Option<Vec<(String, String)>> {
    let (schema, table) = find_table(cx, connection_id, name)?;
    let columns = schema::get(cx, connection_id)?.columns(&schema, &table)?;
    Some(columns.iter().map(|c| (c.name.clone(), c.data_type.clone())).collect())
}

// Made-up values, so the preview reads like a table without reading the database. The column's name picks what kind of
// value looks right, its type the rest, and the column's place shifts them so no two columns repeat each other.
fn sample(family: TypeFamily, name: &str, column: usize, row: usize) -> String {
    let name = name.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|word| name.contains(word));
    let pick = |set: [&str; PREVIEW_ROWS]| set[(row + column) % PREVIEW_ROWS].to_string();
    match () {
        _ if name == "id" => (row + 1).to_string(),
        _ if name.ends_with("_id") => ((row + column) % PREVIEW_ROWS + 1).to_string(),
        _ if has(&["nombre", "name", "razon", "empresa", "locador", "label"]) => {
            pick(["Ana García", "Luis Pérez", "Marta Díaz"])
        }
        _ if has(&["fecha", "date", "day", "creado", "visto"]) => pick(["2024-03-14", "2024-05-02", "2024-06-21"]),
        _ if has(&["monto", "importe", "total", "precio", "costo", "expected", "neto"]) => {
            pick(["1250.00", "890.50", "42.75"])
        }
        _ if has(&["activo", "sospechosa"]) => pick(["1", "0", "1"]),
        _ => match family {
            TypeFamily::Number => pick(["12", "47", "108"]),
            TypeFamily::Text => pick(["Lorem", "Ipsum", "Dolor"]),
            TypeFamily::Boolean => pick(["true", "false", "true"]),
            TypeFamily::Date => pick(["2024-03-14", "2024-05-02", "2024-06-21"]),
            TypeFamily::Time => pick(["08:15:00", "12:40:30", "18:05:10"]),
            TypeFamily::DateTime => pick(["2024-03-14 08:15", "2024-05-02 12:40", "2024-06-21 18:05"]),
            TypeFamily::Json => pick(["{\"a\": 1}", "{}", "[1, 2]"]),
            TypeFamily::Uuid => pick(["9b2f…e1", "41c7…0a", "d03e…7b"]),
            TypeFamily::Binary => pick(["0x1F8A", "0x00C4", "0xB7E2"]),
            TypeFamily::Array => pick(["{1,2}", "{}", "{3}"]),
            TypeFamily::Enum | TypeFamily::Other => pick(["…", "…", "…"]),
        },
    }
}

// What an operator's table would look like at that point: its columns, with example rows. Every width is set, because
// text that has to work out its own width wraps in one size and is drawn in another.
struct Preview {
    symbol: &'static str,
    title: SharedString,
    detail: String,
    tone: Hsla,
    columns: Vec<(Column, TypeFamily)>,
    more: Option<SharedString>,
    empty: SharedString,
}

impl Preview {
    fn new(
        node: &AlgebraNode,
        connection: &ConnectionConfig,
        tone: Hsla,
        title: SharedString,
        cx: &mut Context<Self>,
    ) -> Self {
        let dialect = connection.driver.dialect();
        let lookup = |name: &str| table_columns(cx, &connection.id, name);
        let columns: Vec<(Column, TypeFamily)> = columns(node, &lookup)
            .into_iter()
            .map(|column| {
                let family = type_family(dialect, &column.data_type);
                (column, family)
            })
            .collect();
        let hidden = columns.len().saturating_sub(PREVIEW_COLUMNS);
        // What a projection, grouping or sort lists is already the header of the table below.
        let says_more = !node.symbol.is_empty() && !matches!(node.symbol, "π" | "γ" | "τ");
        Self {
            symbol: node.symbol,
            title,
            detail: if says_more { node.detail.clone() } else { String::new() },
            tone,
            columns,
            more: (hidden > 0).then(|| t_with(cx, "algebra.moreColumns", &[("count", &hidden.to_string())])),
            empty: t(cx, "algebra.noColumns"),
        }
    }

    fn column(&self, ix: usize, cx: &App) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let (column, family) = &self.columns[ix];
        let origin = match (column.table.is_empty(), column.data_type.is_empty()) {
            (false, false) => format!("{} · {}", column.table, column.data_type),
            (false, true) => column.table.clone(),
            (true, _) => column.data_type.clone(),
        };
        v_flex()
            .flex_none()
            .w(rems(PREVIEW_COL_W))
            .when(ix > 0, |el| el.border_l_1().border_color(theme.border))
            .child(
                v_flex()
                    .h(rems(2.769))
                    .justify_center()
                    .px(rems(0.615))
                    .bg(theme.secondary)
                    .border_b_1()
                    .border_color(theme.border)
                    .child(div().truncate().font_semibold().child(column.name.clone()))
                    .child(div().truncate().text_size(rems(0.692)).text_color(theme.muted_foreground).child(origin)),
            )
            .children((0..PREVIEW_ROWS).map(|row| {
                div()
                    .h(rems(1.846))
                    .px(rems(0.615))
                    .flex()
                    .items_center()
                    .truncate()
                    .when(row > 0, |el| el.border_t_1().border_color(theme.border.opacity(0.5)))
                    .child(sample(*family, &column.name, ix, row))
            }))
    }
}

impl Render for Preview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let shown = self.columns.len().min(PREVIEW_COLUMNS);
        let table_w = shown as f32 * PREVIEW_COL_W + 0.154;
        // As wide as its table, but never so narrow the title and the notes crowd.
        let width = table_w.max(PREVIEW_MIN_W);
        v_flex()
            .w(rems(width + 2. * PREVIEW_PAD))
            .gap(rems(0.615))
            .p(rems(PREVIEW_PAD))
            .bg(theme.popover)
            .border_1()
            .border_color(theme.border)
            .rounded(RADIUS)
            .shadow_md()
            .text_size(TEXT_XS)
            .child(
                h_flex()
                    .gap(rems(0.462))
                    .text_size(TEXT_SM)
                    .when(!self.symbol.is_empty(), |el| {
                        el.child(div().font_semibold().text_color(self.tone).child(self.symbol))
                    })
                    .child(div().font_semibold().child(self.title.clone())),
            )
            .when(!self.detail.is_empty(), |el| {
                el.child(
                    div().w(rems(width)).line_clamp(3).text_color(theme.muted_foreground).child(self.detail.clone()),
                )
            })
            .child(match self.columns.is_empty() {
                true => div().text_color(theme.muted_foreground).child(self.empty.clone()).into_any_element(),
                false => v_flex()
                    .gap(rems(0.462))
                    .child(
                        h_flex()
                            .w(rems(table_w))
                            .flex_none()
                            .items_start()
                            .overflow_hidden()
                            .rounded(RADIUS)
                            .border_1()
                            .border_color(theme.border)
                            .children((0..shown).map(|ix| self.column(ix, cx))),
                    )
                    .children(self.more.clone().map(|more| div().text_color(theme.muted_foreground).child(more)))
                    .into_any_element(),
            })
    }
}

// Drawing a drag as a pan: GPUI draws a dragged value, so this one draws nothing.
struct Pan;

impl Render for Pan {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

// A query as relational algebra, in a tab. Drag to pan, ⌘ or Ctrl with the wheel, a pinch or the buttons to zoom.
// Hovering an operator previews its table, and a click marks where in the query it comes from.
pub struct AlgebraGraph {
    pub(crate) id: String,
    pub(crate) connection: ConnectionConfig,
    sql: String,
    // The query tab the statement came from, and where in it the statement began.
    source: String,
    offset: usize,
    focus: FocusHandle,
    layout: Rc<Layout>,
    selected: Option<usize>,
    zoom: f32,
    // Fitted once the viewport's size is known.
    fitted: bool,
    scroll: ScrollHandle,
    // Where the pointer and the scroll were when a press started, and whether it moved far enough to be a pan.
    pan: Option<(Point<Pixels>, Point<Pixels>)>,
    panned: bool,
}

impl EventEmitter<AlgebraEvent> for AlgebraGraph {}

impl AlgebraGraph {
    pub(crate) fn new(
        id: String,
        connection: ConnectionConfig,
        sql: String,
        origin: AlgebraRef,
        root: &AlgebraNode,
        cx: &mut Context<Self>,
    ) -> Self {
        let layout = arrange(root);
        // The previews list the tables' columns, which load on first use.
        schema::ensure_loaded(&connection.id, connection.driver.clone(), cx);
        for table in layout.flat.iter().filter_map(|flat| flat.node.table.as_ref()) {
            if let Some((schema, table)) = find_table(cx, &connection.id, &table.name) {
                // The schema cache keeps the load going, so this handle isn't needed.
                drop(schema::columns(&connection.id, &schema, &table, cx));
            }
        }
        Self {
            id,
            connection,
            sql,
            source: origin.source,
            offset: origin.offset,
            focus: cx.focus_handle(),
            layout: Rc::new(layout),
            selected: None,
            zoom: 1.,
            fitted: false,
            scroll: ScrollHandle::new(),
            pan: None,
            panned: false,
        }
    }

    pub(crate) fn title(&self, cx: &App) -> SharedString {
        t(cx, "algebra.title")
    }

    pub(crate) fn stored(&self, cx: &App) -> EditorTab {
        EditorTab {
            id: self.id.clone(),
            connection_id: self.connection.id.clone(),
            title: self.title(cx).to_string(),
            sql: self.sql.clone(),
            color: self.connection.color.clone(),
            algebra: Some(AlgebraRef { source: self.source.clone(), offset: self.offset }),
            ..Default::default()
        }
    }

    // A renamed connection or new settings. The graph stays as drawn.
    pub(crate) fn set_connection(&mut self, connection: ConnectionConfig, cx: &mut Context<Self>) {
        self.connection = connection;
        cx.notify();
    }

    pub(crate) fn focus(&self, window: &mut Window, cx: &mut App) {
        window.focus(&self.focus, cx);
    }

    // The operator, lit in its own colour here and in the query. Picking it again lets go.
    fn select(&mut self, ix: Option<usize>, cx: &mut Context<Self>) {
        self.selected = if self.selected == ix { None } else { ix };
        let base = palette(cx);
        let picked = self.selected.map(|ix| &self.layout.flat[ix].node);
        let event = AlgebraEvent::Reveal {
            source: self.source.clone(),
            offset: self.offset,
            sql: self.sql.clone(),
            range: picked.and_then(|node| node.span.clone()),
            color: picked.map_or(base[3], |node| tone(node.symbol, base)),
        };
        cx.emit(event);
        cx.notify();
    }

    fn content(&self, rem: Pixels, zoom: f32) -> Size<Pixels> {
        size(rem * self.layout.width * zoom, rem * self.layout.height * zoom)
    }

    // The empty room a graph smaller than the viewport is centred in. A bigger one has none, so it can be scrolled
    // to its edges.
    fn margin(&self, rem: Pixels, zoom: f32) -> Point<Pixels> {
        let (view, content) = (self.scroll.bounds().size, self.content(rem, zoom));
        point(((view.width - content.width) / 2.).max(px(0.)), ((view.height - content.height) / 2.).max(px(0.)))
    }

    fn zoom_to(&mut self, zoom: f32, anchor: Option<Point<Pixels>>, window: &Window, cx: &mut Context<Self>) {
        let zoom = zoom.clamp(zoom::MIN, zoom::MAX);
        if (zoom - self.zoom).abs() < 1e-4 {
            return;
        }
        let (view, rem) = (self.scroll.bounds(), window.rem_size());
        let anchor = anchor.map_or(point(view.size.width / 2., view.size.height / 2.), |at| at - view.origin);
        let from = self.scroll.offset() + self.margin(rem, self.zoom);
        let offset = zoom::anchored(from, anchor, self.zoom, zoom) - self.margin(rem, zoom);
        self.zoom = zoom;
        self.scroll.set_offset(offset);
        cx.notify();
    }

    fn fit(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.zoom = zoom::fit(self.content(window.rem_size(), 1.), self.scroll.bounds().size);
        self.scroll.set_offset(point(px(0.), px(0.)));
        cx.notify();
    }

    fn toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let modifier = if cfg!(target_os = "macos") { "⌘" } else { "Ctrl" };
        h_flex()
            .flex_none()
            .justify_end()
            .gap(rems(0.462))
            .px(rems(1.231))
            .py(rems(0.615))
            .border_b_1()
            .border_color(theme.border)
            .child(
                Button::new("ra-help")
                    .ghost()
                    .tool_icon(Icon::new(Lucide::CircleQuestionMark), ICON_XS)
                    .tooltip(t_with(cx, "algebra.hint", &[("modifier", modifier)])),
            )
            .child(
                Button::new("ra-zoom-out")
                    .tool_icon(Icon::new(IconName::Minus), ICON_XS)
                    .disabled(self.zoom <= zoom::MIN)
                    .tooltip(t(cx, "erDiagram.zoomOut"))
                    .on_click(
                        cx.listener(|this, _, window, cx| this.zoom_to(zoom::step(this.zoom, false), None, window, cx)),
                    ),
            )
            .child(
                Button::new("ra-zoom-reset")
                    .tool_label(format!("{}%", (self.zoom * 100.).round()))
                    .tooltip(t(cx, "erDiagram.zoomReset"))
                    .on_click(cx.listener(|this, _, window, cx| this.zoom_to(1., None, window, cx))),
            )
            .child(
                Button::new("ra-zoom-in")
                    .tool_icon(Icon::new(IconName::Plus), ICON_XS)
                    .disabled(self.zoom >= zoom::MAX)
                    .tooltip(t(cx, "erDiagram.zoomIn"))
                    .on_click(
                        cx.listener(|this, _, window, cx| this.zoom_to(zoom::step(this.zoom, true), None, window, cx)),
                    ),
            )
            .child(
                Button::new("ra-fit")
                    .tool(Icon::new(Lucide::Scan), ICON_XS, t(cx, "erDiagram.fit"))
                    .tooltip(t(cx, "erDiagram.fitTooltip"))
                    .on_click(cx.listener(|this, _, window, cx| this.fit(window, cx))),
            )
    }

    // Like an ER table: a header with the operator, and under it what it works on: pills where it lists things, else
    // the condition or expression as code.
    fn node(&self, ix: usize, tone: Hsla, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let flat = &self.layout.flat[ix];
        let node = &flat.node;
        let (x, y, height) = self.layout.boxes[ix];
        let table = node.symbol.is_empty();
        let picked = self.selected == Some(ix);
        let title = match table {
            true => node.detail.clone(),
            false => t(cx, &format!("algebra.ops.{}", op(node.symbol))).to_string(),
        };
        // The kind of join sits in the header, and the body keeps the condition.
        let tag = if node.symbol == "⋈" { join_parts(&node.detail).0.map(str::to_string) } else { None };
        let has_body = !matches!(flat.body, Body::None);
        let code = |text: String, lines: usize| {
            div().min_w_0().font_family(theme.mono_font_family.clone()).text_size(TEXT_XS).line_clamp(lines).child(text)
        };
        let header = h_flex()
            .h(rems(HEADER_H))
            .flex_none()
            .px(rems(0.615))
            .gap(rems(0.462))
            .bg(if picked { tone.opacity(0.14) } else { theme.secondary })
            .when(has_body, |el| el.border_b_1().border_color(if picked { tone.opacity(0.4) } else { theme.border }))
            .child(match table {
                true => div().child(Icon::new(Lucide::Table2).size(ICON_XS).text_color(theme.muted_foreground)),
                false => div().flex_none().min_w(ICON_XS).font_semibold().text_color(tone).child(node.symbol),
            })
            .child(div().flex_1().min_w_0().truncate().font_semibold().child(title.clone()))
            .children(tag.map(|tag| {
                div()
                    .flex_none()
                    .px(rems(0.462))
                    .rounded(rems(PILL_H / 2.))
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.background)
                    .text_size(TEXT_2XS)
                    .text_color(theme.muted_foreground)
                    .child(tag)
            }));
        let body = match &flat.body {
            Body::None => None,
            Body::Pills(rows) => Some(
                v_flex()
                    .flex_none()
                    .gap(rems(PILL_GAP))
                    .px(rems(0.615))
                    .py(rems(BODY_PAD))
                    .children(rows.iter().map(|row| {
                        h_flex().gap(rems(PILL_GAP)).children(row.iter().map(|label| {
                            let more = label.starts_with('+') && label[1..].chars().all(|c| c.is_ascii_digit());
                            div()
                                .flex_none()
                                .h(rems(PILL_H))
                                .px(rems(0.538))
                                .flex()
                                .items_center()
                                .max_w(rems(INNER_W))
                                .rounded(rems(PILL_H / 2.))
                                .border_1()
                                .border_color(theme.border)
                                .bg(if more { theme.background } else { theme.secondary })
                                .text_size(TEXT_XS)
                                .when(more, |el| el.text_color(theme.muted_foreground))
                                .truncate()
                                .child(label.clone())
                        }))
                    }))
                    .into_any_element(),
            ),
            Body::Lines(lines) => Some(
                v_flex()
                    .flex_none()
                    .gap(rems(LINE_GAP))
                    .px(rems(0.615))
                    .py(rems(BODY_PAD))
                    .children(lines.iter().map(|text| code(text.clone(), line_count(text))))
                    .into_any_element(),
            ),
            Body::Conditions(list) => Some(
                v_flex()
                    .flex_none()
                    .gap(rems(LINE_GAP))
                    .px(rems(0.615))
                    .py(rems(BODY_PAD))
                    .children(list.iter().enumerate().map(|(row, text)| {
                        h_flex()
                            .gap(rems(0.385))
                            .child(
                                div()
                                    .flex_none()
                                    .w(rems(AND_W))
                                    .font_family(theme.mono_font_family.clone())
                                    .text_size(TEXT_2XS)
                                    .text_color(theme.muted_foreground)
                                    .child(if row > 0 { "AND" } else { "" }),
                            )
                            .child(code(text.clone(), 1).flex_1())
                    }))
                    .into_any_element(),
            ),
        };
        let (preview, connection, title) = (node.clone(), self.connection.clone(), SharedString::from(title));
        v_flex()
            .id(("ra-node", ix))
            .absolute()
            .left(rems(x))
            .top(rems(y))
            .w(rems(NODE_W))
            .h(rems(height))
            .overflow_hidden()
            .rounded(RADIUS)
            .border_1()
            .border_color(if picked { tone } else { theme.border })
            .bg(theme.background)
            .text_size(TEXT_SM)
            .cursor_pointer()
            .child(header)
            .children(body)
            .tooltip(move |_, cx| {
                let (node, connection, title) = (preview.clone(), connection.clone(), title.clone());
                cx.new(|cx| Preview::new(&node, &connection, tone, title, cx)).into()
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                if !this.panned {
                    this.select(Some(ix), cx);
                }
            }))
            .into_any_element()
    }
}

impl Render for AlgebraGraph {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        // Opens with all of it in view, unless that would make it too small to read.
        if !self.fitted {
            let view = self.scroll.bounds().size;
            if view.width > px(0.) && view.height > px(0.) {
                self.zoom = zoom::fit(self.content(window.rem_size(), 1.), view).clamp(zoom::READABLE, 1.);
                self.fitted = true;
            } else {
                cx.on_next_frame(window, |_, _, cx| cx.notify());
            }
        }
        let layout = self.layout.clone();
        let base = palette(cx);
        let edge = theme.muted_foreground.opacity(0.6);
        let margin = self.margin(window.rem_size(), self.zoom);
        let nodes: Vec<AnyElement> = (0..layout.flat.len())
            .map(|ix| {
                let tone = tone(layout.flat[ix].node.symbol, base);
                self.node(ix, tone, cx)
            })
            .collect();
        let content = div()
            .relative()
            .flex_none()
            .ml(margin.x)
            .mt(margin.y)
            .w(rems(layout.width))
            .h(rems(layout.height))
            .child(
                canvas(|_, _, _| {}, move |bounds, _, window, _| paint_edges(&layout, edge, bounds, window))
                    .absolute()
                    .inset_0(),
            )
            .children(nodes);
        let viewport = div()
            .id("ra-graph")
            .size_full()
            .flex()
            .overflow_scroll()
            .track_scroll(&self.scroll)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, _| {
                    this.pan = Some((event.position, this.scroll.offset()));
                    this.panned = false;
                }),
            )
            .on_drag(Pan, |_, _, _, cx| cx.new(|_| Pan))
            .on_drag_move(cx.listener(|this, event: &DragMoveEvent<Pan>, _, cx| {
                if let Some((start, offset)) = this.pan {
                    let moved = event.event.position - start;
                    this.panned |= moved.x.abs().max(moved.y.abs()) > PAN_SLOP;
                    this.scroll.set_offset(offset + moved);
                    cx.notify();
                }
            }))
            .on_click(cx.listener(|this, _, _, cx| {
                if !this.panned && this.selected.is_some() {
                    this.select(None, cx);
                }
            }))
            .on_pinch(cx.listener(|this, event: &PinchEvent, window, cx| {
                let zoom = this.zoom * (1. + event.delta);
                this.zoom_to(zoom, Some(event.position), window, cx);
            }))
            .child(zoom::zoomed(self.zoom, content));
        // ⌘ or Ctrl with the wheel zooms, ahead of the viewport scrolling. A plain wheel scrolls.
        let this = cx.entity().downgrade();
        let wheel = canvas(
            |_, _, _| {},
            move |bounds, _, window, _| {
                let this = this.clone();
                window.on_mouse_event(move |event: &ScrollWheelEvent, phase, window, cx| {
                    let zooming = event.modifiers.platform || event.modifiers.control;
                    if phase != DispatchPhase::Capture || !zooming || !bounds.contains(&event.position) {
                        return;
                    }
                    cx.stop_propagation();
                    let dy = event.delta.pixel_delta(window.line_height()).y;
                    let _ = this.update(cx, |view, cx| {
                        let zoom = zoom::wheel(view.zoom, dy);
                        view.zoom_to(zoom, Some(event.position), window, cx);
                    });
                });
            },
        )
        .absolute()
        .inset_0();
        v_flex().size_full().track_focus(&self.focus).bg(theme.background).child(self.toolbar(cx)).child(
            div()
                .relative()
                .flex_1()
                .min_h_0()
                .child(viewport)
                .child(wheel)
                .hover_scrollbar(&self.scroll, ScrollbarAxis::Both),
        )
    }
}

// The query as a tree for this connection's dialect.
pub(crate) fn parse(sql: &str, connection: &ConnectionConfig) -> Result<AlgebraNode, AlgebraError> {
    algebra(sql, connection.driver.dialect().unwrap_or(SqlDialect::Postgres))
}

pub(crate) fn report(error: AlgebraError, cx: &mut App) {
    match error {
        AlgebraError::NotQuery => toast::error(t(cx, "algebra.notQuery"), cx),
        AlgebraError::Parse(error) => toast::error(error, cx),
    }
}

#[cfg(test)]
mod tests {
    use barsql_core::SqlDialect;
    use barsql_sql::{Item, Source, algebra};

    use barsql_sql::TypeFamily;

    use super::{arrange, pill_rows, sample};

    fn item(label: &str) -> Item {
        Item { label: label.into(), expr: label.into(), source: Source::Expr }
    }

    #[test]
    fn parents_sit_between_their_children() {
        let join = algebra("SELECT * FROM a JOIN b ON a.id = b.id", SqlDialect::Sqlite).unwrap();
        let layout = arrange(&join);
        let find = |name: &str| layout.flat.iter().position(|f| f.node.detail == name).unwrap();
        let join_ix = layout.flat.iter().position(|f| f.node.symbol == "⋈").unwrap();
        let (a, b) = (layout.boxes[find("a")], layout.boxes[find("b")]);
        assert_eq!(layout.boxes[join_ix].0, (a.0 + b.0) / 2.);
        assert!(layout.boxes[join_ix].1 < a.1);
    }

    #[test]
    fn pills_wrap_into_rows_and_count_what_does_not_fit() {
        assert_eq!(pill_rows(&[item("a"), item("b"), item("c")]), vec![vec!["a", "b", "c"]]);
        let many: Vec<Item> = (0..60).map(|n| item(&format!("column_{n}"))).collect();
        let rows = pill_rows(&many);
        assert_eq!(rows.len(), 4, "capped");
        let shown = rows.iter().map(Vec::len).sum::<usize>() - 1;
        assert_eq!(rows.last().and_then(|row| row.last()), Some(&format!("+{}", 60 - shown)));
    }

    #[test]
    fn example_values_differ_by_column_and_follow_the_name() {
        let first = |name: &str, column: usize| sample(TypeFamily::Number, name, column, 0);
        assert_ne!(first("a", 0), first("b", 1), "columns don't repeat each other");
        assert_eq!(sample(TypeFamily::Text, "id", 3, 1), "2");
        assert_eq!(sample(TypeFamily::Text, "fecha", 0, 0), "2024-03-14");
    }
}
