mod data;
mod plot;

use std::rc::Rc;
use std::time::Duration;

use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::menu::PopupMenuItem;
use gpui_kit::component::plot::label::measure_text_width;
use gpui_kit::component::scroll::ScrollbarAxis;
use gpui_kit::component::{ActiveTheme, Disableable, StyledExt, Theme, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

#[cfg(test)]
pub(crate) use self::data::{Aggregate, Kind};
#[cfg(not(test))]
use self::data::{Aggregate, Kind};
use self::data::{
    ChartData, Cut, MAX_ROWS, PROFILE_ROWS, Profile, SLOTS, Series, Settings, compute, defaults, nice_ticks, profile,
    tick_format,
};
use self::plot::{ResultPlot, ValueAxis, min_width};
use crate::form::{self, SelectMenu as _};
use crate::fuzzy::rank_candidate;
use crate::grid::Grid;
use crate::i18n::{I18n, count, number, t, t_with};
use crate::scrollbars::HoverScrollbar as _;
use crate::tokens::{RADIUS, TEXT_BASE, TEXT_SM, TEXT_XS};

const FILTER_DEBOUNCE: Duration = Duration::from_millis(150);
const PANEL_WIDTH: Rems = rems(16.923);
// Ticks the value axis aims for.
const TICKS: usize = 5;
const CAPTION_HEIGHT: f32 = 2.154;
// Room under the labels for the plot's sideways scrollbar, so it never covers them.
const SCROLLBAR_LANE: Rems = rems(0.769);
// The values list takes a filter past this many columns.
const FILTER_FROM: usize = 8;

// A column as the lists show it.
struct Column {
    name: SharedString,
    type_name: SharedString,
}

// A quick look at a result: values against labels, as grouped bars or lines, set up in a panel beside the plot.
pub struct ChartView {
    grid: Entity<Grid>,
    settings: Settings,
    // Until the panel is used, the picks follow the rows as they stream in.
    touched: bool,
    filter: Entity<InputState>,
    needle: String,
    filter_task: Task<()>,
    values_scroll: ScrollHandle,
    plot_scroll: ScrollHandle,
    // Kept between renders, since the plot repaints on every pointer move: the profiles by the rows they read, the
    // data by the rows and the picks.
    profiles: Option<(usize, Rc<Vec<Profile>>)>,
    data: Option<(usize, Settings, Rc<ChartData>)>,
    _subscriptions: Vec<Subscription>,
}

impl ChartView {
    pub fn new(grid: Entity<Grid>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let placeholder = t(cx, "chart.filterColumns");
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        let subscriptions = vec![
            cx.observe(&grid, |_, _, cx| cx.notify()),
            cx.subscribe(&filter, |this, filter, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    let value = filter.read(cx).value().trim().to_string();
                    this.filter_task = cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(FILTER_DEBOUNCE).await;
                        let _ = this.update(cx, |this, cx| {
                            this.needle = value;
                            cx.notify();
                        });
                    });
                }
            }),
            cx.observe_global_in::<I18n>(window, |this, window, cx| {
                let placeholder = t(cx, "chart.filterColumns");
                this.filter.update(cx, |filter, cx| filter.set_placeholder(placeholder, window, cx));
            }),
        ];
        Self {
            grid,
            settings: defaults(&[]),
            touched: false,
            filter,
            needle: String::new(),
            filter_task: Task::ready(()),
            values_scroll: ScrollHandle::new(),
            plot_scroll: ScrollHandle::new(),
            profiles: None,
            data: None,
            _subscriptions: subscriptions,
        }
    }

    // Profiles the first rows, and groups the values again when the rows or the picks change.
    fn sync(&mut self, cx: &App) -> (Rc<Vec<Profile>>, Rc<ChartData>) {
        let set = self.grid.read(cx).set();
        let profiled = set.rows().min(PROFILE_ROWS);
        let profiles = match &self.profiles {
            Some((rows, profiles)) if *rows == profiled => profiles.clone(),
            _ => {
                let profiles = Rc::new(profile(set));
                if !self.touched {
                    self.settings = defaults(&profiles);
                }
                self.profiles = Some((profiled, profiles.clone()));
                profiles
            }
        };
        let read = set.rows().min(MAX_ROWS + 1);
        let data = match &self.data {
            Some((rows, settings, data)) if *rows == read && *settings == self.settings => data.clone(),
            _ => {
                let data = Rc::new(compute(set, &profiles, &self.settings));
                self.data = Some((read, self.settings.clone(), data.clone()));
                data
            }
        };
        (profiles, data)
    }

    fn edit(&mut self, cx: &mut Context<Self>, change: impl FnOnce(&mut Settings)) {
        change(&mut self.settings);
        self.touched = true;
        cx.notify();
    }

    fn plot_area(
        &self,
        rows: usize,
        columns: &[Column],
        profiles: &[Profile],
        data: &Rc<ChartData>,
        window: &mut Window,
        cx: &App,
    ) -> AnyElement {
        let kind = self.settings.kind;
        if rows == 0 {
            return message(t(cx, "chart.noRows"), cx);
        }
        if data.series.is_empty() {
            return how_to(cx);
        }
        let Some((lo, hi)) = data.range(kind) else {
            return message(t(cx, "chart.noValues"), cx);
        };
        let theme = cx.theme();
        let ticks: Rc<[f64]> = nice_ticks(lo, hi, TICKS).into();
        let reach = ticks.iter().fold(0f64, |reach, tick| reach.max(tick.abs()));
        let (unit, suffix, decimals) = tick_format(reach, ticks[1] - ticks[0]);
        let tick_labels: Rc<[SharedString]> = ticks
            .iter()
            .map(|&tick| match tick {
                0. => "0".into(),
                tick => format!("{}{suffix}", number(cx, tick / unit, decimals)).into(),
            })
            .collect();
        let font = TEXT_XS.to_pixels(window.rem_size());
        let gutter = tick_labels.iter().map(|label| measure_text_width(label, font, window)).fold(0., f32::max) + 12.;
        let colors: Rc<[Hsla]> = data.series.iter().map(|series| slot_color(series.slot, theme)).collect();
        let names: Rc<[SharedString]> = data.series.iter().map(|series| series_name(series, columns, cx)).collect();
        // Dates without a time of day drop the zeros.
        let dates = profiles.get(self.settings.label).is_some_and(|p| p.temporal)
            && data.labels.iter().flatten().all(|label| midnight(label).is_some());
        let labels: Rc<[SharedString]> = data
            .labels
            .iter()
            .map(|label| match label.as_deref() {
                Some(label) if dates => midnight(label).unwrap_or(label).into(),
                label => label_text(label, cx),
            })
            .collect();
        let width = min_width(labels.len(), data.series.len(), kind);
        let header = h_flex()
            .flex_none()
            .gap(rems(0.923))
            .px(rems(1.231))
            .pt(rems(0.769))
            .pb(rems(0.462))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(TEXT_SM)
                    .font_semibold()
                    .text_color(theme.foreground)
                    .debug_selector(|| "chart-title".into())
                    .child(title(&self.settings, data, columns, cx)),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(TEXT_XS)
                    .text_color(theme.muted_foreground)
                    .debug_selector(|| "chart-meta".into())
                    .child(meta(data, cx)),
            );
        let legend = (names.len() > 1).then(|| {
            h_flex()
                .flex_none()
                .flex_wrap()
                .gap(rems(0.923))
                .px(rems(1.231))
                .pb(rems(0.462))
                .text_size(TEXT_XS)
                .text_color(theme.muted_foreground)
                .children(names.iter().zip(colors.iter()).map(|(name, color)| {
                    h_flex()
                        .gap(rems(0.385))
                        .child(div().size(rems(0.615)).rounded(px(2.)).bg(*color))
                        .child(name.clone())
                }))
        });
        let plot = ResultPlot { data: data.clone(), kind, labels, names, colors, ticks: ticks.clone(), font };
        let axis = ValueAxis { ticks, labels: tick_labels, font };
        v_flex()
            .size_full()
            .child(header)
            .children(legend)
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .pr(rems(1.231))
                    .pb(rems(0.615))
                    .child(div().flex_none().w(px(gutter)).h_full().pb(SCROLLBAR_LANE).child(axis))
                    .child(
                        div()
                            .relative()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .child(
                                div()
                                    .id("chart-plot")
                                    .debug_selector(|| "chart-plot".into())
                                    .size_full()
                                    .overflow_x_scroll()
                                    .track_scroll(&self.plot_scroll)
                                    .child(div().h_full().w_full().min_w(px(width)).pb(SCROLLBAR_LANE).child(plot)),
                            )
                            .hover_scrollbar(&self.plot_scroll, ScrollbarAxis::Horizontal),
                    ),
            )
            .into_any_element()
    }

    // Bars or Line and the aggregate, then the label column, then the value columns, filtered once there are many.
    fn panel(&self, columns: &[Column], profiles: &[Profile], window: &mut Window, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let settings = &self.settings;
        let numeric: Vec<usize> =
            (0..columns.len()).filter(|&ix| profiles.get(ix).is_some_and(|p| p.numeric)).collect();
        let filtered = numeric.len() > FILTER_FROM;
        let needle = if filtered { self.needle.as_str() } else { "" };
        let picked = settings.picked().count();
        let value_rows: Vec<Stateful<Div>> = numeric
            .iter()
            .copied()
            .filter(|&ix| {
                let column = &columns[ix];
                needle.is_empty() || rank_candidate(needle, &column.name, &[&column.type_name]).is_some()
            })
            .map(|ix| {
                let slot = settings.slot_of(ix);
                let open = slot.is_some() || picked < SLOTS;
                let on = slot.map(|slot| slot_color(slot, &theme));
                let (name, type_name) = (columns[ix].name.clone(), columns[ix].type_name.clone());
                let row = form::pick_row(format!("chart-value-{ix}").into(), on, open, name, type_name, cx)
                    .debug_selector(move || format!("chart-value-{ix}"));
                match open {
                    true => row.on_click(cx.listener(move |this, _, _, cx| {
                        this.edit(cx, |s| {
                            s.toggle_value(ix);
                        })
                    })),
                    false => row,
                }
            })
            .collect();
        let kind = settings.kind;
        let kinds = form::toggle_group().children(
            [(Kind::Bars, "chart-bars", "chart.bar"), (Kind::Line, "chart-line", "chart.line")].map(
                |(choice, id, key)| {
                    form::toggle(id, t(cx, key), kind == choice)
                        .debug_selector(move || id.into())
                        .on_click(cx.listener(move |this, _, _, cx| this.edit(cx, |s| s.kind = choice)))
                },
            ),
        );
        let aggregate = settings.aggregate;
        let choices = [
            (Aggregate::Sum, "chart-sum", "chart.sum"),
            (Aggregate::Count, "chart-count", "chart.count"),
            (Aggregate::Average, "chart-average", "chart.average"),
        ];
        let aggregates = form::toggle_group().children(choices.map(|(choice, id, key)| {
            form::toggle(id, t(cx, key), aggregate == choice)
                .debug_selector(move || id.into())
                .disabled(choice != Aggregate::Count && numeric.is_empty())
                .on_click(cx.listener(move |this, _, _, cx| this.edit(cx, |s| s.aggregate = choice)))
        }));
        let hint = match numeric.is_empty() {
            true => t(cx, "chart.noNumbers"),
            false => t(cx, "chart.noMatch"),
        };
        let values = match value_rows.is_empty() {
            true => div()
                .px(rems(0.923))
                .text_size(TEXT_XS)
                .text_color(theme.muted_foreground)
                .child(hint)
                .into_any_element(),
            false => div()
                .relative()
                .size_full()
                .child(
                    v_flex()
                        .id("chart-values")
                        .debug_selector(|| "chart-values".into())
                        .size_full()
                        .overflow_y_scroll()
                        .track_scroll(&self.values_scroll)
                        .px(rems(0.462))
                        .pb(rems(0.462))
                        .children(value_rows),
                )
                .hover_scrollbar(&self.values_scroll, ScrollbarAxis::Vertical)
                .into_any_element(),
        };
        v_flex()
            .flex_none()
            .w(PANEL_WIDTH)
            .h_full()
            .border_l_1()
            .border_color(theme.border)
            .bg(theme.sidebar)
            .debug_selector(|| "chart-panel".into())
            .child(v_flex().flex_none().gap(rems(0.462)).p(rems(0.769)).child(kinds).child(aggregates))
            .child(caption(t(cx, "chart.labels"), None, cx))
            .child(div().flex_none().px(rems(0.769)).child(self.label_select(columns, window, cx)))
            .child(caption(t(cx, "chart.values"), Some(format!("{picked}/{SLOTS}").into()), cx))
            .when(filtered, |el| {
                el.child(
                    div()
                        .flex_none()
                        .px(rems(0.769))
                        .pb(rems(0.462))
                        .debug_selector(|| "chart-filter".into())
                        .child(form::filter_input(&self.filter, window, cx)),
                )
            })
            .child(div().flex_1().min_h_0().child(values))
    }

    // Any column can label, from a menu that scrolls.
    fn label_select(&self, columns: &[Column], window: &mut Window, cx: &mut Context<Self>) -> Div {
        let current = self.settings.label;
        let name = columns.get(current).map(|column| column.name.clone()).unwrap_or_default();
        let options: Vec<(usize, SharedString, SharedString)> =
            columns.iter().enumerate().map(|(ix, c)| (ix, c.name.clone(), c.type_name.clone())).collect();
        let this = cx.entity().downgrade();
        form::select("chart-label", name, cx).debug_selector(|| "chart-label".into()).select_menu(
            window,
            cx,
            move |mut menu, _, _| {
                for (ix, name, type_name) in &options {
                    let (this, ix, name, type_name) = (this.clone(), *ix, name.clone(), type_name.clone());
                    let item = PopupMenuItem::element(move |_, cx| {
                        h_flex()
                            .w_full()
                            .gap(rems(0.923))
                            .justify_between()
                            .child(div().min_w_0().truncate().child(name.clone()))
                            .child(
                                div()
                                    .flex_none()
                                    .text_size(TEXT_XS)
                                    .text_color(cx.theme().muted_foreground)
                                    .child(type_name.clone()),
                            )
                    });
                    menu = menu.item(item.checked(ix == current).on_click(move |_, _, cx| {
                        let _ = this.update(cx, |view, cx| view.edit(cx, |s| s.label = ix));
                    }));
                }
                menu.scrollable(true)
            },
        )
    }
}

impl Render for ChartView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (profiles, data) = self.sync(cx);
        let set = self.grid.read(cx).set();
        let rows = set.rows();
        let columns: Vec<Column> = set
            .columns
            .iter()
            .map(|c| Column { name: c.name.clone().into(), type_name: c.type_name.to_lowercase().into() })
            .collect();
        h_flex()
            .size_full()
            .debug_selector(|| "result-chart".into())
            .child(
                div().flex_1().min_w_0().h_full().child(self.plot_area(rows, &columns, &profiles, &data, window, cx)),
            )
            .child(self.panel(&columns, &profiles, window, cx))
    }
}

#[cfg(test)]
impl ChartView {
    pub(crate) fn settings(&self) -> &Settings {
        &self.settings
    }

    pub(crate) fn shown(&self) -> Option<Rc<ChartData>> {
        self.data.as_ref().map(|(_, _, data)| data.clone())
    }

    // How far the plot scrolls sideways.
    pub(crate) fn plot_overflow(&self) -> Pixels {
        self.plot_scroll.max_offset().x
    }
}

// A section's caption, with a note at its end.
fn caption(text: SharedString, note: Option<SharedString>, cx: &App) -> Div {
    h_flex()
        .flex_none()
        .h(rems(CAPTION_HEIGHT))
        .gap(rems(0.615))
        .justify_between()
        .px(rems(0.923))
        .child(form::caption(text, cx))
        .children(note.map(|note| div().text_size(TEXT_XS).text_color(cx.theme().muted_foreground).child(note)))
}

fn message(text: SharedString, cx: &App) -> AnyElement {
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .p(rems(1.846))
        .text_center()
        .text_color(cx.theme().muted_foreground)
        .debug_selector(|| "chart-message".into())
        .child(text)
        .into_any_element()
}

// What to pick, while nothing is.
fn how_to(cx: &App) -> AnyElement {
    let theme = cx.theme();
    let point = |key: &str| {
        h_flex()
            .items_start()
            .gap(rems(0.615))
            .child(div().flex_none().mt(rems(0.462)).size(rems(0.385)).rounded_full().bg(theme.muted_foreground))
            .child(div().flex_1().min_w_0().child(t(cx, key)))
    };
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .p(rems(1.846))
        .child(
            v_flex()
                .debug_selector(|| "chart-how-to".into())
                .max_w(rems(27.692))
                .gap(rems(0.615))
                .p(rems(1.231))
                .rounded(RADIUS)
                .border_1()
                .border_color(theme.border)
                .text_size(TEXT_SM)
                .text_color(theme.muted_foreground)
                .child(
                    div()
                        .text_size(TEXT_BASE)
                        .font_semibold()
                        .text_color(theme.foreground)
                        .child(t(cx, "chart.howTitle")),
                )
                .child(point("chart.howLabels"))
                .child(point("chart.howValues"))
                .child(point("chart.howAggregate")),
        )
        .into_any_element()
}

// "Sum of revenue, cost by region", or "Rows by region" when rows are counted.
fn title(settings: &Settings, data: &ChartData, columns: &[Column], cx: &App) -> SharedString {
    let label = columns.get(settings.label).map(|column| column.name.to_string()).unwrap_or_default();
    let values: Vec<&str> = data
        .series
        .iter()
        .filter_map(|series| columns.get(series.column?))
        .map(|column| column.name.as_ref())
        .collect();
    let key = match (values.is_empty(), settings.aggregate) {
        (true, _) => "chart.rowsBy",
        (false, Aggregate::Sum) => "chart.sumBy",
        (false, Aggregate::Count) => "chart.countBy",
        (false, Aggregate::Average) => "chart.averageBy",
    };
    t_with(cx, key, &[("values", &values.join(", ")), ("label", &label)])
}

// The rows read, and which labels were left out.
fn meta(data: &ChartData, cx: &App) -> String {
    let rows = match (data.rows_cut, data.rows) {
        (true, _) => t_with(cx, "chart.cappedRows", &[("count", &count(cx, MAX_ROWS))]),
        (false, 1) => t_with(cx, "chart.rowCount_one", &[("count", &count(cx, 1))]),
        (false, rows) => t_with(cx, "chart.rowCount_other", &[("count", &count(cx, rows))]),
    };
    let cut = data.cut.map(|cut| {
        let key = match cut {
            Cut::Top => "chart.cappedTop",
            Cut::First => "chart.cappedFirst",
        };
        t_with(cx, key, &[("shown", &count(cx, data.labels.len())), ("total", &count(cx, data.groups))])
    });
    std::iter::once(rows).chain(cut).map(|part| part.to_string()).collect::<Vec<_>>().join(" · ")
}

fn series_name(series: &Series, columns: &[Column], cx: &App) -> SharedString {
    series
        .column
        .and_then(|column| columns.get(column))
        .map_or_else(|| t(cx, "chart.rows"), |column| column.name.clone())
}

// The date of a timestamp at midnight, like 2026-07-04 of 2026-07-04T00:00:00Z.
fn midnight(label: &str) -> Option<&str> {
    let (date, time) = label.split_at_checked(10)?;
    let zeros = time.chars().all(|c| matches!(c, '0' | ':' | '.' | 'T' | 'Z' | ' ' | '+'));
    (date.as_bytes()[4] == b'-' && zeros).then_some(date)
}

// NULL as the grid writes it, and an empty label as words. A label keeps to one line.
fn label_text(label: Option<&str>, cx: &App) -> SharedString {
    match label {
        None => "NULL".into(),
        Some("") => t(cx, "chart.blank"),
        Some(text) => text.replace(['\n', '\r'], " ").into(),
    }
}

fn slot_color(slot: usize, theme: &Theme) -> Hsla {
    [theme.chart_1, theme.chart_2, theme.chart_3, theme.chart_4, theme.chart_5][slot % SLOTS]
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::Arc;

    use barsql_db::{ChunkBuilder, ColumnMeta};
    use gpui_kit::component::Root;
    use gpui_kit::{AppContext as _, Entity, TestAppContext, VisualTestContext};

    use super::{Aggregate, ChartView};
    use crate::grid::Grid;
    use crate::test_support::Env;

    fn picks(view: &Entity<ChartView>, cx: &mut VisualTestContext) -> (usize, Vec<usize>, Aggregate) {
        cx.update(|window, cx| window.draw(cx).clear(cx));
        view.read_with(cx, |view, _| {
            let settings = view.settings();
            (settings.label, settings.picked().map(|(_, column)| column).collect(), settings.aggregate)
        })
    }

    // A chart opened before the rows arrive picks its columns once they do, and keeps following them until the panel
    // is used.
    #[gpui_kit::test]
    fn the_picks_follow_streamed_rows_until_the_panel_is_used(cx: &mut TestAppContext) {
        let _env = Env::new(cx);
        let columns: Arc<[ColumnMeta]> = vec![
            ColumnMeta { name: "region".into(), type_name: "TEXT".into() },
            ColumnMeta { name: "sales".into(), type_name: "INTEGER".into() },
        ]
        .into();
        let slot = Rc::new(RefCell::new(None));
        let out = slot.clone();
        let window = cx.add_window(move |window, cx| {
            let grid = cx.new(|cx| Grid::new(columns, cx));
            let view = cx.new(|cx| ChartView::new(grid.clone(), window, cx));
            *out.borrow_mut() = Some((grid, view.clone()));
            Root::new(view, window, cx)
        });
        let cx = VisualTestContext::from_window(window.into(), cx).into_mut();
        let (grid, view) = slot.borrow_mut().take().unwrap();
        assert_eq!(picks(&view, cx), (0, vec![], Aggregate::Count), "nothing to measure yet");
        let push = |rows: &[(&str, &str)], cx: &mut VisualTestContext| {
            let mut chunk = ChunkBuilder::new(2, rows.len());
            for (region, sales) in rows {
                chunk.push_text(|s| s.push_str(region));
                chunk.push_number(|s| s.push_str(sales));
                chunk.end_row();
            }
            grid.update(cx, |grid, cx| grid.push(Arc::new(chunk.finish()), cx));
        };
        push(&[("North", "3"), ("South", "4")], cx);
        assert_eq!(picks(&view, cx), (0, vec![1], Aggregate::Sum));
        view.update(cx, |view, cx| view.edit(cx, |s| s.aggregate = Aggregate::Average));
        push(&[("East", "5")], cx);
        assert_eq!(picks(&view, cx), (0, vec![1], Aggregate::Average), "a used panel keeps its picks");
    }
}
