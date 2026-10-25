use std::rc::Rc;
use std::sync::Arc;

use crate::grid::{Aggregate, ColumnStats};
use crate::i18n::{I18n, format_number, t, t_count, t_with};
use crate::spinner::Spinner;
use crate::toast;
use crate::tokens::{ICON_XS, RADIUS_SM, TEXT_XS};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme, Icon, Sizable, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

// 1-based, as editors show it. `selections` counts what Select Next Occurrence holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caret {
    pub line: usize,
    pub column: usize,
    pub selected: usize,
    pub selections: usize,
}

// The connection the bar speaks for: the tab's, or with no tab open the sidebar's.
pub struct StatusConnection {
    pub name: SharedString,
    pub engine: SharedString,
    pub connected: bool,
}

#[derive(Default)]
pub struct StatusBarState {
    pub connection: Option<StatusConnection>,
    // The connection's colour, as its tabs show it.
    pub color: Option<Hsla>,
    pub read_only: bool,
    // The connection asks before changes.
    pub protected: bool,
    // An open transaction, and whether a statement in it failed.
    pub transaction: Option<bool>,
    // The editor's caret while the grid doesn't have the focus, and the grid's stats while it does. Never both.
    pub caret: Option<Caret>,
    pub aggregate: Option<Aggregate>,
    // What's going on now, like a query running. Once it's done the results say how it went.
    pub live: Option<SharedString>,
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
}

// An item that does something on a click, with a hover wash and a tooltip saying what.
fn clickable(el: Stateful<Div>, action: Option<StatusAction>, tip: SharedString, cx: &App) -> Stateful<Div> {
    let Some(action) = action else { return el };
    let hover = cx.theme().foreground.opacity(0.08);
    el.px_1()
        .rounded(RADIUS_SM)
        .cursor_pointer()
        .hover(move |style| style.bg(hover))
        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
        .on_click(move |_, window, cx| action(window, cx))
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

fn stats_segment(total: &Aggregate, cx: &App) -> AnyElement {
    let theme = cx.theme();
    match stats(total, cx.global::<I18n>().lang()) {
        Stats::Column(shown) => h_flex()
            .gap_1()
            .font_features(tabular())
            .children(shown.into_iter().map(|stat| {
                let (key, plain) = (stat.key, stat.plain);
                let copy: StatusAction = Rc::new(move |_, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(plain.clone()));
                    toast::success(t(cx, "toast.copiedClipboard"), cx);
                });
                let item = h_flex()
                    .id(SharedString::from(format!("status-stat-{key}")))
                    .debug_selector(move || format!("status-stat-{key}"))
                    .gap_1()
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

pub fn render(state: StatusBarState, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    let tint = guard_tint(&state);
    let (background, border) = match tint {
        Some(color) => (theme.status_bar.blend(color.opacity(0.2)), color),
        None => (theme.status_bar, theme.status_bar_border),
    };
    let actions = state.actions;
    let settings = t(cx, "status.connectionSettings");
    let left = match &state.connection {
        Some(connection) => {
            let name = h_flex()
                .id("status-connection")
                .debug_selector(|| "status-connection".into())
                .gap_1p5()
                .child(div().size(px(8.)).rounded_full().bg(match connection.connected {
                    true => theme.success,
                    false => theme.muted_foreground,
                }))
                .child(div().text_color(theme.foreground).child(connection.name.clone()))
                .child(connection.engine.clone())
                .when(!connection.connected, |el| el.child(t(cx, "sidebar.notConnected")));
            h_flex()
                .gap_1p5()
                .child(clickable(name, actions.switch_connection, t(cx, "status.switchConnection"), cx))
                .when(state.read_only, |el| {
                    let chip = chip("status-read-only", theme.warning)
                        .child(icon(Lucide::Lock))
                        .child(t(cx, "status.readOnly"));
                    el.child(clickable(chip, actions.edit_connection.clone(), settings.clone(), cx))
                })
                .when(state.protected && !state.read_only, |el| {
                    let chip = chip("status-protected", theme.warning)
                        .child(icon(Lucide::ShieldCheck))
                        .child(t(cx, "status.protected"));
                    el.child(clickable(chip, actions.edit_connection.clone(), settings.clone(), cx))
                })
                .when_some(state.transaction, |el, failed| {
                    let (key, color) =
                        if failed { ("editor.txnError", theme.danger) } else { ("editor.txnActive", theme.warning) };
                    let dot = div().size(px(6.)).rounded_full().bg(color);
                    el.child(chip("status-transaction", color).child(dot).child(t(cx, key)))
                })
        }
        None => h_flex().child(t(cx, "app.statusNoConnection")),
    };
    let caret = state.caret.map(|caret| {
        let place = div()
            .id("status-caret")
            .debug_selector(|| "status-caret".into())
            .flex_none()
            .whitespace_nowrap()
            .child(caret_label(caret, cx));
        clickable(place, actions.go_to_line, t(cx, "goToLine.title"), cx)
    });
    let live = state.live.map(|text| {
        let stop = div().id("status-stop").debug_selector(|| "status-stop".into()).text_color(theme.primary);
        h_flex()
            .debug_selector(|| "status-live".into())
            .min_w_0()
            .gap_1p5()
            .font_features(tabular())
            .child(Spinner::new(ICON_XS))
            .child(div().truncate().child(text))
            .when_some(actions.stop, |el, action| {
                let stop = clickable(stop, Some(action), t(cx, "tooltip.stopQuery"), cx).child(t(cx, "editor.stop"));
                el.child(div().w(px(1.)).h(px(12.)).bg(theme.border)).child(stop)
            })
    });
    h_flex()
        .h(px(22.))
        .flex_none()
        .px_3()
        .gap_4()
        .justify_between()
        .text_size(TEXT_XS)
        .text_color(theme.muted_foreground)
        .bg(background)
        .border_t_1()
        .border_color(border)
        .child(left.flex_none())
        .child(
            h_flex()
                .min_w_0()
                .gap_4()
                .children(state.aggregate.as_ref().map(|total| stats_segment(total, cx)))
                .children(caret)
                .children(live),
        )
}

// A small tinted label beside the connection, for what's special about it.
fn chip(id: &'static str, color: Hsla) -> Stateful<Div> {
    h_flex()
        .id(id)
        .debug_selector(move || id.into())
        .gap_1()
        .px(rems(0.385))
        .rounded(RADIUS_SM)
        .bg(color.opacity(0.14))
        .text_color(color)
}

fn icon(name: Lucide) -> Icon {
    Icon::new(name).size(ICON_XS)
}

#[cfg(test)]
mod tests {
    use gpui_kit::{Hsla, hsla};

    use super::{Stat, Stats, StatusBarState, guard_tint, shown_stat, stats};
    use crate::grid::{Aggregate, ColumnStats};

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
    }
}
