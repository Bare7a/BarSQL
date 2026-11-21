use std::rc::Rc;
use std::sync::Arc;

use crate::grid::{Aggregate, ColumnStats};
use crate::i18n::{I18n, format_number, t, t_count, t_with};
use crate::shortcuts;
use crate::spinner::Spinner;
use crate::toast;
use crate::tokens::{ICON_XS, RADIUS_SM, TEXT_XS};
use crate::update_dialog::UpdateChip;
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme, Icon, Sizable, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

mod fit;
pub(crate) mod server;

// 1-based, as editors show it. `selections` counts what Select Next Occurrence holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caret {
    pub line: usize,
    pub column: usize,
    pub selected: usize,
    pub selections: usize,
}

// The connection the bar speaks for: the tab's, or with no tab open the sidebar's.
#[derive(Default)]
pub struct StatusConnection {
    pub name: SharedString,
    pub engine: SharedString,
    pub connected: bool,
    // "PostgreSQL 17.2", while connected and once the server has said which version it runs.
    pub product: Option<SharedString>,
    pub details: Vec<server::Detail>,
}

// Shared with AI apps over MCP, to query only or to change data too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiAccess {
    Read,
    Write,
}

#[derive(Default)]
pub struct StatusBarState {
    pub connection: Option<StatusConnection>,
    // The connection's colour, as its tabs show it.
    pub color: Option<Hsla>,
    pub read_only: bool,
    // The connection asks before changes.
    pub protected: bool,
    // The extensions the connection loads, by file name.
    pub extensions: Vec<String>,
    pub ai: Option<AiAccess>,
    // An open transaction, and whether a statement in it failed.
    pub transaction: Option<bool>,
    // The editor's caret while the grid doesn't have the focus, and the grid's stats while it does. Never both.
    pub caret: Option<Caret>,
    pub aggregate: Option<Aggregate>,
    // What's going on now, like a query running. Once it's done the results say how it went.
    pub live: Option<SharedString>,
    // The side panels the bar's ends show and hide.
    pub sidebar_open: bool,
    pub json_open: bool,
    pub update: Option<UpdateChip>,
    pub actions: StatusActions,
}

pub type StatusAction = Rc<dyn Fn(&mut Window, &mut App)>;

// What the bar's items do on a click, where they can do something.
#[derive(Default)]
pub struct StatusActions {
    pub go_to_line: Option<StatusAction>,
    pub switch_connection: Option<StatusAction>,
    pub edit_connection: Option<StatusAction>,
    pub stop: Option<StatusAction>,
    pub toggle_sidebar: Option<StatusAction>,
    pub toggle_json: Option<StatusAction>,
    pub open_update: Option<StatusAction>,
}

// An item that does something on a click, with a hover wash and a tooltip saying what.
fn clickable(el: Stateful<Div>, action: Option<StatusAction>, tip: SharedString, cx: &App) -> Stateful<Div> {
    clickable_with(el, action, move |window, cx| Tooltip::new(tip.clone()).build(window, cx), cx)
}

fn clickable_with(
    el: Stateful<Div>,
    action: Option<StatusAction>,
    tooltip: impl Fn(&mut Window, &mut App) -> AnyView + 'static,
    cx: &App,
) -> Stateful<Div> {
    let Some(action) = action else { return el };
    let hover = cx.theme().foreground.opacity(0.08);
    el.px(rems(CLICK_PAD))
        .rounded(RADIUS_SM)
        .cursor_pointer()
        .hover(move |style| style.bg(hover))
        .tooltip(tooltip)
        .on_click(move |_, window, cx| action(window, cx))
}

// What the connection is and how it's reached, a fact a line, then what a click does.
fn connection_tooltip(connection: &StatusConnection, cx: &App) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let mut lines: Vec<SharedString> = connection.details.iter().map(|detail| detail_line(detail, cx)).collect();
    if lines.is_empty() {
        lines.push(connection.engine.clone());
    }
    let switch = t(cx, "status.switchConnection");
    move |window, cx| {
        let (lines, switch) = (lines.clone(), switch.clone());
        Tooltip::element(move |_, cx| {
            v_flex().children(lines.clone()).child(div().text_color(cx.theme().muted_foreground).child(switch.clone()))
        })
        .build(window, cx)
    }
}

// A side panel's button at the bar's end, lit while the panel is open.
fn panel_toggle(
    id: &'static str,
    name: Lucide,
    open: bool,
    (show, hide): (&str, &str),
    shortcut: &str,
    action: Option<StatusAction>,
    cx: &App,
) -> Stateful<Div> {
    let theme = cx.theme();
    let tip = format!("{} · {}", t(cx, if open { hide } else { show }), shortcuts::shown(shortcut, cx));
    let el = h_flex()
        .id(id)
        .debug_selector(move || id.into())
        .flex_none()
        .text_color(if open { theme.foreground } else { theme.muted_foreground })
        .child(icon(name));
    clickable(el, action, tip.into(), cx)
}

fn update_label(update: &UpdateChip, cx: &App) -> SharedString {
    match update {
        UpdateChip::Available(version) => t_with(cx, "status.update.available", &[("version", version)]),
        UpdateChip::Downloading(Some(percent)) => {
            t_with(cx, "status.update.downloading", &[("percent", &percent.to_string())])
        }
        UpdateChip::Downloading(None) => t(cx, "status.update.busy"),
        UpdateChip::Ready => t(cx, "status.update.ready"),
        UpdateChip::Failed => t(cx, "status.update.failed"),
    }
}

// Where an update stands, until it's in place. The tooltip repeats the words, for when the bar keeps only the icon.
fn update_chip(update: &UpdateChip, words: bool, action: Option<StatusAction>, cx: &App) -> Stateful<Div> {
    let theme = cx.theme();
    let label = update_label(update, cx);
    let mark = match update {
        UpdateChip::Downloading(_) => Spinner::new(ICON_XS).into_any_element(),
        _ => icon(Lucide::ArrowUp).into_any_element(),
    };
    let el = h_flex()
        .id("status-update")
        .debug_selector(|| "status-update".into())
        .flex_none()
        .gap(rems(INNER_GAP))
        .font_features(tabular())
        .text_color(if *update == UpdateChip::Failed { theme.danger } else { theme.primary })
        .child(mark)
        .when(words, |el| el.child(label.clone()));
    clickable(el, action, label, cx)
}

fn detail_line(detail: &server::Detail, cx: &App) -> SharedString {
    use server::Detail;
    match detail {
        Detail::Version(text) | Detail::Address(text) => text.clone().into(),
        Detail::Database { database, schema: Some(schema) } => {
            t_with(cx, "status.details.database", &[("database", database), ("schema", schema)])
        }
        Detail::Database { database, schema: None } => {
            t_with(cx, "status.details.databaseOnly", &[("database", database)])
        }
        Detail::Ssh(address) => t_with(cx, "status.details.ssh", &[("address", address)]),
        Detail::Tls(mode) => t_with(cx, "status.details.tls", &[("mode", mode)]),
        Detail::Latency(ms) => t_with(cx, "status.details.latency", &[("ms", &ms.to_string())]),
    }
}

fn caret_label(caret: Caret, cx: &App) -> String {
    let position =
        t_with(cx, "status.caret", &[("line", &caret.line.to_string()), ("column", &caret.column.to_string())]);
    match (caret.selections, caret.selected) {
        (n, _) if n > 1 => format!("{position} ({})", t_count(cx, "status.selections", n as i64, &[])),
        (_, 0) => position.to_string(),
        (_, chars) => format!("{position} ({})", t_count(cx, "status.selected", chars as i64, &[])),
    }
}

// A stat as shown, and as copied: plain digits, without grouping.
#[derive(Debug, Clone, PartialEq)]
pub struct Stat {
    pub key: &'static str,
    pub shown: String,
    pub plain: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Stats {
    // One column's Count, then its Sum, Avg, Min and Max if it holds numbers.
    Column(Vec<Stat>),
    // Numbers from different columns don't add up, so several columns show only how many cells they hold.
    Spread { cells: usize, columns: usize },
}

pub fn stats(total: &Aggregate, lang: &str) -> Stats {
    match total.columns.as_slice() {
        [column] => Stats::Column(column_stats(column, lang)),
        columns => Stats::Spread { cells: total.cells, columns: columns.len() },
    }
}

fn column_stats(column: &ColumnStats, lang: &str) -> Vec<Stat> {
    let stat = |key, value: f64, decimals| Stat {
        key,
        shown: shown_stat(value, decimals, lang),
        plain: plain_stat(value, decimals),
    };
    let mut shown = vec![stat("count", column.count as f64, 0)];
    if column.numbers > 0 {
        let decimals = column.decimals;
        shown.extend([
            stat("sum", column.sum, decimals),
            stat("avg", column.sum / column.numbers as f64, decimals.max(2)),
            stat("min", column.min, decimals),
            stat("max", column.max, decimals),
        ]);
    }
    shown
}

// To the column's own precision, and no finer than 2 decimals unless the value is below 1.
fn places(value: f64, decimals: usize) -> usize {
    if value != 0. && value.abs() < 1. { decimals.min(6) } else { decimals.min(2) }
}

fn shown_stat(value: f64, decimals: usize, lang: &str) -> String {
    format_number(value, places(value, decimals), lang)
}

fn plain_stat(value: f64, decimals: usize) -> String {
    let places = places(value, decimals);
    let text = format!("{value:.places$}");
    match text.contains('.') {
        true => text.trim_end_matches('0').trim_end_matches('.').to_string(),
        false => text,
    }
}

// Digits of one width, so the stats hold still while a selection grows.
fn tabular() -> FontFeatures {
    FontFeatures(Arc::new(vec![("tnum".into(), 1)]))
}

fn stats_segment(total: &Aggregate, visible: [bool; 5], cx: &App) -> AnyElement {
    let theme = cx.theme();
    match stats(total, cx.global::<I18n>().lang()) {
        Stats::Column(shown) => h_flex()
            .flex_none()
            .gap(rems(INNER_GAP))
            .font_features(tabular())
            .children(shown.into_iter().filter(|stat| visible[stat_index(stat.key)]).map(|stat| {
                let (key, plain) = (stat.key, stat.plain);
                let copy: StatusAction = Rc::new(move |_, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(plain.clone()));
                    toast::success(t(cx, "toast.copiedClipboard"), cx);
                });
                let item = h_flex()
                    .id(SharedString::from(format!("status-stat-{key}")))
                    .debug_selector(move || format!("status-stat-{key}"))
                    .gap(rems(INNER_GAP))
                    .child(t(cx, &format!("status.stat.{key}")))
                    .child(div().text_color(theme.foreground).child(stat.shown));
                clickable(item, Some(copy), t(cx, "common.copy"), cx)
            }))
            .into_any_element(),
        Stats::Spread { cells, columns } => {
            let label = t_with(
                cx,
                "status.cellsInColumns",
                &[("cells", &cells.to_string()), ("columns", &columns.to_string())],
            );
            let columns = total.columns.clone();
            Popover::new("status-spread")
                .anchor(Anchor::BottomRight)
                .trigger(
                    Button::new("status-spread-button")
                        .debug_selector(|| "status-spread".into())
                        .ghost()
                        .xsmall()
                        .label(label)
                        .dropdown_caret(true),
                )
                .content(move |_, _, cx| spread_table(&columns, cx))
                .into_any_element()
        }
    }
}

// Each selected column's own stats, a row each. Columns without numbers show only their count.
fn spread_table(columns: &[ColumnStats], cx: &App) -> impl IntoElement + use<> {
    let theme = cx.theme();
    let lang = cx.global::<I18n>().lang();
    let keys = ["count", "sum", "avg", "min", "max"];
    let cell = |text: SharedString| div().w(rems(5.5)).flex_none().text_right().child(text);
    let header = h_flex()
        .text_color(theme.muted_foreground)
        .child(div().w(rems(9.)).flex_none().child(t(cx, "status.column")))
        .children(keys.map(|key| cell(t(cx, &format!("status.stat.{key}")))));
    v_flex()
        .debug_selector(|| "status-spread-table".into())
        .gap_1()
        .text_size(TEXT_XS)
        .font_features(tabular())
        .child(header)
        .children(columns.iter().map(|column| {
            let shown = column_stats(column, lang);
            h_flex().child(div().w(rems(9.)).flex_none().truncate().child(column.name.clone())).children(keys.map(
                |key| {
                    let value = shown.iter().find(|stat| stat.key == key).map(|stat| stat.shown.clone());
                    cell(value.unwrap_or_default().into())
                },
            ))
        }))
}

// A read-only or ask-first connection, usually a production one, tints the whole bar with its colour. It's hard to
// miss without anything moving.
fn guard_tint(state: &StatusBarState) -> Option<Hsla> {
    state.color.filter(|_| state.read_only || state.protected)
}

// The bar's spacing in rems, shared by the drawing and by working out what fits.
const PAD: f32 = 0.75;
const HALF_GAP: f32 = 1.;
const SIDE_GAP: f32 = 0.5;
const ITEM_GAP: f32 = 0.375;
const INNER_GAP: f32 = 0.25;
const CLICK_PAD: f32 = 0.25;
const CHIP_PAD: f32 = 0.385;
// The layout rounds each edge to a whole pixel, which measuring doesn't see.
const SLACK: f32 = 0.25;
const DOT: Pixels = px(8.);
const TXN_DOT: Pixels = px(6.);
const STAT_KEYS: [&str; 5] = ["count", "sum", "avg", "min", "max"];

fn stat_index(key: &str) -> usize {
    STAT_KEYS.iter().position(|k| *k == key).unwrap_or(0)
}

fn engine_label(connection: &StatusConnection) -> SharedString {
    connection.product.clone().unwrap_or_else(|| connection.engine.clone())
}

fn short_caret(caret: Caret) -> String {
    format!("{}:{}", caret.line, caret.column)
}

// Text widths as the bar draws them.
struct Ruler<'a> {
    window: &'a Window,
    font: Font,
    digits: Font,
    size: Pixels,
    rem: Pixels,
}

impl<'a> Ruler<'a> {
    fn new(window: &'a Window, cx: &App) -> Self {
        let font = font(cx.theme().font_family.clone());
        let digits = Font { features: tabular(), ..font.clone() };
        Self { window, font, digits, size: TEXT_XS.to_pixels(window.rem_size()), rem: window.rem_size() }
    }

    fn width(&self, text: &str, font: &Font) -> Pixels {
        if text.is_empty() {
            return px(0.);
        }
        let run = TextRun {
            len: text.len(),
            font: font.clone(),
            color: Hsla::default(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        // Up to a whole pixel, as a text element lays itself out.
        self.window.text_system().shape_line(text.to_string().into(), self.size, &[run], None).width().ceil()
    }

    fn text(&self, text: &str) -> Pixels {
        self.width(text, &self.font)
    }

    fn digits(&self, text: &str) -> Pixels {
        self.width(text, &self.digits)
    }

    fn rems(&self, value: f32) -> Pixels {
        self.rem * value
    }
}

// A small tinted label beside the connection, for what's special about it. A narrow bar keeps only its mark.
struct Chip {
    id: &'static str,
    color: Hsla,
    // A dot without one.
    mark: Option<Lucide>,
    words: SharedString,
    // What stays beside the mark without the words: the extensions' count.
    keep: Option<SharedString>,
    // What a click opens. Only the transaction's chip has none.
    tip: Option<SharedString>,
}

fn chips(state: &StatusBarState, settings: &SharedString, cx: &App) -> Vec<Chip> {
    let theme = cx.theme();
    let guard = |id, mark, key| Chip {
        id,
        color: theme.warning,
        mark: Some(mark),
        words: t(cx, key),
        keep: None,
        tip: Some(settings.clone()),
    };
    let mut chips = Vec::new();
    if state.read_only {
        chips.push(guard("status-read-only", Lucide::Lock, "status.readOnly"));
    } else if state.protected {
        chips.push(guard("status-protected", Lucide::ShieldCheck, "status.protected"));
    }
    if let Some(ai) = state.ai {
        let (color, key, hint) = match ai {
            AiAccess::Read => (theme.primary, "status.aiRead", "status.aiReadHint"),
            AiAccess::Write => (theme.warning, "status.aiWrite", "status.aiWriteHint"),
        };
        let tip = format!("{} · {settings}", t(cx, hint)).into();
        chips.push(Chip {
            id: "status-ai",
            color,
            mark: Some(Lucide::Bot),
            words: t(cx, key),
            keep: None,
            tip: Some(tip),
        });
    }
    // Not a guard, so not in the guards' colour.
    if !state.extensions.is_empty() {
        let count = state.extensions.len();
        chips.push(Chip {
            id: "status-extensions",
            color: theme.primary,
            mark: Some(Lucide::Puzzle),
            words: t_count(cx, "status.extensions", count as i64, &[]),
            keep: Some(count.to_string().into()),
            tip: Some(format!("{} · {settings}", state.extensions.join(", ")).into()),
        });
    }
    if let Some(failed) = state.transaction {
        let (key, color) = if failed { ("editor.txnError", theme.danger) } else { ("editor.txnActive", theme.warning) };
        chips.push(Chip { id: "status-transaction", color, mark: None, words: t(cx, key), keep: None, tip: None });
    }
    chips
}

// Down to its mark, a chip's tooltip leads with its words.
fn draw_chip(chip: &Chip, words: bool, action: Option<StatusAction>, cx: &App) -> Stateful<Div> {
    let id = chip.id;
    let mark = match chip.mark {
        Some(name) => icon(name).into_any_element(),
        None => div().flex_none().size(TXN_DOT).rounded_full().bg(chip.color).into_any_element(),
    };
    let el = h_flex()
        .id(id)
        .debug_selector(move || id.into())
        .flex_none()
        .gap(rems(INNER_GAP))
        .px(rems(CHIP_PAD))
        .rounded(RADIUS_SM)
        .bg(chip.color.opacity(0.14))
        .text_color(chip.color)
        .child(mark)
        .map(|el| if words { el.child(chip.words.clone()) } else { el.children(chip.keep.clone()) });
    match (action.filter(|_| chip.tip.is_some()), &chip.tip) {
        (Some(action), Some(tip)) if words => clickable(el, Some(action), tip.clone(), cx),
        (Some(action), Some(tip)) => clickable(el, Some(action), format!("{} · {tip}", chip.words).into(), cx),
        _ if !words => {
            let tip = chip.words.clone();
            el.tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
        }
        _ => el,
    }
}

// Each part's natural width, for what the window has room for.
fn measure(state: &StatusBarState, chips: &[Chip], ruler: &Ruler, cx: &App) -> fit::Widths {
    let r = |value: f32| ruler.rems(value);
    let icon = ICON_XS.to_pixels(ruler.rem);
    let click = r(CLICK_PAD * 2.);
    let toggle = click + icon;
    let mut widths = fit::Widths::default();
    let name = match &state.connection {
        Some(connection) => {
            widths.engine = r(ITEM_GAP) + ruler.text(&engine_label(connection));
            let off = match connection.connected {
                true => px(0.),
                false => r(ITEM_GAP) + ruler.text(&t(cx, "sidebar.notConnected")),
            };
            click + DOT + r(ITEM_GAP) + ruler.text(&connection.name) + off
        }
        None => ruler.text(&t(cx, "app.statusNoConnection")),
    };
    widths.base = r(PAD * 2. + SIDE_GAP + HALF_GAP + SLACK) + toggle * 2. + name;
    for chip in chips {
        let pad = if chip.tip.is_some() && state.actions.edit_connection.is_some() { CLICK_PAD } else { CHIP_PAD };
        let shell = r(ITEM_GAP + pad * 2.) + if chip.mark.is_some() { icon } else { TXN_DOT };
        widths.chips += shell + r(INNER_GAP) + ruler.text(&chip.words);
        let keep = chip.keep.as_ref().map_or(px(0.), |keep| r(INNER_GAP) + ruler.text(keep));
        widths.chip_icons += shell + keep;
    }
    if let Some(total) = &state.aggregate {
        match stats(total, cx.global::<I18n>().lang()) {
            Stats::Column(shown) => {
                for stat in shown {
                    let label = t(cx, &format!("status.stat.{}", stat.key));
                    // Count leads, so it has no gap before it.
                    let gaps = if stat.key == STAT_KEYS[0] { INNER_GAP } else { INNER_GAP * 2. };
                    widths.stats[stat_index(stat.key)] =
                        r(gaps) + click + ruler.text(&label) + ruler.digits(&stat.shown);
                }
            }
            Stats::Spread { cells, columns } => {
                let label = t_with(
                    cx,
                    "status.cellsInColumns",
                    &[("cells", &cells.to_string()), ("columns", &columns.to_string())],
                );
                widths.stats[0] = ruler.text(&label) + icon + r(1.5);
            }
        }
        widths.stats[0] += r(HALF_GAP);
    }
    if let Some(caret) = state.caret {
        let click = if state.actions.go_to_line.is_some() { click } else { px(0.) };
        widths.caret = r(HALF_GAP) + click + ruler.digits(&caret_label(caret, cx));
        widths.caret_short = r(HALF_GAP) + click + ruler.digits(&short_caret(caret));
    }
    if let Some(text) = &state.live {
        let stop = match state.actions.stop.is_some() {
            true => r(ITEM_GAP * 2.) + px(1.) + click + ruler.text(&t(cx, "editor.stop")),
            false => px(0.),
        };
        widths.live = r(HALF_GAP + ITEM_GAP) + icon + ruler.digits(text) + stop;
    }
    if let Some(update) = &state.update {
        widths.update_icon = r(HALF_GAP) + click + icon;
        widths.update = widths.update_icon + r(INNER_GAP) + ruler.digits(&update_label(update, cx));
    }
    widths
}

pub fn render(state: StatusBarState, window: &Window, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    let tint = guard_tint(&state);
    let (background, border) = match tint {
        Some(color) => (theme.status_bar.blend(color.opacity(0.2)), color),
        None => (theme.status_bar, theme.status_bar_border),
    };
    let settings = t(cx, "status.connectionSettings");
    let chips = chips(&state, &settings, cx);
    let ruler = Ruler::new(window, cx);
    let widths = measure(&state, &chips, &ruler, cx);
    let available = window.viewport_size().width;
    let step = fit::step_for(&widths, available);
    let shown = fit::shown(step);
    // Past the last step, the name gives up characters, down to a few, and then the run's text does, all of it if it
    // must: the spinner and Stop say enough.
    let mut over = fit::width_at(&widths, step) - available;
    let mut cap = |natural: Pixels, floor: Pixels| -> Option<Pixels> {
        if over <= px(0.) || natural <= floor {
            return None;
        }
        let cut = if over < natural - floor { over } else { natural - floor };
        over -= cut;
        Some(natural - cut)
    };
    let name_width = state.connection.as_ref().map(|connection| ruler.text(&connection.name));
    let name_floor = name_width.map(|natural| if natural < ruler.rems(3.) { natural } else { ruler.rems(3.) });
    let name_cap = name_width.zip(name_floor).and_then(|(natural, floor)| cap(natural, floor));
    let live_cap = state.live.as_ref().and_then(|text| cap(ruler.digits(text), px(0.)));
    let actions = state.actions;
    let connection = match &state.connection {
        Some(connection) => {
            let name = h_flex()
                .id("status-connection")
                .debug_selector(|| "status-connection".into())
                .min_w_0()
                .gap(rems(ITEM_GAP))
                .child(div().flex_none().size(DOT).rounded_full().bg(match connection.connected {
                    true => theme.success,
                    false => theme.muted_foreground,
                }))
                .child(
                    div()
                        .debug_selector(|| "status-name".into())
                        .when_some(name_floor, |el, floor| el.min_w(floor))
                        .when_some(name_cap, |el, cap| el.max_w(cap))
                        .truncate()
                        .text_color(theme.foreground)
                        .child(connection.name.clone()),
                )
                .when(shown.engine, |el| {
                    el.child(
                        div().flex_none().debug_selector(|| "status-engine".into()).child(engine_label(connection)),
                    )
                })
                .when(!connection.connected, |el| el.child(div().flex_none().child(t(cx, "sidebar.notConnected"))));
            h_flex()
                .min_w_0()
                .gap(rems(ITEM_GAP))
                .child(clickable_with(name, actions.switch_connection, connection_tooltip(connection, cx), cx))
                .children(
                    chips.iter().map(|chip| draw_chip(chip, shown.chip_words, actions.edit_connection.clone(), cx)),
                )
        }
        None => h_flex().min_w_0().child(div().truncate().child(t(cx, "app.statusNoConnection"))),
    };
    let caret = state.caret.map(|caret| {
        let label = if shown.short_caret { short_caret(caret) } else { caret_label(caret, cx) };
        let place = div()
            .id("status-caret")
            .debug_selector(|| "status-caret".into())
            .flex_none()
            .whitespace_nowrap()
            .font_features(tabular())
            .child(label);
        clickable(place, actions.go_to_line, t(cx, "goToLine.title"), cx)
    });
    let live = state.live.map(|text| {
        let stop =
            div().id("status-stop").debug_selector(|| "status-stop".into()).flex_none().text_color(theme.primary);
        h_flex()
            .debug_selector(|| "status-live".into())
            .min_w_0()
            .gap(rems(ITEM_GAP))
            .font_features(tabular())
            .child(div().flex_none().child(Spinner::new(ICON_XS)))
            .child(div().min_w_0().when_some(live_cap, |el, cap| el.max_w(cap)).truncate().child(text))
            .when_some(actions.stop, |el, action| {
                let stop = clickable(stop, Some(action), t(cx, "tooltip.stopQuery"), cx).child(t(cx, "editor.stop"));
                el.child(div().flex_none().w(px(1.)).h(px(12.)).bg(theme.border)).child(stop)
            })
    });
    let update =
        state.update.as_ref().map(|update| update_chip(update, shown.update_words, actions.open_update.clone(), cx));
    // The right half keeps what it holds; the left gives up characters of the name when even the last step won't do.
    h_flex()
        .h(px(22.))
        .flex_none()
        .px(rems(PAD))
        .gap(rems(HALF_GAP))
        .justify_between()
        .text_size(TEXT_XS)
        .text_color(theme.muted_foreground)
        .bg(background)
        .border_t_1()
        .border_color(border)
        .child(
            h_flex()
                .debug_selector(|| "status-left".into())
                .min_w_0()
                .gap(rems(SIDE_GAP))
                .child(panel_toggle(
                    "status-toggle-sidebar",
                    Lucide::PanelLeft,
                    state.sidebar_open,
                    ("status.showSidebar", "status.hideSidebar"),
                    "toggleSidebar",
                    actions.toggle_sidebar,
                    cx,
                ))
                .child(connection),
        )
        .child(
            h_flex()
                .debug_selector(|| "status-right".into())
                .gap(rems(HALF_GAP))
                .children(state.aggregate.as_ref().map(|total| stats_segment(total, shown.stats, cx)))
                .children(caret)
                .children(live)
                .children(update)
                .child(panel_toggle(
                    "status-toggle-json",
                    Lucide::PanelRight,
                    state.json_open,
                    ("status.showJsonViewer", "status.hideJsonViewer"),
                    "toggleJsonPanel",
                    actions.toggle_json,
                    cx,
                )),
        )
}

fn icon(name: Lucide) -> Icon {
    Icon::new(name).size(ICON_XS)
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use gpui_kit::{Hsla, ParentElement as _, Styled as _, hsla};

    use super::{AiAccess, Stat, Stats, StatusBarState, guard_tint, shown_stat, stats};
    use crate::grid::{Aggregate, ColumnStats};
    use crate::update_dialog::UpdateChip;

    #[test]
    fn stats_round_to_the_data() {
        assert_eq!(shown_stat(778.153_248, 14, "en"), "778.15");
        assert_eq!(shown_stat(628_569.562_823, 14, "en"), "628,569.56");
        assert_eq!(shown_stat(3., 0, "en"), "3");
        assert_eq!(shown_stat(0.000_123, 6, "en"), "0.000123");
        assert_eq!(shown_stat(2.5, 1, "de"), "2,5");
    }

    #[test]
    fn one_column_shows_its_stats_and_several_only_their_size() {
        let n = ColumnStats { name: "n".into(), count: 2, numbers: 2, sum: 3.5, min: 1., max: 2.5, decimals: 1 };
        let t = ColumnStats { name: "t".into(), count: 2, ..Default::default() };
        let stat = |key, shown: &str, plain: &str| Stat { key, shown: shown.into(), plain: plain.into() };
        let one = |column: &ColumnStats| Aggregate { cells: 3, columns: vec![column.clone()] };
        assert_eq!(
            stats(&one(&n), "en"),
            Stats::Column(vec![
                stat("count", "2", "2"),
                stat("sum", "3.5", "3.5"),
                stat("avg", "1.75", "1.75"),
                stat("min", "1", "1"),
                stat("max", "2.5", "2.5"),
            ])
        );
        assert_eq!(stats(&one(&t), "en"), Stats::Column(vec![stat("count", "2", "2")]));
        let big = ColumnStats { sum: 1_234_567.891, numbers: 1, count: 1, decimals: 3, ..n.clone() };
        let Stats::Column(shown) = stats(&one(&big), "en") else { panic!("one column") };
        assert_eq!((shown[1].shown.as_str(), shown[1].plain.as_str()), ("1,234,567.89", "1234567.89"), "copies plain");
        let both = Aggregate { cells: 6, columns: vec![n, t] };
        assert_eq!(stats(&both, "en"), Stats::Spread { cells: 6, columns: 2 });
    }

    fn state(read_only: bool, protected: bool, color: Option<Hsla>) -> StatusBarState {
        StatusBarState { color, read_only, protected, ..Default::default() }
    }

    // A read-only or ask-first connection, usually production, colours the bar. An ordinary one leaves it alone.
    #[test]
    fn only_a_guarded_connection_tints_the_bar() {
        let red = hsla(0., 0.8, 0.6, 1.);
        assert_eq!(guard_tint(&state(true, false, Some(red))), Some(red));
        assert_eq!(guard_tint(&state(false, true, Some(red))), Some(red));
        assert_eq!(guard_tint(&state(false, false, Some(red))), None);
        assert_eq!(guard_tint(&state(true, true, None)), None);
        let extended = StatusBarState { extensions: vec!["crsqlite.dylib".into()], ..state(false, false, Some(red)) };
        assert_eq!(guard_tint(&extended), None, "extensions aren't a guard");
        let shared = StatusBarState { ai: Some(AiAccess::Write), ..state(false, false, Some(red)) };
        assert_eq!(guard_tint(&shared), None, "AI access isn't a guard");
    }

    // The bar alone, drawn from whatever state the test builds.
    struct Bar(Rc<dyn Fn() -> StatusBarState>);

    impl Bar {
        fn new(state: impl Fn() -> StatusBarState + 'static) -> Self {
            Self(Rc::new(state))
        }
    }

    // Across the window's foot, as the workspace draws it.
    impl gpui_kit::Render for Bar {
        fn render(
            &mut self,
            window: &mut gpui_kit::Window,
            cx: &mut gpui_kit::Context<Self>,
        ) -> impl gpui_kit::IntoElement {
            gpui_kit::component::v_flex().size_full().justify_end().child(super::render((self.0)(), window, cx))
        }
    }

    fn notes() -> StatusBarState {
        let connection = super::StatusConnection {
            name: "Notes".into(),
            engine: "SQLite".into(),
            connected: true,
            ..Default::default()
        };
        StatusBarState { connection: Some(connection), ..Default::default() }
    }

    #[gpui_kit::test]
    fn only_a_connection_with_extensions_shows_their_chip(cx: &mut gpui_kit::TestAppContext) {
        let _env = crate::test_support::Env::new(cx);
        for (extensions, shown) in [(vec![], false), (vec!["crsqlite.dylib".to_string()], true)] {
            let (_, cx) = cx.add_window_view(move |_, _| {
                Bar::new(move || StatusBarState { extensions: extensions.clone(), ..notes() })
            });
            cx.update(|window, cx| window.draw(cx).clear(cx));
            assert_eq!(cx.debug_bounds("status-extensions").is_some(), shown);
        }
    }

    #[gpui_kit::test]
    fn only_a_connection_shared_with_ai_shows_the_ai_chip(cx: &mut gpui_kit::TestAppContext) {
        let _env = crate::test_support::Env::new(cx);
        for (ai, shown) in [(None, false), (Some(AiAccess::Read), true), (Some(AiAccess::Write), true)] {
            let (_, cx) = cx.add_window_view(move |_, _| Bar::new(move || StatusBarState { ai, ..notes() }));
            cx.update(|window, cx| window.draw(cx).clear(cx));
            assert_eq!(cx.debug_bounds("status-ai").is_some(), shown, "{ai:?}");
        }
    }

    #[gpui_kit::test]
    fn the_update_chip_opens_the_update_dialog(cx: &mut gpui_kit::TestAppContext) {
        let _env = crate::test_support::Env::new(cx);
        let opened = Rc::new(std::cell::Cell::new(false));
        let open: super::StatusAction = Rc::new({
            let opened = opened.clone();
            move |_, _| opened.set(true)
        });
        let (_, cx) = cx.add_window_view(move |_, _| {
            Bar::new(move || StatusBarState {
                update: Some(UpdateChip::Available("1.3.0".into())),
                actions: super::StatusActions { open_update: Some(open.clone()), ..Default::default() },
                ..notes()
            })
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let chip = cx.debug_bounds("status-update").expect("the update chip");
        cx.simulate_click(chip.center(), gpui_kit::Modifiers::none());
        assert!(opened.get());
    }

    // Everything the bar can say at once.
    fn full(name: &str) -> StatusBarState {
        let connection = super::StatusConnection {
            name: name.to_string().into(),
            engine: "PostgreSQL".into(),
            connected: true,
            product: Some("PostgreSQL 17.2".into()),
            ..Default::default()
        };
        let column = ColumnStats {
            name: "amount".into(),
            count: 1234,
            numbers: 1234,
            sum: 628_569.56,
            min: 0.01,
            max: 9_999.99,
            decimals: 2,
        };
        let noop: super::StatusAction = Rc::new(|_, _| {});
        StatusBarState {
            connection: Some(connection),
            read_only: true,
            ai: Some(AiAccess::Write),
            extensions: vec!["crsqlite.dll".into(), "vec0.dll".into()],
            transaction: Some(false),
            aggregate: Some(Aggregate { cells: 1234, columns: vec![column] }),
            live: Some("Streaming… 12,345 rows · 3.2s".into()),
            update: Some(UpdateChip::Available("1.3.0".into())),
            sidebar_open: true,
            json_open: true,
            actions: super::StatusActions {
                switch_connection: Some(noop.clone()),
                edit_connection: Some(noop.clone()),
                stop: Some(noop.clone()),
                toggle_sidebar: Some(noop.clone()),
                toggle_json: Some(noop.clone()),
                open_update: Some(noop),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn window<'a>(
        cx: &'a mut gpui_kit::TestAppContext,
        width: f32,
        name: &'static str,
    ) -> &'a mut gpui_kit::VisualTestContext {
        let (_, cx) = cx.add_window_view(move |_, _| Bar::new(move || full(name)));
        cx.simulate_resize(gpui_kit::size(gpui_kit::px(width), gpui_kit::px(600.)));
        cx
    }

    // Where the left half's drawing ends and the right half's begins, and the right half's far edge. A half can
    // shrink while what it holds spills out, so this reads what's drawn, not the halves' boxes.
    fn edges(cx: &mut gpui_kit::VisualTestContext) -> (gpui_kit::Pixels, gpui_kit::Pixels, gpui_kit::Pixels) {
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let end = cx.debug_bounds("status-transaction").expect("the transaction chip").right();
        let start = cx.debug_bounds("status-stat-count").expect("the count").left();
        let far = cx.debug_bounds("status-toggle-json").expect("the JSON toggle").right();
        (end, start, far)
    }

    // At the narrowest window the two halves never meet, in any of the app's languages.
    #[gpui_kit::test]
    fn a_full_bar_fits_the_narrowest_window(cx: &mut gpui_kit::TestAppContext) {
        let _env = crate::test_support::Env::new(cx);
        for lang in ["en", "de", "bg", "es"] {
            cx.update(|cx| cx.set_global(crate::i18n::I18n::new(lang)));
            let cx = window(cx, 800., "Production analytics");
            let (end, start, far) = edges(cx);
            assert!(end <= start && far <= gpui_kit::px(800.), "{lang}: {end:?} {start:?} {far:?}");
            assert!(cx.debug_bounds("status-stat-max").is_none(), "{lang}: Max gave way first");
        }
    }

    #[gpui_kit::test]
    fn a_wide_window_shows_everything(cx: &mut gpui_kit::TestAppContext) {
        let _env = crate::test_support::Env::new(cx);
        let cx = window(cx, 2400., "Production analytics");
        edges(cx);
        for id in ["status-engine", "status-stat-max", "status-ai", "status-update", "status-toggle-json"] {
            assert!(cx.debug_bounds(id).is_some(), "{id}");
        }
    }

    // A name with no spaces at the largest zoom gives up characters instead of pushing the right half away.
    #[gpui_kit::test]
    fn a_long_name_at_the_largest_zoom_truncates(cx: &mut gpui_kit::TestAppContext) {
        let _env = crate::test_support::Env::new(cx);
        let name: &'static str = "x".repeat(60).leak();
        let cx = window(cx, 800., name);
        cx.update(|window, _| window.set_rem_size(gpui_kit::px(crate::theme::ZOOM_RANGE.1)));
        let (end, start, far) = edges(cx);
        assert!(end <= start && far <= gpui_kit::px(800.), "{end:?} {start:?} {far:?}");
    }

    // The name's floor is for cutting a long name, not for padding a short one.
    #[gpui_kit::test]
    fn a_short_name_keeps_its_own_width(cx: &mut gpui_kit::TestAppContext) {
        let _env = crate::test_support::Env::new(cx);
        let cx = window(cx, 2400., "QA");
        edges(cx);
        let floor = cx.update(|window, _| window.rem_size() * 3.);
        let name = cx.debug_bounds("status-name").expect("the name");
        assert!(name.size.width < floor, "{name:?}");
    }

    // What the bar works out it needs is what it draws, give or take its slack, so items give way neither late nor
    // much too early.
    #[gpui_kit::test]
    fn the_measure_matches_the_drawing(cx: &mut gpui_kit::TestAppContext) {
        let _env = crate::test_support::Env::new(cx);
        let cx = window(cx, 2400., "Production analytics");
        edges(cx);
        let left = cx.debug_bounds("status-left").expect("the left half").size.width;
        let right = cx.debug_bounds("status-right").expect("the right half").size.width;
        let (measured, rem) = cx.update(|window, cx| {
            let state = full("Production analytics");
            let settings = super::t(cx, "status.connectionSettings");
            let chips = super::chips(&state, &settings, cx);
            let widths = super::measure(&state, &chips, &super::Ruler::new(window, cx), cx);
            (super::fit::width_at(&widths, 0), window.rem_size())
        });
        let drawn = left + right + rem * (super::PAD * 2. + super::HALF_GAP);
        assert!(drawn <= measured && measured <= drawn + rem, "measured {measured:?}, drawn {drawn:?}");
    }
}
