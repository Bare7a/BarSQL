// The chart's drawing: grouped bars or lines over the labels. The value axis is a plot of its own, in a gutter
// beside this one, so it stays put while the bars scroll sideways. Both lay out by the same Frame.
use std::rc::Rc;

use gpui_kit::component::ActiveTheme;
use gpui_kit::component::plot::label::{PlotLabel, TEXT_GAP, Text, measure_text_width, truncate_text_to_width};
use gpui_kit::component::plot::shape::Line;
use gpui_kit::component::plot::tooltip::{CrossLine, Dot, Tooltip, TooltipState};
use gpui_kit::component::plot::{Curve, Grid, IntoPlot, Plot, axis_gutter};
use gpui_kit::*;

use super::data::{ChartData, Kind};
use crate::i18n::number;

// Bars are at most 36px wide and 2px apart within a label, and a label's group leaves a fifth of its room empty.
const MAX_BAR: f32 = 36.;
const MIN_BAR: f32 = 6.;
const BAR_GAP: f32 = 2.;
const GROUP_PAD: f32 = 0.2;
// Points of a line keep this far apart before the plot scrolls, and get dots up to this many.
const MIN_POINT_GAP: f32 = 3.;
const DOTS: usize = 60;
// Labels are cut to this width and keep this far apart.
const MAX_LABEL: f32 = 120.;
const LABEL_GAP: f32 = 8.;
// A label's width per character, as a share of the font size, to thin them out without shaping every one.
const CHAR_WIDTH: f32 = 0.62;

// Where values sit, top to bottom: half a label clear at the top, the labels' gutter at the bottom.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    pub top: f32,
    pub bottom: f32,
    lo: f64,
    hi: f64,
}

impl Frame {
    pub fn new(height: f32, font: f32, ticks: &[f64]) -> Self {
        let (lo, hi) = (ticks.first().copied().unwrap_or(0.), ticks.last().copied().unwrap_or(1.));
        let top = font / 2. + TEXT_GAP;
        Self { top, bottom: (height - axis_gutter(px(font))).max(top + 1.), lo, hi }
    }

    pub fn y(&self, value: f64) -> f32 {
        let span = (self.hi - self.lo).max(f64::EPSILON);
        self.bottom - ((value - self.lo) / span) as f32 * (self.bottom - self.top)
    }
}

// A group's bars: their width and the gap between them, for groups `step` apart.
pub fn bar_layout(step: f32, series: usize) -> (f32, f32) {
    let count = series.max(1) as f32;
    let gap = if series > 1 { BAR_GAP } else { 0. };
    let room = step * (1. - GROUP_PAD) - gap * (count - 1.);
    ((room / count).clamp(1., MAX_BAR), gap)
}

// Below this width the plot scrolls sideways, so bars stay at least 6px and points 3px apart.
pub fn min_width(groups: usize, series: usize, kind: Kind) -> f32 {
    let groups = groups as f32;
    match kind {
        Kind::Bars => {
            let count = series.max(1) as f32;
            groups * (count * MIN_BAR + (count - 1.) * BAR_GAP) / (1. - GROUP_PAD)
        }
        Kind::Line => groups * MIN_POINT_GAP,
    }
}

// Every how many groups a label fits, `width` wide, without touching the next.
pub fn label_stride(step: f32, width: f32) -> usize {
    ((width + LABEL_GAP) / step.max(f32::EPSILON)).ceil().max(1.) as usize
}

pub fn nearest(x: f32, step: f32, groups: usize) -> usize {
    ((x / step.max(f32::EPSILON)).floor().max(0.) as usize).min(groups.saturating_sub(1))
}

#[derive(IntoPlot)]
pub struct ResultPlot {
    pub data: Rc<ChartData>,
    pub kind: Kind,
    pub labels: Rc<[SharedString]>,
    pub names: Rc<[SharedString]>,
    pub colors: Rc<[Hsla]>,
    pub ticks: Rc<[f64]>,
    pub font: Pixels,
}

impl ResultPlot {
    fn frame(&self, bounds: &Bounds<Pixels>) -> Frame {
        Frame::new(bounds.size.height.as_f32(), self.font.as_f32(), &self.ticks)
    }

    fn step(&self, bounds: &Bounds<Pixels>) -> f32 {
        bounds.size.width.as_f32() / self.data.labels.len().max(1) as f32
    }

    // The group's bars side by side, centred on it.
    fn bar_x(&self, group: usize, series: usize, step: f32) -> (f32, f32) {
        let count = self.data.series.len();
        let (width, gap) = bar_layout(step, count);
        let span = width * count as f32 + gap * (count.max(1) - 1) as f32;
        (step * (group as f32 + 0.5) - span / 2. + series as f32 * (width + gap), width)
    }

    fn paint_bars(&self, frame: &Frame, step: f32, bounds: &Bounds<Pixels>, window: &mut Window) {
        let zero = frame.y(0.);
        for (k, series) in self.data.series.iter().enumerate() {
            let color = self.colors[k];
            for (group, value) in series.values.iter().enumerate() {
                let Some(value) = *value else { continue };
                let (x, width) = self.bar_x(group, k, step);
                let y = frame.y(value);
                let (top, height) = (y.min(zero), (y - zero).abs().max(1.));
                // Rounded at the end away from zero.
                let radius = px((width / 2.).min(3.));
                let corners = match value >= 0. {
                    true => Corners { top_left: radius, top_right: radius, ..Default::default() },
                    false => Corners { bottom_left: radius, bottom_right: radius, ..Default::default() },
                };
                let bar = Bounds::new(bounds.origin + point(px(x), px(top)), size(px(width), px(height)));
                window.paint_quad(fill(bar, color).corner_radii(corners));
            }
        }
    }

    // NULLs leave gaps in a line.
    fn paint_lines(&self, frame: &Frame, step: f32, bounds: &Bounds<Pixels>, window: &mut Window, cx: &App) {
        let dots = self.data.labels.len() <= DOTS;
        for (k, series) in self.data.series.iter().enumerate() {
            let color = self.colors[k];
            let mut runs: Vec<Vec<(f32, f32)>> = vec![Vec::new()];
            for (group, value) in series.values.iter().enumerate() {
                match value {
                    Some(value) => runs.last_mut().unwrap().push((step * (group as f32 + 0.5), frame.y(*value))),
                    None if runs.last().is_some_and(|run| !run.is_empty()) => runs.push(Vec::new()),
                    None => {}
                }
            }
            for run in runs.into_iter().filter(|run| !run.is_empty()) {
                let line = Line::new()
                    .data(run)
                    .x(|p: &(f32, f32)| Some(p.0))
                    .y(|p: &(f32, f32)| Some(p.1))
                    .stroke(color)
                    .stroke_width(px(2.))
                    .curve(Curve::Linear);
                let line = match dots {
                    true => line.dot().dot_size(px(6.)).dot_fill(cx.theme().background).dot_stroke(color),
                    false => line,
                };
                line.paint(bounds, window);
            }
        }
    }

    // As many labels as fit, cut to their room, each centred under its group but kept inside the plot. A label that
    // would touch the one before it, as one pushed in from an edge can, is left out.
    fn paint_labels(&self, frame: &Frame, step: f32, bounds: &Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        let font = self.font.as_f32();
        let longest = self.labels.iter().map(|label| label.chars().count()).max().unwrap_or(0);
        let stride = label_stride(step, (longest as f32 * font * CHAR_WIDTH).min(MAX_LABEL));
        let room = (step * stride as f32 - LABEL_GAP).min(MAX_LABEL);
        let (width, color) = (bounds.size.width.as_f32(), cx.theme().muted_foreground);
        let mut end = f32::NEG_INFINITY;
        let mut texts = Vec::new();
        for group in (0..self.labels.len()).step_by(stride) {
            let text = truncate_text_to_width(&self.labels[group], self.font, room, window);
            let half = measure_text_width(&text, self.font, window) / 2.;
            let x = (step * (group as f32 + 0.5)).clamp(half, (width - half).max(half));
            if x - half < end + LABEL_GAP {
                continue;
            }
            end = x + half;
            let at = point(px(x), px(frame.bottom + TEXT_GAP * 3.));
            texts.push(Text::new(text, at, color).font_size(self.font).align(TextAlign::Center));
        }
        PlotLabel::new(texts).paint(bounds, window, cx);
    }
}

impl Plot for ResultPlot {
    fn paint(&mut self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        if self.data.labels.is_empty() {
            return;
        }
        let (frame, step) = (self.frame(&bounds), self.step(&bounds));
        let theme = cx.theme();
        let (grid, axis) = (theme.chart_grid, theme.border);
        Grid::new()
            .y(self.ticks.iter().map(|tick| px(frame.y(*tick))))
            .stroke(grid)
            .dash_array(&[px(4.), px(2.)])
            .paint(&bounds, window);
        // The zero line, or the bottom when every value is above it.
        let base = frame.y(0f64.clamp(self.ticks[0], self.ticks[self.ticks.len() - 1]));
        let line = Bounds::new(bounds.origin + point(px(0.), px(base)), size(bounds.size.width, px(1.)));
        window.paint_quad(fill(line, axis));
        match self.kind {
            Kind::Bars => self.paint_bars(&frame, step, &bounds, window),
            Kind::Line => self.paint_lines(&frame, step, &bounds, window, cx),
        }
        self.paint_labels(&frame, step, &bounds, window, cx);
    }

    fn id(&self) -> Option<ElementId> {
        Some("result-chart-plot".into())
    }

    fn tooltip_state(&self, position: Point<Pixels>, bounds: Bounds<Pixels>, _: &App) -> Option<TooltipState> {
        let groups = self.data.labels.len();
        if groups == 0 || position.y.as_f32() > self.frame(&bounds).bottom {
            return None;
        }
        let step = self.step(&bounds);
        let group = nearest(position.x.as_f32(), step, groups);
        Some(TooltipState::new(group, point(px(step * (group as f32 + 0.5)), position.y), vec![]))
    }

    fn tooltip(
        &self,
        state: &TooltipState,
        cursor: Point<Pixels>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        let group = state.index;
        let label = self.labels.get(group)?.clone();
        let (frame, step) = (self.frame(&bounds), self.step(&bounds));
        let cross = CrossLine::new(state.cross_line).height(frame.bottom);
        let cross = match self.kind {
            Kind::Bars => cross.band(px(step * (1. - GROUP_PAD / 2.))),
            Kind::Line => cross,
        };
        let mut tooltip = Tooltip::new(cursor, bounds.size).gap(px(8.)).cross_line(cross).title(label);
        if self.kind == Kind::Line {
            let background = cx.theme().background;
            let dots = self.data.series.iter().zip(self.colors.iter()).filter_map(|(series, color)| {
                let y = frame.y(series.values[group]?);
                Some(Dot::new(point(state.cross_line.x, px(y))).size(px(8.)).stroke(*color).fill(background))
            });
            tooltip = tooltip.dots(dots.collect::<Vec<_>>());
        }
        for ((series, name), color) in self.data.series.iter().zip(self.names.iter()).zip(self.colors.iter()) {
            let value = series.values[group].map_or_else(|| "NULL".to_string(), |value| number(cx, value, 2));
            tooltip = tooltip.row(*color, name.clone(), value);
        }
        Some(tooltip.into_any_element())
    }
}

// The tick values, right-aligned beside the plot.
#[derive(IntoPlot)]
pub struct ValueAxis {
    pub ticks: Rc<[f64]>,
    pub labels: Rc<[SharedString]>,
    pub font: Pixels,
}

impl Plot for ValueAxis {
    fn paint(&mut self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        let font = self.font.as_f32();
        let frame = Frame::new(bounds.size.height.as_f32(), font, &self.ticks);
        let right = bounds.size.width.as_f32() - LABEL_GAP;
        let color = cx.theme().muted_foreground;
        let texts = self
            .ticks
            .iter()
            .zip(self.labels.iter())
            .map(|(tick, label)| {
                let at = point(px(right), px(frame.y(*tick) - font / 2.));
                Text::new(label.clone(), at, color).font_size(self.font).align(TextAlign::Right)
            })
            .collect();
        PlotLabel::new(texts).paint(&bounds, window, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::{Frame, bar_layout, label_stride, min_width, nearest};
    use crate::chart_view::data::Kind;

    #[test]
    fn bars_cap_their_width_and_share_a_group() {
        assert_eq!(bar_layout(100., 1), (36., 0.), "wide groups keep bars slim");
        let (width, gap) = bar_layout(40., 3);
        assert_eq!(gap, 2.);
        assert!((width * 3. + gap * 2. - 32.).abs() < 1e-4, "a fifth of the group stays empty");
        assert_eq!(bar_layout(2., 4).0, 1., "never thinner than a pixel");
    }

    #[test]
    fn many_groups_need_room_to_scroll() {
        assert_eq!(min_width(200, 1, Kind::Bars), 1500.);
        assert_eq!(min_width(100, 2, Kind::Bars), 1750.);
        assert_eq!(min_width(1000, 3, Kind::Line), 3000.);
    }

    #[test]
    fn labels_thin_out_to_fit_and_the_pointer_finds_its_group() {
        assert_eq!(label_stride(100., 40.), 1);
        assert_eq!(label_stride(10., 40.), 5);
        assert_eq!(nearest(-5., 10., 4), 0);
        assert_eq!(nearest(25., 10., 4), 2);
        assert_eq!(nearest(500., 10., 4), 3);
    }

    #[test]
    fn the_frame_maps_ticks_between_its_margins() {
        let frame = Frame::new(200., 10., &[0., 50., 100.]);
        assert_eq!((frame.y(0.), frame.y(100.)), (frame.bottom, frame.top));
        assert_eq!(frame.bottom, 182., "the labels take 18px");
        assert!((frame.y(50.) - (frame.top + frame.bottom) / 2.).abs() < 1e-4);
    }
}
