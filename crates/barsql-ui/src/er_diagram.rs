mod layout;
mod svg;
#[cfg(test)]
mod tests;
mod zoom;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use barsql_app::{DiagramRef, EditorTab};
use barsql_core::{ColumnInfo, ConnectionConfig};
use barsql_sql::display_type;
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::scroll::ScrollbarAxis;
use gpui_kit::component::{ActiveTheme, Disableable, Icon, IconName, Selectable, Sizable, StyledExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use self::layout::{BOX_W, ErTable, HEADER_H, Layout, ROW_H, Spot, arrange, find, key_count};
use crate::assets::AppAssets;
use crate::form::{self, ToolButton as _};
use crate::i18n::{I18n, compact, t, t_count, t_with};
use crate::schema;
use crate::scrollbars::HoverScrollbar as _;
use crate::tokens::{ICON_XS, RADIUS, STRIP, TEXT_SM, TEXT_XS, TINT};
use crate::{file_dialogs, theme, toast};

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

pub(crate) type OnOpen = Rc<dyn Fn(String, &mut Window, &mut App)>;

// A drag on the diagram pans it. GPUI draws a dragged value, so this one draws nothing.
struct Pan;

impl Render for Pan {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

// The tables of a schema and the keys between them, in a tab of their own. Drag to pan, ⌘ or Ctrl with the wheel, a
// pinch or the buttons to zoom. Hovering a table previews its keys and a click keeps them lit; a double click opens
// it.
pub struct ErDiagram {
    pub(crate) id: String,
    pub(crate) connection: ConnectionConfig,
    pub(crate) schema: String,
    focus: FocusHandle,
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
    // So only the row estimates' arrival redraws it, not every schema change.
    sized: bool,
    // Empty until the next frame builds it again, as the tables or the language changed.
    placeholder_shown: SharedString,
    on_open: OnOpen,
    _load: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl ErDiagram {
    pub(crate) fn new(
        id: String,
        connection: ConnectionConfig,
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
            cx.observe_global_in::<I18n>(window, |this, _, cx| {
                this.placeholder_shown = SharedString::default();
                cx.notify();
            }),
            cx.observe_global::<schema::Schemas>(|this, cx| {
                // Estimates come back once a reload's schemas do, so a disconnected connection doesn't reconnect.
                if schema::get(cx, &this.connection.id).is_some_and(|entry| entry.schemas().is_some()) {
                    schema::load_sizes(&this.connection.id, &this.schema, cx);
                }
                let sized = schema::get(cx, &this.connection.id).is_some_and(|entry| entry.sizes_loaded(&this.schema));
                if sized != this.sized {
                    this.sized = sized;
                    cx.notify();
                }
            }),
        ];
        Self {
            id,
            connection,
            schema,
            focus: cx.focus_handle(),
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
            sized: false,
            placeholder_shown: SharedString::default(),
            on_open,
            _load: Task::ready(()),
            _subscriptions: subscriptions,
        }
    }

    pub(crate) fn load(&mut self, cx: &mut Context<Self>) {
        schema::load_sizes(&self.connection.id, &self.schema, cx);
        let load = schema::load_schema_columns(&self.connection.id, &self.schema, MAX_TABLES, cx);
        self._load = cx.spawn(async move |this, cx| {
            let loaded = load.await;
            let _ = this.update(cx, |this, cx| {
                match loaded {
                    Err(error) => this.state = State::Failed(error),
                    Ok((_, tables)) if tables.is_empty() => this.state = State::Empty,
                    Ok((total, tables)) => {
                        let (id, schema) = (&this.connection.id, &this.schema);
                        let views = tables
                            .iter()
                            .filter(|(name, _)| schema::is_view(id, schema, name, cx))
                            .map(|(name, _)| name.clone())
                            .collect();
                        this.set_tables_with_views(total, tables, views);
                    }
                }
                cx.notify();
            });
        });
    }

    #[cfg(test)]
    fn set_tables(&mut self, total: usize, tables: Vec<(String, Vec<ColumnInfo>)>) {
        self.set_tables_with_views(total, tables, HashSet::new());
    }

    fn set_tables_with_views(&mut self, total: usize, tables: Vec<(String, Vec<ColumnInfo>)>, views: HashSet<String>) {
        let tables: Vec<ErTable> = tables
            .into_iter()
            .map(|(name, columns)| {
                let view = views.contains(&name);
                ErTable { view, ..ErTable::new(name, columns) }
            })
            .collect();
        self.total = total;
        self.placeholder_shown = SharedString::default();
        self.state = State::Ready(Rc::new(tables));
        self.fitted = false;
        self.relayout();
    }

    pub(crate) fn title(&self, cx: &App) -> SharedString {
        t_with(cx, "erDiagram.title", &[("schema", &self.schema)])
    }

    pub(crate) fn stored(&self, cx: &App) -> EditorTab {
        EditorTab {
            id: self.id.clone(),
            connection_id: self.connection.id.clone(),
            title: self.title(cx).to_string(),
            color: self.connection.color.clone(),
            diagram: Some(DiagramRef { schema: self.schema.clone() }),
            ..Default::default()
        }
    }

    // A renamed connection or new settings. The tables stay as drawn.
    pub(crate) fn set_connection(&mut self, connection: ConnectionConfig, cx: &mut Context<Self>) {
        self.connection = connection;
        cx.notify();
    }

    pub(crate) fn focus(&self, window: &mut Window, cx: &mut App) {
        window.focus(&self.focus, cx);
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

    // After this update ends: opening the table reads every tab, this one included.
    fn open(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let State::Ready(tables) = &self.state else { return };
        let Some(table) = tables.get(ix) else { return };
        let (name, on_open) = (table.name.clone(), self.on_open.clone());
        window.defer(cx, move |window, cx| on_open(name, window, cx));
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
        // Only a cut diagram has something to say along the toolbar. How to get around waits behind the "?".
        let note = (self.total > MAX_TABLES).then(|| {
            let (shown, total) = (MAX_TABLES.to_string(), self.total.to_string());
            t_with(cx, "erDiagram.truncated", &[("shown", &shown), ("total", &total)])
        });
        let modifier = if cfg!(target_os = "macos") { "⌘" } else { "Ctrl" };
        let hint = t_with(cx, "erDiagram.hint", &[("modifier", modifier)]);
        // The hit count sits inside the field, so typing moves nothing.
        let search = form::search_input(&self.search, window, cx).when_some(found, |input, found| {
            input.suffix(
                div()
                    .debug_selector(|| "er-matches".into())
                    .text_size(TEXT_XS)
                    .text_color(theme.muted_foreground)
                    .child(found),
            )
        });
        h_flex()
            .debug_selector(|| "er-toolbar".into())
            .flex_none()
            .h(STRIP)
            .gap(rems(0.615))
            .px(rems(0.769))
            .bg(theme.sidebar)
            .border_b_1()
            .border_color(theme.border)
            // Fills the bar, and shrinks alongside the note when the bar runs short.
            .child(
                div()
                    .flex_grow_1()
                    .flex_basis(rems(15.385))
                    .min_w_0()
                    .debug_selector(|| "er-search".into())
                    .child(search),
            )
            .children(note.map(|note| {
                div()
                    .debug_selector(|| "er-note".into())
                    .min_w_0()
                    .truncate()
                    .text_size(TEXT_XS)
                    .text_color(theme.muted_foreground)
                    .child(note)
            }))
            .child(
                form::filter_button("er-keys-only", Icon::new(Lucide::KeyRound))
                    .debug_selector(|| "er-keys-only".into())
                    .selected(self.keys_only)
                    .disabled(!ready)
                    .tooltip(t(cx, "erDiagram.keysOnlyTooltip"))
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_keys(window, cx))),
            )
            .child(self.zoom_buttons(ready, cx))
            .child(
                form::filter_button("er-fit", Icon::new(Lucide::Scan))
                    .debug_selector(|| "er-fit".into())
                    .disabled(!ready)
                    .tooltip(t(cx, "erDiagram.fitTooltip"))
                    .on_click(cx.listener(|this, _, window, cx| this.fit(window, cx))),
            )
            .child(
                form::filter_button("er-export", Icon::new(Lucide::Download))
                    .debug_selector(|| "er-export".into())
                    .disabled(!ready)
                    .tooltip(t(cx, "erDiagram.exportSvg"))
                    .on_click(cx.listener(|this, _, _, cx| this.export(cx))),
            )
            .child(
                form::filter_button("er-help", Icon::new(Lucide::CircleQuestionMark))
                    .debug_selector(|| "er-help".into())
                    .ghost()
                    .tooltip(hint),
            )
    }

    fn rows_estimate(&self, table: &str, cx: &App) -> Option<String> {
        let rows = schema::get(cx, &self.connection.id)?.size(&self.schema, table)?.rows?;
        Some(compact(cx, rows))
    }

    // As shown, keys-only and theme colours included.
    fn export(&mut self, cx: &mut Context<Self>) {
        let State::Ready(tables) = &self.state else { return };
        let theme = cx.theme();
        let palette = svg::Palette {
            background: theme::opaque(theme.background),
            header: theme.secondary,
            border: theme.border,
            text: theme.foreground,
            muted: theme.muted_foreground,
            key: theme.warning,
            link: theme.primary,
            edge: theme.muted_foreground.opacity(0.6),
        };
        let icon = |name: Lucide| {
            AppAssets.load(&name.path()).ok().flatten().map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        };
        let (key, link) = (icon(Lucide::KeyRound).unwrap_or_default(), icon(Lucide::Link).unwrap_or_default());
        let text_system = cx.text_system().clone();
        let regular = text_system.resolve_font(&font(theme.font_family.clone()));
        let bold = text_system.resolve_font(&Font { weight: FontWeight::SEMIBOLD, ..font(theme.font_family.clone()) });
        // Shaped with fallback fonts, as the canvas is, so a character Inter lacks still has a width.
        let widths = RefCell::new(HashMap::new());
        let measure = |text: &str, size: f32, heavy: bool| {
            let id = if heavy { bold } else { regular };
            let mut widths = widths.borrow_mut();
            let width = |ch| f32::from(text_system.layout_width(id, px(size), ch));
            text.chars().map(|ch| *widths.entry((ch, heavy, size.to_bits())).or_insert_with(|| width(ch))).sum()
        };
        let look = svg::Look { palette, icons: [&key, &link], measure: &measure };
        let rows = |table: &str| self.rows_estimate(table, cx);
        let text = svg::render(tables, &self.layout, self.keys_only, self.connection.driver.dialect(), &look, rows);
        let picked = file_dialogs::pick_save_path(&format!("{}.svg", self.schema), cx);
        cx.spawn(async move |_, cx| {
            let Some(path) = picked.await else { return };
            let file_name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
            let written = std::fs::write(&path, text);
            cx.update(|cx| match written {
                Ok(()) => toast::success(t_with(cx, "toast.savedFile", &[("fileName", &file_name)]), cx),
                Err(error) => toast::error(format!("{}: {error}", t(cx, "errors.exportFailed")), cx),
            });
        })
        .detach();
    }

    // The counts ride in the placeholder, so they never crowd what's typed.
    fn placeholder(&self, cx: &App) -> SharedString {
        let find = t(cx, "erDiagram.search");
        let State::Ready(tables) = &self.state else { return find };
        let keys = key_count(tables);
        let tables = t_count(cx, "erDiagram.tables", tables.len() as i64, &[("count", &tables.len().to_string())]);
        let keys = t_count(cx, "erDiagram.foreignKeys", keys as i64, &[("count", &keys.to_string())]);
        format!("{find} ({tables} · {keys})").into()
    }

    // Joined as Explain and its menu are. The zoom keeps one width, so nothing shifts as it changes.
    fn zoom_buttons(&self, ready: bool, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let percent = format!("{}%", (self.zoom * 100.).round());
        h_flex()
            .child(
                form::filter_button("er-zoom-out", Icon::new(IconName::Minus))
                    .debug_selector(|| "er-zoom-out".into())
                    .rounded_tr(px(0.))
                    .rounded_br(px(0.))
                    .disabled(!ready || self.zoom <= zoom::MIN)
                    .tooltip(t(cx, "erDiagram.zoomOut"))
                    .on_click(
                        cx.listener(|this, _, window, cx| this.zoom_to(zoom::step(this.zoom, false), None, window, cx)),
                    ),
            )
            .child(
                Button::new("er-zoom-reset")
                    .debug_selector(|| "er-zoom-reset".into())
                    .small()
                    .h(rems(form::FIELD_REM))
                    .w(rems(3.692))
                    .p_0()
                    .rounded(px(0.))
                    .border_l_0()
                    .child(div().text_size(TEXT_XS).child(percent))
                    .disabled(!ready)
                    .tooltip(t(cx, "erDiagram.zoomReset"))
                    .on_click(cx.listener(|this, _, window, cx| this.zoom_to(1., None, window, cx))),
            )
            .child(
                form::filter_button("er-zoom-in", Icon::new(IconName::Plus))
                    .debug_selector(|| "er-zoom-in".into())
                    .rounded_tl(px(0.))
                    .rounded_bl(px(0.))
                    .border_l_0()
                    .disabled(!ready || self.zoom >= zoom::MAX)
                    .tooltip(t(cx, "erDiagram.zoomIn"))
                    .on_click(
                        cx.listener(|this, _, window, cx| this.zoom_to(zoom::step(this.zoom, true), None, window, cx)),
                    ),
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
                        .child(display_type(self.connection.driver.dialect(), &column.data_type)),
                )
        });
        let border = match () {
            _ if focused => theme.primary,
            _ if marked => theme.primary.opacity(0.5),
            _ => theme.border,
        };
        let rows_estimate = self.rows_estimate(&table.name, cx);
        let header = h_flex()
            .h(rems(HEADER_H))
            .flex_none()
            .px(rems(0.615))
            .gap(rems(0.462))
            .bg(if focused { theme.primary.opacity(TINT) } else { theme.secondary })
            .border_b_1()
            .border_color(theme.border)
            .child(match table.view {
                true => Icon::new(Lucide::View).size(ICON_XS).text_color(theme.primary),
                false => Icon::new(Lucide::Table2).size(ICON_XS).text_color(theme.muted_foreground),
            })
            .child(div().flex_1().min_w_0().truncate().font_semibold().child(table.name.clone()))
            .children(rows_estimate.map(|rows| {
                div()
                    .debug_selector(move || format!("er-rows-{ix}"))
                    .flex_none()
                    .text_size(TEXT_XS)
                    .text_color(theme.muted_foreground)
                    .child(rows)
            }))
            .when(focused, |el| {
                el.child(
                    Button::new(("er-open", ix))
                        .debug_selector(move || format!("er-open-{ix}"))
                        .ghost()
                        .tool_icon(Icon::new(IconName::ExternalLink), ICON_XS)
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
            .when(table.view, |el| el.border_dashed())
            .bg(theme::opaque(theme.background))
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
            .test_support()
            .into_any_element()
    }
}

// Doubling the spacing keeps zoomed-out dots from crowding.
fn paint_dots(bounds: Bounds<Pixels>, offset: Point<Pixels>, zoom: f32, color: Hsla, window: &mut Window) {
    let mut step = 20. * zoom;
    while step < 12. {
        step *= 2.;
    }
    let side = px(1.5);
    let phase = |offset: Pixels| f32::from(offset).rem_euclid(step);
    let (first_x, first_y) = (phase(offset.x), phase(offset.y));
    let (width, height) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
    let mut y = first_y;
    while y < height {
        let mut x = first_x;
        while x < width {
            let at = point(bounds.left() + px(x), bounds.top() + px(y));
            window.paint_quad(fill(Bounds::new(at, size(side, side)), color));
            x += step;
        }
        y += step;
    }
}

// A crow's foot marks the referencing column, the many end, and a bar the referenced one. `only` draws just those
// edges, thicker and with arrowheads where they arrive.
fn paint_edges(layout: &Layout, only: Option<&[usize]>, bounds: Bounds<Pixels>, color: Hsla, window: &mut Window) {
    let rem = window.rem_size();
    let at = |(x, y): Spot| point(bounds.left() + rem * x, bounds.top() + rem * y);
    let width = if only.is_some() { px(2.) } else { px(1.5) };
    for (ix, edge) in layout.edges.iter().enumerate() {
        if only.is_some_and(|only| !only.contains(&ix)) {
            continue;
        }
        let path = layout.path(edge);
        let mut line = PathBuilder::stroke(width);
        line.move_to(at(path.from));
        line.cubic_bezier_to(at(path.to), at(path.controls.0), at(path.controls.1));
        if let Ok(line) = line.build() {
            window.paint_path(line, color);
        }
        let mut crow = PathBuilder::stroke(width);
        for (start, end) in path.prongs {
            crow.move_to(at(start));
            crow.line_to(at(end));
        }
        if let Ok(crow) = crow.build() {
            window.paint_path(crow, color);
        }
        let end = match only {
            Some(_) => {
                let mut head = PathBuilder::fill();
                head.move_to(at(path.head[0]));
                head.line_to(at(path.head[1]));
                head.line_to(at(path.head[2]));
                head.close();
                head.build()
            }
            None => {
                let mut bar = PathBuilder::stroke(width);
                bar.move_to(at(path.bar.0));
                bar.line_to(at(path.bar.1));
                bar.build()
            }
        };
        if let Ok(end) = end {
            window.paint_path(end, color);
        }
    }
}

// README screenshots.
#[cfg(feature = "snapshot")]
impl ErDiagram {
    pub(crate) fn fitted(&self) -> bool {
        self.fitted
    }

    // Lights up a table's keys, as a click on it does.
    pub(crate) fn select_table(&mut self, name: &str, cx: &mut Context<Self>) {
        if let State::Ready(tables) = &self.state {
            self.selected = tables.iter().position(|table| table.name == name);
            cx.notify();
        }
    }

    // Its box registers as `("er-table", ix)`.
    pub(crate) fn table_index(&self, name: &str) -> Option<usize> {
        match &self.state {
            State::Ready(tables) => tables.iter().position(|table| table.name == name),
            _ => None,
        }
    }

    // As the zoom buttons do, past Fit's 100% cap.
    pub(crate) fn set_zoom(&mut self, zoom: f32, window: &Window, cx: &mut Context<Self>) {
        self.zoom_to(zoom, None, window, cx);
    }
}

impl Render for ErDiagram {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        if self.placeholder_shown.is_empty() {
            self.placeholder_shown = self.placeholder(cx);
            let placeholder = self.placeholder_shown.clone();
            self.search.update(cx, |search, cx| search.set_placeholder(placeholder, window, cx));
        }
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
            State::Failed(error) => form::error(error.clone(), cx).m_auto().max_w(rems(40.)).into_any_element(),
            State::Empty => form::empty_state(None, t(cx, "erDiagram.empty"), cx).m_auto().into_any_element(),
            _ => form::loading_state(t(cx, "erDiagram.loading"), cx).m_auto().into_any_element(),
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
                window.on_mouse_event(move |event: &ScrollEvent, phase, window, cx| {
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
        let (scroll, zoom, dot) = (self.scroll.clone(), self.zoom, theme.border);
        let dots =
            canvas(|_, _, _| {}, move |bounds, _, window, _| paint_dots(bounds, scroll.offset(), zoom, dot, window))
                .absolute()
                .inset_0();
        v_flex().size_full().track_focus(&self.focus).bg(theme.background).child(self.toolbar(window, cx)).child(
            div()
                .relative()
                .flex_1()
                .min_h_0()
                .child(dots)
                .child(viewport)
                .child(wheel)
                .hover_scrollbar(&self.scroll, ScrollbarAxis::Both),
        )
    }
}
