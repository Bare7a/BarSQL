use std::cell::Cell;
use std::panic::Location;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use gpui_kit::component::Theme;
use gpui_kit::component::scroll::{ScrollableElement, Scrollbar, ScrollbarAxis, ScrollbarHandle, ScrollbarMode};
use gpui_kit::*;

// GPUI Kit shows a bar while its area scrolls, or in Hover mode while the pointer is on the bar itself. The bars in
// an element wrapped here also show while the pointer is anywhere over it. GPUI Kit draws a bar only when there's
// something to scroll, so an element that fits still shows none.
//
// The theme holds the mode while the wrapped element draws, so every bar inside follows the hover, not only the
// element's own. Wrap the scroll area itself, not a panel holding other bars, such as a tab strip whose track would
// take clicks on the tabs.
pub trait ScrollbarsOnHover: IntoElement {
    #[track_caller]
    fn scrollbars_on_hover(self) -> HoverArea {
        HoverArea::new(ElementId::CodeLocation(*Location::caller()), self.into_any_element(), None)
    }
}

impl<E: IntoElement> ScrollbarsOnHover for E {}

pub trait HoverScrollbar: ScrollableElement {
    // `.scrollbar(handle, axis)` with its bars shown on hover. Knowing where they are, the area also keeps a press of
    // another button on a shown bar from scrolling, since GPUI Kit's bar takes every button.
    #[track_caller]
    fn hover_scrollbar<H: ScrollbarHandle + Clone>(self, handle: &H, axis: ScrollbarAxis) -> HoverArea {
        let bars = Bars { handle: Rc::new(handle.clone()), axis };
        let element = self.scrollbar(handle, axis).into_any_element();
        HoverArea::new(ElementId::CodeLocation(*Location::caller()), element, Some(bars))
    }
}

impl<E: ScrollableElement> HoverScrollbar for E {}

pub struct HoverArea {
    id: ElementId,
    element: Option<AnyElement>,
    bars: Option<Bars>,
}

impl HoverArea {
    fn new(id: ElementId, element: AnyElement, bars: Option<Bars>) -> Self {
        Self { id, element: Some(element), bars }
    }

    // An area is told apart by its call site. A helper that can draw more than one under the same parent gives each
    // an id: areas sharing one would share their hover and redraw each other forever.
    pub fn id(mut self, id: impl Into<ElementId>) -> Self {
        self.id = ElementId::NamedChild(Arc::new(id.into()), "scrollbars".into());
        self
    }
}

// The bars GPUI Kit draws along the area for one handle.
struct Bars {
    handle: Rc<dyn ScrollbarHandle>,
    axis: ScrollbarAxis,
}

impl Bars {
    // Whether `position` is on one of the bars, which GPUI Kit draws only for an axis with something to scroll.
    fn contain(&self, area: Bounds<Pixels>, position: Point<Pixels>) -> bool {
        let (content, width) = (self.handle.content_size(), Scrollbar::width());
        let vertical =
            self.axis.has_vertical() && content.height > area.size.height && position.x >= area.right() - width;
        let horizontal =
            self.axis.has_horizontal() && content.width > area.size.width && position.y >= area.bottom() - width;
        area.contains(&position) && (vertical || horizontal)
    }
}

impl IntoElement for HoverArea {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for HoverArea {
    type RequestLayoutState = AnyElement;
    type PrepaintState = (Hitbox, Rc<Hover>);

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static Location<'static>> {
        None
    }

    // Takes the element's own layout, so the wrapper moves nothing.
    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, AnyElement) {
        let mut element = self.element.take().expect("laid out once");
        (element.request_layout(window, cx), element)
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        element: &mut AnyElement,
        window: &mut Window,
        cx: &mut App,
    ) -> (Hitbox, Rc<Hover>) {
        let hover = hover_state(id, window);
        // Later than any bar activity from the events that ended the hover.
        if hover.left.take() {
            hover.quiet_since.set(Some(Instant::now()));
        }
        with_mode(&hover, cx, |cx| element.prepaint(window, cx));
        // After the element's own hitboxes, so none of them can hide it. It hides none of them.
        (window.insert_hitbox(bounds, HitboxBehavior::Normal), hover)
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        element: &mut AnyElement,
        (hitbox, hover): &mut (Hitbox, Rc<Hover>),
        window: &mut Window,
        cx: &mut App,
    ) {
        with_mode(hover, cx, |cx| element.paint(window, cx));
        let view = window.current_view();
        // Checked once the frame is drawn, against that frame and whatever the last input was: the area can move or
        // be covered under a still pointer, after a key press too.
        window.defer(cx, {
            let (hover, hitbox) = (hover.clone(), hitbox.clone());
            move |window, cx| {
                let over = hitbox.is_hovered_at(window.mouse_position(), window);
                if set_over(&hover, over, view, cx) && !over {
                    hover.rearm.set(true);
                }
            }
        });
        window.on_mouse_event({
            let (hover, hitbox) = (hover.clone(), hitbox.clone());
            move |_: &MouseMoveEvent, phase, window, cx| {
                if !phase.bubble() {
                    return;
                }
                hover.dropped.set(false);
                let over = hitbox.is_hovered(window);
                // The hover ended without a move, so this first move is when the bars let go of the pointer.
                if hover.rearm.take() && !over {
                    hover.left.set(true);
                }
                set_over(&hover, over, view, cx);
            }
        });
        window.on_mouse_event({
            let hover = hover.clone();
            move |_: &MouseUpEvent, phase, _, _| {
                if phase.bubble() {
                    hover.dropped.set(true);
                    // A bar dragged out of the area counts its release as activity.
                    if !hover.over.get() {
                        hover.left.set(true);
                    }
                }
            }
        });
        window.on_mouse_event({
            let hover = hover.clone();
            move |event: &MouseExitEvent, phase, window, cx| {
                if phase.bubble() && event.pressed_button.is_none() {
                    pointer_left(&hover, view, window, cx);
                }
            }
        });
        // macOS ends a drop with Exited too, after the drop's mouse up.
        window.on_mouse_event({
            let hover = hover.clone();
            move |event: &FileDropEvent, phase, window, cx| {
                if phase.bubble() && matches!(event, FileDropEvent::Exited) && !hover.dropped.get() {
                    pointer_left(&hover, view, window, cx);
                }
            }
        });
        if let Some(bars) = self.bars.take() {
            let hover = hover.clone();
            // Registered after the bars, so it goes first.
            window.on_mouse_event(move |event: &MouseDownEvent, phase, _, cx| {
                if phase.bubble()
                    && event.button != MouseButton::Left
                    && hover.over.get()
                    && bars.contain(bounds, event.position)
                {
                    cx.stop_propagation();
                }
            });
        }
    }
}

#[derive(Default)]
pub struct Hover {
    // The pointer is over the area.
    over: Cell<bool>,
    // The hover ended since the last prepaint, which then starts `quiet_since`.
    left: Cell<bool>,
    // When the hover last ended. Bar activity from before then, such as the pointer crossing a bar on its way out,
    // doesn't keep a bar shown.
    quiet_since: Cell<Option<Instant>>,
    // The hover ended without a pointer move, so the bars still hold the pointer until the next one.
    rearm: Cell<bool>,
    // A drop landed, so the FileDropEvent::Exited after it doesn't mean the pointer left the window.
    dropped: Cell<bool>,
}

fn hover_state(id: Option<&GlobalElementId>, window: &mut Window) -> Rc<Hover> {
    window.with_element_state(id.expect("HoverArea has an id"), |state: Option<Rc<Hover>>, _| {
        let state = state.unwrap_or_default();
        (state.clone(), state)
    })
}

// Whether it changed.
fn set_over(hover: &Hover, over: bool, view: EntityId, cx: &mut App) -> bool {
    if hover.over.replace(over) == over {
        return false;
    }
    if !over {
        hover.left.set(true);
    }
    cx.notify(view);
    true
}

// A bar that keeps the system's mode, whatever the theme holds while it draws. For bars that mustn't show on hover,
// such as a tab strip's, whose track would take clicks on the tabs.
pub fn system_bar(bar: Scrollbar, cx: &App) -> Scrollbar {
    bar.viewport_from_layout().mode(Theme::global(cx).scrollbar_mode)
}

// Off any window, for a pointer that has left this one.
const OFF_WINDOW: Point<Pixels> = Point { x: px(-1_000_000.), y: px(-1_000_000.) };

// GPUI keeps the last position when the pointer leaves the window, and GPUI Kit's bars keep the hover they last saw.
// A bar the pointer left through would then stay shown, so the pointer is moved off the window. The first area to
// get there moves it; the others find it moved.
fn pointer_left(hover: &Hover, view: EntityId, window: &mut Window, cx: &mut App) {
    set_over(hover, false, view, cx);
    hover.rearm.set(true);
    window.defer(cx, |window, cx| {
        if window.mouse_position() != OFF_WINDOW {
            let modifiers = window.modifiers();
            let event = MouseMoveEvent { position: OFF_WINDOW, pressed_button: None, modifiers };
            window.dispatch_event(PlatformInput::MouseMove(event), cx);
        }
    });
}

// GPUI Kit's editors draw their own bar, which reads its mode from the theme alone. So the theme holds the mode while
// the element draws: Always when hovered, else the system's, even inside a hovered area. For a while after the hover
// ends, the idle hold is cut to the time since: GPUI Kit counts the pointer crossing a shown bar as activity, and the
// bar would otherwise stay up for the whole hold. A scroll after that still shows the bar for the usual time.
fn with_mode<R>(hover: &Hover, cx: &mut App, draw: impl FnOnce(&mut App) -> R) -> R {
    let Some(outer) = cx.try_global::<base::Theme>().map(|theme| theme.scrollbar.clone()) else { return draw(cx) };
    let hovered = hover.over.get();
    let mode = if hovered { ScrollbarMode::Always } else { Theme::global(cx).scrollbar_mode };
    let motion = outer.motion();
    let quiet = hover
        .quiet_since
        .get()
        .filter(|_| !hovered)
        .map(|since| since.elapsed())
        .filter(|quiet| *quiet < motion.idle());
    if mode == outer.mode() && quiet.is_none() {
        return draw(cx);
    }
    let mut scrollbar = outer.clone().with_mode(mode);
    if let Some(quiet) = quiet {
        scrollbar = scrollbar.with_motion(motion.with_idle(quiet));
    }
    base::Theme::global_mut(cx).scrollbar = scrollbar;
    let drawn = draw(cx);
    base::Theme::global_mut(cx).scrollbar = outer;
    drawn
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use gpui_kit::component::Theme;
    use gpui_kit::component::scroll::{ScrollableElement as _, ScrollbarAxis, ScrollbarMode};
    use gpui_kit::prelude::FluentBuilder as _;
    use gpui_kit::{
        Context, Entity, InteractiveElement as _, IntoElement, Modifiers, MouseButton, MouseDownEvent, MouseExitEvent,
        MouseUpEvent, ParentElement as _, Pixels, Point, Render, ScrollHandle, StatefulInteractiveElement as _,
        Styled as _, TestAppContext, VisualTestContext, Window, div, point, px,
    };

    use super::{HoverArea, HoverScrollbar as _, ScrollbarsOnHover as _};
    use crate::test_support::Env;

    // A 200px square area of 1000px, with a 100px square area of its own at the top left.
    struct Host {
        outer: ScrollHandle,
        inner: ScrollHandle,
        // Moves the outer area 300px right.
        shifted: bool,
        // Draws the outer area from another call site, so as an area with no hover state yet.
        late: bool,
    }

    impl Render for Host {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let inner =
                div().id("inner").size_full().overflow_y_scroll().track_scroll(&self.inner).child(div().h(px(1000.)));
            let outer = div()
                .id("outer")
                .size_full()
                .overflow_y_scroll()
                .track_scroll(&self.outer)
                .child(
                    div()
                        .relative()
                        .flex_none()
                        .size(px(100.))
                        .child(inner)
                        .hover_scrollbar(&self.inner, ScrollbarAxis::Vertical),
                )
                .child(div().h(px(1000.)));
            let area = div().relative().size(px(200.)).child(outer);
            let area = if self.late {
                area.vertical_scrollbar(&self.outer).scrollbars_on_hover().id("late")
            } else {
                area.hover_scrollbar(&self.outer, ScrollbarAxis::Vertical)
            };
            div().size_full().when(self.shifted, |el| el.pl(px(300.))).child(area)
        }
    }

    fn draw(cx: &mut VisualTestContext) {
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
    }

    // Like a Mac with a trackpad, where a bar hides until its area scrolls.
    fn open(cx: &mut TestAppContext) -> (Env, Entity<Host>, ScrollHandle, ScrollHandle, &mut VisualTestContext) {
        let env = Env::new(cx);
        cx.update(|cx| Theme::set_scrollbar_mode(ScrollbarMode::Scrolling, cx));
        let (outer, inner) = (ScrollHandle::new(), ScrollHandle::new());
        let host = Host { outer: outer.clone(), inner: inner.clone(), shifted: false, late: false };
        let (host, cx) = cx.add_window_view(|_, _| host);
        // The pointer starts at the window's corner, over the inner area.
        hover(cx, point(px(300.), px(300.)));
        (env, host, outer, inner, cx)
    }

    fn hover(cx: &mut VisualTestContext, at: Point<Pixels>) {
        cx.simulate_mouse_move(at, None, Modifiers::none());
        draw(cx);
    }

    // A click low on a bar's track jumps there, but only while the bar shows.
    fn jumps(cx: &mut VisualTestContext, handle: &ScrollHandle, track: Point<Pixels>) -> bool {
        jumps_with(cx, handle, track, MouseButton::Left)
    }

    fn jumps_with(
        cx: &mut VisualTestContext,
        handle: &ScrollHandle,
        track: Point<Pixels>,
        button: MouseButton,
    ) -> bool {
        let before = handle.offset().y;
        let modifiers = Modifiers::none();
        cx.simulate_event(MouseDownEvent { button, position: track, modifiers, click_count: 1, first_mouse: false });
        cx.simulate_event(MouseUpEvent { button, position: track, modifiers, click_count: 1 });
        draw(cx);
        handle.offset().y < before - px(100.)
    }

    fn outer_track() -> Point<Pixels> {
        point(px(192.), px(180.))
    }

    fn inner_track() -> Point<Pixels> {
        point(px(92.), px(80.))
    }

    #[gpui_kit::test]
    fn a_bar_shows_while_the_pointer_is_over_its_area(cx: &mut TestAppContext) {
        let (_env, _, outer, inner, cx) = open(cx);
        hover(cx, point(px(150.), px(150.)));
        assert!(!jumps(cx, &inner, inner_track()), "an area inside keeps its bar hidden");
        hover(cx, point(px(300.), px(300.)));
        assert!(!jumps(cx, &outer, outer_track()), "leaving the area hides its bar at once");

        hover(cx, point(px(150.), px(150.)));
        assert!(jumps(cx, &outer, outer_track()), "the hovered area shows its bar");

        outer.set_offset(point(px(0.), px(0.)));
        hover(cx, point(px(50.), px(50.)));
        assert!(jumps(cx, &inner, inner_track()), "hovering the area inside shows its bar too");
    }

    // GPUI Kit counts the pointer crossing a shown bar as activity, which would hold the bar for its idle time.
    #[gpui_kit::test]
    fn leaving_across_the_bar_hides_it_at_once(cx: &mut TestAppContext) {
        let (_env, _, outer, _, cx) = open(cx);
        hover(cx, point(px(150.), px(150.)));
        hover(cx, point(px(192.), px(150.)));
        hover(cx, point(px(300.), px(150.)));
        draw(cx);
        assert!(!jumps(cx, &outer, outer_track()));
    }

    // GPUI keeps the last position after the pointer leaves the window, so the area still tests as under it.
    #[gpui_kit::test]
    fn leaving_the_window_hides_the_bar(cx: &mut TestAppContext) {
        let (_env, _, outer, _, cx) = open(cx);
        hover(cx, point(px(150.), px(150.)));
        cx.simulate_event(MouseExitEvent { position: point(px(150.), px(150.)), ..Default::default() });
        draw(cx);
        // A hover seen while painting shows on the frame after.
        draw(cx);
        assert!(!jumps(cx, &outer, outer_track()));
    }

    // With no move after the exit, GPUI Kit's bar would keep the hover it saw on its track.
    #[gpui_kit::test]
    fn leaving_the_window_through_the_bar_hides_it(cx: &mut TestAppContext) {
        let (_env, _, outer, _, cx) = open(cx);
        hover(cx, point(px(150.), px(150.)));
        hover(cx, point(px(192.), px(150.)));
        cx.simulate_event(MouseExitEvent { position: point(px(199.), px(150.)), ..Default::default() });
        draw(cx);
        draw(cx);
        assert!(!jumps(cx, &outer, outer_track()));
    }

    #[gpui_kit::test]
    fn an_area_first_drawn_after_the_pointer_left_shows_no_bar(cx: &mut TestAppContext) {
        let (_env, host, outer, _, cx) = open(cx);
        hover(cx, point(px(150.), px(150.)));
        cx.simulate_event(MouseExitEvent { position: point(px(150.), px(150.)), ..Default::default() });
        draw(cx);
        host.update(cx, |host, cx| {
            host.late = true;
            cx.notify();
        });
        draw(cx);
        draw(cx);
        assert!(!jumps(cx, &outer, outer_track()));
    }

    // Typing keeps a hovered editor's bar, but an area moved away from a still pointer by a key lets its bar go.
    #[gpui_kit::test]
    fn a_key_press_doesnt_freeze_the_hover(cx: &mut TestAppContext) {
        let (_env, host, outer, _, cx) = open(cx);
        hover(cx, point(px(150.), px(150.)));
        cx.simulate_keystrokes("a");
        draw(cx);
        assert!(jumps(cx, &outer, outer_track()), "a key press keeps the hovered area's bar");

        outer.set_offset(point(px(0.), px(0.)));
        hover(cx, point(px(150.), px(150.)));
        cx.simulate_keystrokes("a");
        host.update(cx, |host, cx| {
            host.shifted = true;
            cx.notify();
        });
        draw(cx);
        draw(cx);
        assert!(!jumps(cx, &outer, outer_track() + point(px(300.), px(0.))), "the area moved away hides its bar");
    }

    // Scrolling still shows a bar the way the system does, with the pointer elsewhere.
    #[gpui_kit::test]
    fn a_scroll_shows_the_bar_of_an_area_that_isnt_hovered(cx: &mut TestAppContext) {
        let (_env, _, outer, _, cx) = open(cx);
        outer.set_offset(point(px(0.), px(-50.)));
        draw(cx);
        assert!(jumps(cx, &outer, outer_track()));
    }

    // GPUI Kit's bar takes every button, so a right-click on it would scroll.
    #[gpui_kit::test]
    fn a_right_click_on_a_shown_bar_doesnt_scroll(cx: &mut TestAppContext) {
        let (_env, _, outer, _, cx) = open(cx);
        hover(cx, point(px(150.), px(150.)));
        assert!(!jumps_with(cx, &outer, outer_track(), MouseButton::Right));
        assert!(!jumps_with(cx, &outer, outer_track(), MouseButton::Middle));
        assert!(jumps(cx, &outer, outer_track()), "a primary click still scrolls");
    }

    // A move over an area with no hover styles draws nothing else, so the move itself asks for the frame with the bar.
    #[gpui_kit::test]
    fn moving_over_the_area_asks_for_a_frame(cx: &mut TestAppContext) {
        let (_env, host, _, _, cx) = open(cx);
        let asked = Rc::new(Cell::new(false));
        let seen = asked.clone();
        cx.update(|_, cx| cx.observe(&host, move |_, _| seen.set(true)).detach());
        cx.simulate_mouse_move(point(px(150.), px(150.)), None, Modifiers::none());
        assert!(asked.get());
    }

    // Two 100px areas of 1000px side by side, drawn by one helper.
    struct Twins {
        left: ScrollHandle,
        right: ScrollHandle,
    }

    fn twin(id: &'static str, handle: &ScrollHandle) -> HoverArea {
        let content = div().id(id).size_full().overflow_y_scroll().track_scroll(handle).child(div().h(px(1000.)));
        div()
            .relative()
            .flex_none()
            .size(px(100.))
            .child(content)
            .vertical_scrollbar(handle)
            .scrollbars_on_hover()
            .id(id)
    }

    impl Render for Twins {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().flex().child(twin("left", &self.left)).child(twin("right", &self.right))
        }
    }

    // Sharing an id, the two would flip one hover between them on every frame.
    #[gpui_kit::test]
    fn areas_from_one_helper_keep_their_own_hover(cx: &mut TestAppContext) {
        let _env = Env::new(cx);
        cx.update(|cx| Theme::set_scrollbar_mode(ScrollbarMode::Scrolling, cx));
        let (left, right) = (ScrollHandle::new(), ScrollHandle::new());
        let twins = Twins { left: left.clone(), right: right.clone() };
        let (_, cx) = cx.add_window_view(|_, _| twins);
        hover(cx, point(px(50.), px(50.)));
        assert!(!jumps(cx, &right, point(px(192.), px(80.))), "the other area keeps its bar hidden");
        hover(cx, point(px(50.), px(50.)));
        assert!(jumps(cx, &left, point(px(92.), px(80.))));
    }
}
