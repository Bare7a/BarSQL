use std::cell::RefCell;
use std::rc::Rc;

use gpui_kit::component::scroll::{Scrollbar, ScrollbarHandle};
use gpui_kit::{Axis, Bounds, Pixels, Point, Size, point, px, size};

// Width of each bar's lane. The bars' tracks are pinned to it, so a theme can't make a bar wider than its lane.
pub const LANE: Pixels = Scrollbar::width();

// Scroll state of the cells right of the gutter and below the header. Shared with the scrollbars.
#[derive(Clone, Default)]
pub struct GridScroll(Rc<RefCell<State>>);

#[derive(Default)]
struct State {
    // Positive distances. ScrollbarHandle offsets are negated. Down is an f64, as past a few million rows an f32 holds
    // only multiples of 8px: rows would be spaced unevenly and small scrolls lost.
    x: Pixels,
    y: f64,
    // The whole grid, with its gutter and header.
    grid: Bounds<Pixels>,
    viewport: Bounds<Pixels>,
    width: Pixels,
    height: f64,
    // The bars' lanes along the grid's right and bottom edges. Zero on an axis that doesn't scroll.
    lanes: Size<Pixels>,
}

impl State {
    fn max(&self) -> (Pixels, f64) {
        let viewport = self.viewport.size;
        ((self.width - viewport.width).max(px(0.)), (self.height - f64::from(viewport.height)).max(0.))
    }

    fn clamp(&mut self) {
        let (x, y) = self.max();
        (self.x, self.y) = (self.x.clamp(px(0.), x), self.y.clamp(0., y));
    }
}

impl GridScroll {
    // Down rounds to an f32 here. Row positions need `y`.
    pub fn position(&self) -> Point<Pixels> {
        let state = self.0.borrow();
        point(state.x, px(state.y as f32))
    }

    pub fn y(&self) -> f64 {
        self.0.borrow().y
    }

    pub fn viewport(&self) -> Bounds<Pixels> {
        self.0.borrow().viewport
    }

    // The whole grid, with its gutter and header.
    pub fn grid(&self) -> Bounds<Pixels> {
        self.0.borrow().grid
    }

    pub fn lanes(&self) -> Size<Pixels> {
        self.0.borrow().lanes
    }

    // The strips beside and below the cells, with the corner where they meet. Empty where the axis doesn't scroll.
    pub fn lane_bounds(&self) -> [Bounds<Pixels>; 2] {
        let state = self.0.borrow();
        let (grid, cells) = (state.grid, state.viewport);
        [
            Bounds::from_corners(point(cells.right(), grid.top()), grid.bottom_right()),
            Bounds::from_corners(point(grid.left(), cells.bottom()), grid.bottom_right()),
        ]
    }

    pub fn max(&self) -> Point<Pixels> {
        let (x, y) = self.0.borrow().max();
        point(x, px(y as f32))
    }

    // `origin` is the top left of the cells, right of the gutter and below the header. Each axis that scrolls keeps
    // a lane for its bar along the grid's far edge, so the bar never covers cells. Returns the cells' viewport, which
    // stops at the lanes.
    pub fn set_layout(
        &self,
        grid: Bounds<Pixels>,
        origin: Point<Pixels>,
        width: Pixels,
        height: f64,
    ) -> Bounds<Pixels> {
        let lane = |scrolls: bool| if scrolls { LANE } else { px(0.) };
        let room = size(grid.right() - origin.x, grid.bottom() - origin.y);
        // A lane takes room from the other axis, which can then scroll too.
        let down = height > f64::from(room.height);
        let across = width > room.width - lane(down);
        let down = height > f64::from(room.height - lane(across));
        let lanes = size(lane(down), lane(across));
        let viewport = size((room.width - lanes.width).max(px(0.)), (room.height - lanes.height).max(px(0.)));
        let mut state = self.0.borrow_mut();
        state.grid = grid;
        state.lanes = lanes;
        state.viewport = Bounds::new(origin, viewport);
        (state.width, state.height) = (width, height);
        state.clamp();
        state.viewport
    }

    // Rows pushed since the last layout, so a reveal before the next one can reach them.
    pub fn set_rows_height(&self, height: f64) {
        self.0.borrow_mut().height = height;
    }

    // Whether the position moved.
    pub fn scroll_to(&self, x: Pixels, y: f64) -> bool {
        let mut state = self.0.borrow_mut();
        let before = (state.x, state.y);
        (state.x, state.y) = (x, y);
        state.clamp();
        (state.x, state.y) != before
    }

    pub fn bar(&self, axis: Axis) -> BarHandle {
        BarHandle { scroll: self.clone(), axis }
    }
}

// GPUI Kit draws a bar inside the far edge of its handle's viewport, so this viewport takes in the bar's lane. It runs
// the grid's whole length, past the header or gutter, and stops at the other bar's lane.
#[derive(Clone)]
pub struct BarHandle {
    scroll: GridScroll,
    axis: Axis,
}

impl ScrollbarHandle for BarHandle {
    fn viewport_bounds(&self) -> Bounds<Pixels> {
        let state = self.scroll.0.borrow();
        let (grid, cells, lanes) = (state.grid, state.viewport, state.lanes);
        match self.axis {
            Axis::Vertical => {
                Bounds::from_corners(point(cells.left(), grid.top()), point(grid.right(), grid.bottom() - lanes.height))
            }
            Axis::Horizontal => {
                Bounds::from_corners(point(grid.left(), cells.top()), point(grid.right() - lanes.width, grid.bottom()))
            }
        }
    }

    fn offset(&self) -> Point<Pixels> {
        -self.scroll.position()
    }

    // A bar moves its own axis only. It hands back the other one rounded to an f32.
    fn set_offset(&self, offset: Point<Pixels>) {
        let (x, y) = (self.scroll.position().x, self.scroll.y());
        match self.axis {
            Axis::Vertical => self.scroll.scroll_to(x, -f64::from(offset.y)),
            Axis::Horizontal => self.scroll.scroll_to(-offset.x, y),
        };
    }

    // As much longer than the cells as the bar is than the viewport, so the thumb reaches the end of its track just as
    // the cells reach the end of theirs.
    fn content_size(&self) -> Size<Pixels> {
        let bar = self.viewport_bounds().size;
        let state = self.scroll.0.borrow();
        size(state.width, px(state.height as f32)) + bar - state.viewport.size
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A 450x330 grid with a 50px gutter and a 30px header, so its cells have 400x300.
    fn lay_out(content: Size<Pixels>) -> GridScroll {
        let scroll = GridScroll::default();
        let grid = Bounds::new(point(px(0.), px(0.)), size(px(450.), px(330.)));
        scroll.set_layout(grid, point(px(50.), px(30.)), content.width, f64::from(content.height));
        scroll
    }

    // An f32 down there holds only multiples of 8px.
    #[test]
    fn three_million_rows_down_a_pixel_still_scrolls() {
        let (scroll, rows) = (GridScroll::default(), 25. * 3_300_535.);
        let grid = Bounds::new(point(px(0.), px(0.)), size(px(450.), px(330.)));
        scroll.set_layout(grid, point(px(50.), px(30.)), px(1000.), rows);
        let far = rows - 1000.5;
        assert!(scroll.scroll_to(px(0.), far) && scroll.scroll_to(px(0.), far + 1.));
        assert_eq!(scroll.y(), far + 1.);
        scroll.bar(Axis::Horizontal).set_offset(point(px(-40.), -scroll.position().y));
        assert_eq!((scroll.position().x, scroll.y()), (px(40.), far + 1.), "a sideways bar keeps the place down");
    }

    fn viewport(width: f32, height: f32) -> (f32, f32) {
        let viewport = lay_out(size(px(width), px(height))).viewport().size;
        (f32::from(viewport.width), f32::from(viewport.height))
    }

    #[test]
    fn each_axis_that_scrolls_keeps_a_lane_for_its_bar() {
        let lane = f32::from(Scrollbar::width());
        let (narrow, short) = (400. - lane, 300. - lane);
        assert_eq!(viewport(300., 200.), (400., 300.), "content that fits keeps no lanes");
        assert_eq!(viewport(300., 1000.), (narrow, 300.));
        assert_eq!(viewport(1000., 200.), (400., short));
        assert_eq!(viewport(1000., 1000.), (narrow, short));
        assert_eq!(viewport(1000., 290.), (narrow, short), "rows that fit only without the bottom lane");
        assert_eq!(viewport(390., 1000.), (narrow, short), "columns that fit only without the right lane");
    }

    #[test]
    fn each_bar_runs_the_grid_up_to_the_other_lane_and_spans_the_whole_scroll() {
        let scroll = lay_out(size(px(1000.), px(1000.)));
        let [down, across] = [Axis::Vertical, Axis::Horizontal].map(|axis| scroll.bar(axis));
        let (right, bottom) = (px(450.), px(330.));
        let lane = Scrollbar::width();
        let beside_the_header = Bounds::from_corners(point(px(50.), px(0.)), point(right, bottom - lane));
        let below_the_gutter = Bounds::from_corners(point(px(0.), px(30.)), point(right - lane, bottom));
        assert_eq!(down.viewport_bounds(), beside_the_header);
        assert_eq!(across.viewport_bounds(), below_the_gutter);
        // A thumb's travel along its track maps onto the whole scroll.
        let travel = |bar: &BarHandle| bar.content_size() - bar.viewport_bounds().size;
        assert_eq!(point(travel(&across).width, travel(&down).height), scroll.max());
    }
}
