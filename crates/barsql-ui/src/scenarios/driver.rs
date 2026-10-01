use barsql_core::{ConnectionConfig, Value};
use gpui_kit::component::scroll::ScrollbarMode;
use gpui_kit::component::{Root, Theme};
use gpui_kit::{
    Action, AppContext as _, Bounds, Entity, Modifiers, MouseButton, MouseDownEvent, MouseUpEvent, Pixels, Point,
    ScrollDelta, ScrollWheelEvent, TestAppContext, VisualTestContext, point, px,
};

use crate::grid::Grid;
use crate::plan_view::PlanView;
use crate::query_tab::QueryTab;
use crate::results::{ResultStatus, ResultsPanel, ShownError};
use crate::sidebar::Sidebar;
use crate::test_support::{Env, settle};
use crate::workspace::Workspace;

pub struct Driver<'a> {
    pub env: Env,
    pub connection: ConnectionConfig,
    pub workspace: Entity<Workspace>,
    pub cx: &'a mut VisualTestContext,
}

pub fn open(cx: &mut TestAppContext) -> Driver<'_> {
    open_with(cx, |env| env.connection.clone())
}

// `connection` must return a saved connection, such as one of the compose databases.
pub fn open_with(cx: &mut TestAppContext, connection: impl FnOnce(&Env) -> ConnectionConfig) -> Driver<'_> {
    let env = Env::new(cx);
    let connection = connection(&env);
    let mut workspace = None;
    let window = cx.add_window(|window, cx| {
        let view = cx.new(|cx| Workspace::new(window, cx));
        workspace = Some(view.clone());
        Root::new(view, window, cx)
    });
    let cx = VisualTestContext::from_window(window.into(), cx).into_mut();
    cx.run_until_parked();
    Driver { env, connection, workspace: workspace.unwrap(), cx }
}

// debug_bounds wants a &'static str, so run-time selectors are leaked. Fine for tests.
pub fn selector(text: String) -> &'static str {
    Box::leak(text.into_boxed_str())
}

// "mod" means the platform's command key, as in the default shortcuts.
pub fn chord(keys: &str) -> String {
    keys.replace("mod-", if cfg!(target_os = "macos") { "cmd-" } else { "ctrl-" })
}

impl Driver<'_> {
    pub fn seed(&self, sql: &str) {
        for statement in sql.split(';').map(str::trim).filter(|s| !s.is_empty()) {
            self.env.runtime.block_on(self.env.bar.execute_query(&self.connection.id, statement)).unwrap();
        }
    }

    pub fn query(&self, sql: &str) -> Vec<Vec<Value>> {
        self.env.runtime.block_on(self.env.bar.execute_query(&self.connection.id, sql)).unwrap().rows
    }

    // Keys go to what the latest frame focused, so the frame is drawn first.
    pub fn keys(&mut self, keys: &str) {
        let was_open = self.dialog_open();
        self.draw();
        self.cx.simulate_keystrokes(&chord(keys));
        self.cx.run_until_parked();
        self.after_dialogs(was_open);
    }

    // Dialogs open at once with motion reduced, but a closing one hands focus back only after GPUI Kit's
    // close delay, which runs on the test clock.
    fn after_dialogs(&mut self, was_open: bool) {
        if self.dialog_open() != was_open {
            self.pause();
        }
    }

    // Pauses afterwards for the debounced filters and searches.
    pub fn type_text(&mut self, text: &str) {
        self.draw();
        self.cx.simulate_input(text);
        self.pause();
    }

    pub fn pause(&mut self) {
        self.cx.run_until_parked();
        self.cx.executor().advance_clock(std::time::Duration::from_millis(500));
        self.cx.run_until_parked();
    }

    pub fn schema_name(&self) -> String {
        match self.connection.driver {
            barsql_core::DriverType::Sqlite => "main".into(),
            barsql_core::DriverType::Postgres => "public".into(),
            _ => self.connection.database.clone(),
        }
    }

    pub fn refresh_schema(&mut self) {
        self.click("refresh-schema");
        self.wait_schema();
    }

    // Waits on the schema cache, which completion, quick search and the tree read.
    pub fn wait_schema(&mut self) {
        let (id, driver) = (self.connection.id.clone(), self.connection.driver.clone());
        let schema_name = self.schema_name();
        self.cx.update(|_, cx| crate::schema::ensure_loaded(&id, driver, cx));
        settle(self.cx, |cx| {
            cx.update(|_, cx| crate::schema::get(cx, &id).is_some_and(|entry| entry.tables(&schema_name).is_some()))
        });
    }

    pub fn dispatch(&mut self, action: impl Action) {
        let was_open = self.dialog_open();
        self.cx.dispatch_action(action);
        self.cx.run_until_parked();
        self.after_dialogs(was_open);
    }

    pub fn settle(&mut self, done: impl FnMut(&mut VisualTestContext) -> bool) {
        settle(self.cx, done);
    }

    pub fn draw(&mut self) {
        self.cx.run_until_parked();
        self.cx.update(|window, cx| window.draw(cx).clear(cx));
    }

    pub fn bounds(&mut self, selector: &'static str) -> Option<Bounds<Pixels>> {
        self.draw();
        self.cx.debug_bounds(selector)
    }

    pub fn shown(&mut self, selector: &'static str) -> bool {
        self.bounds(selector).is_some()
    }

    fn center(&mut self, selector: &'static str) -> gpui_kit::Point<Pixels> {
        self.bounds(selector).unwrap_or_else(|| panic!("{selector} is not drawn")).center()
    }

    pub fn click(&mut self, selector: &'static str) {
        self.click_with(selector, Modifiers::none());
    }

    pub fn click_with(&mut self, selector: &'static str, modifiers: Modifiers) {
        let was_open = self.dialog_open();
        let at = self.center(selector);
        self.cx.simulate_click(at, modifiers);
        self.pause();
        self.after_dialogs(was_open);
    }

    pub fn hover(&mut self, selector: &'static str) {
        let at = self.center(selector);
        self.cx.simulate_mouse_move(at, None, Modifiers::none());
        self.cx.run_until_parked();
    }

    // Picks item `downs` with the keyboard. Separators don't count.
    pub fn context_menu(&mut self, selector: &'static str, downs: usize) {
        let position = self.center(selector);
        let down = MouseDownEvent {
            button: MouseButton::Right,
            position,
            modifiers: Modifiers::none(),
            click_count: 1,
            first_mouse: false,
        };
        self.cx.simulate_event(down);
        self.cx.run_until_parked();
        self.cx.update(|window, cx| window.draw(cx).clear(cx));
        self.keys(&format!("{}enter", "down ".repeat(downs + 1)));
        self.pause();
    }

    // Input and editor menus open on mouse up, not down.
    pub fn input_menu(&mut self, selector: &'static str, downs: usize) {
        let position = self.center(selector);
        let (button, modifiers) = (MouseButton::Right, Modifiers::none());
        self.cx.simulate_event(MouseDownEvent { button, position, modifiers, click_count: 1, first_mouse: false });
        self.cx.simulate_event(MouseUpEvent { button, position, modifiers, click_count: 1 });
        self.cx.run_until_parked();
        self.cx.update(|window, cx| window.draw(cx).clear(cx));
        self.keys(&format!("{}enter", "down ".repeat(downs + 1)));
        self.pause();
    }

    // Same as clicking OK, which dispatches Confirm.
    pub fn confirm_dialog(&mut self) {
        self.dispatch(gpui_kit::component::dialog::Confirm { secondary: false });
        self.pause();
    }

    pub fn dialog_open(&mut self) -> bool {
        self.cx.update(gpui_kit::component::WindowExt::has_active_dialog)
    }

    // For dialogs that close after async work, like a save or an insert.
    pub fn settle_dialog_closed(&mut self) {
        self.settle(|cx| !cx.update(gpui_kit::component::WindowExt::has_active_dialog));
        self.after_dialogs(true);
    }

    pub fn sidebar(&mut self) -> Entity<Sidebar> {
        self.cx.update(|_, cx| self.workspace.read(cx).sidebar())
    }

    // Choosing the connection in the switcher lands in its query tab.
    pub fn connect(&mut self) -> Entity<QueryTab> {
        let id = self.connection.id.clone();
        self.env.runtime.block_on(self.env.bar.connect(&id)).unwrap();
        let sidebar = self.sidebar();
        sidebar.update(self.cx, |sidebar, cx| sidebar.choose(&id, cx));
        self.cx.run_until_parked();
        self.tab()
    }

    // A new window on the same data folder, like a restart.
    pub fn reopen(&mut self) {
        let mut workspace = None;
        let window = self.cx.add_window(|window, cx| {
            let view = cx.new(|cx| Workspace::new(window, cx));
            workspace = Some(view.clone());
            Root::new(view, window, cx)
        });
        let cx = VisualTestContext::from_window(window.into(), self.cx).into_mut();
        cx.run_until_parked();
        self.cx = cx;
        self.workspace = workspace.unwrap();
    }

    pub fn tab(&mut self) -> Entity<QueryTab> {
        self.cx.update(|_, cx| self.workspace.read(cx).query_tab()).expect("a query tab is active")
    }

    pub fn titles(&mut self) -> Vec<String> {
        self.cx.update(|_, cx| self.workspace.read(cx).titles(cx))
    }

    pub fn active(&mut self) -> usize {
        self.cx.update(|_, cx| self.workspace.read(cx).active_index())
    }

    pub fn set_sql(&mut self, sql: &str) {
        let tab = self.tab();
        self.cx.update(|window, cx| {
            let editor = tab.read(cx).editor();
            editor.update(cx, |state, cx| state.set_value(sql.to_string(), window, cx));
            tab.update(cx, |tab, cx| tab.focus_editor(window, cx));
        });
        self.cx.run_until_parked();
    }

    pub fn sql(&mut self) -> String {
        let tab = self.tab();
        self.cx.update(|_, cx| tab.read(cx).sql(cx).to_string())
    }

    pub fn wait_idle(&mut self) {
        let tab = self.tab();
        settle(self.cx, |cx| cx.update(|_, cx| !tab.read(cx).is_running()));
    }

    pub fn run(&mut self, sql: &str) {
        self.set_sql(sql);
        self.click("run-all");
        self.wait_idle();
    }

    pub fn results(&mut self) -> Entity<ResultsPanel> {
        let tab = self.tab();
        self.cx.update(|_, cx| tab.read(cx).results())
    }

    pub fn status(&mut self) -> ResultStatus {
        let results = self.results();
        self.cx.update(|_, cx| results.read(cx).status(cx))
    }

    pub fn status_is_error(&mut self) -> bool {
        let tab = self.tab();
        self.cx.update(|_, cx| tab.read(cx).status(cx).error)
    }

    pub fn result_tabs(&mut self) -> usize {
        let results = self.results();
        self.cx.update(|_, cx| results.read(cx).result_count())
    }

    pub fn error(&mut self) -> Option<ShownError> {
        let results = self.results();
        self.cx.update(|_, cx| results.read(cx).active_error())
    }

    pub fn grid(&mut self) -> Option<Entity<Grid>> {
        let results = self.results();
        self.cx.update(|_, cx| results.read(cx).active_grid())
    }

    pub fn plan(&mut self) -> Option<Entity<PlanView>> {
        let results = self.results();
        self.cx.update(|_, cx| results.read(cx).active_plan())
    }

    // `row` and `column` are display positions, after sorting and hiding columns.
    pub fn cell(&mut self, row: usize, column: usize) -> Option<String> {
        let grid = self.grid()?;
        self.cx.update(|_, cx| grid.read(cx).text_at(row, column))
    }

    pub fn click_at(&mut self, at: Point<Pixels>, modifiers: Modifiers) {
        self.cx.simulate_click(at, modifiers);
        self.cx.run_until_parked();
    }

    // A mouse wheel or trackpad over `at`. Negative `dy` scrolls down.
    pub fn wheel(&mut self, at: Point<Pixels>, dy: Pixels) {
        self.draw();
        self.cx.simulate_event(ScrollWheelEvent {
            position: at,
            delta: ScrollDelta::Pixels(point(px(0.), dy)),
            ..Default::default()
        });
        self.cx.run_until_parked();
    }

    // The wheel leaves the pointer over the list, which shows its bar, and a click low on its track then jumps down.
    // That a scroll alone shows a bar, with the pointer away, is tested in scrollbars.rs.
    pub fn scrolls_by_its_bar(&mut self, list: &'static str, first_row: &'static str) {
        let frame = self.bounds(list).unwrap_or_else(|| panic!("{list} is not drawn"));
        let row = self.bounds(first_row).unwrap_or_else(|| panic!("{first_row} is not drawn"));
        self.wheel(frame.center(), px(-40.));
        self.draw();
        self.click_at(point(frame.right() - px(8.), frame.bottom() - px(12.)), Modifiers::none());
        // A virtual list stops drawing rows it scrolled past.
        let jumped = self.bounds(first_row).is_none_or(|moved| moved.top() < row.top() - px(200.));
        assert!(jumped, "a click on the bar's track scrolls {list}");
    }

    // Like a Mac with a trackpad, where GPUI Kit hides a bar until its area scrolls.
    pub fn overlay_scrollbars(&mut self) {
        self.cx.update(|_, cx| Theme::set_scrollbar_mode(ScrollbarMode::Scrolling, cx));
        self.draw();
    }

    pub fn hover_at(&mut self, at: Point<Pixels>) {
        self.cx.simulate_mouse_move(at, None, Modifiers::none());
        self.draw();
    }

    // After overlay_scrollbars, the list's bar shows only while the pointer is over the list. With the pointer away,
    // a click low on its track goes to the list. With it over the list, the click jumps down.
    pub fn shows_its_bar_on_hover(&mut self, list: &'static str, first_row: &'static str) {
        let frame = self.bounds(list).unwrap_or_else(|| panic!("{list} is not drawn"));
        let row = self.bounds(first_row).unwrap_or_else(|| panic!("{first_row} is not drawn"));
        let track = point(frame.right() - px(8.), frame.bottom() - px(12.));
        self.hover_at(point(frame.right() + px(40.), frame.center().y));
        self.click_at(track, Modifiers::none());
        assert_eq!(self.bounds(first_row), Some(row), "away from {list}, its bar is hidden");
        self.hover_at(frame.center());
        self.click_at(track, Modifiers::none());
        let jumped = self.bounds(first_row).is_none_or(|moved| moved.top() < row.top() - px(200.));
        assert!(jumped, "hovering {list} shows its bar");
    }

    pub fn double_click_at(&mut self, position: Point<Pixels>) {
        for click_count in [1, 2] {
            let down = MouseDownEvent {
                button: MouseButton::Left,
                position,
                modifiers: Modifiers::none(),
                click_count,
                first_mouse: false,
            };
            self.cx.simulate_event(down);
            let up = MouseUpEvent { button: MouseButton::Left, position, modifiers: Modifiers::none(), click_count };
            self.cx.simulate_event(up);
        }
        self.cx.run_until_parked();
    }

    // Draws first so the grid's layout is current.
    pub fn grid_point(&mut self, grid: &Entity<Grid>, at: impl FnOnce(&Grid) -> Point<Pixels>) -> Point<Pixels> {
        self.cx.run_until_parked();
        self.cx.update(|window, cx| window.draw(cx).clear(cx));
        self.cx.update(|_, cx| at(grid.read(cx)))
    }

    pub fn menu_pick(&mut self, selector: &'static str, downs: usize) {
        self.click(selector);
        self.cx.update(|window, cx| window.draw(cx).clear(cx));
        self.keys(&format!("{}enter", "down ".repeat(downs + 1)));
        self.pause();
    }
}
