use std::time::Duration;

use barsql_app::{AppEvent, EditorSession, EditorTab, TableViewRef, is_sqlite_file, sqlite_file_payload};
use barsql_core::{ConnectionConfig, SavedQuery};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::menu::AppMenuBar;
use gpui_kit::component::resizable::{ResizableState, h_resizable, resizable_panel};
use gpui_kit::component::scroll::Scrollbar;
use gpui_kit::component::{
    ActiveTheme, Icon, IconName, Root, StyledExt, ThemeMode, TitleBar, WindowExt, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::actions::*;
use crate::dialogs;
use crate::grid::RowRef;
use crate::i18n::{I18n, t, t_with};
use crate::json_panel::JsonPanel;
use crate::query_tab::{QueryTab, QueryTabEvent, new_tab_id};
use crate::quick_search::{self, QuickSearchDialog, TabEntry, Target};
use crate::schema_tree::TableChange;
use crate::sidebar::{Sidebar, SidebarEvent};
use crate::state::{self, set_setting, set_setting_bool, set_setting_json, setting_bool};
use crate::status_bar::{self, StatusBarState};
use crate::table_tab::{TableTab, TableTabEvent};
use crate::toast;
use crate::tokens::{ICON_2XS, ICON_XL, ICON_XS, RADIUS, RADIUS_LG, TEXT_LG, TEXT_MD, TEXT_SM};
use crate::window_state::{self, WindowState};
use crate::{
    LaunchOptions, about_dialog, context_menu, grid, schema, screenshots, shortcuts_dialog, theme, tips_dialog,
    title_bar, update_dialog,
};

const SIDEBAR_OPEN_KEY: &str = "barsql-sidebar-open";
const JSON_OPEN_KEY: &str = "barsql-json-open";
const SIDEBAR_WIDTH_KEY: &str = "barsql-sidebar-w";
const JSON_WIDTH_KEY: &str = "barsql-json-w";
const SIDEBAR_WIDTHS: (f32, f32, f32) = (280., 200., 520.);
const JSON_WIDTHS: (f32, f32, f32) = (320., 220., 640.);
const CLOSED_TABS_LIMIT: usize = 10;
const SESSION_SAVE_DEBOUNCE: Duration = Duration::from_secs(1);

#[derive(Clone, PartialEq)]
enum TabView {
    Query(Entity<QueryTab>),
    Table(Entity<TableTab>),
}

impl TabView {
    fn query(&self) -> Option<&Entity<QueryTab>> {
        match self {
            Self::Query(tab) => Some(tab),
            Self::Table(_) => None,
        }
    }

    fn id(&self, cx: &App) -> String {
        match self {
            Self::Query(tab) => tab.read(cx).id.clone(),
            Self::Table(tab) => tab.read(cx).id.clone(),
        }
    }

    fn title(&self, cx: &App) -> SharedString {
        match self {
            Self::Query(tab) => tab.read(cx).title.clone(),
            Self::Table(tab) => tab.read(cx).title.clone(),
        }
    }

    fn connection<'a>(&self, cx: &'a App) -> &'a ConnectionConfig {
        match self {
            Self::Query(tab) => &tab.read(cx).connection,
            Self::Table(tab) => &tab.read(cx).connection,
        }
    }

    fn stored(&self, cx: &App) -> EditorTab {
        match self {
            Self::Query(tab) => tab.read(cx).stored(cx),
            Self::Table(tab) => tab.read(cx).stored(cx),
        }
    }

    fn is_dirty(&self, cx: &App) -> bool {
        self.query().is_some_and(|tab| tab.read(cx).is_dirty(cx))
    }

    fn focus(&self, window: &mut Window, cx: &mut App) {
        match self {
            Self::Query(tab) => tab.update(cx, |tab, cx| tab.focus_editor(window, cx)),
            Self::Table(tab) => tab.update(cx, |tab, cx| tab.focus(window, cx)),
        }
    }

    fn focused_row(&self, cx: &App) -> Option<RowRef> {
        match self {
            Self::Query(tab) => tab.read(cx).focused_row(cx),
            Self::Table(tab) => tab.read(cx).focused_row(cx),
        }
    }

    fn icon(&self, cx: &App) -> Lucide {
        match self {
            Self::Query(tab) if tab.read(cx).saved_query_id().is_empty() => Lucide::File,
            Self::Query(_) => Lucide::Bookmark,
            Self::Table(tab) => {
                let tab = tab.read(cx);
                if schema::is_view(&tab.connection.id, &tab.schema, &tab.table, cx) {
                    Lucide::View
                } else {
                    Lucide::Table2
                }
            }
        }
    }

    fn quick_entry(&self, cx: &App) -> TabEntry {
        let (table, saved_query_id) = match self {
            Self::Query(tab) => (None, tab.read(cx).saved_query_id().to_string()),
            Self::Table(tab) => {
                let tab = tab.read(cx);
                (Some((tab.schema.clone(), tab.table.clone())), String::new())
            }
        };
        TabEntry {
            id: self.id(cx),
            title: self.title(cx).to_string(),
            connection_id: self.connection(cx).id.clone(),
            icon: self.icon(cx),
            table,
            saved_query_id,
        }
    }

    fn view(&self) -> AnyView {
        match self {
            Self::Query(tab) => tab.clone().into(),
            Self::Table(tab) => tab.clone().into(),
        }
    }
}

struct OpenTab {
    view: TabView,
    _subscriptions: [Subscription; 2],
}

#[derive(Clone)]
struct TabDrag {
    ix: usize,
    title: SharedString,
}

impl Render for TabDrag {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .px_2()
            .py_1()
            .rounded(RADIUS)
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .text_size(TEXT_SM)
            .opacity(0.9)
            .child(self.title.clone())
    }
}

pub struct Workspace {
    sidebar: Entity<Sidebar>,
    menu_bar: Option<Entity<AppMenuBar>>,
    context_menu: Entity<context_menu::Overlay>,
    sidebar_open: bool,
    panels: Entity<ResizableState>,
    sidebar_width: Pixels,
    json_panel: Entity<JsonPanel>,
    json_open: bool,
    json_width: Pixels,
    window_state: Option<WindowState>,
    window_save: Task<()>,
    focus: FocusHandle,
    tabs: Vec<OpenTab>,
    active: usize,
    tab_scroll: ScrollHandle,
    hovered_close: Option<usize>,
    closed: Vec<EditorTab>,
    // Tabs whose connection is gone. Saving puts them back at their old index.
    kept: Vec<(usize, EditorTab)>,
    saved: Option<EditorSession>,
    save_task: Task<()>,
    _subscriptions: Vec<Subscription>,
    _app_events: Task<()>,
}

pub fn open(options: LaunchOptions, cx: &mut App) {
    // Screenshot runs start at 80% of the screen and leave the saved window state alone.
    let track = options.snapshot.is_none() && options.screenshots.is_none();
    let saved = if track { window_state::load(cx) } else { Some(WindowState::default()) };
    let window_options = WindowOptions {
        titlebar: Some(TitlebarOptions { title: Some("BarSQL".into()), ..TitleBar::title_bar_options() }),
        window_bounds: Some(window_state::initial_bounds(saved.as_ref(), cx)),
        window_min_size: Some(size(px(window_state::MIN_WIDTH as f32), px(window_state::MIN_HEIGHT as f32))),
        display_id: cx.primary_display().map(|display| display.id()),
        app_id: Some("BarSQL".into()),
        ..TitleBar::window_options()
    };
    cx.spawn(async move |cx| {
        let panel = options.snapshot_panel.clone();
        let mut after: Option<crate::snapshot::AfterRun> = None;
        let window = cx.open_window(window_options, |window, cx| {
            window.set_rem_size(px(theme::zoom(cx)));
            let view = cx.new(|cx| Workspace::new(window, cx));
            if track {
                view.update(cx, |workspace, cx| workspace.track_window(saved.clone(), window, cx));
            }
            match panel.as_deref() {
                Some("plan") => {
                    let view = view.clone();
                    after = Some(Box::new(move |window, cx| {
                        if let Some(tab) = view.read(cx).active_query().cloned() {
                            tab.update(cx, |tab, cx| tab.focus_editor(window, cx));
                        }
                        window.dispatch_action(Box::new(ExplainAnalyze), cx);
                    }));
                }
                Some("json") => {
                    let view = view.clone();
                    after = Some(Box::new(move |window, cx| {
                        view.update(cx, |workspace, cx| {
                            if !workspace.json_open {
                                workspace.toggle_json_panel(window, cx);
                            }
                        })
                    }));
                }
                Some("about") => after = Some(Box::new(about_dialog::open)),
                Some("shortcuts") => {
                    after = Some(Box::new(|window, cx| {
                        shortcuts_dialog::open(window, cx);
                    }))
                }
                Some("tips") => after = Some(Box::new(tips_dialog::open)),
                Some("toasts") => {
                    after = Some(Box::new(|_, cx| {
                        toast::success(t(cx, "toast.copiedClipboard"), cx);
                        toast::error_in(t(cx, "errors.ddlFailed"), "relation \"nope\" does not exist", cx);
                        let action =
                            toast::ToastAction { label: t(cx, "toast.update"), on_click: std::rc::Rc::new(|_, _| {}) };
                        let message = t_with(cx, "toast.updateAvailable", &[("version", "1.1.0")]);
                        toast::push(message, toast::ToastKind::Info, Some(Duration::ZERO), Some(action), cx);
                    }))
                }
                Some(update) if update.starts_with("update") => {
                    let state = snapshot_update_state(update);
                    after = Some(Box::new(move |window, cx| {
                        about_dialog::open(window, cx);
                        update_dialog::open_in(String::new(), Some(state.clone()), window, cx);
                    }));
                }
                Some(quick) if quick.starts_with("quick") => {
                    let (view, query) = (view.clone(), quick.strip_prefix("quick=").unwrap_or_default().to_string());
                    after = Some(Box::new(move |window, cx| {
                        let dialog = view.update(cx, |workspace, cx| workspace.open_quick_search(window, cx));
                        if let Some(dialog) = dialog.filter(|_| !query.is_empty()) {
                            quick_search::type_query(&dialog, &query, window, cx);
                        }
                    }));
                }
                Some(suggest) if suggest.starts_with("suggest=") => {
                    let (view, text) = (view.clone(), suggest["suggest=".len()..].to_string());
                    after = Some(Box::new(move |window, cx| {
                        let (query, table) = match view.read(cx).active_tab() {
                            Some(TabView::Query(tab)) => (Some(tab.read(cx).completion()), None),
                            Some(TabView::Table(tab)) => (None, Some(tab.read(cx).completion())),
                            None => (None, None),
                        };
                        if let Some(completion) = query {
                            completion.update(cx, |completion, cx| completion.preview(&text, window, cx));
                        }
                        if let Some(completion) = table {
                            completion.update(cx, |completion, cx| completion.preview(&text, window, cx));
                        }
                    }));
                }
                Some(action @ ("cell" | "export")) => {
                    let (view, action) = (view.clone(), action.to_string());
                    after = Some(Box::new(move |window, cx| {
                        let focus = view.read(cx).active_query().and_then(|tab| tab.read(cx).results_focus(cx));
                        if let Some(focus) = focus {
                            window.focus(&focus, cx);
                        }
                        let action: Box<dyn Action> =
                            if action == "cell" { Box::new(grid::ViewCell) } else { Box::new(grid::ExportResults) };
                        window.dispatch_action(action, cx);
                    }));
                }
                Some(panel) => {
                    let (sidebar, panel) = (view.read(cx).sidebar.clone(), panel.to_string());
                    window.defer(cx, move |window, cx| {
                        sidebar.update(cx, |sidebar, cx| sidebar.show_panel(&panel, window, cx))
                    });
                }
                None => {}
            }
            cx.new(|cx| Root::new(view, window, cx))
        });
        if track && let Ok(window) = window {
            let _ = window.update(cx, |root, window, cx| {
                if let Some((file_path, name)) = state::bar(cx).take_pending_file()
                    && let Ok(view) = root.view().clone().downcast::<Workspace>()
                {
                    window.defer(cx, move |window, cx| {
                        view.update(cx, |workspace, cx| workspace.open_sqlite(file_path, name, window, cx))
                    });
                }
            });
        }
        if options.check_updates
            && let Ok(window) = window
        {
            let _ = window.update(cx, |_, _, cx| update_dialog::check_on_startup(cx));
        }
        if let Ok(window) = window {
            // macOS keeps a titled window inside the screen, so a screenshot larger than the screen goes
            // borderless first.
            if let Some((width, height)) = options.snapshot_size.filter(|_| !track) {
                let _ = window.update(cx, |_, window, _| {
                    window.toggle_simple_fullscreen();
                    window.resize(size(px(width), px(height)));
                });
            }
            if let Some(path) = options.snapshot {
                crate::snapshot::schedule(window.into(), path, options.snapshot_run, after, cx);
            }
            if let Some(path) = options.screenshots {
                screenshots::run(window, path, cx);
            }
        }
    })
    .detach();
}

impl Workspace {
    pub(crate) fn context_menu(&self) -> Entity<context_menu::Overlay> {
        self.context_menu.clone()
    }

    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let sidebar = cx.new(|cx| Sidebar::new(window, cx));
        let subscriptions = vec![
            cx.subscribe_in(&sidebar, window, |this, _, event: &SidebarEvent, window, cx| match event {
                SidebarEvent::Selected(connection) => this.focus_or_open_tab(connection.clone(), window, cx),
                SidebarEvent::Insert(text) => this.insert_into_editor(text, window, cx),
                SidebarEvent::OpenQuery { connection, sql, title } => {
                    this.open_query_tab_with(connection.clone(), sql.clone(), title.clone().into(), window, cx)
                }
                SidebarEvent::OpenSql { connection, sql } => this.open_sql(connection.clone(), sql.clone(), window, cx),
                SidebarEvent::OpenSaved(query) => this.open_saved(query.clone(), window, cx),
                SidebarEvent::Browse { connection, schema, table } => {
                    this.open_table(connection.clone(), schema.clone(), table.clone(), None, window, cx)
                }
                SidebarEvent::ConnectionsChanged(connections) => {
                    for tab in this.query_tabs() {
                        let id = tab.read(cx).connection.id.clone();
                        if let Some(connection) = connections.iter().find(|c| c.id == id).cloned() {
                            tab.update(cx, |tab, cx| tab.set_connection(connection, cx));
                        }
                    }
                }
                SidebarEvent::SavedRenamed { id, name } => {
                    for tab in this.query_tabs() {
                        if tab.read(cx).saved_query_id() == id {
                            tab.update(cx, |tab, cx| tab.set_title(name.clone().into(), cx));
                        }
                    }
                }
                SidebarEvent::TableChanged { connection_id, schema, table, change } => {
                    this.table_changed(connection_id, schema, table, change, window, cx)
                }
            }),
            // Quitting doesn't wait for the debounces.
            cx.on_app_quit(|this, cx| {
                this.save_session(cx);
                this.save_window_state(cx);
                async {}
            }),
        ];
        let subscriptions = subscriptions
            .into_iter()
            .chain([cx.observe_global::<toast::Toasts>(|_, cx| cx.notify())])
            .collect::<Vec<_>>();
        let events = state::bar(cx).events();
        let app_events = cx.spawn_in(window, async move |this, cx| {
            while let Ok(event) = events.recv().await {
                if this.update_in(cx, |this, window, cx| this.app_event(event, window, cx)).is_err() {
                    break;
                }
            }
        });
        let mut this = Self {
            sidebar,
            menu_bar: (!cfg!(target_os = "macos") || screenshots::windows_title_bar(cx)).then(|| {
                let menu_bar = AppMenuBar::new(cx);
                crate::actions::register_menu_bar(&menu_bar, cx);
                menu_bar
            }),
            context_menu: cx.new(|_| context_menu::Overlay::new()),
            sidebar_open: setting_bool(cx, SIDEBAR_OPEN_KEY, true),
            panels: cx.new(|_| ResizableState::default()),
            sidebar_width: panel_width(cx, SIDEBAR_WIDTH_KEY, SIDEBAR_WIDTHS),
            json_panel: cx.new(|cx| JsonPanel::new(window, cx)),
            json_open: setting_bool(cx, JSON_OPEN_KEY, false),
            json_width: panel_width(cx, JSON_WIDTH_KEY, JSON_WIDTHS),
            window_state: None,
            window_save: Task::ready(()),
            focus,
            tabs: Vec::new(),
            active: 0,
            tab_scroll: ScrollHandle::new(),
            hovered_close: None,
            closed: Vec::new(),
            kept: Vec::new(),
            saved: None,
            save_task: Task::ready(()),
            _subscriptions: subscriptions,
            _app_events: app_events,
        };
        this.restore_session(window, cx);
        this
    }

    fn restore_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let session = state::bar(cx).editor_session();
        self.saved = Some(session.clone());
        for (ix, tab) in session.tabs.into_iter().enumerate() {
            match self.sidebar.read(cx).connection(&tab.connection_id).cloned() {
                Some(connection) => self.add_tab(tab, connection, window, cx),
                None => self.kept.push((ix, tab)),
            }
        }
        let active = self.tabs.iter().position(|tab| tab.view.id(cx) == session.active_tab).unwrap_or(0);
        self.activate(active, window, cx);
        if !self.tabs.is_empty() {
            self.reveal_active_tab(window, cx);
        }
    }

    // GPUI drops a scroll request made before the strip's first layout, so a restored session asks again
    // once the strip has been laid out.
    fn reveal_active_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tab_scroll.bounds().size.width > px(0.) {
            self.tab_scroll.scroll_to_item(self.active);
            cx.notify();
            return;
        }
        let this = cx.weak_entity();
        window.on_next_frame(move |window, cx| {
            let _ = this.update(cx, |this, cx| this.reveal_active_tab(window, cx));
        });
    }

    fn session(&self, cx: &App) -> EditorSession {
        let mut tabs: Vec<EditorTab> = self.tabs.iter().map(|tab| tab.view.stored(cx)).collect();
        for (ix, tab) in &self.kept {
            tabs.insert((*ix).min(tabs.len()), tab.clone());
        }
        let active_tab = self.active_tab().map(|tab| tab.id(cx)).unwrap_or_default();
        EditorSession { tabs, active_tab }
    }

    fn save_session(&mut self, cx: &mut Context<Self>) {
        self.save_task = Task::ready(());
        let session = self.session(cx);
        if self.saved.as_ref() == Some(&session) {
            return;
        }
        if state::bar(cx).save_editor_session(session.clone()).is_ok() {
            self.saved = Some(session);
        }
    }

    fn schedule_save(&mut self, cx: &mut Context<Self>) {
        self.save_task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SESSION_SAVE_DEBOUNCE).await;
            let _ = this.update(cx, |this, cx| this.save_session(cx));
        });
    }

    // Mode changes save at once. Moves and resizes save after a pause.
    fn track_window(&mut self, saved: Option<WindowState>, window: &mut Window, cx: &mut Context<Self>) {
        self.window_state = Some(saved.unwrap_or_default());
        let subscription = cx.observe_window_bounds(window, |this, window, cx| {
            let last = this.window_state.clone().unwrap_or_default();
            let next = window_state::capture(&last, window, cx);
            if next == last {
                return;
            }
            // A first record of a normal window waits like a resize.
            let mode_changed = next.mode != last.mode && !last.mode.is_empty();
            let immediate = mode_changed || (last.mode.is_empty() && next.mode != "normal");
            this.window_state = Some(next);
            if immediate {
                this.save_window_state(cx);
            } else {
                this.window_save = cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(window_state::DEBOUNCE).await;
                    let _ = this.update(cx, |this, cx| this.save_window_state(cx));
                });
            }
        });
        self._subscriptions.push(subscription);
    }

    // Nothing is written before a mode is known.
    fn save_window_state(&mut self, cx: &mut Context<Self>) {
        self.window_save = Task::ready(());
        if let Some(state) = self.window_state.as_ref().filter(|state| !state.mode.is_empty()) {
            set_setting_json(cx, window_state::KEY, state);
        }
    }

    fn panels_resized(&mut self, sizes: &[Pixels], cx: &mut Context<Self>) {
        let store = |key: &str, width: Pixels| set_setting(cx, key, &(width.as_f32().round() as i64).to_string());
        if self.sidebar_open
            && let Some(&width) = sizes.first()
        {
            self.sidebar_width = width;
            store(SIDEBAR_WIDTH_KEY, width);
        }
        if self.json_open
            && let Some(&width) = sizes.last()
        {
            self.json_width = width;
            store(JSON_WIDTH_KEY, width);
        }
    }

    fn app_event(&mut self, event: AppEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            AppEvent::TransactionsEnded { tab_ids } => {
                for tab in self.query_tabs() {
                    if tab_ids.contains(&tab.read(cx).id) {
                        tab.update(cx, |tab, cx| tab.set_transaction_ended(cx));
                    }
                }
            }
            AppEvent::OpenSqlite { file_path, name } => self.open_sqlite(file_path, name, window, cx),
            AppEvent::Activate => {
                cx.activate(true);
                window.activate_window();
            }
            AppEvent::UpdateAvailable { .. } => {}
        }
    }

    fn open_sqlite(&mut self, file_path: String, name: String, window: &mut Window, cx: &mut Context<Self>) {
        self.sidebar.update(cx, |sidebar, cx| sidebar.open_sqlite(file_path, name, window, cx));
    }

    fn dropped(&mut self, paths: &ExternalPaths, window: &mut Window, cx: &mut Context<Self>) {
        let path = paths.paths().iter().map(|path| path.display().to_string()).find(|path| is_sqlite_file(path));
        if let Some(path) = path {
            let (file_path, name) = sqlite_file_payload(&path);
            self.open_sqlite(file_path, name, window, cx);
        }
    }

    fn drop_overlay(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        div()
            .id("file-drop-overlay")
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .opacity(0.)
            .drag_over::<ExternalPaths>(|style, _, _, cx| style.opacity(1.).bg(cx.theme().overlay))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| this.dropped(paths, window, cx)))
            .child(
                v_flex()
                    .items_center()
                    .gap(rems(1.))
                    .px(rems(3.5))
                    .py(rems(2.5))
                    .border_2()
                    .border_dashed()
                    .border_color(theme.primary)
                    .rounded(RADIUS_LG)
                    .bg(theme.popover)
                    .text_size(TEXT_MD)
                    .font_medium()
                    .child(Icon::new(Lucide::Database).size(ICON_XL).text_color(theme.primary))
                    .child(t(cx, "connection.dropSqlite")),
            )
    }

    fn active_tab(&self) -> Option<&TabView> {
        self.tabs.get(self.active).map(|tab| &tab.view)
    }

    fn active_query(&self) -> Option<&Entity<QueryTab>> {
        self.active_tab().and_then(TabView::query)
    }

    fn query_tabs(&self) -> Vec<Entity<QueryTab>> {
        self.tabs.iter().filter_map(|tab| tab.view.query().cloned()).collect()
    }

    fn add_tab(&mut self, tab: EditorTab, connection: ConnectionConfig, window: &mut Window, cx: &mut Context<Self>) {
        if tab.table_view.is_some() {
            let view = cx.new(|cx| TableTab::new(tab, connection, window, cx));
            let subscriptions = [
                cx.observe(&view, |_, _, cx| cx.notify()),
                cx.subscribe_in(&view, window, |this, view, event: &TableTabEvent, window, cx| match event {
                    TableTabEvent::Edited => this.schedule_save(cx),
                    TableTabEvent::FocusedRowChanged => {
                        if this.active_tab() == Some(&TabView::Table(view.clone())) {
                            this.sync_json_panel(window, cx);
                        }
                    }
                    TableTabEvent::OpenTable { schema, table, filter } => {
                        let connection = view.read(cx).connection.clone();
                        this.open_table(connection, schema.clone(), table.clone(), Some(filter.clone()), window, cx);
                    }
                }),
            ];
            self.tabs.push(OpenTab { view: TabView::Table(view), _subscriptions: subscriptions });
            return;
        }
        let view = cx.new(|cx| QueryTab::new(tab, connection, window, cx));
        let subscriptions = [
            cx.observe(&view, |_, _, cx| cx.notify()),
            cx.subscribe_in(&view, window, |this, view, event: &QueryTabEvent, window, cx| match event {
                QueryTabEvent::Edited => this.schedule_save(cx),
                QueryTabEvent::RunFinished => this.sidebar.update(cx, |sidebar, cx| sidebar.refresh_history(cx)),
                QueryTabEvent::FocusedRowChanged => {
                    if this.active_query() == Some(view) {
                        this.sync_json_panel(window, cx);
                    }
                }
            }),
        ];
        self.tabs.push(OpenTab { view: TabView::Query(view), _subscriptions: subscriptions });
    }

    fn open_table(
        &mut self,
        connection: ConnectionConfig,
        schema: String,
        table: String,
        filter: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let existing = self.tabs.iter().position(|tab| match &tab.view {
            TabView::Table(view) => {
                let view = view.read(cx);
                view.connection.id == connection.id && view.schema == schema && view.table == table
            }
            TabView::Query(_) => false,
        });
        if let Some(ix) = existing {
            if let (Some(filter), TabView::Table(view)) = (filter, self.tabs[ix].view.clone()) {
                view.update(cx, |view, cx| view.show_filter(filter, window, cx));
            }
            self.activate(ix, window, cx);
            return;
        }
        let tab = EditorTab {
            id: new_tab_id(),
            connection_id: connection.id.clone(),
            title: table.clone(),
            color: connection.color.clone(),
            table_view: Some(TableViewRef { schema, table, filter: filter.unwrap_or_default(), ..Default::default() }),
            ..Default::default()
        };
        self.add_tab(tab, connection, window, cx);
        self.activate(self.tabs.len() - 1, window, cx);
    }

    fn table_changed(
        &mut self,
        connection_id: &str,
        schema: &str,
        table: &str,
        change: &TableChange,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let showing: Vec<(usize, Entity<TableTab>)> = self
            .tabs
            .iter()
            .enumerate()
            .filter_map(|(ix, tab)| match &tab.view {
                TabView::Table(view) => {
                    let shown = view.read(cx);
                    (shown.connection.id == connection_id && shown.schema == schema && shown.table == table)
                        .then(|| (ix, view.clone()))
                }
                TabView::Query(_) => None,
            })
            .collect();
        for (ix, view) in showing.into_iter().rev() {
            match change {
                TableChange::Dropped => self.close_tab(ix, window, cx),
                TableChange::Renamed(name) => {
                    view.update(cx, |view, cx| view.table_changed(Some(name.clone()), window, cx))
                }
                TableChange::Altered => view.update(cx, |view, cx| view.table_changed(None, window, cx)),
            }
        }
        cx.notify();
    }

    fn next_query_number(&self, connection_id: &str, cx: &App) -> usize {
        self.query_tabs()
            .iter()
            .filter(|tab| {
                let tab = tab.read(cx);
                tab.connection.id == connection_id && tab.saved_query_id().is_empty()
            })
            .count()
            + 1
    }

    fn open_query_tab(&mut self, connection: ConnectionConfig, window: &mut Window, cx: &mut Context<Self>) {
        let num = self.next_query_number(&connection.id, cx).to_string();
        let title = t_with(cx, "app.queryTabWithConn", &[("num", &num), ("conn", &connection.name)]);
        self.open_query_tab_with(connection, String::new(), title, window, cx);
    }

    // History entries reuse the connection's plain query tab if it has one.
    fn open_sql(&mut self, connection: ConnectionConfig, sql: String, window: &mut Window, cx: &mut Context<Self>) {
        let prefix = t(cx, "app.queryTabPrefix");
        let existing = self.tabs.iter().position(|tab| {
            tab.view.query().is_some_and(|tab| {
                let tab = tab.read(cx);
                tab.connection.id == connection.id
                    && tab.saved_query_id().is_empty()
                    && tab.title.starts_with(prefix.as_ref())
            })
        });
        if let Some(ix) = existing
            && let Some(view) = self.tabs[ix].view.query().cloned()
        {
            view.update(cx, |tab, cx| tab.set_sql(sql, window, cx));
            self.activate(ix, window, cx);
            return;
        }
        let num = self.next_query_number(&connection.id, cx).to_string();
        let title = t_with(cx, "app.queryTab", &[("num", &num)]);
        self.open_query_tab_with(connection, sql, title, window, cx);
    }

    // Prefers the linked tab, then an unlinked one of the same name opened before the query was linked.
    fn open_saved(&mut self, saved: SavedQuery, window: &mut Window, cx: &mut Context<Self>) {
        let fallback = self.active_tab().map(|tab| tab.connection(cx).id.clone());
        let sidebar = self.sidebar.read(cx);
        let connection = [Some(saved.connection_id.clone()), sidebar.selected().map(|c| c.id.clone()), fallback]
            .into_iter()
            .flatten()
            .find_map(|id| sidebar.connection(&id).cloned());
        let Some(connection) = connection else { return };
        let query = |ix: usize| self.tabs[ix].view.query().map(|tab| tab.read(cx));
        let found = (0..self.tabs.len())
            .find(|&ix| query(ix).is_some_and(|tab| tab.saved_query_id() == saved.id))
            .or_else(|| {
                (0..self.tabs.len()).find(|&ix| {
                    query(ix).is_some_and(|tab| {
                        tab.saved_query_id().is_empty()
                            && *tab.title == *saved.name
                            && (saved.connection_id.is_empty() || tab.connection.id == saved.connection_id)
                    })
                })
            });
        match found.filter(|&ix| self.tabs[ix].view.connection(cx).id == connection.id) {
            Some(ix) => {
                if let Some(view) = self.tabs[ix].view.query().cloned() {
                    view.update(cx, |tab, cx| tab.load_saved(&saved, window, cx));
                }
                self.activate(ix, window, cx);
            }
            None => {
                let tab = EditorTab {
                    id: new_tab_id(),
                    connection_id: connection.id.clone(),
                    title: saved.name.clone(),
                    sql: saved.sql.clone(),
                    color: connection.color.clone(),
                    saved_query_id: saved.id.clone(),
                    saved_sql_baseline: saved.sql,
                    table_view: None,
                };
                self.add_tab(tab, connection, window, cx);
                self.activate(self.tabs.len() - 1, window, cx);
            }
        }
    }

    fn open_query_tab_with(
        &mut self,
        connection: ConnectionConfig,
        sql: String,
        title: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tab = EditorTab {
            id: new_tab_id(),
            connection_id: connection.id.clone(),
            title: title.to_string(),
            sql,
            color: connection.color.clone(),
            ..Default::default()
        };
        self.add_tab(tab, connection, window, cx);
        self.activate(self.tabs.len() - 1, window, cx);
    }

    fn insert_into_editor(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(tab) = self.active_query().cloned() {
            tab.update(cx, |tab, cx| tab.insert(text, window, cx));
        }
    }

    fn focus_or_open_tab(&mut self, connection: ConnectionConfig, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_tab().is_some_and(|tab| tab.connection(cx).id == connection.id) {
            return;
        }
        let plain = |tab: &OpenTab| {
            tab.view.query().is_some_and(|tab| {
                let tab = tab.read(cx);
                tab.connection.id == connection.id && tab.saved_query_id().is_empty()
            })
        };
        match self.tabs.iter().position(plain) {
            Some(ix) => self.activate(ix, window, cx),
            None => self.open_query_tab(connection, window, cx),
        }
    }

    pub(crate) fn open_quick_search(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<QuickSearchDialog>> {
        if window.has_active_dialog(cx) {
            return None;
        }
        let tabs = self.tabs.iter().map(|tab| tab.view.quick_entry(cx)).collect();
        let connections = self.sidebar.read(cx).connections().to_vec();
        let this = cx.entity().downgrade();
        Some(quick_search::open(tabs, connections, window, cx, move |target, window, cx| {
            let _ = this.update(cx, |this, cx| this.open_quick_target(target, window, cx));
        }))
    }

    // Unlike the switcher, a connection always gets a new query tab here.
    fn open_quick_target(&mut self, target: Target, window: &mut Window, cx: &mut Context<Self>) {
        match target {
            Target::Tab(id) => {
                if let Some(ix) = self.tabs.iter().position(|tab| tab.view.id(cx) == id) {
                    self.activate(ix, window, cx);
                }
            }
            Target::Table { connection_id, schema, table } => {
                if let Some(connection) = self.sidebar.read(cx).connection(&connection_id).cloned() {
                    self.open_table(connection, schema, table, None, window, cx);
                }
            }
            Target::Saved(saved) => self.open_saved(*saved, window, cx),
            Target::Connection(connection) => self.open_query_tab(*connection, window, cx),
        }
    }

    fn activate(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(view) = self.tabs.get(ix).map(|tab| tab.view.clone()) else { return };
        self.active = ix;
        self.tab_scroll.scroll_to_item(ix);
        let connection_id = view.connection(cx).id.clone();
        self.sidebar.update(cx, |sidebar, cx| sidebar.set_selected(&connection_id, cx));
        view.focus(window, cx);
        self.sync_json_panel(window, cx);
        self.save_session(cx);
        cx.notify();
    }

    fn sync_json_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.json_open {
            return;
        }
        let row = self.active_tab().and_then(|tab| tab.focused_row(cx));
        self.json_panel.update(cx, |panel, cx| panel.show(row, window, cx));
    }

    fn toggle_json_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.json_open = !self.json_open;
        set_setting_bool(cx, JSON_OPEN_KEY, self.json_open);
        set_menus(cx);
        self.sync_json_panel(window, cx);
        cx.notify();
    }

    fn request_close(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(view) =
            self.tabs.get(ix).and_then(|tab| tab.view.query().cloned()).filter(|view| view.read(cx).is_dirty(cx))
        else {
            self.close_tab(ix, window, cx);
            return;
        };
        let (name, id) = (view.read(cx).title.clone(), view.read(cx).id.clone());
        let (on_save, on_discard) = (cx.entity().downgrade(), cx.entity().downgrade());
        let discard_id = id.clone();
        dialogs::unsaved(
            name,
            window,
            cx,
            move |window, cx| {
                let saved = view.update(cx, |tab, cx| {
                    let sql = tab.sql(cx).to_string();
                    tab.persist(sql, window, cx)
                });
                if saved {
                    let _ = on_save.update(cx, |this, cx| this.close_tab_by_id(&id, window, cx));
                }
            },
            move |window, cx| {
                let _ = on_discard.update(cx, |this, cx| this.close_tab_by_id(&discard_id, window, cx));
            },
        );
    }

    fn close_tab_by_id(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.tabs.iter().position(|tab| tab.view.id(cx) == id) {
            self.close_tab(ix, window, cx);
        }
    }

    // Closing the active tab activates the last one. cleanup_tab rolls back its transaction.
    fn close_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        let closing = self.tabs.remove(ix).view.stored(cx);
        let id = closing.id.clone();
        self.closed.push(closing);
        if self.closed.len() > CLOSED_TABS_LIMIT {
            self.closed.remove(0);
        }
        let bar = state::bar(cx);
        state::spawn(cx, async move { bar.cleanup_tab(&id).await }).detach();
        if self.tabs.is_empty() {
            self.active = 0;
            self.sync_json_panel(window, cx);
            window.focus(&self.focus, cx);
            self.save_session(cx);
            cx.notify();
            return;
        }
        let next = if ix == self.active {
            self.tabs.len() - 1
        } else if ix < self.active {
            self.active - 1
        } else {
            self.active
        };
        self.activate(next, window, cx);
    }

    fn reopen_closed_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(closed) = self.closed.pop() else { return };
        let Some(connection) = self.sidebar.read(cx).connection(&closed.connection_id).cloned() else { return };
        self.add_tab(closed, connection, window, cx);
        self.activate(self.tabs.len() - 1, window, cx);
    }

    fn cycle_tab(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.tabs.len();
        if count > 1 {
            let next = if forward { (self.active + 1) % count } else { (self.active + count - 1) % count };
            self.activate(next, window, cx);
        }
    }

    fn set_language(&mut self, lang: &str, window: &mut Window, cx: &mut Context<Self>) {
        cx.set_global(I18n::new(lang));
        set_setting(cx, "barsql-language", lang);
        set_menus(cx);
        window.refresh();
    }

    fn set_mode(mode: ThemeMode, window: &mut Window, cx: &mut Context<Self>) {
        if cx.theme().mode != mode {
            theme::toggle(window, cx);
        }
    }

    fn empty_state(&self, cx: &App) -> impl IntoElement {
        let theme = cx.theme();
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_2()
            .child(div().text_size(TEXT_LG).font_semibold().child(t(cx, "app.emptyTitle")))
            .child(div().text_color(theme.muted_foreground).child(t(cx, "app.emptyDescription")))
    }

    fn tab_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let tabs: Vec<Stateful<Div>> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(ix, open)| {
                let connection = open.view.connection(cx);
                let tab_color = match &open.view {
                    TabView::Query(tab) => tab.read(cx).color(),
                    TabView::Table(_) => &connection.color,
                };
                let color = parse_color(tab_color).unwrap_or(theme.muted_foreground);
                let active = ix == self.active;
                let read_only = connection.read_only;
                let dirty = open.view.is_dirty(cx);
                let close_hovered = self.hovered_close == Some(ix);
                h_flex()
                    .id(("editor-tab", ix))
                    .debug_selector(move || format!("editor-tab-{ix}"))
                    .relative()
                    .flex_none()
                    .h_full()
                    .gap(rems(0.462))
                    .px(rems(0.923))
                    .border_r_1()
                    .border_color(theme.border)
                    .whitespace_nowrap()
                    .text_size(TEXT_SM)
                    .cursor_pointer()
                    .map(|el| match active {
                        true => el.bg(theme.background).text_color(theme.foreground),
                        false => el.text_color(theme.muted_foreground),
                    })
                    .when(active, |el| el.child(div().absolute().top_0().left_0().right_0().h(rems(0.154)).bg(color)))
                    .child(
                        div()
                            .flex_none()
                            .size(rems(1.077))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(Icon::new(open.view.icon(cx)).size(ICON_2XS).text_color(color)),
                    )
                    .when(read_only, |el| {
                        let lock = if active { theme.foreground.opacity(0.7) } else { theme.muted_foreground };
                        el.child(Icon::new(Lucide::Lock).size(ICON_2XS).text_color(lock))
                    })
                    .child(
                        div()
                            .max_w(rems(15.385))
                            .truncate()
                            .when(dirty, |el| el.text_color(theme.warning))
                            .child(open.view.title(cx)),
                    )
                    .child(
                        div()
                            .id(("close-tab", ix))
                            .debug_selector(move || format!("close-tab-{ix}"))
                            .flex_none()
                            .ml(rems(0.308))
                            .px(rems(0.154))
                            .opacity(if close_hovered { 1. } else { 0.5 })
                            .child(
                                Icon::new(IconName::Close)
                                    .size(ICON_2XS)
                                    .when(close_hovered, |icon| icon.text_color(theme.danger)),
                            )
                            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                                let hovered = hovered.then_some(ix);
                                if this.hovered_close != hovered
                                    && (hovered.is_some() || this.hovered_close == Some(ix))
                                {
                                    this.hovered_close = hovered;
                                    cx.notify();
                                }
                            }))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.hovered_close = None;
                                this.request_close(ix, window, cx);
                            })),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| this.activate(ix, window, cx)))
                    .on_mouse_down(
                        MouseButton::Middle,
                        cx.listener(move |this, _, window, cx| this.request_close(ix, window, cx)),
                    )
                    .on_drag(TabDrag { ix, title: open.view.title(cx) }, |drag, _, _, cx| cx.new(|_| drag.clone()))
                    .drag_over::<TabDrag>(|style, _, window, cx| {
                        style.shadow(vec![BoxShadow {
                            color: cx.theme().primary,
                            offset: point(window.rem_size() * 0.154, px(0.)),
                            blur_radius: px(0.),
                            spread_radius: px(0.),
                            inset: true,
                        }])
                    })
                    .on_drop(
                        cx.listener(move |this, drag: &TabDrag, window, cx| this.move_tab(drag.ix, ix, window, cx)),
                    )
            })
            .collect();
        let add = div()
            .id("new-tab")
            .debug_selector(|| "new-tab".into())
            .flex_none()
            .mx(rems(0.615))
            .p(rems(0.308))
            .rounded(RADIUS)
            .border_1()
            .border_color(theme.border)
            .bg(theme.secondary)
            .text_color(theme.muted_foreground)
            .cursor_pointer()
            .hover(|style| style.bg(theme.secondary_hover).text_color(theme.foreground))
            .child(Icon::new(IconName::Plus).size(ICON_XS))
            .on_click(cx.listener(|this, _, window, cx| this.new_tab(window, cx)));
        div()
            .relative()
            .flex_none()
            .h(rems(2.769))
            .bg(theme.tab_bar)
            .border_b_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .id("editor-tabs")
                    .debug_selector(|| "editor-tabs".into())
                    .size_full()
                    .overflow_x_scroll()
                    .track_scroll(&self.tab_scroll)
                    .children(tabs)
                    .child(add),
            )
            .child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .h(Scrollbar::width())
                    .child(Scrollbar::horizontal(&self.tab_scroll).viewport_from_layout()),
            )
    }

    fn move_tab(&mut self, from: usize, to: usize, window: &mut Window, cx: &mut Context<Self>) {
        if from == to || from >= self.tabs.len() || to >= self.tabs.len() {
            return;
        }
        let active = self.active_tab().map(|tab| tab.id(cx));
        let moved = self.tabs.remove(from);
        self.tabs.insert(to, moved);
        let ix = active.and_then(|id| self.tabs.iter().position(|tab| tab.view.id(cx) == id)).unwrap_or(0);
        self.activate(ix, window, cx);
    }

    fn new_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(connection) = self.sidebar.read(cx).selected().cloned() {
            self.open_query_tab(connection, window, cx);
        }
    }

    fn status(&self, cx: &App) -> StatusBarState {
        let Some(tab) = self.active_tab() else {
            return StatusBarState {
                connection: None,
                read_only: false,
                status: t(cx, "app.statusReady"),
                error: false,
            };
        };
        let connection = tab.connection(cx);
        let connected = state::bar(cx).is_connected(&connection.id);
        let (status, error) = match tab {
            TabView::Query(tab) => {
                let status = tab.read(cx).status(cx);
                (status.text, status.error)
            }
            TabView::Table(tab) => tab.read(cx).status(cx),
        };
        StatusBarState {
            connection: Some((format!("{} ({})", connection.name, connection.driver).into(), connected)),
            read_only: connection.read_only,
            status,
            error,
        }
    }
}

#[cfg(test)]
impl Workspace {
    pub(crate) fn tab_count(&self) -> usize {
        self.tabs.len()
    }

    pub(crate) fn active_index(&self) -> usize {
        self.active
    }

    pub(crate) fn titles(&self, cx: &App) -> Vec<String> {
        self.tabs.iter().map(|tab| tab.view.title(cx).to_string()).collect()
    }
}

#[cfg(any(test, feature = "snapshot"))]
impl Workspace {
    pub(crate) fn active_title(&self, cx: &App) -> String {
        self.active_tab().map(|tab| tab.title(cx).to_string()).unwrap_or_default()
    }

    pub(crate) fn active_sql(&self, cx: &App) -> Option<String> {
        self.active_query().map(|tab| tab.read(cx).sql(cx).to_string())
    }

    pub(crate) fn query_tab(&self) -> Option<Entity<QueryTab>> {
        self.active_query().cloned()
    }

    pub(crate) fn table_tab(&self) -> Option<Entity<TableTab>> {
        match self.active_tab() {
            Some(TabView::Table(tab)) => Some(tab.clone()),
            _ => None,
        }
    }

    pub(crate) fn sidebar(&self) -> Entity<Sidebar> {
        self.sidebar.clone()
    }

    pub(crate) fn json_panel(&self) -> Option<Entity<JsonPanel>> {
        self.json_open.then(|| self.json_panel.clone())
    }
}

// For the README screenshots.
#[cfg(feature = "snapshot")]
impl Workspace {
    pub(crate) fn activate_title(&mut self, title: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let found = self.tabs.iter().position(|tab| *tab.view.title(cx) == *title);
        if let Some(ix) = found {
            self.activate(ix, window, cx);
        }
        found.is_some()
    }

    pub(crate) fn open_query(
        &mut self,
        connection: ConnectionConfig,
        title: &str,
        sql: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_query_tab_with(connection, sql.to_string(), title.to_string().into(), window, cx);
    }

    pub(crate) fn close_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_tab(self.active, window, cx);
    }

    pub(crate) fn set_json_open(&mut self, open: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.json_open != open {
            self.toggle_json_panel(window, cx);
        }
    }

    pub(crate) fn set_panel_widths(
        &mut self,
        sidebar: Pixels,
        json: Pixels,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (sidebar_open, json_open) = (self.sidebar_open, self.json_open);
        self.panels.update(cx, |panels, cx| {
            if sidebar_open {
                panels.resize_panel(0, sidebar, window, cx);
            }
            if json_open {
                let last = panels.sizes().len().saturating_sub(1);
                panels.resize_panel(last, json, window, cx);
            }
        });
    }

    pub(crate) fn active_grid(&self, cx: &App) -> Option<Entity<grid::Grid>> {
        match self.active_tab()? {
            TabView::Query(tab) => tab.read(cx).results().read(cx).active_grid(),
            TabView::Table(tab) => Some(tab.read(cx).grid()),
        }
    }
}

fn snapshot_update_state(panel: &str) -> update_dialog::UpdateState {
    use barsql_app::update::{Asset, Release, Stage, Staged};
    use update_dialog::{Transfer, UpdateState};
    let release = Release {
        version: "1.1.0".into(),
        name: "BarSQL v1.1.0".into(),
        notes: "## What's new\n\n- Faster grids and lower memory use\n- Suggestions in the table filter\n\n## Fixes\n\n- Cancel stops the server-side query".into(),
        html_url: "https://github.com/Bare7a/BarSQL/releases".into(),
        asset: Asset { name: "BarSQL-darwin-arm64.zip".into(), size: 14_680_064, url: String::new() },
        digest: None,
    };
    match panel {
        "update-current" => UpdateState::UpToDate,
        "update-downloading" => {
            UpdateState::Downloading(release, Some(Transfer { written: 6_291_456, total: 14_680_064, rate: 2_202_009 }))
        }
        "update-ready" => {
            let staged = Staged { path: "BarSQL.app".into(), dir: std::env::temp_dir() };
            UpdateState::Ready(release, staged)
        }
        "update-failed" => UpdateState::Failed {
            stage: Stage::Verify,
            message: "the release lists no SHA-256 for BarSQL-darwin-arm64.zip".into(),
            release: Some(release),
        },
        _ => UpdateState::Available(release),
    }
}

fn panel_width(cx: &App, key: &str, (default, min, max): (f32, f32, f32)) -> Pixels {
    window_state::stored_width(cx, key, default, min, max)
}

// Accepts `#rrggbb` only.
fn parse_color(color: &str) -> Option<Hsla> {
    let hex = color.strip_prefix('#')?;
    (hex.len() == 6).then(|| u32::from_str_radix(hex, 16).ok()).flatten().map(|rgb_value| rgb(rgb_value).into())
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let title = SharedString::from("BarSQL");
        let main = match self.active_tab() {
            Some(tab) => v_flex()
                .size_full()
                .child(self.tab_bar(cx))
                .child(div().flex_1().min_h_0().child(tab.view()))
                .into_any_element(),
            None => self.empty_state(cx).into_any_element(),
        };
        v_flex()
            .size_full()
            .track_focus(&self.focus)
            .bg(theme.background)
            .text_color(theme.foreground)
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| {
                this.sidebar_open = !this.sidebar_open;
                set_setting_bool(cx, SIDEBAR_OPEN_KEY, this.sidebar_open);
                set_menus(cx);
                cx.notify();
            }))
            .on_action(cx.listener(|_, _: &MinimizeWindow, window, _| window.minimize_window()))
            .on_action(cx.listener(|_, _: &ZoomWindow, window, _| window.zoom_window()))
            .on_action(cx.listener(|this, _: &ToggleJsonPanel, window, cx| this.toggle_json_panel(window, cx)))
            .on_action(cx.listener(|this, _: &NewTab, window, cx| this.new_tab(window, cx)))
            .on_action(cx.listener(|_, _: &About, window, cx| {
                if !window.has_active_dialog(cx) {
                    about_dialog::open(window, cx);
                }
            }))
            .on_action(cx.listener(|_, _: &KeyboardShortcuts, window, cx| {
                if !window.has_active_dialog(cx) {
                    shortcuts_dialog::open(window, cx);
                }
            }))
            .on_action(cx.listener(|_, _: &KeyboardTips, window, cx| {
                if !window.has_active_dialog(cx) {
                    tips_dialog::open(window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &QuickSearch, window, cx| {
                this.open_quick_search(window, cx);
            }))
            .on_action(cx.listener(|this, _: &CloseTab, window, cx| this.request_close(this.active, window, cx)))
            .on_action(cx.listener(|this, _: &ReopenClosedTab, window, cx| this.reopen_closed_tab(window, cx)))
            .on_action(cx.listener(|this, _: &NextTab, window, cx| this.cycle_tab(true, window, cx)))
            .on_action(cx.listener(|this, _: &PrevTab, window, cx| this.cycle_tab(false, window, cx)))
            .on_action(cx.listener(|_, _: &ThemeDark, window, cx| Self::set_mode(ThemeMode::Dark, window, cx)))
            .on_action(cx.listener(|_, _: &ThemeLight, window, cx| Self::set_mode(ThemeMode::Light, window, cx)))
            .on_action(cx.listener(|this, _: &LanguageEn, window, cx| this.set_language("en", window, cx)))
            .on_action(cx.listener(|this, _: &LanguageDe, window, cx| this.set_language("de", window, cx)))
            .on_action(cx.listener(|this, _: &LanguageBg, window, cx| this.set_language("bg", window, cx)))
            .on_action(cx.listener(|_, _: &ZoomIn, window, cx| theme::set_zoom(theme::zoom(cx) + 1., window, cx)))
            .on_action(cx.listener(|_, _: &ZoomOut, window, cx| theme::set_zoom(theme::zoom(cx) - 1., window, cx)))
            .on_action(cx.listener(|_, _: &ResetZoom, window, cx| theme::set_zoom(theme::DEFAULT_ZOOM, window, cx)))
            .on_action(cx.listener(|_, _: &IncreaseEditorFontSize, window, cx| {
                theme::set_editor_font_size(theme::editor_font_size(cx) + 1., window, cx)
            }))
            .on_action(cx.listener(|_, _: &DecreaseEditorFontSize, window, cx| {
                theme::set_editor_font_size(theme::editor_font_size(cx) - 1., window, cx)
            }))
            .on_action(cx.listener(|_, _: &ResetEditorFontSize, window, cx| {
                theme::set_editor_font_size(theme::DEFAULT_EDITOR_FONT, window, cx)
            }))
            .on_action(cx.listener(|_, _: &ToggleFullscreen, window, _| window.toggle_fullscreen()))
            .child(title_bar::render(self.menu_bar.as_ref(), title, cx))
            .child(
                div().flex_1().min_h_0().child(
                    h_resizable("workspace")
                        .with_state(&self.panels)
                        .on_resize({
                            let this = cx.entity().downgrade();
                            move |state, _, cx| {
                                let sizes = state.read(cx).sizes().clone();
                                let _ = this.update(cx, |this, cx| this.panels_resized(&sizes, cx));
                            }
                        })
                        .when(self.sidebar_open, |panels| {
                            panels.child(
                                resizable_panel()
                                    .flex_none()
                                    .size(self.sidebar_width)
                                    .size_range(px(SIDEBAR_WIDTHS.1)..px(SIDEBAR_WIDTHS.2))
                                    .child(self.sidebar.clone()),
                            )
                        })
                        .child(resizable_panel().child(main))
                        .when(self.json_open, |panels| {
                            panels.child(
                                resizable_panel()
                                    .flex_none()
                                    .size(self.json_width)
                                    .size_range(px(JSON_WIDTHS.1)..px(JSON_WIDTHS.2))
                                    .child(self.json_panel.clone()),
                            )
                        }),
                ),
            )
            .child(status_bar::render(self.status(cx), cx))
            .children(Root::render_sheet_layer(window, cx))
            .children(Root::render_dialog_layer(window, cx))
            .children(Root::render_notification_layer(window, cx))
            .children(toast::render(window, cx))
            .child(self.drop_overlay(cx))
            .child(self.context_menu.clone())
    }
}

#[cfg(test)]
mod tests;
