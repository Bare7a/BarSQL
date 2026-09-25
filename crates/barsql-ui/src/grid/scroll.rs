use std::cell::RefCell;
use std::rc::Rc;

use gpui_kit::component::scroll::ScrollbarHandle;
use gpui_kit::{Bounds, Pixels, Point, Size, point, px};

// Scroll state of the cells right of the gutter and below the header. Shared with the scrollbars.
#[derive(Clone, Default)]
pub struct GridScroll(Rc<RefCell<State>>);

#[derive(Default)]
struct State {
    // Positive distances. ScrollbarHandle offsets are negated.
    position: Point<Pixels>,
    viewport: Bounds<Pixels>,
    content: Size<Pixels>,
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

    pub fn max(&self) -> Point<Pixels> {
        self.0.borrow().max()
    }

    pub fn set_layout(&self, viewport: Bounds<Pixels>, content: Size<Pixels>) {
        let mut state = self.0.borrow_mut();
        state.viewport = viewport;
        state.content = content;
        state.clamp();
    }

    // Whether the position moved.
    pub fn scroll_to(&self, position: Point<Pixels>) -> bool {
        let mut state = self.0.borrow_mut();
        let before = state.position;
        state.position = position;
        state.clamp();
        state.position != before
    }
}

impl ScrollbarHandle for GridScroll {
    fn viewport_bounds(&self) -> Bounds<Pixels> {
        self.viewport()
    }

    fn offset(&self) -> Point<Pixels> {
        -self.position()
    }

    fn set_offset(&self, offset: Point<Pixels>) {
        self.scroll_to(-offset);
    }

    fn content_size(&self) -> Size<Pixels> {
        self.0.borrow().content
    }
}
