use crate::tokens::RADIUS;
use gpui_kit::component::{ActiveTheme, h_flex};
use gpui_kit::*;

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

// Rings the cursor row only after a key moves it, not after a click or when the list takes focus.
pub struct ListNav {
    pub focus: FocusHandle,
    pub scroll: ScrollHandle,
    cursor: Option<String>,
    keyed: bool,
}

impl ListNav {
    pub fn new(cx: &mut App) -> Self {
        Self { focus: cx.focus_handle(), scroll: ScrollHandle::new(), cursor: None, keyed: false }
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
        self.scroll.scroll_to_item(target.1);
    }

    pub fn ringed(&self, id: &str, window: &Window) -> bool {
        self.keyed && self.focus.is_focused(window) && self.cursor.as_deref() == Some(id)
    }
}

// Added as the last child so the row's hover actions don't cover it.
pub fn ring<E: ParentElement + Styled>(el: E, cx: &App) -> E {
    el.relative().child(div().absolute().inset_0().rounded(RADIUS).border_2().border_color(cx.theme().primary))
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
    use gpui_kit::component::{Root, WindowExt};
    use gpui_kit::{AppContext as _, Entity, Render, TestAppContext, VisualTestContext};

    use crate::connections_panel::{ConnectionsEvent, ConnectionsPanel};
    use crate::history_panel::{HistoryEvent, HistoryPanel};
    use crate::saved_panel::{SavedEvent, SavedPanel};
    use crate::saved_queries;
    use crate::test_support::Env;

    fn window<V: Render>(
        cx: &mut TestAppContext,
        build: impl FnOnce(&mut gpui_kit::Window, &mut gpui_kit::Context<V>) -> V,
    ) -> (Entity<V>, &mut VisualTestContext) {
        let mut view = None;
        let handle = cx.add_window(|window, cx| {
            let entity = cx.new(|cx| build(window, cx));
            view = Some(entity.clone());
            Root::new(entity, window, cx)
        });
        (view.unwrap(), VisualTestContext::from_window(handle.into(), cx).into_mut())
    }

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
        let (host, cx) = window(cx, |_, cx| Host { nav: super::ListNav::new(cx) });
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
        let (panel, cx) = window(cx, SavedPanel::new);
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
        let (panel, cx) = window(cx, HistoryPanel::new);
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

    #[gpui_kit::test]
    fn connections_activate_from_the_keyboard(cx: &mut TestAppContext) {
        let env = Env::new(cx);
        let (panel, cx) = window(cx, |_, cx| ConnectionsPanel::new(cx));
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
