use std::cell::Cell;
use std::rc::Rc;

use crate::tokens::RADIUS;
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::Button;
use gpui_kit::component::scroll::{Scrollbar, ScrollbarHandle};
use gpui_kit::component::{ActiveTheme, Icon, Sizable, h_flex};
use gpui_kit::*;

// The sidebar lists' padding. Saved, Recent and the Schema tree pad the list by it, and connection rows pad
// themselves by it.
pub const LIST_INSET: Rems = rems(0.615);
// The padding of a Saved or Recent row inside its list.
pub const ROW_INSET: Rems = rems(0.5);

// Keyboard navigation for the sidebar lists. Delete triggers the row's delete button.
pub const CONTEXT: &str = "ListNav";

actions!(list_nav, [NavUp, NavDown, NavFirst, NavLast, NavOpen, NavDelete]);

pub fn init(cx: &mut App) {
    let context = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("up", NavUp, context),
        KeyBinding::new("down", NavDown, context),
        KeyBinding::new("home", NavFirst, context),
        KeyBinding::new("end", NavLast, context),
        KeyBinding::new("enter", NavOpen, context),
        KeyBinding::new("space", NavOpen, context),
        KeyBinding::new("delete", NavDelete, context),
    ]);
}

// Rings the cursor row only after a key moves it, not after a click or when the list takes focus. `S` scrolls the
// list: a div's handle, or a virtual list's, which builds only the rows on screen.
pub struct ListNav<S = ScrollHandle> {
    pub focus: FocusHandle,
    pub scroll: S,
    pub clearance: BarClearance,
    cursor: Option<String>,
    keyed: bool,
}

// A list's scroll handle, which can bring a row into view.
pub trait NavScroll: ScrollbarHandle + Clone {
    fn reveal(&self, ix: usize);
}

impl NavScroll for ScrollHandle {
    fn reveal(&self, ix: usize) {
        self.scroll_to_item(ix);
    }
}

impl NavScroll for UniformListScrollHandle {
    fn reveal(&self, ix: usize) {
        self.scroll_to_item(ix, ScrollStrategy::Nearest);
    }
}

impl NavScroll for ListState {
    fn reveal(&self, ix: usize) {
        self.scroll_to_reveal_item(ix);
    }
}

impl ListNav {
    pub fn new(cx: &mut App) -> Self {
        Self::with_scroll(ScrollHandle::new(), cx)
    }
}

impl<S: NavScroll> ListNav<S> {
    pub fn with_scroll(scroll: S, cx: &mut App) -> Self {
        Self { focus: cx.focus_handle(), scroll, clearance: BarClearance::default(), cursor: None, keyed: false }
    }

    pub fn focus_list(&mut self, window: &mut Window, cx: &mut App) {
        self.keyed = false;
        self.focus.focus(window, cx);
    }

    pub fn cursor(&self) -> Option<&str> {
        self.cursor.as_deref()
    }

    pub fn pick(&mut self, id: &str, window: &mut Window, cx: &mut App) {
        self.cursor = Some(id.to_string());
        self.keyed = false;
        self.focus.focus(window, cx);
    }

    // A right-click rings its row, as Finder does, so it's clear which one the menu is for.
    pub fn point_at(&mut self, id: &str, window: &mut Window, cx: &mut App) {
        self.cursor = Some(id.to_string());
        self.keyed = true;
        self.focus.focus(window, cx);
    }

    // `rows` pairs each row id with its child index, and `isize::MIN`/`MAX` mean Home/End. With no cursor yet, Down
    // starts at the top and Up at the bottom.
    pub fn step(&mut self, rows: &[(String, usize)], step: isize) {
        let (Some(first), Some(last)) = (rows.first(), rows.last()) else { return };
        let current = self.cursor.as_ref().and_then(|id| rows.iter().position(|(row, _)| row == id));
        let target = match (current, step) {
            (_, isize::MIN) => first,
            (_, isize::MAX) => last,
            (None, step) if step > 0 => first,
            (None, _) => last,
            (Some(at), step) => &rows[(at as isize + step).clamp(0, rows.len() as isize - 1) as usize],
        };
        self.cursor = Some(target.0.clone());
        self.keyed = true;
        self.scroll.reveal(target.1);
    }

    pub fn ringed(&self, id: &str, window: &Window) -> bool {
        self.keyed && self.focus.is_focused(window) && self.cursor.as_deref() == Some(id)
    }

    // `hover_actions` for a row padded `row_inset` inside a list padded `list_inset`, kept clear of the list's bar.
    pub fn hover_actions(
        &self,
        group: impl Into<SharedString>,
        row_inset: Rems,
        list_inset: Rems,
        window: &Window,
        cx: &App,
    ) -> Div {
        hover_actions(group, row_inset, cx).pr(self.clearance(row_inset, list_inset, window))
    }

    // How far `hover_actions` move clear of the list's bar. A `list` can't be read while it builds its rows, so they
    // take this from before and call `hover_actions` themselves.
    pub fn clearance(&self, row_inset: Rems, list_inset: Rems, window: &Window) -> Pixels {
        self.clearance.get(&self.scroll, rems(row_inset.0 + list_inset.0), window)
    }
}

// A shown bar takes clicks on its track, so buttons at the end of a list's rows move clear of it while the list can
// scroll. Rows are built before the frame's layout, from the frame before's, so `watch` draws them again on the frame
// the list starts or stops scrolling.
#[derive(Clone, Default)]
pub struct BarClearance(Rc<Cell<bool>>);

impl BarClearance {
    // How far to move buttons that end `edge` from the list's right side.
    pub fn get(&self, handle: &impl ScrollbarHandle, edge: Rems, window: &Window) -> Pixels {
        let scrolls = scrolls(handle);
        self.0.set(scrolls);
        if !scrolls {
            return px(0.);
        }
        (Scrollbar::width() - edge.to_pixels(window.rem_size())).max(px(0.))
    }

    // Goes after the list, so it runs once the list is laid out.
    pub fn watch(&self, handle: impl ScrollbarHandle) -> impl IntoElement {
        let used = self.0.clone();
        canvas(
            move |_, window, cx| {
                let scrolls = scrolls(&handle);
                if used.replace(scrolls) != scrolls {
                    let view = window.current_view();
                    window.defer(cx, move |_, cx| cx.notify(view));
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
    }
}

fn scrolls(handle: &impl ScrollbarHandle) -> bool {
    handle.content_size().height > handle.viewport_bounds().size.height
}

// Added as the last child so the row's hover actions don't cover it.
pub fn ring<E: ParentElement + Styled>(el: E, cx: &App) -> E {
    el.relative().child(div().absolute().inset_0().rounded(RADIUS).border_2().border_color(cx.theme().primary))
}

// A button for `hover_actions`. Its row opens on a click and shows the hand, so the button keeps the hand rather
// than GPUI Kit's arrow.
pub fn row_button(id: impl Into<ElementId>, icon: Lucide) -> Button {
    Button::new(id).small().icon(Icon::new(icon)).cursor_pointer()
}

// Small buttons over the end of a relative row, shown while `group` is hovered. Absolute, so the row's text has
// its whole width until then. `inset` is the row's right padding.
pub fn hover_actions(group: impl Into<SharedString>, inset: Rems, cx: &App) -> Div {
    h_flex()
        .absolute()
        .top_0()
        .bottom_0()
        .right(inset)
        .gap(rems(0.231))
        .pl(rems(0.462))
        .bg(cx.theme().sidebar_accent)
        .invisible()
        .group_hover(group, |s| s.visible())
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use barsql_core::{HistoryEntry, SavedQuery};
    use gpui_kit::component::WindowExt;
    use gpui_kit::{Render, TestAppContext, VisualTestContext};

    use crate::connections_panel::{ConnectionsEvent, ConnectionsPanel};
    use crate::history_panel::{HistoryEvent, HistoryPanel};
    use crate::saved_panel::{SavedEvent, SavedPanel};
    use crate::saved_queries;
    use crate::test_support::{Env, in_root};

    struct Host {
        nav: super::ListNav,
    }

    impl Render for Host {
        fn render(&mut self, _: &mut gpui_kit::Window, _: &mut gpui_kit::Context<Self>) -> impl gpui_kit::IntoElement {
            use gpui_kit::{InteractiveElement as _, Styled as _};
            gpui_kit::div().track_focus(&self.nav.focus).size_full()
        }
    }

    // The connection switcher focuses its list when it opens, and that must not show the ring.
    #[gpui_kit::test]
    fn the_ring_shows_only_once_the_keys_move_the_row(cx: &mut TestAppContext) {
        let _env = Env::new(cx);
        let (host, cx) = in_root(cx, |_, cx| Host { nav: super::ListNav::new(cx) });
        let rows = vec![("a".to_string(), 0), ("b".to_string(), 1)];
        let ringed = |id: &'static str, cx: &mut VisualTestContext| {
            cx.run_until_parked();
            host.update_in(cx, |host, window, _| host.nav.ringed(id, window))
        };
        host.update_in(cx, |host, window, cx| host.nav.pick("a", window, cx));
        assert!(!ringed("a", cx), "a click");
        host.update_in(cx, |host, _, _| host.nav.step(&rows, 1));
        assert!(ringed("b", cx), "the keys");
        host.update_in(cx, |host, window, cx| host.nav.focus_list(window, cx));
        assert!(!ringed("b", cx), "the list taking focus again");
    }

    #[gpui_kit::test]
    fn saved_queries_open_and_ask_to_delete_from_the_keyboard(cx: &mut TestAppContext) {
        let env = Env::new(cx);
        for name in ["Alpha", "Beta"] {
            let query = SavedQuery {
                name: name.into(),
                connection_id: env.connection.id.clone(),
                sql: "SELECT 1".into(),
                ..Default::default()
            };
            env.bar.save_saved_query(query).unwrap();
        }
        cx.update(saved_queries::refresh);
        let (panel, cx) = in_root(cx, SavedPanel::new);
        let (id, connections) = (env.connection.id.clone(), env.bar.list_connections());
        panel.update(cx, |panel, cx| panel.set_connection(Some(id), connections, cx));
        let opened = Rc::new(RefCell::new(Vec::new()));
        let seen = opened.clone();
        cx.update(|_, cx| {
            cx.subscribe(&panel, move |_, event: &SavedEvent, _| {
                if let SavedEvent::Open(query) = event {
                    seen.borrow_mut().push(query.name.clone());
                }
            })
            .detach()
        });
        cx.run_until_parked();
        panel.update_in(cx, |panel, window, cx| panel.focus_list(window, cx));
        cx.simulate_keystrokes("down down enter");
        cx.run_until_parked();
        assert_eq!(*opened.borrow(), ["Beta"]);
        cx.simulate_keystrokes("up delete");
        cx.run_until_parked();
        assert!(cx.update(|window, cx| window.has_active_dialog(cx)), "Delete asks first");
    }

    #[gpui_kit::test]
    fn history_entries_open_from_the_keyboard(cx: &mut TestAppContext) {
        let env = Env::new(cx);
        let (panel, cx) = in_root(cx, HistoryPanel::new);
        let now = jiff::Timestamp::now().to_string();
        let entry = |id: &str, sql: &str| HistoryEntry {
            id: id.into(),
            connection_id: env.connection.id.clone(),
            sql: sql.into(),
            executed_at: now.clone(),
            success: true,
            ..Default::default()
        };
        panel.update(cx, |panel, cx| panel.show_entries(vec![entry("h1", "SELECT 1"), entry("h2", "SELECT 2")], cx));
        let opened = Rc::new(RefCell::new(Vec::new()));
        let seen = opened.clone();
        cx.update(|_, cx| {
            cx.subscribe(&panel, move |_, event: &HistoryEvent, _| {
                let HistoryEvent::Open { sql, .. } = event;
                seen.borrow_mut().push(sql.clone());
            })
            .detach()
        });
        cx.run_until_parked();
        panel.update_in(cx, |panel, window, cx| panel.focus_list(window, cx));
        cx.simulate_keystrokes("end enter");
        cx.run_until_parked();
        assert_eq!(*opened.borrow(), ["SELECT 2"]);
    }

    // Only the rows on screen are built, so a long list scrolls as lightly as a short one.
    #[gpui_kit::test]
    fn a_long_history_builds_only_the_rows_on_screen(cx: &mut TestAppContext) {
        let env = Env::new(cx);
        let (panel, cx) = in_root(cx, HistoryPanel::new);
        let now = jiff::Timestamp::now();
        let entries = (0..200)
            .map(|n| HistoryEntry {
                id: format!("h{n}"),
                connection_id: env.connection.id.clone(),
                sql: format!("SELECT {n}"),
                executed_at: (now - jiff::SignedDuration::from_mins(n * 37)).to_string(),
                success: true,
                ..Default::default()
            })
            .collect();
        panel.update(cx, |panel, cx| panel.show_entries(entries, cx));
        cx.run_until_parked();
        assert!(cx.debug_bounds("history-row-0").is_some(), "the first run is on screen");
        assert!(cx.debug_bounds("history-row-150").is_none(), "a run far below isn't built");
        panel.update_in(cx, |panel, window, cx| panel.focus_list(window, cx));
        cx.simulate_keystrokes("end");
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(cx.debug_bounds("history-row-199").is_some(), "End scrolls to the last run");
    }

    #[gpui_kit::test]
    fn a_long_saved_list_builds_only_the_rows_on_screen(cx: &mut TestAppContext) {
        let env = Env::new(cx);
        for n in 0..200 {
            let query = SavedQuery {
                name: format!("Query {n:03}"),
                connection_id: env.connection.id.clone(),
                sql: format!("SELECT {n}"),
                ..Default::default()
            };
            env.bar.save_saved_query(query).unwrap();
        }
        cx.update(saved_queries::refresh);
        let (panel, cx) = in_root(cx, SavedPanel::new);
        let (id, connections) = (env.connection.id.clone(), env.bar.list_connections());
        panel.update(cx, |panel, cx| panel.set_connection(Some(id), connections, cx));
        cx.run_until_parked();
        assert!(cx.debug_bounds("saved-row-0").is_some(), "the first query is on screen");
        assert!(cx.debug_bounds("saved-row-150").is_none(), "a query far below isn't built");
        panel.update_in(cx, |panel, window, cx| panel.focus_list(window, cx));
        cx.simulate_keystrokes("end");
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(cx.debug_bounds("saved-row-199").is_some(), "End scrolls to the last query");
    }

    #[test]
    fn row_buttons_keep_the_rows_hand() {
        use gpui_kit::Styled as _;
        let mut button = super::row_button("delete", gpui_kit::assets::IconName::Trash);
        assert_eq!(button.style().mouse_cursor, Some(gpui_kit::CursorStyle::PointingHand));
    }

    #[gpui_kit::test]
    fn connections_activate_from_the_keyboard(cx: &mut TestAppContext) {
        let env = Env::new(cx);
        let (panel, cx) = in_root(cx, |_, cx| ConnectionsPanel::new(cx));
        let connections = env.bar.list_connections();
        panel.update(cx, |panel, cx| panel.set_state(connections, Vec::new(), None, cx));
        let activated = Rc::new(RefCell::new(Vec::new()));
        let seen = activated.clone();
        cx.update(|_, cx| {
            cx.subscribe(&panel, move |_, event: &ConnectionsEvent, _| {
                if let ConnectionsEvent::Activate(id) = event {
                    seen.borrow_mut().push(id.clone());
                }
            })
            .detach()
        });
        cx.run_until_parked();
        panel.update_in(cx, |panel, window, cx| panel.focus_list(window, cx));
        cx.simulate_keystrokes("up space");
        cx.run_until_parked();
        assert_eq!(*activated.borrow(), std::slice::from_ref(&env.connection.id));
    }
}
