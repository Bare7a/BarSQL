use std::ops::Range;
use std::rc::Rc;

use anyhow::Result;
use barsql_core::{ColumnInfo, ObjectKind, TableInfo};
use barsql_sql::lang::labels::fill;
use barsql_sql::lang::suggestions::relation_type_label;
use barsql_sql::lang::{HoverSubject, SqlLabels, TableBinding};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::input::{EditorState, HoverProvider, InputEvent};
use gpui_kit::component::scroll::{Scrollbar, ScrollbarMode};
use gpui_kit::component::{ActiveTheme, Icon, Rope, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::i18n::{I18n, t_count, t_with};
use crate::sql_language::SqlLanguage;
use crate::tokens::{RADIUS, TEXT_2XS};
use crate::{completion, form, schema, theme};

// GPUI Kit's hover popover sizes itself to its narrowest content, caps its size, has no scrollbar and can't be
// restyled, so the SQL editor draws its own card. GPUI Kit still notices the pointer resting on a name and asks the
// provider, which hands the request to the card and tells GPUI Kit there's nothing to show.

// Kept clear of the window's edges.
const MARGIN: Pixels = px(8.);
// Rows shown before the list scrolls, and the fewest kept when the window is short.
const MAX_ROWS: usize = 16;
const MIN_ROWS: usize = 3;
const MAX_WIDTH: Pixels = px(760.);
// Widest list columns in characters, so one long name or default can't stretch the card.
const NAME_CAP: usize = 32;
const TYPE_CAP: usize = 32;
const NOTE_CAP: usize = 48;
const LIST_PAD: Pixels = px(4.);
// Added on the right of a list that scrolls, so its longest line stays clear of the bar.
const BAR_ROOM: Pixels = px(8.);

pub struct HoverRequests(WeakEntity<HoverCard>);

impl HoverProvider for HoverRequests {
    fn hover(
        &self,
        text: &Rope,
        offset: usize,
        window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<Option<lsp_types::Hover>>> {
        let pointer = window.mouse_position();
        let _ = self.0.update(cx, |card, cx| card.request(&text.to_string(), offset, Some(pointer), cx));
        Task::ready(Ok(None))
    }
}

pub struct HoverCard {
    editor: Entity<EditorState>,
    language: Rc<SqlLanguage>,
    card: Option<Card>,
    // Where the card was last drawn.
    drawn: Option<Bounds<Pixels>>,
    // The name being looked up, so resting on it again doesn't start over.
    pending: Option<Range<usize>>,
    scroll: UniformListScrollHandle,
    request: Task<()>,
    // Shown by the snapshot scene, with no pointer over the name.
    pinned: bool,
    _subscriptions: Vec<Subscription>,
}

struct Card {
    // Byte range of the name in the editor's text.
    span: Range<usize>,
    header: Header,
    rows: Vec<Row>,
}

struct Header {
    icon: Lucide,
    // Views take the accent colour, as in the Schema tree.
    accent: bool,
    name: SharedString,
    detail: SharedString,
    count: Option<SharedString>,
}

// A line of the list: a column with its type, keys and notes, or just a name for a CTE's columns.
#[derive(Default)]
struct Row {
    name: Option<SharedString>,
    data_type: Option<SharedString>,
    pk: bool,
    fk: bool,
    note: Option<SharedString>,
}

impl HoverCard {
    pub fn new(editor: Entity<EditorState>, language: Rc<SqlLanguage>, cx: &mut Context<Self>) -> Self {
        let subscriptions = vec![cx.subscribe(&editor, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.hide(cx);
            }
        })];
        Self {
            editor,
            language,
            card: None,
            drawn: None,
            pending: None,
            scroll: UniformListScrollHandle::new(),
            request: Task::ready(()),
            pinned: false,
            _subscriptions: subscriptions,
        }
    }

    pub fn provider(this: &Entity<Self>) -> Rc<dyn HoverProvider> {
        Rc::new(HoverRequests(this.downgrade()))
    }

    // Called while GPUI Kit updates the editor, so it's handed the text rather than reading the editor.
    fn request(&mut self, text: &str, offset: usize, pointer: Option<Point<Pixels>>, cx: &mut Context<Self>) {
        // GPUI Kit asks once the pointer has rested on the text, but it may have moved onto the card since.
        if self.card.is_some() && pointer.zip(self.drawn).is_some_and(|(pointer, card)| card.contains(&pointer)) {
            return;
        }
        let Some((span, subject)) = self.language.hover_subject(text, offset, cx) else {
            return self.hide(cx);
        };
        if self.card.as_ref().is_some_and(|card| card.span == span) || self.pending.as_ref() == Some(&span) {
            return;
        }
        self.pending = Some(span.clone());
        let labels = cx.global::<I18n>().sql_labels();
        let language = self.language.clone();
        self.request = match subject {
            HoverSubject::Table { table, alias } => {
                let load = language.columns(&table.schema, &table.name, cx);
                cx.spawn(async move |this, cx| {
                    let columns = load.await.unwrap_or_default();
                    let _ = this.update(cx, |this, cx| {
                        let card = table_card(span.clone(), &table, alias.as_deref(), &columns, &labels, cx);
                        this.settle(&span, Some(card), cx);
                    });
                })
            }
            // One table at a time, stopping at the first that has the column.
            HoverSubject::Column(lookup) => cx.spawn(async move |this, cx| {
                let mut found = None;
                for binding in lookup.bindings {
                    let load = cx.update(|cx| language.columns(&binding.schema, &binding.table, cx));
                    let columns = load.await.unwrap_or_default();
                    if let Some(column) = columns.iter().find(|c| c.name.to_lowercase() == lookup.name) {
                        found = Some((column.clone(), binding));
                        break;
                    }
                }
                let _ = this.update(cx, |this, cx| {
                    let card = found.map(|(column, table)| column_card(span.clone(), &column, &table, &labels, cx));
                    this.settle(&span, card, cx);
                });
            }),
            subject => {
                let card = named_card(span.clone(), subject, &labels, &language, cx);
                self.settle(&span, Some(card), cx);
                Task::ready(())
            }
        };
    }

    // A lookup's result. A newer request or a hide since then wins.
    fn settle(&mut self, span: &Range<usize>, card: Option<Card>, cx: &mut Context<Self>) {
        if self.pending.as_ref() != Some(span) {
            return;
        }
        self.pending = None;
        self.card = card;
        self.scroll = UniformListScrollHandle::new();
        cx.notify();
    }

    pub fn hide(&mut self, cx: &mut Context<Self>) {
        self.pending = None;
        self.pinned = false;
        self.drawn = None;
        self.request = Task::ready(());
        if self.card.take().is_some() {
            cx.notify();
        }
    }

    // For BARSQL_SNAPSHOT_PANEL=hover=<name>. Shows the card for the first `name` that is a whole word, as resting
    // the pointer there would.
    pub fn preview(&mut self, name: &str, cx: &mut Context<Self>) {
        let text = self.editor.read(cx).value().to_string();
        let word = |c: char| c.is_alphanumeric() || c == '_';
        let whole =
            |&(ix, _): &(usize, &str)| !text[..ix].ends_with(word) && !text[ix + name.len()..].starts_with(word);
        if let Some((offset, _)) = text.match_indices(name).find(whole) {
            self.request(&text, offset, None, cx);
            self.pinned = true;
        }
    }

    // Where the card goes this frame. None while its name is scrolled out of view.
    fn layout(&self, window: &Window, cx: &App) -> Option<(Layout, Bounds<Pixels>)> {
        let card = self.card.as_ref()?;
        let trigger = self.editor.read(cx).range_to_bounds(&card.span)?;
        let labels = cx.global::<I18n>().sql_labels();
        let layout = layout(card, Metrics::new(window, cx), [&labels.pk, &labels.fk], trigger, window.viewport_size());
        Some((layout, trigger))
    }

    fn row(&self, ix: usize, layout: &Layout, style: &RowStyle) -> AnyElement {
        let Some(row) = self.card.as_ref().and_then(|card| card.rows.get(ix)) else { return Empty.into_any_element() };
        let m = &layout.metrics;
        let cell = |width: Pixels, text: &Option<SharedString>, color: Hsla| {
            div().w(width).min_w_0().truncate().text_color(color).children(text.clone())
        };
        h_flex()
            .id(ix)
            .debug_selector(move || format!("hover-row-{ix}"))
            .h(m.row)
            .pl(m.pad)
            .pr(m.pad + layout.bar_room)
            .gap(m.gap)
            .children(layout.name.map(|width| cell(width, &row.name, style.text)))
            .children(layout.data_type.map(|width| cell(width, &row.data_type, style.muted)))
            .when(layout.notes, |el| {
                el.child(
                    h_flex()
                        .flex_1()
                        .min_w_0()
                        .gap(m.badge_gap)
                        .when(row.pk, |el| el.child(form::badge(style.pk.clone(), style.primary)))
                        .when(row.fk, |el| el.child(form::badge(style.fk.clone(), style.warning)))
                        .children(row.note.clone().map(|note| {
                            div().min_w_0().truncate().text_size(m.small).text_color(style.muted).child(note)
                        })),
                )
            })
            .into_any_element()
    }
}

#[cfg(test)]
impl HoverCard {
    // The header's name and detail, then each row's parts, all joined by spaces.
    pub(crate) fn lines(&self) -> Vec<String> {
        let Some(card) = &self.card else { return Vec::new() };
        let mut lines = vec![format!("{} {}", card.header.name, card.header.detail)];
        lines.extend(card.rows.iter().map(|row| {
            let mut parts: Vec<String> =
                [&row.name, &row.data_type].into_iter().flatten().map(|s| s.to_string()).collect();
            parts.extend(row.pk.then(|| "PK".to_string()));
            parts.extend(row.fk.then(|| "FK".to_string()));
            parts.extend(row.note.iter().map(|s| s.to_string()));
            parts.join(" ")
        }));
        lines
    }
}

fn joined(parts: impl IntoIterator<Item = String>) -> String {
    parts.into_iter().filter(|part| !part.is_empty()).collect::<Vec<_>>().join(" · ")
}

fn column_row(column: &ColumnInfo, name: bool, labels: &SqlLabels, cx: &App) -> Row {
    let target = match (column.foreign_table.as_str(), column.foreign_column.as_str()) {
        ("", _) => String::new(),
        (table, "") => format!("→ {table}"),
        (table, column) => format!("→ {table}.{column}"),
    };
    // A primary key is never null, so saying so is noise.
    let not_null = if column.is_nullable || column.is_primary { String::new() } else { labels.not_null.clone() };
    let default = match column.default_val.as_str() {
        "" => String::new(),
        value => t_with(cx, "editor.sql.defaultValue", &[("value", value)]).to_string(),
    };
    let note = joined([target, not_null, default]);
    Row {
        name: name.then(|| column.name.clone().into()),
        data_type: (!column.data_type.is_empty()).then(|| column.data_type.clone().into()),
        pk: column.is_primary,
        fk: column.is_foreign,
        note: (!note.is_empty()).then(|| note.into()),
    }
}

fn columns_count(count: usize, cx: &App) -> Option<SharedString> {
    (count > 0).then(|| t_count(cx, "editor.sql.columnsCount", count as i64, &[]))
}

fn table_card(
    span: Range<usize>,
    table: &TableInfo,
    alias: Option<&str>,
    columns: &[ColumnInfo],
    labels: &SqlLabels,
    cx: &App,
) -> Card {
    let kind = relation_type_label(&table.kind, labels);
    let detail = match alias {
        Some(_) => joined([fill(&labels.alias_for, &[("table", &table.name)]), kind, table.schema.clone()]),
        None => joined([kind, table.schema.clone()]),
    };
    let view = ObjectKind::for_relation(&table.kind) != ObjectKind::Table;
    Card {
        span,
        header: Header {
            icon: if view { Lucide::View } else { Lucide::Table2 },
            accent: view,
            name: alias.unwrap_or(&table.name).to_string().into(),
            detail: detail.into(),
            count: columns_count(columns.len(), cx),
        },
        rows: columns.iter().map(|column| column_row(column, true, labels, cx)).collect(),
    }
}

fn column_card(span: Range<usize>, column: &ColumnInfo, table: &TableBinding, labels: &SqlLabels, cx: &App) -> Card {
    let target =
        if table.schema.is_empty() { table.table.clone() } else { format!("{}.{}", table.schema, table.table) };
    Card {
        span,
        header: Header {
            icon: Lucide::Columns3,
            accent: false,
            name: column.name.clone().into(),
            detail: fill(&labels.column_of, &[("target", &target)]).into(),
            count: None,
        },
        rows: vec![column_row(column, false, labels, cx)],
    }
}

// Names described without loading anything.
fn named_card(span: Range<usize>, subject: HoverSubject, labels: &SqlLabels, language: &SqlLanguage, cx: &App) -> Card {
    let header = |icon: Lucide, name: String, detail: String, count: Option<SharedString>| Header {
        icon,
        accent: false,
        name: name.into(),
        detail: detail.into(),
        count,
    };
    let derived = |cte: bool| if cte { labels.cte.clone() } else { labels.subquery.clone() };
    let (header, rows) = match subject {
        HoverSubject::Derived { name, cte, columns } => {
            let count = columns_count(columns.len(), cx);
            let rows = columns.into_iter().map(|c| Row { name: Some(c.into()), ..Default::default() }).collect();
            (header(Lucide::Table2, name, derived(cte), count), rows)
        }
        HoverSubject::DerivedColumn { name, source, cte } => {
            let target = format!("{} {source}", derived(cte));
            (header(Lucide::Columns3, name, fill(&labels.column_of, &[("target", &target)]), None), Vec::new())
        }
        HoverSubject::Schema { name } => {
            let tables =
                schema::get(cx, language.connection_id()).and_then(|entry| entry.tables(&name)).map(<[_]>::len);
            let count = tables.map(|n| t_count(cx, "editor.sql.tablesCount", n as i64, &[]));
            (header(Lucide::FolderOpen, name, labels.schema.clone(), count), Vec::new())
        }
        HoverSubject::Alias { name, table } => {
            (header(Lucide::Table2, name, fill(&labels.alias_for, &[("table", &table)]), None), Vec::new())
        }
        HoverSubject::Table { .. } | HoverSubject::Column(_) => unreachable!("tables and columns load first"),
    };
    Card { span, header, rows }
}

#[derive(Clone, Copy)]
struct Metrics {
    size: Pixels,
    // Header details and row notes.
    small: Pixels,
    row: Pixels,
    header: Pixels,
    icon: Pixels,
    pad: Pixels,
    // Between the list's columns, the header's parts, and a row's badges and note.
    gap: Pixels,
    title_gap: Pixels,
    badge_gap: Pixels,
    // Advances of the editor's monospace font at `size`, at `small` and at a badge's size.
    ch: Pixels,
    small_ch: Pixels,
    badge_ch: Pixels,
    badge_pad: Pixels,
}

impl Metrics {
    // From the editor's font size, like the suggestion list. Badges keep their UI size.
    fn new(window: &Window, cx: &App) -> Self {
        let size = px(theme::editor_font_size(cx));
        let small = size * 0.85;
        let text_system = window.text_system();
        let mono = text_system.resolve_font(&font(cx.theme().mono_font_family.clone()));
        let ch = |size: Pixels| text_system.ch_advance(mono, size).unwrap_or(size * 0.6);
        let rem = window.rem_size();
        Self {
            size,
            small,
            row: size + px(7.),
            header: size + px(17.),
            icon: size + px(1.),
            pad: size * 0.77,
            gap: ch(size) * 2.,
            title_gap: px(6.),
            badge_gap: px(4.),
            ch: ch(size),
            small_ch: ch(small),
            badge_ch: ch(TEXT_2XS.to_pixels(rem)),
            badge_pad: rems(0.385).to_pixels(rem),
        }
    }
}

// Where the card goes and how wide its list columns are. A column no row fills is None.
#[derive(Clone, Copy)]
struct Layout {
    metrics: Metrics,
    bounds: Bounds<Pixels>,
    name: Option<Pixels>,
    data_type: Option<Pixels>,
    notes: bool,
    // Rows shown without scrolling.
    shown: usize,
    bar_room: Pixels,
}

fn chars(text: &Option<SharedString>) -> usize {
    text.as_ref().map_or(0, |text| text.chars().count())
}

fn layout(card: &Card, m: Metrics, badges: [&str; 2], trigger: Bounds<Pixels>, viewport: Size<Pixels>) -> Layout {
    let widest = |cell: fn(&Row) -> usize, cap: usize| card.rows.iter().map(cell).max().unwrap_or(0).min(cap);
    let name = m.ch * widest(|row| chars(&row.name), NAME_CAP) as f32;
    let data_type = m.ch * widest(|row| chars(&row.data_type), TYPE_CAP) as f32;
    let badge = |label: &str| m.badge_pad * 2. + m.badge_ch * label.chars().count() as f32 + m.badge_gap;
    let notes = card
        .rows
        .iter()
        .map(|row| {
            let keys = [row.pk, row.fk].into_iter().zip(badges).filter(|(on, _)| *on).map(|(_, label)| badge(label));
            keys.sum::<Pixels>() + m.small_ch * chars(&row.note).min(NOTE_CAP) as f32
        })
        .fold(px(0.), |widest, width| if width > widest { width } else { widest });
    let cells: Vec<Pixels> = [name, data_type, notes].into_iter().filter(|width| *width > px(0.)).collect();
    let list = cells.iter().sum::<Pixels>() + m.gap * cells.len().saturating_sub(1) as f32;
    let header = &card.header;
    let count =
        header.count.as_ref().map_or(px(0.), |count| m.title_gap + m.gap + m.small_ch * count.chars().count() as f32);
    // Spare pixels, so rounding can't make the detail truncate.
    let title = m.icon
        + m.title_gap * 2.
        + m.ch * header.name.chars().count() as f32
        + m.small_ch * header.detail.chars().count() as f32
        + count
        + px(2.);
    let max_width = MAX_WIDTH.min(viewport.width - MARGIN * 2.);
    // The border's 2px on top of the content.
    let width =
        |room: Pixels| (m.pad * 2. + room + if list > title { list } else { title } + px(2.)).ceil().min(max_width);
    let rows = card.rows.len();
    let chrome = m.header + px(2.) + if rows > 0 { px(1.) + LIST_PAD * 2. } else { px(0.) };
    let shown = place(trigger, viewport, width(BAR_ROOM), chrome, m.row, rows).1;
    let bar_room = if shown < rows { BAR_ROOM } else { px(0.) };
    let (origin, shown) = place(trigger, viewport, width(bar_room), chrome, m.row, rows);
    Layout {
        metrics: m,
        bounds: Bounds::new(origin, size(width(bar_room), chrome + m.row * shown as f32)),
        name: (name > px(0.)).then_some(name),
        data_type: (data_type > px(0.)).then_some(data_type),
        notes: notes > px(0.),
        shown,
        bar_room,
    }
}

// Below the name when the card fits there, else above it, else on the roomier side with fewer rows.
fn place(
    trigger: Bounds<Pixels>,
    viewport: Size<Pixels>,
    width: Pixels,
    chrome: Pixels,
    row: Pixels,
    rows: usize,
) -> (Point<Pixels>, usize) {
    let wanted = rows.min(MAX_ROWS);
    let height = |n: usize| chrome + row * n as f32;
    let below = viewport.height - MARGIN - trigger.bottom();
    let above = trigger.top() - MARGIN;
    let fit = |space: Pixels| (((space - chrome) / row).floor().max(0.) as usize).clamp(MIN_ROWS.min(wanted), wanted);
    let (down, shown) = if height(wanted) <= below {
        (true, wanted)
    } else if height(wanted) <= above {
        (false, wanted)
    } else if below >= above {
        (true, fit(below))
    } else {
        (false, fit(above))
    };
    // Flush with the name's line, so the pointer never crosses other text on its way to the card.
    let y = if down { trigger.bottom() } else { trigger.top() - height(shown) };
    let x = trigger.left().min(viewport.width - MARGIN - width).max(MARGIN);
    (point(x, y), shown)
}

#[derive(Clone)]
struct RowStyle {
    text: Hsla,
    muted: Hsla,
    primary: Hsla,
    warning: Hsla,
    pk: SharedString,
    fk: SharedString,
}

impl Render for HoverCard {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let hidden = || div().absolute().into_any_element();
        self.drawn = None;
        let Some((layout, trigger)) = self.layout(window, cx) else { return hidden() };
        // Stays open while the pointer is on the name or the card, or passes between them along the name's line.
        let keep_open = trigger.union(&layout.bounds);
        if !self.pinned && !keep_open.contains(&window.mouse_position()) {
            // Ready after the pointer moved on, or drawn again after its tab was away.
            self.card = None;
            return hidden();
        }
        self.drawn = Some(layout.bounds);
        let Some(card) = &self.card else { return hidden() };
        let theme = cx.theme();
        let labels = cx.global::<I18n>().sql_labels();
        let (background, border) = completion::surface(cx);
        let m = layout.metrics;
        let rows = card.rows.len();
        let header = &card.header;
        let muted = theme.muted_foreground;
        let icon_color = if header.accent { theme.primary } else { muted };
        let title = h_flex()
            .flex_none()
            .h(m.header)
            .px(m.pad)
            .gap(m.title_gap)
            .when(rows > 0, |el| el.border_b_1().border_color(border))
            .child(Icon::new(header.icon).flex_none().size(m.icon).text_color(icon_color))
            .child(div().min_w_0().truncate().font_semibold().child(header.name.clone()))
            .child(
                div().flex_1().min_w_0().truncate().text_size(m.small).text_color(muted).child(header.detail.clone()),
            )
            .children(
                header
                    .count
                    .clone()
                    .map(|count| div().flex_none().pl(m.gap).text_size(m.small).text_color(muted).child(count)),
            );
        let style = RowStyle {
            text: theme.foreground,
            muted,
            primary: theme.primary,
            warning: theme.warning,
            pk: labels.pk.clone().into(),
            fk: labels.fk.clone().into(),
        };
        let list = (rows > 0).then(|| {
            let list = uniform_list(
                "hover-card-rows",
                rows,
                cx.processor(move |this, range: Range<usize>, _, _| {
                    range.map(|ix| this.row(ix, &layout, &style)).collect()
                }),
            )
            .track_scroll(&self.scroll)
            .py(LIST_PAD)
            .h(m.row * layout.shown as f32 + LIST_PAD * 2.);
            // The card is up only briefly, so unlike other lists its bar shows whenever there are more columns.
            let bar = Scrollbar::vertical(&self.scroll).viewport_from_layout().mode(ScrollbarMode::Always);
            div().relative().flex_none().child(list).child(div().absolute().inset_0().child(bar))
        });
        let dismiss = (!self.pinned).then(|| dismissal(cx.entity().downgrade(), keep_open, layout.bounds));
        deferred(
            anchored().position(layout.bounds.origin).child(
                v_flex()
                    .id("hover-card")
                    .debug_selector(|| "hover-card".into())
                    .occlude()
                    .relative()
                    .w(layout.bounds.size.width)
                    .h(layout.bounds.size.height)
                    .overflow_hidden()
                    .bg(background)
                    .border_1()
                    .border_color(border)
                    .rounded(RADIUS)
                    .shadow_md()
                    .font_family(theme.mono_font_family.clone())
                    .text_size(m.size)
                    .text_color(theme.foreground)
                    .child(title)
                    .children(list)
                    .children(dismiss),
            ),
        )
        .with_priority(1)
        .into_any_element()
    }
}

// Closes the card once the pointer leaves the name and the card, or a click or wheel lands outside the card.
fn dismissal(card: WeakEntity<HoverCard>, keep_open: Bounds<Pixels>, inside: Bounds<Pixels>) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |_, _, window, _| {
            let hide = |card: &WeakEntity<HoverCard>, cx: &mut App| {
                let _ = card.update(cx, |card, cx| card.hide(cx));
            };
            window.on_mouse_event({
                let card = card.clone();
                move |event: &MouseMoveEvent, phase, _, cx| {
                    if phase.capture() && !keep_open.contains(&event.position) {
                        hide(&card, cx);
                    }
                }
            });
            window.on_mouse_event({
                let card = card.clone();
                move |event: &MouseDownEvent, phase, _, cx| {
                    if phase.capture() && !inside.contains(&event.position) {
                        hide(&card, cx);
                    }
                }
            });
            window.on_mouse_event({
                let card = card.clone();
                move |event: &ScrollWheelEvent, phase, _, cx| {
                    if phase.capture() && !inside.contains(&event.position) {
                        hide(&card, cx);
                    }
                }
            });
            // The pointer can leave the window without a last move.
            window.on_mouse_event(move |_: &MouseExitEvent, phase, _, cx| {
                if phase.capture() {
                    hide(&card, cx);
                }
            });
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

#[cfg(test)]
mod tests {
    use gpui_kit::{Bounds, Pixels, Size, point, px, size};

    use super::{MARGIN, MAX_ROWS, MIN_ROWS, place};

    const WINDOW: Size<Pixels> = Size { width: px(1200.), height: px(800.) };

    fn trigger(x: f32, y: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(x), px(y)), size(px(40.), px(20.)))
    }

    #[test]
    fn the_card_goes_below_the_name_when_it_fits() {
        let (origin, shown) = place(trigger(100., 100.), WINDOW, px(400.), px(40.), px(20.), 40);
        assert_eq!(shown, MAX_ROWS);
        assert_eq!(origin, point(px(100.), px(120.)));
    }

    #[test]
    fn the_card_goes_above_when_only_that_side_fits() {
        let (origin, shown) = place(trigger(100., 700.), WINDOW, px(400.), px(40.), px(20.), 10);
        assert_eq!(shown, 10);
        assert_eq!(origin.y, px(700. - 240.));
    }

    #[test]
    fn a_short_window_shows_fewer_rows_on_the_roomier_side() {
        let short = Size { width: px(1200.), height: px(300.) };
        let (origin, shown) = place(trigger(100., 100.), short, px(400.), px(40.), px(20.), 40);
        // 172px below fits a 40px header and 6 rows. Above has only 92px.
        assert_eq!((origin.y, shown), (px(120.), 6));
        let tiny = Size { width: px(1200.), height: px(130.) };
        assert_eq!(place(trigger(100., 40.), tiny, px(400.), px(40.), px(20.), 40).1, MIN_ROWS);
    }

    #[test]
    fn the_card_stays_inside_the_window() {
        assert_eq!(place(trigger(1100., 100.), WINDOW, px(400.), px(40.), px(20.), 2).0.x, px(1200. - 8. - 400.));
        assert_eq!(place(trigger(2., 100.), WINDOW, px(400.), px(40.), px(20.), 2).0.x, MARGIN);
    }

    #[test]
    fn a_header_alone_takes_no_rows() {
        let (origin, shown) = place(trigger(100., 790.), WINDOW, px(200.), px(30.), px(20.), 0);
        assert_eq!((origin.y, shown), (px(790. - 30.), 0));
    }
}
