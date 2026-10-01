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
    // Positive distances. ScrollbarHandle offsets are negated.
    position: Point<Pixels>,
    // The whole grid, with its gutter and header.
    grid: Bounds<Pixels>,
    viewport: Bounds<Pixels>,
    content: Size<Pixels>,
    // The bars' lanes along the grid's right and bottom edges. Zero on an axis that doesn't scroll.
    lanes: Size<Pixels>,
}

impl State {
    fn max(&self) -> Point<Pixels> {
        point(
            (self.content.width - self.viewport.size.width).max(px(0.)),
            (self.content.height - self.viewport.size.height).max(px(0.)),
        )
    }

    fn clamp(&mut self) {
        let max = self.max();
        self.position = point(self.position.x.clamp(px(0.), max.x), self.position.y.clamp(px(0.), max.y));
    }
}

impl GridScroll {
    pub fn position(&self) -> Point<Pixels> {
        self.0.borrow().position
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
        self.0.borrow().max()
    }

    // `origin` is the top left of the cells, right of the gutter and below the header. Each axis that scrolls keeps
    // a lane for its bar along the grid's far edge, so the bar never covers cells. Returns the cells' viewport, which
    // stops at the lanes.
    pub fn set_layout(&self, grid: Bounds<Pixels>, origin: Point<Pixels>, content: Size<Pixels>) -> Bounds<Pixels> {
        let lane = |scrolls: bool| if scrolls { LANE } else { px(0.) };
        let room = size(grid.right() - origin.x, grid.bottom() - origin.y);
        // A lane takes room from the other axis, which can then scroll too.
        let down = content.height > room.height;
        let across = content.width > room.width - lane(down);
        let down = content.height > room.height - lane(across);
        let lanes = size(lane(down), lane(across));
        let viewport = size((room.width - lanes.width).max(px(0.)), (room.height - lanes.height).max(px(0.)));
        let mut state = self.0.borrow_mut();
        state.grid = grid;
        state.lanes = lanes;
        state.viewport = Bounds::new(origin, viewport);
        state.content = content;
        state.clamp();
        state.viewport
    }

    // Whether the position moved.
    pub fn scroll_to(&self, position: Point<Pixels>) -> bool {
        let mut state = self.0.borrow_mut();
        let before = state.position;
        state.position = position;
        state.clamp();
        state.position != before
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

    fn set_offset(&self, offset: Point<Pixels>) {
        self.scroll.scroll_to(-offset);
    }

    // As much longer than the cells as the bar is than the viewport, so the thumb reaches the end of its track just as
    // the cells reach the end of theirs.
    fn content_size(&self) -> Size<Pixels> {
        let bar = self.viewport_bounds().size;
        let state = self.scroll.0.borrow();
        state.content + bar - state.viewport.size
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A 450x330 grid with a 50px gutter and a 30px header, so its cells have 400x300.
    fn lay_out(content: Size<Pixels>) -> GridScroll {
        let scroll = GridScroll::default();
        let grid = Bounds::new(point(px(0.), px(0.)), size(px(450.), px(330.)));
        scroll.set_layout(grid, point(px(50.), px(30.)), content);
        scroll
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
