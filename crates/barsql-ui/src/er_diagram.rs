mod layout;
#[cfg(test)]
mod tests;
mod zoom;

use std::rc::Rc;

use barsql_core::ColumnInfo;
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::scroll::ScrollbarAxis;
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, IconName, Selectable, Sizable, StyledExt, WindowExt, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use self::layout::{BOX_W, ErTable, HEADER_H, Layout, ROW_H, arrange, find};
use crate::form::{self, ToolButton};
use crate::i18n::{I18n, t, t_with};
use crate::modal::{self, Modal};
use crate::schema;
use crate::scrollbars::HoverScrollbar as _;
use crate::tokens::{ICON_XS, RADIUS, TEXT_SM, TEXT_XS, TINT};

// Tables a diagram loads.
pub const MAX_TABLES: usize = 200;
// Boxes away from the picked table fade to this.
const FADED: f32 = 0.3;
// A press that moves further pans, and doesn't count as a click.
const PAN_SLOP: Pixels = px(4.);

enum State {
    Loading,
    Ready(Rc<Vec<ErTable>>),
    Empty,
    Failed(String),
}

type OnOpen = Rc<dyn Fn(String, &mut Window, &mut App)>;

// A drag on the diagram pans it. GPUI draws a dragged value, so this one draws nothing.
struct Pan;

impl Render for Pan {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

// The tables of a schema and the keys between them. Drag to pan, ⌘ or Ctrl with the wheel, a pinch or the buttons
// to zoom. Hovering a table previews its keys and a click keeps them lit; a double click opens it.
pub struct ErDiagram {
    connection_id: String,
    schema: String,
    state: State,
    total: usize,
    keys_only: bool,
    layout: Rc<Layout>,
    zoom: f32,
    // Fitted once the viewport's size is known.
    fitted: bool,
    hovered: Option<usize>,
    selected: Option<usize>,
    search: Entity<InputState>,
    query: String,
    matches: Vec<usize>,
    hit: usize,
    scroll: ScrollHandle,
    // Where the pointer and the scroll were when a press started, for a drag to pan from, and whether it moved far
    // enough to be a pan rather than a click.
    pan: Option<(Point<Pixels>, Point<Pixels>)>,
    panned: bool,
    on_open: OnOpen,
    _load: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl ErDiagram {
    fn new(
        connection_id: String,
        schema: String,
        on_open: OnOpen,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let placeholder = t(cx, "erDiagram.search");
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        let subscriptions = vec![
            // The input also reports a change after Enter, with the same text, which mustn't start the hits over.
            cx.subscribe_in(&search, window, |this, input, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    let query = input.read(cx).value().to_string();
                    if query != this.query {
                        this.find(query, window, cx);
                    }
                }
                InputEvent::PressEnter { shift, .. } => this.step_hit(if *shift { -1 } else { 1 }, window, cx),
                _ => {}
            }),
            cx.observe_global_in::<I18n>(window, |this, window, cx| {
                let placeholder = t(cx, "erDiagram.search");
                this.search.update(cx, |search, cx| search.set_placeholder(placeholder, window, cx));
            }),
        ];
        Self {
            connection_id,
            schema,
            state: State::Loading,
            total: 0,
            keys_only: false,
            layout: Rc::new(arrange(&[], false)),
            zoom: 1.,
            fitted: false,
            hovered: None,
            selected: None,
            search,
            query: String::new(),
            matches: Vec::new(),
            hit: 0,
            scroll: ScrollHandle::new(),
            pan: None,
            panned: false,
            on_open,
            _load: Task::ready(()),
            _subscriptions: subscriptions,
        }
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let load = schema::load_schema_columns(&self.connection_id, &self.schema, MAX_TABLES, cx);
        self._load = cx.spawn(async move |this, cx| {
            let loaded = load.await;
            let _ = this.update(cx, |this, cx| {
                match loaded {
                    Err(error) => this.state = State::Failed(error),
                    Ok((_, tables)) if tables.is_empty() => this.state = State::Empty,
                    Ok((total, tables)) => this.set_tables(total, tables),
                }
                cx.notify();
            });
        });
    }

    fn set_tables(&mut self, total: usize, tables: Vec<(String, Vec<ColumnInfo>)>) {
        let tables: Vec<ErTable> = tables.into_iter().map(|(name, columns)| ErTable::new(name, columns)).collect();
        self.total = total;
        self.state = State::Ready(Rc::new(tables));
        self.fitted = false;
        self.relayout();
    }

    fn relayout(&mut self) {
        if let State::Ready(tables) = &self.state {
            self.layout = Rc::new(arrange(tables, self.keys_only));
        }
    }

    fn content(&self, rem: Pixels, zoom: f32) -> Size<Pixels> {
        size(rem * self.layout.width * zoom, rem * self.layout.height * zoom)
    }

    // The empty room a diagram smaller than the viewport is centred in.
    fn margin(&self, rem: Pixels, zoom: f32) -> Point<Pixels> {
        let (view, content) = (self.scroll.bounds().size, self.content(rem, zoom));
        point(((view.width - content.width) / 2.).max(px(0.)), ((view.height - content.height) / 2.).max(px(0.)))
    }

    // `anchor`, in the window, stays over the same part of the diagram. Without one, the middle of the view does.
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

    fn centre_on(&mut self, ix: usize, window: &Window) {
        let Some(placed) = self.layout.boxes.get(ix) else { return };
        let scale = window.rem_size() * self.zoom;
        let (x, y) = placed.center();
        let target = point(scale * x, scale * y);
        let content = self.content(window.rem_size(), self.zoom);
        self.scroll.set_offset(zoom::centred(target, self.scroll.bounds().size, content));
    }

    fn find(&mut self, query: String, window: &Window, cx: &mut Context<Self>) {
        let State::Ready(tables) = &self.state else { return };
        self.matches = find(tables, &query);
        self.query = query;
        self.hit = 0;
        if let Some(&first) = self.matches.first() {
            self.selected = Some(first);
            self.centre_on(first, window);
        }
        cx.notify();
    }

    fn step_hit(&mut self, by: isize, window: &Window, cx: &mut Context<Self>) {
        if self.matches.is_empty() {
            return;
        }
        self.hit = (self.hit as isize + by).rem_euclid(self.matches.len() as isize) as usize;
        let ix = self.matches[self.hit];
        self.selected = Some(ix);
        self.centre_on(ix, window);
        cx.notify();
    }

    fn toggle_keys(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.keys_only = !self.keys_only;
        self.relayout();
        if let Some(ix) = self.selected {
            self.centre_on(ix, window);
        }
        cx.notify();
    }

    fn select(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.selected = if self.selected == Some(ix) { None } else { Some(ix) };
        cx.notify();
    }

    fn open(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let State::Ready(tables) = &self.state else { return };
        let Some(table) = tables.get(ix) else { return };
        let (name, on_open) = (table.name.clone(), self.on_open.clone());
        on_open(name, window, cx);
    }

    fn pan_to(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some((start, offset)) = self.pan else { return };
        let moved = position - start;
        self.panned |= moved.x.abs().max(moved.y.abs()) > PAN_SLOP;
        self.scroll.set_offset(offset + moved);
        cx.notify();
    }

    fn toolbar(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let ready = matches!(self.state, State::Ready(_));
        let found = match (!self.query.trim().is_empty(), self.matches.len()) {
            (false, _) => None,
            (true, 0) => Some(t(cx, "erDiagram.noMatches")),
            (true, count) => {
                let (index, count) = ((self.hit + 1).to_string(), count.to_string());
                Some(t_with(cx, "erDiagram.matches", &[("index", &index), ("count", &count)]))
            }
        };
        let percent = format!("{}%", (self.zoom * 100.).round());
        let truncated = (self.total > MAX_TABLES).then(|| {
            let (shown, total) = (MAX_TABLES.to_string(), self.total.to_string());
            t_with(cx, "erDiagram.truncated", &[("shown", &shown), ("total", &total)])
        });
        h_flex()
            .flex_none()
            .gap(rems(0.462))
            .px(rems(1.231))
            .py(rems(0.615))
            .border_b_1()
            .border_color(theme.border)
            .child(div().flex_none().w(rems(15.385)).debug_selector(|| "er-search".into()).child(form::filter_input(
                &self.search,
                window,
                cx,
            )))
            .children(found.map(|text| {
                div()
                    .flex_none()
                    .text_size(TEXT_XS)
                    .text_color(theme.muted_foreground)
                    .debug_selector(|| "er-matches".into())
                    .child(text)
            }))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_right()
                    .text_size(TEXT_XS)
                    .text_color(theme.muted_foreground)
                    .children(truncated),
            )
            .child(
                Button::new("er-keys-only")
                    .debug_selector(|| "er-keys-only".into())
                    .tool(Icon::new(Lucide::KeyRound), ICON_XS, t(cx, "erDiagram.keysOnly"))
                    .selected(self.keys_only)
                    .disabled(!ready)
                    .tooltip(t(cx, "erDiagram.keysOnlyTooltip"))
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_keys(window, cx))),
            )
            .child(div().flex_none().w(px(1.)).h(rems(1.231)).mx(rems(0.154)).bg(theme.border))
            .child(
                Button::new("er-zoom-out")
                    .debug_selector(|| "er-zoom-out".into())
                    .tool_icon(Icon::new(IconName::Minus), ICON_XS)
                    .disabled(!ready || self.zoom <= zoom::MIN)
                    .tooltip(t(cx, "erDiagram.zoomOut"))
                    .on_click(
                        cx.listener(|this, _, window, cx| this.zoom_to(zoom::step(this.zoom, false), None, window, cx)),
                    ),
            )
            .child(
                Button::new("er-zoom-reset")
                    .debug_selector(|| "er-zoom-reset".into())
                    .tool_label(percent)
                    .disabled(!ready)
                    .tooltip(t(cx, "erDiagram.zoomReset"))
                    .on_click(cx.listener(|this, _, window, cx| this.zoom_to(1., None, window, cx))),
            )
            .child(
                Button::new("er-zoom-in")
                    .debug_selector(|| "er-zoom-in".into())
                    .tool_icon(Icon::new(IconName::Plus), ICON_XS)
                    .disabled(!ready || self.zoom >= zoom::MAX)
                    .tooltip(t(cx, "erDiagram.zoomIn"))
                    .on_click(
                        cx.listener(|this, _, window, cx| this.zoom_to(zoom::step(this.zoom, true), None, window, cx)),
                    ),
            )
            .child(
                Button::new("er-fit")
                    .debug_selector(|| "er-fit".into())
                    .tool(Icon::new(Lucide::Scan), ICON_XS, t(cx, "erDiagram.fit"))
                    .disabled(!ready)
                    .tooltip(t(cx, "erDiagram.fitTooltip"))
                    .on_click(cx.listener(|this, _, window, cx| this.fit(window, cx))),
            )
    }

    fn diagram(&self, tables: &[ErTable], cx: &mut Context<Self>) -> impl IntoElement {
        let layout = self.layout.clone();
        let focus = self.selected.or(self.hovered);
        let (lit, near) = focus.map(|ix| layout.related(ix)).unwrap_or_default();
        let boxes: Vec<AnyElement> =
            tables.iter().enumerate().map(|(ix, table)| self.table_box(ix, table, focus, &near, &lit, cx)).collect();
        let theme = cx.theme();
        let base = theme.muted_foreground.opacity(if focus.is_some() { 0.15 } else { 0.6 });
        let accent = theme.primary;
        let (all, picked) = (layout.clone(), layout.clone());
        let content = div()
            .relative()
            .flex_none()
            .m_auto()
            .w(rems(layout.width))
            .h(rems(layout.height))
            .child(
                canvas(|_, _, _| {}, move |bounds, _, window, _| paint_edges(&all, None, bounds, base, window))
                    .absolute()
                    .inset_0(),
            )
            .children(boxes)
            .child(
                canvas(
                    |_, _, _| {},
                    move |bounds, _, window, _| paint_edges(&picked, Some(&lit), bounds, accent, window),
                )
                .absolute()
                .inset_0(),
            );
        zoom::zoomed(self.zoom, content)
    }

    // The focused table has an accent border and header, and the open button. Its neighbours' borders are tinted, as
    // are search hits', the rows its keys join are tinted, and every other box fades.
    fn table_box(
        &self,
        ix: usize,
        table: &ErTable,
        focus: Option<usize>,
        near: &[usize],
        lit: &[usize],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let at = self.layout.boxes[ix];
        let focused = focus == Some(ix);
        let marked = near.contains(&ix) || self.matches.contains(&ix);
        let faded = focus.is_some() && !focused && !near.contains(&ix);
        let lit_rows: Vec<usize> = lit
            .iter()
            .map(|&edge| &self.layout.edges[edge])
            .flat_map(|edge| [edge.from, edge.to])
            .filter(|(table, _)| *table == ix)
            .filter_map(|(_, row)| row)
            .collect();
        let rows = table.shown(self.keys_only).iter().enumerate().map(|(row, column)| {
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
                .when(lit_rows.contains(&row), |el| el.bg(theme.primary.opacity(TINT)))
                .child(div().flex_none().w(ICON_XS).children(key))
                .child(div().flex_1().min_w_0().truncate().child(column.name.clone()))
                .child(
                    div()
                        .flex_none()
                        .max_w(rems(6.154))
                        .truncate()
                        .text_size(TEXT_XS)
                        .text_color(theme.muted_foreground)
                        .child(column.data_type.to_lowercase()),
                )
        });
        let border = match () {
            _ if focused => theme.primary,
            _ if marked => theme.primary.opacity(0.5),
            _ => theme.border,
        };
        let header = h_flex()
            .h(rems(HEADER_H))
            .flex_none()
            .px(rems(0.615))
            .gap(rems(0.462))
            .bg(if focused { theme.primary.opacity(TINT) } else { theme.secondary })
            .border_b_1()
            .border_color(theme.border)
            .child(Icon::new(Lucide::Table2).size(ICON_XS).text_color(theme.muted_foreground))
            .child(div().flex_1().min_w_0().truncate().font_semibold().child(table.name.clone()))
            .when(focused, |el| {
                el.child(
                    Button::new(("er-open", ix))
                        .debug_selector(move || format!("er-open-{ix}"))
                        .ghost()
                        .xsmall()
                        .icon(IconName::ExternalLink)
                        .tooltip(t(cx, "erDiagram.open"))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.open(ix, window, cx);
                        })),
                )
            });
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
            .when(faded, |el| el.opacity(FADED))
            .text_size(TEXT_SM)
            .cursor_pointer()
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                match (*hovered, this.hovered == Some(ix)) {
                    (true, _) => this.hovered = Some(ix),
                    (false, true) => this.hovered = None,
                    (false, false) => return,
                }
                cx.notify();
            }))
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                cx.stop_propagation();
                if this.panned {
                    return;
                }
                match event.click_count() {
                    2.. => this.open(ix, window, cx),
                    _ => this.select(ix, cx),
                }
            }))
            .child(header)
            .children(rows)
            .into_any_element()
    }
}

// From the side facing the other box, with horizontal tangents. Both ends in the same column loop out on the right.
// `only` draws just those edges, thicker and with arrowheads where they arrive.
fn paint_edges(layout: &Layout, only: Option<&[usize]>, bounds: Bounds<Pixels>, color: Hsla, window: &mut Window) {
    let rem = window.rem_size();
    let at = |x: f32, y: f32| point(bounds.left() + rem * x, bounds.top() + rem * y);
    let row_y = |placed: layout::Placed, row: Option<usize>| match row {
        Some(row) => placed.y + HEADER_H + ROW_H * (row as f32 + 0.5),
        None => placed.y + HEADER_H / 2.,
    };
    let width = if only.is_some() { px(2.) } else { px(1.5) };
    for (ix, edge) in layout.edges.iter().enumerate() {
        if only.is_some_and(|only| !only.contains(&ix)) {
            continue;
        }
        let (from, to) = (layout.boxes[edge.from.0], layout.boxes[edge.to.0]);
        let (y1, y2) = (row_y(from, edge.from.1), row_y(to, edge.to.1));
        // `inward` is the way the line arrives: +1 into a box's left side, -1 into its right.
        let (x1, x2, bend, inward) = if to.x + BOX_W <= from.x {
            (from.x, to.x + BOX_W, -1., -1.)
        } else if to.x >= from.x + BOX_W {
            (from.x + BOX_W, to.x, 1., 1.)
        } else {
            (from.x + BOX_W, to.x + BOX_W, 0., -1.)
        };
        let reach = ((x2 - x1).abs() / 2.).max(2.308);
        let (c1, c2) = match bend {
            0. => (x1 + reach, x2 + reach),
            _ => (x1 + reach * bend, x2 - reach * bend),
        };
        let mut path = PathBuilder::stroke(width);
        path.move_to(at(x1, y1));
        path.cubic_bezier_to(at(x2, y2), at(c1, y1), at(c2, y2));
        if let Ok(path) = path.build() {
            window.paint_path(path, color);
        }
        match only {
            Some(_) => {
                let (back, half) = (x2 - inward * 0.692, 0.346);
                let mut head = PathBuilder::fill();
                head.move_to(at(x2, y2));
                head.line_to(at(back, y2 - half));
                head.line_to(at(back, y2 + half));
                head.close();
                if let Ok(head) = head.build() {
                    window.paint_path(head, color);
                }
            }
            None => {
                let dot = Bounds::centered_at(at(x2, y2), size(px(6.), px(6.)));
                window.paint_quad(fill(dot, color).corner_radii(px(3.)));
            }
        }
    }
}

impl Render for ErDiagram {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        // Opens with all of it in view, unless that would make it too small to read.
        if matches!(self.state, State::Ready(_)) && !self.fitted {
            let view = self.scroll.bounds().size;
            if view.width > px(0.) && view.height > px(0.) {
                self.zoom = zoom::fit(self.content(window.rem_size(), 1.), view).clamp(zoom::READABLE, 1.);
                self.fitted = true;
            } else {
                cx.on_next_frame(window, |_, _, cx| cx.notify());
            }
        }
        let body = match &self.state {
            State::Ready(tables) => self.diagram(&tables.clone(), cx).into_any_element(),
            state => {
                let message = match state {
                    State::Failed(error) => error.clone().into(),
                    State::Empty => t(cx, "erDiagram.empty"),
                    _ => t(cx, "erDiagram.loading"),
                };
                div().m_auto().text_color(theme.muted_foreground).child(message).into_any_element()
            }
        };
        let viewport = div()
            .id("er-diagram")
            .debug_selector(|| "er-diagram".into())
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
            .on_drag_move(cx.listener(|this, event: &DragMoveEvent<Pan>, _, cx| this.pan_to(event.event.position, cx)))
            .on_click(cx.listener(|this, _, _, cx| {
                if !this.panned {
                    this.selected = None;
                    cx.notify();
                }
            }))
            .on_pinch(cx.listener(|this, event: &PinchEvent, window, cx| {
                let zoom = this.zoom * (1. + event.delta);
                this.zoom_to(zoom, Some(event.position), window, cx);
            }))
            .child(body);
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
        v_flex().size_full().child(self.toolbar(window, cx)).child(
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

// Opening a table closes the diagram and hands its name to `on_open`.
pub fn open(
    connection_id: String,
    schema: String,
    window: &mut Window,
    cx: &mut App,
    on_open: impl Fn(String, &mut Window, &mut App) + 'static,
) {
    let title = t_with(cx, "erDiagram.title", &[("schema", &schema)]);
    let hint = t_with(cx, "erDiagram.hint", &[("modifier", if cfg!(target_os = "macos") { "⌘" } else { "Ctrl" })]);
    let on_open: OnOpen = Rc::new(move |table, window, cx| {
        window.close_dialog(cx);
        on_open(table, window, cx);
    });
    let view = cx.new(|cx| {
        let mut view = ErDiagram::new(connection_id, schema, on_open, window, cx);
        view.load(cx);
        view
    });
    window.open_dialog(cx, move |dialog, window, cx| {
        let height = window.viewport_size().height * 0.85;
        // Enter belongs to the search, which steps through its hits. Escape closes.
        Modal::new("er-diagram", title.clone())
            .extra(hint.clone())
            .size(modal::Size::Rem(110.))
            .height(height)
            .build(dialog, view.clone(), window, cx)
            .on_ok(|_, _, _| false)
    });
}
