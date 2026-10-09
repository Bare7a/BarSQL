use barsql_db::{Cell, ResultSet};
use gpui_kit::component::button::{Button, ButtonGroup, ButtonVariants};
use gpui_kit::component::chart::{BarChart, LineChart};
use gpui_kit::component::menu::{DropdownMenu, PopupMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, Disableable, Selectable, Sizable, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::grid::Grid;
use crate::i18n::{t, t_with};
use crate::tokens::{TEXT_SM, TEXT_XS};

// Rows a chart reads. Bars group by label, so at most BAR_LIMIT labels show; a line plots LINE_LIMIT points.
const MAX_ROWS: usize = 10_000;
const BAR_LIMIT: usize = 60;
const LINE_LIMIT: usize = 2_000;
// Rows sampled to tell numeric columns.
const SAMPLE_ROWS: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChartKind {
    Bar,
    Line,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Point {
    pub label: SharedString,
    pub value: f64,
}

// Columns whose non-NULL cells are all numbers, among the first rows.
pub fn numeric_columns(set: &ResultSet) -> Vec<usize> {
    let rows = set.rows().min(SAMPLE_ROWS);
    (0..set.columns.len())
        .filter(|&column| {
            let cells = (0..rows).map(|row| set.cell(row, column));
            let mut numbers = cells.filter(|cell| *cell != Cell::Null).peekable();
            numbers.peek().is_some() && numbers.all(|cell| matches!(cell, Cell::Number(_)))
        })
        .collect()
}

// Bars add up the values of each label, in the order labels first appear. A line takes the rows in order. The flag
// says some rows were left out.
pub fn points(set: &ResultSet, x: usize, y: usize, kind: ChartKind) -> (Vec<Point>, bool) {
    let mut out: Vec<Point> = Vec::new();
    let mut truncated = set.rows() > MAX_ROWS;
    for row in 0..set.rows().min(MAX_ROWS) {
        let Some(value) = set.display(row, y).and_then(|text| text.trim().parse::<f64>().ok()) else { continue };
        let label = SharedString::from(set.display(row, x).unwrap_or("NULL").to_string());
        match kind {
            ChartKind::Bar => match out.iter().position(|point| point.label == label) {
                Some(ix) => out[ix].value += value,
                None if out.len() < BAR_LIMIT => out.push(Point { label, value }),
                None => truncated = true,
            },
            ChartKind::Line if out.len() < LINE_LIMIT => out.push(Point { label, value }),
            ChartKind::Line => {
                truncated = true;
                break;
            }
        }
    }
    (out, truncated)
}

// A quick look at a result: one numeric column against a label column, as bars or a line.
pub struct ChartView {
    grid: Entity<Grid>,
    kind: ChartKind,
    x: usize,
    y: Option<usize>,
}

impl ChartView {
    // Labels from the first column that isn't numeric, else the first one. Values from the last other number, since
    // a query's measures, like its aggregates, usually come last.
    pub fn new(grid: Entity<Grid>, cx: &App) -> Self {
        let set = grid.read(cx).set();
        let numeric = numeric_columns(set);
        let x = (0..set.columns.len()).find(|column| !numeric.contains(column)).unwrap_or(0);
        let y = numeric.iter().copied().rfind(|&column| column != x);
        Self { grid, kind: ChartKind::Bar, x, y }
    }

    fn picker(
        &self,
        id: &'static str,
        label: SharedString,
        current: Option<usize>,
        columns: Vec<(usize, SharedString)>,
        pick: fn(&mut Self, usize),
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let name = current.and_then(|ix| columns.iter().find(|(c, _)| *c == ix)).map(|(_, name)| name.clone());
        let this = cx.entity().downgrade();
        Button::new(id)
            .debug_selector(move || id.into())
            .small()
            .ghost()
            .label(format!("{label}: {}", name.unwrap_or_default()))
            .dropdown_caret(true)
            .disabled(columns.is_empty())
            .dropdown_menu(move |mut menu: PopupMenu, _, _| {
                for (ix, name) in &columns {
                    let (this, ix) = (this.clone(), *ix);
                    menu = menu.item(PopupMenuItem::new(name.clone()).checked(current == Some(ix)).on_click(
                        move |_, _, cx| {
                            let _ = this.update(cx, |view, cx| {
                                pick(view, ix);
                                cx.notify();
                            });
                        },
                    ));
                }
                menu
            })
    }
}

impl Render for ChartView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let set = self.grid.read(cx).set();
        let names: Vec<(usize, SharedString)> =
            set.columns.iter().enumerate().map(|(ix, c)| (ix, SharedString::from(c.name.clone()))).collect();
        let numeric = numeric_columns(set);
        let values: Vec<(usize, SharedString)> = names.iter().filter(|(ix, _)| numeric.contains(ix)).cloned().collect();
        let x = self.x.min(names.len().saturating_sub(1));
        let y = self.y.filter(|y| numeric.contains(y));
        let (data, truncated) = y.map_or((Vec::new(), false), |y| points(set, x, y, self.kind));
        let series = y.and_then(|y| names.get(y)).map(|(_, name)| name.clone()).unwrap_or_default();
        let kind = self.kind;
        let controls = h_flex()
            .flex_none()
            .gap(rems(0.615))
            .px(rems(0.769))
            .py(rems(0.462))
            .border_b_1()
            .border_color(theme.border)
            .text_size(TEXT_SM)
            .child(
                ButtonGroup::new("chart-kind")
                    .small()
                    .outline()
                    .child(Button::new("chart-bar").label(t(cx, "chart.bar")).selected(kind == ChartKind::Bar))
                    .child(Button::new("chart-line").label(t(cx, "chart.line")).selected(kind == ChartKind::Line))
                    .on_click(cx.listener(|view, clicked: &Vec<usize>, _, cx| {
                        view.kind = if clicked.first() == Some(&1) { ChartKind::Line } else { ChartKind::Bar };
                        cx.notify();
                    })),
            )
            .child(self.picker("chart-x", t(cx, "chart.labels"), Some(x), names, |view, ix| view.x = ix, cx))
            .child(self.picker("chart-y", t(cx, "chart.values"), y, values, |view, ix| view.y = Some(ix), cx))
            .child(div().flex_1())
            .when(truncated, |el| {
                let limit = match kind {
                    ChartKind::Bar => BAR_LIMIT,
                    ChartKind::Line => LINE_LIMIT,
                };
                el.child(div().text_size(TEXT_XS).text_color(theme.muted_foreground).child(t_with(
                    cx,
                    "chart.truncated",
                    &[("count", &limit.to_string())],
                )))
            });
        let accent = theme.primary;
        let body = match (y, data.is_empty()) {
            (None, _) => empty(t(cx, "chart.noNumbers"), &theme),
            (Some(_), true) => empty(t(cx, "chart.noValues"), &theme),
            (Some(_), false) => match kind {
                ChartKind::Bar => BarChart::new(data)
                    .band(|point: &Point| point.label.clone())
                    .value(|point| point.value)
                    .name(series)
                    .fill(move |_, _, _, _| accent)
                    .corner_radii(Corners { top_left: px(4.), top_right: px(4.), ..Default::default() })
                    .value_axis(true)
                    .value_tick_count(4)
                    .id("result-chart-bar")
                    .into_any_element(),
                ChartKind::Line => LineChart::new(data)
                    .x(|point: &Point| point.label.clone())
                    .y(|point| point.value)
                    .stroke(accent)
                    .linear()
                    .name(series)
                    .y_axis(true)
                    .x_tick_count(6)
                    .id("result-chart-line")
                    .into_any_element(),
            },
        };
        v_flex()
            .size_full()
            .debug_selector(|| "result-chart".into())
            .child(controls)
            .child(div().flex_1().min_h_0().p(rems(1.231)).child(body))
    }
}

fn empty(message: SharedString, theme: &gpui_kit::component::Theme) -> AnyElement {
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .text_color(theme.muted_foreground)
        .child(message)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use barsql_db::{ChunkBuilder, ColumnMeta, ResultSet};

    use super::{ChartKind, numeric_columns, points};

    fn set(rows: &[(&str, Option<&str>)]) -> ResultSet {
        let columns: Arc<[ColumnMeta]> = vec![
            ColumnMeta { name: "city".into(), type_name: "TEXT".into() },
            ColumnMeta { name: "sales".into(), type_name: "INTEGER".into() },
        ]
        .into();
        let mut set = ResultSet::new(columns);
        let mut chunk = ChunkBuilder::new(2, rows.len());
        for (city, sales) in rows {
            chunk.push_text(|s| s.push_str(city));
            match sales {
                Some(n) => chunk.push_number(|s| s.push_str(n)),
                None => chunk.push_null(),
            }
            chunk.end_row();
        }
        set.push(Arc::new(chunk.finish()));
        set
    }

    #[test]
    fn bars_add_up_each_label_and_a_line_keeps_the_rows() {
        let set = set(&[("Sofia", Some("3")), ("Berlin", Some("2")), ("Sofia", Some("4")), ("Paris", None)]);
        assert_eq!(numeric_columns(&set), [1]);
        let bars = points(&set, 0, 1, ChartKind::Bar).0;
        let bars: Vec<(&str, f64)> = bars.iter().map(|p| (p.label.as_ref(), p.value)).collect();
        assert_eq!(bars, [("Sofia", 7.), ("Berlin", 2.)], "NULL values are left out");
        let line = points(&set, 0, 1, ChartKind::Line).0;
        assert_eq!(line.len(), 3);
    }
}
