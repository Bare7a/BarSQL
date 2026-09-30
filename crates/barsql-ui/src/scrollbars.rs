use std::cell::Cell;
use std::panic::Location;
use std::rc::Rc;

use gpui_kit::component::Theme;
use gpui_kit::component::scroll::ScrollbarMode;
use gpui_kit::*;

// GPUI Kit shows a bar while its area scrolls, or in Hover mode while the pointer is on the bar itself. The bars in
// an element wrapped here also show while the pointer is anywhere over it. GPUI Kit draws a bar only when there's
// something to scroll, so an element that fits still shows none.
pub trait ScrollbarsOnHover: IntoElement {
    #[track_caller]
    fn scrollbars_on_hover(self) -> HoverArea {
        HoverArea { id: ElementId::CodeLocation(*Location::caller()), element: Some(self.into_any_element()) }
    }
}

impl<E: IntoElement> ScrollbarsOnHover for E {}

pub struct HoverArea {
    id: ElementId,
    element: Option<AnyElement>,
}

impl IntoElement for HoverArea {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for HoverArea {
    type RequestLayoutState = AnyElement;
    type PrepaintState = Hitbox;

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
    ) -> Hitbox {
        let hovered = hover_state(id, window).over.get();
        with_mode(hovered, cx, |cx| element.prepaint(window, cx));
        // After the element's own hitboxes, so none of them can hide it. It hides none of them.
        window.insert_hitbox(bounds, HitboxBehavior::Normal)
    }

    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        element: &mut AnyElement,
        hitbox: &mut Hitbox,
        window: &mut Window,
        cx: &mut App,
    ) {
        let hover = hover_state(id, window);
        with_mode(hover.over.get(), cx, |cx| element.paint(window, cx));
        let view = window.current_view();
        // The element can also move under a still pointer. Keys change nothing, so typing keeps an editor's bar.
        let over = hitbox.is_hovered(window);
        if !hover.outside.get() && !window.last_input_was_keyboard() && over != hover.over.get() {
            let hover = hover.clone();
            window.defer(cx, move |_, cx| set_over(&hover, over, view, cx));
        }
        window.on_mouse_event({
            let (hover, hitbox) = (hover.clone(), hitbox.clone());
            move |_: &MouseMoveEvent, phase, window, cx| {
                if phase.bubble() {
                    hover.outside.set(false);
                    set_over(&hover, hitbox.is_hovered(window), view, cx);
                }
            }
        });
        // The pointer can leave the window without a last move.
        window.on_mouse_event(move |_: &MouseExitEvent, phase, _, cx| {
            if phase.bubble() {
                hover.outside.set(true);
                set_over(&hover, false, view, cx);
            }
        });
    }
}

#[derive(Default)]
struct Hover {
    // The pointer is over the element.
    over: Cell<bool>,
    // The pointer left the window. GPUI keeps its last position, so hit tests still find the element.
    outside: Cell<bool>,
}

fn hover_state(id: Option<&GlobalElementId>, window: &mut Window) -> Rc<Hover> {
    window.with_element_state(id.expect("HoverArea has an id"), |state: Option<Rc<Hover>>, _| {
        let state = state.unwrap_or_default();
        (state.clone(), state)
    })
}

fn set_over(hover: &Hover, over: bool, view: EntityId, cx: &mut App) {
    if hover.over.replace(over) != over {
        cx.notify(view);
    }
}

// GPUI Kit's editors draw their own bar, which reads its mode from the theme alone. So the theme holds the mode
// while the element draws: Always when hovered, else the system's, even inside a hovered area.
fn with_mode<R>(hovered: bool, cx: &mut App, draw: impl FnOnce(&mut App) -> R) -> R {
    let mode = if hovered { ScrollbarMode::Always } else { Theme::global(cx).scrollbar_mode };
    let outer = cx
        .try_global::<base::Theme>()
        .filter(|theme| theme.scrollbar.mode() != mode)
        .map(|theme| theme.scrollbar.clone());
    let Some(outer) = outer else { return draw(cx) };
    base::Theme::global_mut(cx).scrollbar = outer.clone().with_mode(mode);
    let drawn = draw(cx);
    base::Theme::global_mut(cx).scrollbar = outer;
    drawn
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use gpui_kit::component::Theme;
    use gpui_kit::component::scroll::{ScrollableElement as _, ScrollbarMode};
    use gpui_kit::{
        Context, Entity, InteractiveElement as _, IntoElement, Modifiers, MouseExitEvent, ParentElement as _, Pixels,
        Point, Render, ScrollHandle, StatefulInteractiveElement as _, Styled as _, TestAppContext, VisualTestContext,
        Window, div, point, px,
    };

    use super::ScrollbarsOnHover as _;
    use crate::test_support::Env;

    // A 200px square area of 1000px, with a 100px square area of its own at the top left.
    struct Host {
        outer: ScrollHandle,
        inner: ScrollHandle,
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
                        .vertical_scrollbar(&self.inner)
                        .scrollbars_on_hover(),
                )
                .child(div().h(px(1000.)));
            div().relative().size(px(200.)).child(outer).vertical_scrollbar(&self.outer).scrollbars_on_hover()
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
        let (host, cx) = cx.add_window_view(|_, _| Host { outer: outer.clone(), inner: inner.clone() });
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
        let before = handle.offset().y;
        cx.simulate_click(track, Modifiers::none());
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
}
