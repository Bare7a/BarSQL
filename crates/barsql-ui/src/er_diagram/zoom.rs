// The diagram's zoom: the steps its buttons take, the zoom that fits it all, and the scroll offsets that keep the
// point under the pointer still or bring a table to the middle. GPUI can't scale a subtree, so Zoomed lays its child
// out at a larger or smaller rem instead. Every size in the diagram is in rems, text and icons too.
use std::panic::Location;

use gpui_kit::*;

pub const MIN: f32 = 0.1;
pub const MAX: f32 = 2.;
// It opens at least this large, so names stay readable. Fit goes further.
pub const READABLE: f32 = 0.5;
const STEPS: [f32; 14] = [0.1, 0.25, 0.33, 0.5, 0.67, 0.75, 0.8, 0.9, 1., 1.1, 1.25, 1.5, 1.75, 2.];
// A wheel or trackpad scroll this far doubles or halves the zoom.
const WHEEL_DOUBLING: f32 = 240.;

// The next step in or out from `zoom`, which may sit between steps after a pinch.
pub fn step(zoom: f32, inward: bool) -> f32 {
    match inward {
        true => STEPS.iter().copied().find(|s| *s > zoom + 0.005).unwrap_or(MAX),
        false => STEPS.iter().rev().copied().find(|s| *s < zoom - 0.005).unwrap_or(MIN),
    }
}

// The zoom that shows all of `content`, as laid out at 100%, in `viewport`. Never above 100%.
pub fn fit(content: Size<Pixels>, viewport: Size<Pixels>) -> f32 {
    let x = viewport.width / content.width.max(px(1.));
    let y = viewport.height / content.height.max(px(1.));
    x.min(y).clamp(MIN, 1.)
}

// The zoom a wheel scroll of `dy` gives, up for in.
pub fn wheel(zoom: f32, dy: Pixels) -> f32 {
    (zoom * 2f32.powf(dy.as_f32() / WHEEL_DOUBLING)).clamp(MIN, MAX)
}

// The scroll offset that keeps the content under `anchor`, a point in the viewport, where it is while the zoom goes
// from `from` to `to`. GPUI offsets are negative as content scrolls up and left.
pub fn anchored(offset: Point<Pixels>, anchor: Point<Pixels>, from: f32, to: f32) -> Point<Pixels> {
    let ratio = to / from;
    point(anchor.x - (anchor.x - offset.x) * ratio, anchor.y - (anchor.y - offset.y) * ratio)
}

// The scroll offset that puts `target`, a point of the content, in the middle of the viewport, as far as the content
// scrolls.
pub fn centred(target: Point<Pixels>, viewport: Size<Pixels>, content: Size<Pixels>) -> Point<Pixels> {
    let axis = |target: Pixels, view: Pixels, size: Pixels| {
        let lowest = (view - size).min(px(0.));
        (view / 2. - target).max(lowest).min(px(0.))
    };
    point(axis(target.x, viewport.width, content.width), axis(target.y, viewport.height, content.height))
}

pub struct Zoomed {
    zoom: f32,
    rem: Pixels,
    child: AnyElement,
}

pub fn zoomed(zoom: f32, child: impl IntoElement) -> Zoomed {
    Zoomed { zoom, rem: px(0.), child: child.into_any_element() }
}

impl IntoElement for Zoomed {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for Zoomed {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static Location<'static>> {
        None
    }

    // The child's own layout, at the zoomed rem.
    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        self.rem = window.rem_size() * self.zoom;
        let rem = self.rem;
        (window.with_rem_size(Some(rem), |window| self.child.request_layout(window, cx)), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let rem = self.rem;
        window.with_rem_size(Some(rem), |window| self.child.prepaint(window, cx));
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let rem = self.rem;
        window.with_rem_size(Some(rem), |window| self.child.paint(window, cx));
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::{point, px, size};

    use super::{MAX, MIN, anchored, centred, fit, step, wheel};

    #[test]
    fn steps_walk_the_ladder_from_anywhere() {
        assert_eq!((step(1., true), step(1., false)), (1.1, 0.9));
        assert_eq!((step(0.6, true), step(0.6, false)), (0.67, 0.5), "after a pinch");
        assert_eq!((step(MAX, true), step(MIN, false)), (MAX, MIN));
    }

    #[test]
    fn fit_never_goes_past_full_size() {
        assert_eq!(fit(size(px(400.), px(300.)), size(px(800.), px(600.))), 1.);
        assert_eq!(fit(size(px(2000.), px(500.)), size(px(1000.), px(500.))), 0.5);
        assert_eq!(fit(size(px(100_000.), px(10.)), size(px(1000.), px(500.))), MIN);
    }

    #[test]
    fn the_wheel_zooms_in_going_up_and_stays_in_range() {
        assert!(wheel(1., px(120.)) > 1. && wheel(1., px(-120.)) < 1.);
        assert!((wheel(1., px(240.)) - 2.).abs() < 1e-5);
        assert_eq!(wheel(1.9, px(1000.)), MAX);
    }

    #[test]
    fn anchoring_keeps_the_point_under_the_pointer() {
        let (offset, anchor) = (point(px(-100.), px(-40.)), point(px(300.), px(200.)));
        let content = |offset: gpui_kit::Point<gpui_kit::Pixels>, zoom: f32| {
            point((anchor.x - offset.x) / zoom, (anchor.y - offset.y) / zoom)
        };
        let moved = anchored(offset, anchor, 1., 1.5);
        let (before, after) = (content(offset, 1.), content(moved, 1.5));
        assert!((before.x - after.x).abs() < px(0.01) && (before.y - after.y).abs() < px(0.01));
    }

    #[test]
    fn centring_stays_within_the_scroll_range() {
        let (viewport, content) = (size(px(800.), px(600.)), size(px(3000.), px(400.)));
        assert_eq!(centred(point(px(1500.), px(200.)), viewport, content), point(px(-1100.), px(0.)));
        assert_eq!(centred(point(px(2950.), px(200.)), viewport, content).x, px(-2200.), "the end");
        assert_eq!(centred(point(px(10.), px(200.)), viewport, content).x, px(0.), "the start");
    }
}
