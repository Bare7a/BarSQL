use std::time::Duration;

use std::cell::Cell;
use std::collections::HashSet;
use std::rc::Rc;

use barsql_app::{
    AppEvent, DiagramRef, EditorSession, EditorTab, StructureRef, TableViewRef, Unsaved, is_sqlite_file,
    sqlite_file_payload,
};
use barsql_core::capabilities::DefaultSchema;
use barsql_core::config::path_parts;
use barsql_core::{ConnectionConfig, SavedQuery};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::{AppMenuBar, ContextMenuExt, PopupMenu, PopupMenuItem};
use gpui_kit::component::resizable::{ResizableState, h_resizable, resizable_panel};
use gpui_kit::component::scroll::Scrollbar;
use gpui_kit::component::{ActiveTheme, Icon, IconName, Root, StyledExt, TitleBar, WindowExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::actions::*;
use crate::assets::LOGO;
use crate::dialogs::{self, Confirm};
use crate::er_diagram::{self, ErDiagram};
use crate::grid::RowRef;
use crate::i18n::{I18n, t, t_count, t_with};
use crate::json_panel::JsonPanel;
use crate::query_tab::{QueryTab, QueryTabEvent, TxnState, new_tab_id};
use crate::quick_search::{self, QuickSearchDialog, TabEntry, Target};
use crate::schema_tree::{ActiveTable, TableChange};
use crate::scrollbars;
use crate::sidebar::{Sidebar, SidebarEvent};
use crate::state::{self, set_setting, set_setting_bool, set_setting_json, setting_bool};
use crate::status_bar::{self, StatusAction, StatusActions, StatusBarState, StatusConnection};
use crate::structure_tab::{StructureTab, StructureTabEvent};
use crate::table_tab::{TableTab, TableTabEvent};
use crate::theme::ThemeChoice;
use crate::toast;
use crate::tokens::{ICON_2XS, ICON_XL, ICON_XS, RADIUS, RADIUS_LG, TEXT_LG, TEXT_MD, TEXT_SM};
use crate::window_state::{self, WindowState};
use crate::{
    LaunchOptions, about_dialog, command_palette, connection_dialog, context_menu, file_dialogs, form, grid, schema,
    screenshots, shortcuts, shortcuts_dialog, theme, tips_dialog, title_bar, update_dialog,
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
    Diagram(Entity<ErDiagram>),
    Structure(Entity<StructureTab>),
}

impl TabView {
    fn query(&self) -> Option<&Entity<QueryTab>> {
        match self {
            Self::Query(tab) => Some(tab),
            Self::Table(_) | Self::Diagram(_) | Self::Structure(_) => None,
        }
    }

    fn id(&self, cx: &App) -> String {
        match self {
            Self::Query(tab) => tab.read(cx).id.clone(),
            Self::Table(tab) => tab.read(cx).id.clone(),
            Self::Diagram(tab) => tab.read(cx).id.clone(),
            Self::Structure(tab) => tab.read(cx).id.clone(),
        }
    }

    fn entity_id(&self) -> EntityId {
        match self {
            Self::Query(tab) => tab.entity_id(),
            Self::Table(tab) => tab.entity_id(),
            Self::Diagram(tab) => tab.entity_id(),
            Self::Structure(tab) => tab.entity_id(),
        }
    }

    fn title(&self, cx: &App) -> SharedString {
        match self {
            Self::Query(tab) => tab.read(cx).title.clone(),
            Self::Table(tab) => tab.read(cx).title.clone(),
            Self::Diagram(tab) => tab.read(cx).title(cx),
            Self::Structure(tab) => tab.read(cx).title(cx),
        }
    }

    fn connection<'a>(&self, cx: &'a App) -> &'a ConnectionConfig {
        match self {
            Self::Query(tab) => &tab.read(cx).connection,
            Self::Table(tab) => &tab.read(cx).connection,
            Self::Diagram(tab) => &tab.read(cx).connection,
            Self::Structure(tab) => &tab.read(cx).connection,
        }
    }

    fn set_connection(&self, connection: ConnectionConfig, cx: &mut App) {
        match self {
            Self::Query(tab) => tab.update(cx, |tab, cx| tab.set_connection(connection, cx)),
            Self::Table(tab) => tab.update(cx, |tab, cx| tab.set_connection(connection, cx)),
            Self::Diagram(tab) => tab.update(cx, |tab, cx| tab.set_connection(connection, cx)),
            Self::Structure(tab) => tab.update(cx, |tab, cx| tab.set_connection(connection, cx)),
        }
    }

    fn stored(&self, cx: &App) -> EditorTab {
        match self {
            Self::Query(tab) => tab.read(cx).stored(cx),
            Self::Table(tab) => tab.read(cx).stored(cx),
            Self::Diagram(tab) => tab.read(cx).stored(cx),
            Self::Structure(tab) => tab.read(cx).stored(cx),
        }
    }

    // A new table's Structure tab has nothing to come back to, so the session and Reopen leave it out.
    fn persisted(&self, cx: &App) -> bool {
        match self {
            Self::Structure(tab) => tab.read(cx).persisted(),
            Self::Query(_) | Self::Table(_) | Self::Diagram(_) => true,
        }
    }

    fn is_dirty(&self, cx: &App) -> bool {
        self.query().is_some_and(|tab| tab.read(cx).is_dirty(cx))
    }

    // Structure changes and a query's unsaved result edits live only in the tab, so closing it loses them.
    fn holds_draft(&self, cx: &App) -> bool {
        match self {
            Self::Structure(tab) => tab.read(cx).has_changes(),
            Self::Query(tab) => tab.read(cx).has_result_edits(cx),
            _ => false,
        }
    }

    fn focus(&self, window: &mut Window, cx: &mut App) {
        match self {
            Self::Query(tab) => tab.update(cx, |tab, cx| tab.focus_editor(window, cx)),
            Self::Table(tab) => tab.update(cx, |tab, cx| tab.focus(window, cx)),
            Self::Diagram(tab) => tab.update(cx, |tab, cx| tab.focus(window, cx)),
            Self::Structure(tab) => tab.update(cx, |tab, cx| tab.focus(window, cx)),
        }
    }

    fn focused_row(&self, cx: &App) -> Option<RowRef> {
        match self {
            Self::Query(tab) => tab.read(cx).focused_row(cx),
            Self::Table(tab) => tab.read(cx).focused_row(cx),
            Self::Diagram(_) | Self::Structure(_) => None,
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
            Self::Diagram(_) => Lucide::Workflow,
            Self::Structure(_) => Lucide::Columns3,
        }
    }

    fn quick_entry(&self, cx: &App) -> TabEntry {
        let (table, saved_query_id) = match self {
            Self::Query(tab) => (None, tab.read(cx).saved_query_id().to_string()),
            Self::Table(tab) => {
                let tab = tab.read(cx);
                (Some((tab.schema.clone(), tab.table.clone())), String::new())
            }
            // A Structure tab isn't its table's data, which quick search still offers.
            Self::Diagram(_) | Self::Structure(_) => (None, String::new()),
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
            Self::Diagram(tab) => tab.clone().into(),
            Self::Structure(tab) => tab.clone().into(),
        }
    }
}

type TabAction = Box<dyn Fn(&mut Workspace, &mut Window, &mut Context<Workspace>)>;

struct OpenTab {
    view: TabView,
    pinned: bool,
    _subscriptions: Vec<Subscription>,
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
    // Whether the active tab's grid, not its editor, had the focus last. The status bar keeps speaking for it while
    // the focus is elsewhere, like in the bar's own popup.
    grid_context: Cell<Option<(EntityId, bool)>>,
    tabs: Vec<OpenTab>,
    active: usize,
    tab_scroll: ScrollHandle,
    hovered_close: Option<usize>,
    closed: Vec<EditorTab>,
    // The tab and SQL a history entry last opened, so the next entry can take its place.
    history_shown: Option<(String, String)>,
    // Tabs whose connection is gone. Saving puts them back at their old index.
    kept: Vec<(usize, EditorTab)>,
    saved: Option<EditorSession>,
    save_task: Task<()>,
    // Focused while the quit confirmation shows.
    quit_dialog: Option<WeakFocusHandle>,
    // Tabs holding changes that closing them would lose.
    drafts: HashSet<EntityId>,
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
                // The first table of `main`, or of the connection's default schema. `structure=<table>` picks one.
                Some(structure) if structure.starts_with("structure") => {
                    let (view, table) = (view.clone(), structure.strip_prefix("structure=").map(str::to_string));
                    after = Some(Box::new(move |window, cx| {
                        view.update(cx, |workspace, cx| workspace.snapshot_structure(table.clone(), window, cx))
                    }));
                }
                Some("er") => {
                    let view = view.clone();
                    after = Some(Box::new(move |_, cx| {
                        let tree = view.read(cx).sidebar.read(cx).schema_tree();
                        tree.update(cx, |tree, cx| tree.open_er_diagram("main".into(), cx));
                    }));
                }
                // Run with BARSQL_SNAPSHOT_RUN, so there's a result to chart.
                Some("chart") => {
                    let view = view.clone();
                    after = Some(Box::new(move |window, cx| {
                        if let Some(tab) = view.read(cx).active_query().cloned() {
                            tab.read(cx).results().update(cx, |results, cx| results.toggle_chart(window, cx));
                        }
                    }));
                }
                // From the editor, so its commands are listed too.
                Some("palette") => {
                    let view = view.clone();
                    after = Some(Box::new(move |window, cx| {
                        if let Some(tab) = view.read(cx).active_query().cloned() {
                            tab.update(cx, |tab, cx| tab.focus_editor(window, cx));
                        }
                        window.dispatch_action(Box::new(CommandPalette), cx);
                    }));
                }
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
                            Some(TabView::Diagram(_) | TabView::Structure(_)) | None => (None, None),
                        };
                        if let Some(completion) = query {
                            completion.update(cx, |completion, cx| completion.preview(&text, window, cx));
                        }
                        if let Some(completion) = table {
                            completion.update(cx, |completion, cx| completion.preview(&text, window, cx));
                        }
                    }));
                }
                Some(hover) if hover.starts_with("hover=") => {
                    let (view, name) = (view.clone(), hover["hover=".len()..].to_string());
                    after = Some(Box::new(move |_, cx| {
                        if let Some(tab) = view.read(cx).active_query().cloned() {
                            let card = tab.read(cx).hover();
                            card.update(cx, |card, cx| card.preview(&name, cx));
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

// The Quit action. Its dispatch can hold the workspace's window, so the workspace is asked once that's done. The
// window handle stays untyped so nothing holds the root view, which dialogs update.
pub fn quit(cx: &mut App) {
    cx.defer(|cx| {
        let asked = cx.windows().into_iter().any(|window| {
            let asked = window.update(cx, |_, window, cx| {
                let workspace = Workspace::in_window(window, cx)?;
                workspace.update(cx, |this, cx| this.quit(window, cx));
                Some(())
            });
            matches!(asked, Ok(Some(())))
        });
        if !asked {
            quit_now(cx);
        }
    });
}

// What an empty schema setting means for the connection's engine.
fn default_schema(connection: &ConnectionConfig) -> String {
    if !connection.schema.is_empty() {
        return connection.schema.clone();
    }
    match connection.driver.capabilities().default_schema {
        DefaultSchema::Named(name) => name.into(),
        DefaultSchema::Database => connection.database.clone(),
    }
}

// For views that quit, like the updater's Restart. `then` runs after the caller's update, or on the quit
// confirmation's OK when a tab holds an open transaction.
pub(crate) fn before_quit(window: &mut Window, cx: &mut App, then: impl Fn(&mut Window, &mut App) + 'static) {
    match Workspace::in_window(window, cx) {
        Some(workspace) => workspace.update(cx, |this, cx| this.before_quit(window, cx, then)),
        None => window.defer(cx, then),
    }
}

// The test platform ignores quitting, so scenarios count the requests.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct QuitRequests(pub usize);

#[cfg(test)]
impl Global for QuitRequests {}

// BarApp::shutdown, in on_app_quit, rolls back what's still open.
pub(crate) fn quit_now(cx: &mut App) {
    #[cfg(test)]
    {
        cx.default_global::<QuitRequests>().0 += 1;
    }
    // So macOS's applicationShouldTerminate: doesn't ask again.
    #[cfg(target_os = "macos")]
    crate::system_quit::allow_terminate();
    cx.quit();
}

impl Workspace {
    pub(crate) fn in_window(window: &Window, cx: &App) -> Option<Entity<Workspace>> {
        window.root::<Root>().flatten()?.read(cx).view().clone().downcast::<Workspace>().ok()
    }

    pub(crate) fn context_menu(&self) -> Entity<context_menu::Overlay> {
        self.context_menu.clone()
    }

    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let this = cx.weak_entity();
        window.on_window_should_close(cx, move |window, cx| {
            this.update(cx, |this, cx| this.close_window(window, cx)).unwrap_or(true)
        });
        let sidebar = cx.new(|cx| Sidebar::new(window, cx));
        let subscriptions = vec![
            cx.subscribe_in(&sidebar, window, |this, _, event: &SidebarEvent, window, cx| match event {
                SidebarEvent::Selected(connection) => this.focus_or_open_tab(connection.clone(), window, cx),
                SidebarEvent::Insert(text) => this.insert_into_editor(text, window, cx),
                SidebarEvent::OpenQuery { connection, sql, title, run } => {
                    this.open_query_tab_with(connection.clone(), sql.clone(), title.clone().into(), window, cx);
                    if *run && let Some(tab) = this.active_query().cloned() {
                        tab.update(cx, |tab, cx| tab.run_all(window, cx));
                    }
                }
                SidebarEvent::OpenSql { connection, sql } => this.open_sql(connection.clone(), sql.clone(), window, cx),
                SidebarEvent::OpenSaved(query) => this.open_saved(query.clone(), window, cx),
                SidebarEvent::Browse { connection, schema, table } => {
                    this.open_table(connection.clone(), schema.clone(), table.clone(), None, window, cx)
                }
                SidebarEvent::OpenDiagram { connection, schema } => {
                    this.open_diagram(connection.clone(), schema.clone(), window, cx)
                }
                SidebarEvent::OpenStructure { connection, schema, table } => {
                    this.open_structure(connection.clone(), schema.clone(), table.clone(), window, cx)
                }
                SidebarEvent::ConnectionsChanged(connections) => {
                    for open in &this.tabs {
                        let id = open.view.connection(cx).id.clone();
                        if let Some(connection) = connections.iter().find(|c| c.id == id).cloned() {
                            open.view.set_connection(connection, cx);
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
            .chain([
                cx.observe_global::<toast::Toasts>(|_, cx| cx.notify()),
                cx.observe_window_appearance(window, |_, window, cx| theme::follow_system(window, cx)),
            ])
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
            grid_context: Cell::new(None),
            tabs: Vec::new(),
            active: 0,
            tab_scroll: ScrollHandle::new(),
            hovered_close: None,
            closed: Vec::new(),
            history_shown: None,
            kept: Vec::new(),
            saved: None,
            save_task: Task::ready(()),
            quit_dialog: None,
            drafts: HashSet::new(),
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
        let mut tabs: Vec<EditorTab> = self
            .tabs
            .iter()
            .filter(|tab| tab.view.persisted(cx))
            .map(|tab| EditorTab { pinned: tab.pinned, ..tab.view.stored(cx) })
            .collect();
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

    // The panel group keeps its sizes by position, so when the sidebar comes or goes the panels after it would take
    // each other's. Cleared, each one lays out at its own width again.
    fn reset_panels(&mut self, cx: &mut Context<Self>) {
        self.panels.update(cx, |panels, _| panels.clear());
    }

    fn app_event(&mut self, event: AppEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            // Closing and quitting ask first, so only a forced rollback lands here.
            AppEvent::TransactionsEnded { tab_ids } => {
                let mut ended = Vec::new();
                for tab in self.query_tabs() {
                    if tab_ids.contains(&tab.read(cx).id) {
                        ended.push(tab.read(cx).title.to_string());
                        tab.update(cx, |tab, cx| tab.set_transaction_ended(cx));
                    }
                }
                if !ended.is_empty() {
                    let tabs = ended.join(", ");
                    toast::info(t_count(cx, "tabs.rolledBack", ended.len() as i64, &[("tabs", &tabs)]), cx);
                }
            }
            AppEvent::OpenSqlite { file_path, name } => self.open_sqlite(file_path, name, window, cx),
            AppEvent::Activate => {
                cx.activate(true);
                window.activate_window();
            }
            AppEvent::UpdateAvailable { .. } => {}
            AppEvent::NotSaved { what, error } => {
                let key = match what {
                    Unsaved::Settings => "errors.settingsNotSaved",
                    Unsaved::Session => "errors.sessionNotSaved",
                    Unsaved::History => "errors.historyNotSaved",
                };
                toast::error(t_with(cx, key, &[("error", &error)]), cx);
            }
        }
    }

    fn open_sqlite(&mut self, file_path: String, name: String, window: &mut Window, cx: &mut Context<Self>) {
        self.sidebar.update(cx, |sidebar, cx| sidebar.open_sqlite(file_path, name, window, cx));
    }

    fn dropped(&mut self, paths: &ExternalPaths, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = paths.paths().iter().find(|path| is_sqlite_file(&path.to_string_lossy())) else { return };
        let Some(path) = file_dialogs::path_text(path, cx) else { return };
        let (file_path, name) = sqlite_file_payload(&path);
        self.open_sqlite(file_path, name, window, cx);
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

    // Observes the tabs that can hold a draft. Only the one that changed is asked again.
    fn tab_changed<T: 'static>(&mut self, view: Entity<T>, cx: &mut Context<Self>) {
        let id = view.entity_id();
        let held = self.tabs.iter().find(|tab| tab.view.entity_id() == id).is_some_and(|tab| tab.view.holds_draft(cx));
        self.note_draft(id, held);
        cx.notify();
    }

    // The Dock's Quit asks from outside GPUI, where the tabs can't be read.
    fn note_draft(&mut self, tab: EntityId, held: bool) {
        if held {
            self.drafts.insert(tab);
        } else {
            self.drafts.remove(&tab);
        }
        #[cfg(target_os = "macos")]
        crate::system_quit::set_drafts(!self.drafts.is_empty());
    }

    fn query_tabs(&self) -> Vec<Entity<QueryTab>> {
        self.tabs.iter().filter_map(|tab| tab.view.query().cloned()).collect()
    }

    fn add_tab(&mut self, tab: EditorTab, connection: ConnectionConfig, window: &mut Window, cx: &mut Context<Self>) {
        let pinned = tab.pinned;
        if let Some(diagram) = tab.diagram {
            // A table picked in the diagram opens as a table view beside it, on the connection as it is by then.
            let (workspace, connection_id, schema) = (cx.weak_entity(), connection.id.clone(), diagram.schema.clone());
            let on_open: er_diagram::OnOpen = Rc::new(move |table, window, cx| {
                let _ = workspace.update(cx, |this, cx| {
                    let Some(connection) = this.sidebar.read(cx).connection(&connection_id).cloned() else { return };
                    this.open_table(connection, schema.clone(), table, None, window, cx);
                });
            });
            let view = cx.new(|cx| {
                let mut view = ErDiagram::new(tab.id, connection, diagram.schema, on_open, window, cx);
                view.load(cx);
                view
            });
            let subscriptions = vec![cx.observe(&view, |_, _, cx| cx.notify())];
            self.tabs.push(OpenTab { view: TabView::Diagram(view), pinned, _subscriptions: subscriptions });
            return;
        }
        if tab.structure.is_some() {
            let view = cx.new(|cx| {
                let mut view = StructureTab::new(tab, connection, window, cx);
                view.load(window, cx);
                view
            });
            let subscriptions = vec![
                cx.observe(&view, Self::tab_changed),
                cx.subscribe_in(&view, window, |this, view, event: &StructureTabEvent, window, cx| match event {
                    StructureTabEvent::Edited => this.schedule_save(cx),
                    StructureTabEvent::Applied { created } => {
                        let shown = view.read(cx);
                        let table = created.clone().unwrap_or_else(|| shown.table.clone());
                        let (id, schema) = (shown.connection.id.clone(), shown.schema.clone());
                        // The tab reloads first, so the tree's Altered doesn't find its old draft stale.
                        view.update(cx, |view, cx| view.after_apply(created.clone(), window, cx));
                        let tree = this.sidebar.read(cx).schema_tree();
                        tree.update(cx, |tree, cx| tree.structure_changed(&id, &schema, &table, created.is_some(), cx));
                        this.schedule_save(cx);
                    }
                }),
            ];
            self.tabs.push(OpenTab { view: TabView::Structure(view), pinned, _subscriptions: subscriptions });
            return;
        }
        if tab.table_view.is_some() {
            let view = cx.new(|cx| TableTab::new(tab, connection, window, cx));
            let subscriptions = vec![
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
                    TableTabEvent::OpenStructure => {
                        let shown = view.read(cx);
                        let (connection, schema, table) =
                            (shown.connection.clone(), shown.schema.clone(), shown.table.clone());
                        this.open_structure(connection, schema, Some(table), window, cx);
                    }
                }),
            ];
            self.tabs.push(OpenTab { view: TabView::Table(view), pinned, _subscriptions: subscriptions });
            return;
        }
        let view = cx.new(|cx| QueryTab::new(tab, connection, window, cx));
        let subscriptions = vec![
            cx.observe(&view, Self::tab_changed),
            cx.subscribe_in(&view, window, |this, view, event: &QueryTabEvent, window, cx| match event {
                QueryTabEvent::Edited => this.schedule_save(cx),
                QueryTabEvent::RunFinished => this.sidebar.update(cx, |sidebar, cx| sidebar.refresh_history(cx)),
                QueryTabEvent::FocusedRowChanged => {
                    if this.active_query() == Some(view) {
                        this.sync_json_panel(window, cx);
                    }
                }
                QueryTabEvent::OpenTable { schema, table } => {
                    let connection = view.read(cx).connection.clone();
                    this.open_table(connection, schema.clone(), table.clone(), None, window, cx);
                }
            }),
        ];
        self.tabs.push(OpenTab { view: TabView::Query(view), pinned, _subscriptions: subscriptions });
    }

    // One tab per connection and schema. Opening it again goes back to it.
    pub(crate) fn open_diagram(
        &mut self,
        connection: ConnectionConfig,
        schema: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let existing = self.tabs.iter().position(|tab| match &tab.view {
            TabView::Diagram(view) => {
                let view = view.read(cx);
                view.connection.id == connection.id && view.schema == schema
            }
            _ => false,
        });
        if let Some(ix) = existing {
            self.activate(ix, window, cx);
            return;
        }
        let tab = EditorTab {
            id: new_tab_id(),
            connection_id: connection.id.clone(),
            title: t_with(cx, "erDiagram.title", &[("schema", &schema)]).to_string(),
            color: connection.color.clone(),
            diagram: Some(DiagramRef { schema }),
            ..Default::default()
        };
        self.add_tab(tab, connection, window, cx);
        self.activate(self.tabs.len() - 1, window, cx);
    }

    // The table the active tab shows, for Edit table structure.
    fn structure_target(&self, cx: &App) -> Option<(ConnectionConfig, String, String)> {
        match self.active_tab()? {
            TabView::Table(view) => {
                let tab = view.read(cx);
                let view = schema::is_view(&tab.connection.id, &tab.schema, &tab.table, cx);
                (!view).then(|| (tab.connection.clone(), tab.schema.clone(), tab.table.clone()))
            }
            TabView::Structure(view) => {
                let tab = view.read(cx);
                tab.persisted().then(|| (tab.connection.clone(), tab.schema.clone(), tab.table.clone()))
            }
            TabView::Query(_) | TabView::Diagram(_) => None,
        }
    }

    // The active tab's connection, when it takes new tables.
    fn creatable_connection(&self, cx: &App) -> Option<ConnectionConfig> {
        let connection = self.active_tab()?.connection(cx);
        let editable = connection.driver.capabilities().structure.editable;
        (editable && !connection.read_only).then(|| connection.clone())
    }

    fn new_table(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(connection) = self.creatable_connection(cx) {
            let schema = default_schema(&connection);
            self.open_structure(connection, schema, None, window, cx);
        }
    }

    fn snapshot_structure(&mut self, table: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(connection) = self.active_tab().map(|tab| tab.connection(cx).clone()) else { return };
        let (schema, first) = {
            let loaded = schema::get(cx, &connection.id).map(|cached| cached.loaded_tables()).unwrap_or_default();
            let schema = match loaded.iter().any(|(name, _)| *name == "main") {
                true => "main".to_string(),
                false => default_schema(&connection),
            };
            let tables = loaded.iter().find(|(name, _)| *name == schema).map(|(_, tables)| *tables).unwrap_or_default();
            let first = tables.iter().find(|t| !schema::is_view_kind(&t.kind)).map(|t| t.name.clone());
            (schema, first)
        };
        if let Some(table) = table.or(first) {
            self.open_structure(connection, schema, Some(table), window, cx);
        }
    }

    // One tab per table, as for diagrams. A new table (no `table`) always gets a tab of its own.
    pub(crate) fn open_structure(
        &mut self,
        connection: ConnectionConfig,
        schema: String,
        table: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let existing = table.as_ref().and_then(|table| {
            self.tabs.iter().position(|tab| match &tab.view {
                TabView::Structure(view) => {
                    let view = view.read(cx);
                    view.connection.id == connection.id && view.schema == schema && view.table == *table
                }
                _ => false,
            })
        });
        if let Some(ix) = existing {
            self.activate(ix, window, cx);
            return;
        }
        let table = table.unwrap_or_default();
        let tab = EditorTab {
            id: new_tab_id(),
            connection_id: connection.id.clone(),
            title: table.clone(),
            color: connection.color.clone(),
            structure: Some(StructureRef { schema, table }),
            ..Default::default()
        };
        self.add_tab(tab, connection, window, cx);
        self.activate(self.tabs.len() - 1, window, cx);
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
            TabView::Query(_) | TabView::Diagram(_) | TabView::Structure(_) => false,
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
                TabView::Query(_) | TabView::Diagram(_) | TabView::Structure(_) => None,
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
        let structures: Vec<(usize, Entity<StructureTab>)> = self
            .tabs
            .iter()
            .enumerate()
            .filter_map(|(ix, tab)| match &tab.view {
                TabView::Structure(view) => {
                    let shown = view.read(cx);
                    (shown.connection.id == connection_id && shown.schema == schema && shown.table == table)
                        .then(|| (ix, view.clone()))
                }
                _ => None,
            })
            .collect();
        // A dropped table closes its Structure tab, unless the tab holds changes to say it lost.
        for (ix, view) in structures.into_iter().rev() {
            if matches!(change, TableChange::Dropped) && !view.read(cx).has_changes() {
                self.close_tab(ix, window, cx);
            } else {
                view.update(cx, |view, cx| view.table_changed(change, window, cx));
            }
        }
        // A rename keeps its tab, so the mark moves to the new name.
        self.sync_active_table(cx);
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

    // History entries reuse the connection's plain query tab while it's empty or still shows the last entry
    // unedited, so stepping through history doesn't pile up tabs or replace anything typed.
    fn open_sql(&mut self, connection: ConnectionConfig, sql: String, window: &mut Window, cx: &mut Context<Self>) {
        let prefix = t(cx, "app.queryTabPrefix");
        let last = self.history_shown.clone();
        let existing = self.tabs.iter().position(|tab| {
            tab.view.query().is_some_and(|tab| {
                let tab = tab.read(cx);
                let current = tab.sql(cx);
                tab.connection.id == connection.id
                    && tab.saved_query_id().is_empty()
                    && tab.title.starts_with(prefix.as_ref())
                    && (current.trim().is_empty() || last.as_ref() == Some(&(tab.id.clone(), current.to_string())))
            })
        });
        if let Some(ix) = existing
            && let Some(view) = self.tabs[ix].view.query().cloned()
        {
            view.update(cx, |tab, cx| tab.set_sql(sql.clone(), window, cx));
            self.history_shown = Some((view.read(cx).id.clone(), sql));
            self.activate(ix, window, cx);
            return;
        }
        let num = self.next_query_number(&connection.id, cx).to_string();
        let title = t_with(cx, "app.queryTab", &[("num", &num)]);
        self.open_query_tab_with(connection, sql.clone(), title, window, cx);
        self.history_shown = self.active_query().map(|tab| (tab.read(cx).id.clone(), sql));
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
                    ..Default::default()
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

    // Into the active query tab, or with a table view or nothing open, into the connection's plain query tab.
    fn insert_into_editor(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_query().is_none()
            && let Some(connection) = self.sidebar.read(cx).selected().cloned()
        {
            let plain = self.tabs.iter().position(|tab| {
                tab.view.query().is_some_and(|tab| {
                    let tab = tab.read(cx);
                    tab.connection.id == connection.id && tab.saved_query_id().is_empty()
                })
            });
            match plain {
                Some(ix) => self.activate(ix, window, cx),
                None => self.open_query_tab(connection, window, cx),
            }
        }
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

    // Lists what the focused view can run. Something in the workspace must hold focus: the palette hands it back
    // and runs the command there.
    pub(crate) fn open_command_palette(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<command_palette::CommandPalette>> {
        if window.has_active_dialog(cx) {
            return None;
        }
        let focus = window.focused(cx).unwrap_or_else(|| {
            window.focus(&self.focus, cx);
            self.focus.clone()
        });
        let commands = command_palette::commands(&focus, window, cx);
        Some(command_palette::open(commands, window, cx))
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
        self.sync_active_table(cx);
        view.focus(window, cx);
        self.sync_json_panel(window, cx);
        self.save_session(cx);
        cx.notify();
    }

    // The tree marks the table the active tab shows, and none for any other tab.
    fn sync_active_table(&self, cx: &mut Context<Self>) {
        let table = match self.active_tab() {
            Some(TabView::Table(tab)) => {
                let tab = tab.read(cx);
                Some(ActiveTable {
                    connection_id: tab.connection.id.clone(),
                    schema: tab.schema.clone(),
                    table: tab.table.clone(),
                })
            }
            _ => None,
        };
        self.sidebar.update(cx, |sidebar, cx| sidebar.set_active_table(table, cx));
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
        if let Some(id) = self.tabs.get(ix).map(|tab| tab.view.id(cx)) {
            self.close_tabs(vec![id], window, cx);
        }
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
        let open = self.tabs.remove(ix);
        let (closing, persisted) = (open.view.stored(cx), open.view.persisted(cx));
        self.note_draft(open.view.entity_id(), false);
        drop(open);
        let id = closing.id.clone();
        if persisted {
            self.closed.push(closing);
            if self.closed.len() > CLOSED_TABS_LIMIT {
                self.closed.remove(0);
            }
        }
        let bar = state::bar(cx);
        state::spawn(cx, async move { bar.cleanup_tab(&id).await }).detach();
        if self.tabs.is_empty() {
            self.active = 0;
            self.sync_active_table(cx);
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

    fn index_of(&self, id: &str, cx: &App) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.view.id(cx) == id)
    }

    fn request_close_id(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.index_of(id, cx) {
            self.request_close(ix, window, cx);
        }
    }

    // For Close, Close Others, To the Right and All. Closing rolls back a tab's open transaction, so that asks first.
    fn close_tabs(&mut self, ids: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        let keys = ["tabs.rollbackTitle", "tabs.rollbackDescription", "tabs.rollbackConfirm"];
        let Some(confirm) = self.confirm_rollback(&ids, keys, cx) else {
            return self.close_tabs_saving(ids, window, cx);
        };
        let this = cx.entity().downgrade();
        dialogs::confirm(confirm, window, cx, move |window, cx| {
            let _ = this.update(cx, |this, cx| this.close_tabs_saving(ids.clone(), window, cx));
        });
    }

    // Asks before rolling back the open transactions among `ids`, listing their tabs. None when there's none.
    fn confirm_rollback(&self, ids: &[String], [title, description, confirm]: [&str; 3], cx: &App) -> Option<Confirm> {
        let bar = state::bar(cx);
        let open: Vec<String> = ids
            .iter()
            .filter(|id| bar.transaction_status(id))
            .filter_map(|id| self.index_of(id, cx).map(|ix| self.tabs[ix].view.title(cx).to_string()))
            .collect();
        (!open.is_empty()).then(|| Confirm {
            title: t(cx, title),
            description: t_count(cx, description, open.len() as i64, &[]),
            detail: Some(open.join("\n").into()),
            confirm: t(cx, confirm),
            danger: true,
        })
    }

    fn tab_ids(&self, cx: &App) -> Vec<String> {
        self.tabs.iter().map(|tab| tab.view.id(cx)).collect()
    }

    fn quit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.before_quit(window, cx, |_, cx| quit_now(cx));
    }

    // Asking again while the confirmation shows doesn't stack a second one.
    fn before_quit(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        then: impl Fn(&mut Window, &mut App) + 'static,
    ) {
        let asking = self.quit_dialog.as_ref().and_then(WeakFocusHandle::upgrade);
        if asking.is_some_and(|dialog| dialog.contains_focused(window, cx)) {
            return;
        }
        let ids = self.tab_ids(cx);
        let keys = ["tabs.quitTitle", "tabs.quitDescription", "tabs.quitConfirm"];
        let discard = self.confirm_discard(&self.drafts(&ids, cx), cx);
        match (self.confirm_rollback(&ids, keys, cx), discard) {
            (None, None) => return window.defer(cx, then),
            (None, Some(discard)) => dialogs::confirm(discard, window, cx, then),
            (Some(rollback), discard) => {
                let (this, then) = (cx.entity().downgrade(), Rc::new(then));
                // Confirming the rollback leads to the drafts' question.
                dialogs::confirm(rollback, window, cx, move |window, cx| match discard.clone() {
                    Some(discard) => {
                        let then = then.clone();
                        dialogs::confirm(discard, window, cx, move |window, cx| then(window, cx));
                        let _ =
                            this.update(cx, |this, cx| this.quit_dialog = window.focused(cx).map(|d| d.downgrade()));
                    }
                    None => then(window, cx),
                });
            }
        }
        // A dialog takes focus as it opens.
        self.quit_dialog = window.focused(cx).map(|dialog| dialog.downgrade());
    }

    // Returns whether the window may close. On macOS it stays open while the app quits: closing it first drops GPUI's
    // AccessKit adapter, which resets the class of the content view while the Touch Bar observes it through KVO, and
    // AppKit aborts on the Touch Bar's next update.
    fn close_window(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let drafts = self.tabs.iter().any(|tab| tab.view.holds_draft(cx));
        if cfg!(target_os = "macos") || drafts || state::bar(cx).any_transaction_open() {
            self.quit(window, cx);
            return false;
        }
        true
    }

    // Clean tabs close at once. Drafts can't be saved, so they ask first, Discard only; then query tabs.
    fn close_tabs_saving(&mut self, ids: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        let drafts = self.drafts(&ids, cx);
        let dirty = |this: &Self, id: &String| this.index_of(id, cx).is_some_and(|ix| this.tabs[ix].view.is_dirty(cx));
        let (dirty, clean): (Vec<String>, Vec<String>) = ids.iter().cloned().partition(|id| dirty(self, id));
        for id in clean.iter().filter(|id| !drafts.contains(id)) {
            self.close_tab_by_id(id, window, cx);
        }
        let Some(confirm) = self.confirm_discard(&drafts, cx) else {
            return self.close_unsaved(dirty, window, cx);
        };
        let this = cx.entity().downgrade();
        dialogs::confirm(confirm, window, cx, move |window, cx| {
            let _ = this.update(cx, |this, cx| {
                for id in drafts.iter().filter(|id| !dirty.contains(id)) {
                    this.close_tab_by_id(id, window, cx);
                }
                this.close_unsaved(dirty.clone(), window, cx);
            });
        });
    }

    fn drafts(&self, ids: &[String], cx: &App) -> Vec<String> {
        let held = |id: &&String| self.index_of(id, cx).is_some_and(|ix| self.tabs[ix].view.holds_draft(cx));
        ids.iter().filter(held).cloned().collect()
    }

    fn confirm_discard(&self, drafts: &[String], cx: &App) -> Option<Confirm> {
        let titles: Vec<String> = drafts
            .iter()
            .filter_map(|id| self.index_of(id, cx).map(|ix| self.tabs[ix].view.title(cx).to_string()))
            .collect();
        (!titles.is_empty()).then(|| Confirm {
            title: t(cx, "tabs.discardTitle"),
            description: t_count(cx, "tabs.discardDescription", titles.len() as i64, &[]),
            detail: Some(titles.join("\n").into()),
            confirm: t(cx, "tabs.discardConfirm"),
            danger: true,
        })
    }

    fn close_unsaved(&mut self, dirty: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        if dirty.is_empty() {
            return;
        }
        let names: Vec<String> = dirty
            .iter()
            .filter_map(|id| self.index_of(id, cx).map(|ix| self.tabs[ix].view.title(cx).to_string()))
            .collect();
        let (on_save, on_discard) = (cx.entity().downgrade(), cx.entity().downgrade());
        let save_ids = dirty.clone();
        dialogs::unsaved(
            names.join(", ").into(),
            window,
            cx,
            move |window, cx| {
                let _ = on_save.update(cx, |this, cx| {
                    for id in &save_ids {
                        if this.persist_tab(id, window, cx) {
                            this.close_tab_by_id(id, window, cx);
                        }
                    }
                });
            },
            move |window, cx| {
                let _ = on_discard.update(cx, |this, cx| {
                    for id in &dirty {
                        this.close_tab_by_id(id, window, cx);
                    }
                });
            },
        );
    }

    fn persist_tab(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(view) = self.index_of(id, cx).and_then(|ix| self.tabs[ix].view.query().cloned()) else {
            return true;
        };
        view.update(cx, |tab, cx| {
            let sql = tab.sql(cx).to_string();
            tab.persist(sql, window, cx)
        })
    }

    // Pinned tabs come first. Pinning makes a tab their last, unpinning the first after them.
    fn set_pinned(&mut self, id: &str, pinned: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(from) = self.index_of(id, cx) else { return };
        let active = self.active_tab().map(|tab| tab.id(cx));
        let mut open = self.tabs.remove(from);
        open.pinned = pinned;
        let to = self.tabs.iter().take_while(|tab| tab.pinned).count();
        self.tabs.insert(to, open);
        let ix = active.and_then(|id| self.index_of(&id, cx)).unwrap_or(to);
        self.activate(ix, window, cx);
    }

    // A new plain query tab with the same SQL, on the same connection.
    fn duplicate_tab(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(view) = self.index_of(id, cx).and_then(|ix| self.tabs[ix].view.query().cloned()) else { return };
        let (connection, sql) = {
            let tab = view.read(cx);
            (tab.connection.clone(), tab.sql(cx).to_string())
        };
        let num = self.next_query_number(&connection.id, cx).to_string();
        let title = t_with(cx, "app.queryTabWithConn", &[("num", &num), ("conn", &connection.name)]);
        self.open_query_tab_with(connection, sql, title, window, cx);
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

    // With no tab open the main area is the way in. Without connections that's a new one or a SQLite file, which can
    // be dropped on the window too. With them, a query tab, or the tab closed last.
    fn empty_state(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let has_connections = self.sidebar.read(cx).has_connections();
        let button =
            |id: &'static str, label: SharedString| Button::new(id).debug_selector(move || id.into()).label(label);
        let actions: Vec<Button> = match has_connections {
            false => vec![
                button("welcome-new-connection", t(cx, "sidebar.newConnection"))
                    .primary()
                    .icon(IconName::Plus)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.sidebar.update(cx, |sidebar, cx| sidebar.add_connection(window, cx))
                    })),
                button("welcome-open-sqlite", t(cx, "app.openSqlite"))
                    .icon(Icon::new(Lucide::Database))
                    .on_click(cx.listener(|this, _, window, cx| this.pick_sqlite(window, cx))),
            ],
            true => {
                let mut actions = vec![
                    button("welcome-new-query", t(cx, "app.newQuery"))
                        .primary()
                        .icon(IconName::Plus)
                        .on_click(cx.listener(|this, _, window, cx| this.welcome_query(window, cx))),
                ];
                if !self.closed.is_empty() {
                    actions.push(
                        button("welcome-reopen", t(cx, "tabs.reopenClosed"))
                            .on_click(cx.listener(|this, _, window, cx| this.reopen_closed_tab(window, cx))),
                    );
                }
                actions
            }
        };
        let hint = |id: &str, key: &str| (shortcuts::shown(id, cx), t(cx, key));
        let mut hints = vec![
            hint("quickSearch", "shortcuts.items.quickSearch"),
            hint("commandPalette", "shortcuts.items.commandPalette"),
        ];
        if has_connections {
            hints.push(hint("newTab", "shortcuts.items.newTab"));
        }
        v_flex()
            .debug_selector(|| "welcome".into())
            .size_full()
            .items_center()
            .justify_center()
            .gap(rems(1.538))
            .p(rems(2.462))
            .child(
                v_flex()
                    .items_center()
                    .gap(rems(0.308))
                    .child(img(LOGO).size(rems(4.308)).mb(rems(0.462)))
                    .child(div().text_size(TEXT_LG).font_semibold().child(t(cx, "app.emptyTitle")))
                    .child(div().text_color(theme.muted_foreground).child(t(cx, "about.tagline"))),
            )
            .child(v_flex().items_center().gap(rems(0.615)).child(h_flex().gap(rems(0.615)).children(actions)).when(
                !has_connections,
                |el| {
                    el.child(
                        div().text_size(TEXT_SM).text_color(theme.muted_foreground).child(t(cx, "app.welcomeDrop")),
                    )
                },
            ))
            .child(form::shortcut_hints(hints, cx))
    }

    // A query tab on the selected connection, or the first one when none is picked yet.
    fn welcome_query(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let sidebar = self.sidebar.read(cx);
        if let Some(connection) = sidebar.selected().or_else(|| sidebar.connections().first()).cloned() {
            self.open_query_tab(connection, window, cx);
        }
    }

    // A picked file becomes a prefilled connection, as a dropped one does.
    fn pick_sqlite(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pick = file_dialogs::pick_sqlite_file(cx);
        cx.spawn_in(window, async move |this, cx| {
            let Some(path) = pick.await else { return };
            let _ = this.update_in(cx, |this, window, cx| {
                let Some(path) = file_dialogs::path_text(&path, cx) else { return };
                let (file_path, name) = sqlite_file_payload(&path);
                this.open_sqlite(file_path, name, window, cx);
            });
        })
        .detach();
    }

    fn tab_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let workspace = cx.entity().downgrade();
        let tabs: Vec<AnyElement> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(ix, open)| {
                let id = open.view.id(cx);
                let connection = open.view.connection(cx);
                let tab_color = match &open.view {
                    TabView::Query(tab) => tab.read(cx).color(),
                    TabView::Table(_) | TabView::Diagram(_) | TabView::Structure(_) => &connection.color,
                };
                let color = parse_color(tab_color).unwrap_or(theme.muted_foreground);
                let active = ix == self.active;
                let read_only = connection.read_only;
                let dirty = open.view.is_dirty(cx);
                let close_hovered = self.hovered_close == Some(ix);
                // Unsaved changes show as a dot in the button's place, as in most editors. Under the pointer the
                // button comes back.
                let unsaved = dirty && !close_hovered;
                let dot = || {
                    let color = if active { theme.foreground } else { theme.muted_foreground };
                    div()
                        .debug_selector(move || format!("unsaved-tab-{ix}"))
                        .size(ICON_2XS)
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(div().size(rems(0.462)).rounded_full().bg(color))
                        .into_any_element()
                };
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
                    .child(div().max_w(rems(15.385)).truncate().child(open.view.title(cx)))
                    .child(
                        match open.pinned {
                            // A pinned tab shows its pin, which unpins it, in place of the close button.
                            true => div()
                                .id(("unpin-tab", ix))
                                .debug_selector(move || format!("unpin-tab-{ix}"))
                                .flex_none()
                                .ml(rems(0.308))
                                .px(rems(0.154))
                                .opacity(if close_hovered || unsaved { 1. } else { 0.7 })
                                .child(match unsaved {
                                    true => dot(),
                                    false => Icon::new(Lucide::Pin).size(ICON_2XS).into_any_element(),
                                })
                                .on_click(cx.listener({
                                    let id = id.clone();
                                    move |this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.set_pinned(&id, false, window, cx);
                                    }
                                })),
                            false => div()
                                .id(("close-tab", ix))
                                .debug_selector(move || format!("close-tab-{ix}"))
                                .flex_none()
                                .ml(rems(0.308))
                                .px(rems(0.154))
                                .opacity(if close_hovered || unsaved { 1. } else { 0.5 })
                                .child(match unsaved {
                                    true => dot(),
                                    false => Icon::new(IconName::Close)
                                        .size(ICON_2XS)
                                        .when(close_hovered, |icon| icon.text_color(theme.danger))
                                        .into_any_element(),
                                })
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.hovered_close = None;
                                    this.request_close(ix, window, cx);
                                })),
                        }
                        .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                            let hovered = hovered.then_some(ix);
                            if this.hovered_close != hovered && (hovered.is_some() || this.hovered_close == Some(ix)) {
                                this.hovered_close = hovered;
                                cx.notify();
                            }
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
                    .context_menu({
                        let workspace = workspace.clone();
                        move |menu, _, cx| Workspace::tab_menu(&workspace, ix, menu, cx)
                    })
                    .into_any_element()
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
            .debug_selector(|| "tab-bar".into())
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
            // Not on hover: its track would take clicks on the tabs' lower half.
            .child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .h(Scrollbar::width())
                    .child(scrollbars::system_bar(Scrollbar::horizontal(&self.tab_scroll), cx)),
            )
    }

    // On a tab. Close Others, To the Right and All leave pinned tabs open.
    fn tab_menu(this: &WeakEntity<Self>, ix: usize, menu: PopupMenu, cx: &mut Context<PopupMenu>) -> PopupMenu {
        let Some(workspace) = this.upgrade() else { return menu };
        let state = workspace.read(cx);
        let Some(open) = state.tabs.get(ix) else { return menu };
        let (id, pinned, active, query) =
            (open.view.id(cx), open.pinned, ix == state.active, open.view.query().cloned());
        let unpinned = |tabs: &mut dyn Iterator<Item = (usize, &OpenTab)>| -> Vec<String> {
            tabs.filter(|(_, tab)| !tab.pinned).map(|(_, tab)| tab.view.id(cx)).collect()
        };
        let others = unpinned(&mut state.tabs.iter().enumerate().filter(|(i, _)| *i != ix));
        let right = unpinned(&mut state.tabs.iter().enumerate().skip(ix + 1));
        let all = unpinned(&mut state.tabs.iter().enumerate());
        let can_reopen = !state.closed.is_empty();
        let run = |f: TabAction| {
            let workspace = workspace.downgrade();
            move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                let _ = workspace.update(cx, |this, cx| f(this, window, cx));
            }
        };
        let close_id = id.clone();
        let menu = menu
            .action_context(state.focus.clone())
            .item(
                PopupMenuItem::new(t(cx, "tabs.close"))
                    .when(active, |item| item.action(Box::new(CloseTab)))
                    .on_click(run(Box::new(move |this, window, cx| this.request_close_id(&close_id, window, cx)))),
            )
            .item(
                PopupMenuItem::new(t(cx, "tabs.closeOthers"))
                    .disabled(others.is_empty())
                    .on_click(run(Box::new(move |this, window, cx| this.close_tabs(others.clone(), window, cx)))),
            )
            .item(
                PopupMenuItem::new(t(cx, "tabs.closeRight"))
                    .disabled(right.is_empty())
                    .on_click(run(Box::new(move |this, window, cx| this.close_tabs(right.clone(), window, cx)))),
            )
            .item(
                PopupMenuItem::new(t(cx, "tabs.closeAll"))
                    .disabled(all.is_empty())
                    .on_click(run(Box::new(move |this, window, cx| this.close_tabs(all.clone(), window, cx)))),
            )
            .separator()
            .item(
                PopupMenuItem::new(t(cx, "tabs.reopenClosed"))
                    .action(Box::new(ReopenClosedTab))
                    .disabled(!can_reopen)
                    .on_click(run(Box::new(|this, window, cx| this.reopen_closed_tab(window, cx)))),
            )
            .separator();
        let pin_id = id.clone();
        let menu = menu.item(
            PopupMenuItem::new(t(cx, if pinned { "tabs.unpin" } else { "tabs.pin" }))
                .on_click(run(Box::new(move |this, window, cx| this.set_pinned(&pin_id, !pinned, window, cx)))),
        );
        let Some(query) = query else { return menu };
        let (rename, copy) = (query.clone(), query);
        menu.item(
            PopupMenuItem::new(t(cx, "tabs.duplicate"))
                .on_click(run(Box::new(move |this, window, cx| this.duplicate_tab(&id, window, cx)))),
        )
        .item(PopupMenuItem::new(t(cx, "tabs.rename")).on_click(move |_, window, cx| {
            rename.update(cx, |tab, cx| tab.rename(window, cx));
        }))
        .separator()
        .item(PopupMenuItem::new(t(cx, "tabs.copySql")).on_click(move |_, _, cx| {
            let sql = copy.read(cx).sql(cx).to_string();
            cx.write_to_clipboard(ClipboardItem::new_string(sql));
            toast::success(t(cx, "toast.copiedClipboard"), cx);
        }))
    }

    // Pinned tabs move only among themselves, and so do the others.
    fn move_tab(&mut self, from: usize, to: usize, window: &mut Window, cx: &mut Context<Self>) {
        if from == to
            || from >= self.tabs.len()
            || to >= self.tabs.len()
            || self.tabs[from].pinned != self.tabs[to].pinned
        {
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

    pub(crate) fn status(&self, window: &Window, cx: &App) -> StatusBarState {
        let named = |connection: &ConnectionConfig| StatusConnection {
            name: connection.name.clone().into(),
            engine: connection_dialog::engine_name(&connection.driver, cx),
            connected: state::bar(cx).is_connected(&connection.id),
        };
        let Some(tab) = self.active_tab() else {
            let connection = self.sidebar.read(cx).selected().map(named);
            return StatusBarState { connection, ..Default::default() };
        };
        let connection = tab.connection(cx);
        let live = match tab {
            TabView::Query(tab) => tab.read(cx).live_status(cx),
            TabView::Table(tab) => tab.read(cx).is_loading().then(|| t(cx, "tableView.loading")),
            TabView::Diagram(_) | TabView::Structure(_) => None,
        };
        let query = tab.query().map(|tab| tab.read(cx));
        let transaction = query.and_then(|tab| match tab.txn_state() {
            TxnState::Idle => None,
            TxnState::Active => Some(false),
            TxnState::Error => Some(true),
        });
        let grid = self.active_grid(cx);
        let in_grid = grid.as_ref().is_some_and(|grid| grid.read(cx).focus_handle(cx).contains_focused(window, cx));
        let in_editor = query.is_some_and(|tab| tab.editor_has_focus(window, cx));
        let last = self.grid_context.get().filter(|(id, _)| *id == tab.entity_id()).map(|(_, grid)| grid);
        let grid_context = in_grid || (!in_editor && last.unwrap_or(query.is_none()));
        self.grid_context.set(Some((tab.entity_id(), grid_context)));
        let grid = grid.filter(|_| grid_context);
        StatusBarState {
            connection: Some(named(connection)),
            color: parse_color(&connection.color),
            read_only: connection.read_only,
            protected: connection.confirm_changes,
            extensions: connection.active_extensions().map(|path| path_parts(path).1.to_string()).collect(),
            transaction,
            caret: query.filter(|_| grid.is_none()).map(|tab| tab.caret(cx)),
            aggregate: grid.and_then(|grid| grid.read(cx).aggregate()),
            live,
            actions: StatusActions::default(),
        }
    }

    // A click on the caret's place goes to a line, on the connection opens the switcher, on its chips opens its
    // settings, and on Stop ends the run.
    fn status_actions(&self, cx: &Context<Self>) -> StatusActions {
        let this = cx.weak_entity();
        let on = |f: fn(&mut Self, &mut Window, &mut Context<Self>)| -> StatusAction {
            let this = this.clone();
            Rc::new(move |window, cx| {
                let _ = this.update(cx, |this, cx| f(this, window, cx));
            })
        };
        let query = self.active_tab().and_then(TabView::query);
        StatusActions {
            go_to_line: query.map(|_| on(Self::go_to_line)),
            switch_connection: Some(on(Self::open_switcher)),
            edit_connection: self.active_tab().map(|_| on(Self::edit_tab_connection)),
            stop: query.filter(|tab| tab.read(cx).is_running()).map(|_| on(Self::stop_run)),
        }
    }

    fn go_to_line(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(tab) = self.active_tab().and_then(TabView::query).cloned() {
            tab.update(cx, |tab, cx| tab.go_to_line(window, cx));
        }
    }

    fn open_switcher(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.sidebar_open {
            self.sidebar_open = true;
            set_setting_bool(cx, SIDEBAR_OPEN_KEY, true);
            self.reset_panels(cx);
        }
        self.sidebar.update(cx, |sidebar, cx| sidebar.open_switcher(window, cx));
        cx.notify();
    }

    fn edit_tab_connection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(connection) = self.active_tab().map(|tab| tab.connection(cx).clone()) else { return };
        self.sidebar.update(cx, |sidebar, cx| sidebar.edit_connection(Some(connection), window, cx));
    }

    fn stop_run(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(tab) = self.active_tab().and_then(TabView::query).cloned() {
            tab.update(cx, |tab, cx| tab.cancel(cx));
        }
    }

    #[cfg(test)]
    pub(crate) fn table_tab_named(&self, table: &str, cx: &App) -> Option<Entity<TableTab>> {
        self.tabs.iter().find_map(|tab| match &tab.view {
            TabView::Table(view) if view.read(cx).table == table => Some(view.clone()),
            _ => None,
        })
    }

    #[cfg(test)]
    pub(crate) fn active_structure(&self) -> Option<Entity<StructureTab>> {
        match self.active_tab()? {
            TabView::Structure(tab) => Some(tab.clone()),
            _ => None,
        }
    }

    pub(crate) fn active_grid(&self, cx: &App) -> Option<Entity<grid::Grid>> {
        match self.active_tab()? {
            TabView::Query(tab) => tab.read(cx).results().read(cx).active_grid(),
            TabView::Table(tab) => Some(tab.read(cx).grid()),
            TabView::Diagram(_) | TabView::Structure(_) => None,
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
                this.reset_panels(cx);
                cx.notify();
            }))
            .on_action(cx.listener(|_, _: &MinimizeWindow, window, _| window.minimize_window()))
            .on_action(cx.listener(|_, _: &ZoomWindow, window, _| window.zoom_window()))
            .on_action(cx.listener(|this, _: &ToggleJsonPanel, window, cx| this.toggle_json_panel(window, cx)))
            .on_action(cx.listener(|_, _: &ToggleGridStripes, window, cx| {
                set_setting_bool(cx, grid::STRIPES_KEY, !setting_bool(cx, grid::STRIPES_KEY, false));
                set_menus(cx);
                window.refresh();
            }))
            .on_action(cx.listener(|this, _: &NewTab, window, cx| this.new_tab(window, cx)))
            .when(self.structure_target(cx).is_some(), |el| {
                el.on_action(cx.listener(|this, _: &EditTableStructure, window, cx| {
                    if let Some((connection, schema, table)) = this.structure_target(cx) {
                        this.open_structure(connection, schema, Some(table), window, cx);
                    }
                }))
            })
            .when(self.creatable_connection(cx).is_some(), |el| {
                el.on_action(cx.listener(|this, _: &NewTable, window, cx| this.new_table(window, cx)))
            })
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
            .on_action(cx.listener(|this, _: &CommandPalette, window, cx| {
                this.open_command_palette(window, cx);
            }))
            .on_action(cx.listener(|this, _: &CloseTab, window, cx| this.request_close(this.active, window, cx)))
            .on_action(cx.listener(|this, _: &ReopenClosedTab, window, cx| this.reopen_closed_tab(window, cx)))
            .on_action(cx.listener(|this, _: &NextTab, window, cx| this.cycle_tab(true, window, cx)))
            .on_action(cx.listener(|this, _: &PrevTab, window, cx| this.cycle_tab(false, window, cx)))
            .on_action(cx.listener(|_, _: &ThemeDark, window, cx| theme::set_choice(ThemeChoice::Dark, window, cx)))
            .on_action(cx.listener(|_, _: &ThemeLight, window, cx| theme::set_choice(ThemeChoice::Light, window, cx)))
            .on_action(cx.listener(|_, _: &ThemeSystem, window, cx| theme::set_choice(ThemeChoice::System, window, cx)))
            .on_action(cx.listener(|this, _: &LanguageEn, window, cx| this.set_language("en", window, cx)))
            .on_action(cx.listener(|this, _: &LanguageDe, window, cx| this.set_language("de", window, cx)))
            .on_action(cx.listener(|this, _: &LanguageBg, window, cx| this.set_language("bg", window, cx)))
            .on_action(cx.listener(|this, _: &LanguageEs, window, cx| this.set_language("es", window, cx)))
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
            .child(title_bar::render(
                self.menu_bar.as_ref(),
                title,
                cx.listener(|this, _: &ClickEvent, window, cx| {
                    if this.close_window(window, cx) {
                        window.remove_window();
                    }
                }),
                cx,
            ))
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
                                    .size(self.sidebar_width)
                                    .size_range(px(SIDEBAR_WIDTHS.1)..px(SIDEBAR_WIDTHS.2))
                                    .fixed()
                                    .child(self.sidebar.clone()),
                            )
                        })
                        .child(resizable_panel().child(main))
                        .when(self.json_open, |panels| {
                            panels.child(
                                resizable_panel()
                                    .size(self.json_width)
                                    .size_range(px(JSON_WIDTHS.1)..px(JSON_WIDTHS.2))
                                    .fixed()
                                    .child(self.json_panel.clone()),
                            )
                        }),
                ),
            )
            .child(status_bar::render(
                StatusBarState { actions: self.status_actions(cx), ..self.status(window, cx) },
                cx,
            ))
            .children(toast::render(window, cx))
            .child(self.drop_overlay(cx))
            .child(self.context_menu.clone())
    }
}

#[cfg(test)]
mod tests;
