use std::ops::Range;
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use barsql_app::{EditorTab, RunEvent};
use barsql_core::{ConnectionConfig, DriverType, SavedQuery};
use barsql_sql::lang::{self, TxnControl, detect_transaction_control};
use barsql_sql::split_statements;
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::resize_handle;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{
    Copy, Cut, Delete, Editor, EditorMode, EditorState, InputEvent, Paste, Redo, Replace, Search, SelectAll, Undo,
};
use gpui_kit::component::native_menu::NativeMenu;
use gpui_kit::component::{ActiveTheme, Icon, IconName, RopeExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use lsp_types::{Diagnostic, DiagnosticSeverity};

use crate::actions::{
    ExplainAnalyze, ExplainQuery, FormatQuery, RenameSavedQuery, RunAll, RunSelection, SaveQuery, TriggerSuggest,
};
use crate::completion::{self, Completion};
use crate::context_menu::{self, Entry};
use crate::dialogs::{self, Prompt};
use crate::editor_commands;
use crate::form::ToolButton;
use crate::grid::{RowRef, copy};
use crate::i18n::{I18n, t, t_with};
use crate::results::{ResultStatus, ResultsEvent, ResultsPanel};
use crate::saved_queries;
use crate::schema::{self, Schemas};
use crate::scrollbars::ScrollbarsOnHover as _;
use crate::sql_language::{self, SqlLanguage, lsp_range};
use crate::tokens::{ICON_SM, TEXT_XS};
use crate::{state, theme};

const EDITOR_CONTEXT: &str = "QueryEditor CodeEditor";
const COMPLETING_CONTEXT: &str = "QueryEditor CodeEditor completing";
const DIAGNOSTICS_DEBOUNCE: Duration = Duration::from_millis(300);
const STATUS_TICK: Duration = Duration::from_millis(100);
// Results pane share of the tab in percent, as (default, min, max).
const RESULTS_SHARE: (f32, f32, f32) = (40., 15., 70.);

// Room left of the line numbers for the run glyphs.
const GLYPH_MARGIN_REM: f32 = 1.077;

// Shared by every query tab and kept for the session only.
struct ResultsShare(f32);

impl Global for ResultsShare {}

fn results_share(cx: &App) -> f32 {
    cx.try_global::<ResultsShare>().map_or(RESULTS_SHARE.0, |share| share.0)
}

#[cfg(feature = "snapshot")]
pub(crate) fn set_results_share(share: f32, cx: &mut App) {
    cx.set_global(ResultsShare(share.clamp(RESULTS_SHARE.1, RESULTS_SHARE.2)));
}

#[derive(Clone)]
struct SplitDrag;

impl Render for SplitDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TxnState {
    #[default]
    Idle,
    Active,
    // A statement failed inside the transaction. Only commit or rollback ends it.
    Error,
}

struct Running {
    started: Instant,
    _work: Task<()>,
    _tick: Task<()>,
}

// Lets jump_to_error find the run's statements in the buffer.
struct RunOrigin {
    base: usize,
    text: String,
}

pub enum QueryTabEvent {
    // SQL, title or saved-query link changed, so the session needs saving.
    Edited,
    // A run ended, so history has a new entry.
    RunFinished,
    // For the JSON row panel.
    FocusedRowChanged,
}

pub struct TabStatus {
    pub text: SharedString,
    pub error: bool,
}

pub struct QueryTab {
    pub id: String,
    pub title: SharedString,
    pub connection: ConnectionConfig,
    // Round-tripped through the session unchanged.
    color: String,
    saved_query_id: String,
    saved_sql_baseline: String,
    editor: Entity<EditorState>,
    completion: Entity<Completion<EditorMode>>,
    results: Entity<ResultsPanel>,
    running: Option<Running>,
    txn: TxnState,
    run_origin: Option<RunOrigin>,
    // Server error the user jumped to. Squiggled until the next edit or run.
    query_error: Option<(Range<usize>, String)>,
    diagnostics: Task<()>,
    _subscriptions: Vec<Subscription>,
}

pub fn new_tab_id() -> String {
    let millis = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis());
    let random = uuid::Uuid::new_v4().simple().to_string();
    format!("tab-{millis}-{}", &random[..5])
}

impl QueryTab {
    pub fn new(tab: EditorTab, connection: ConnectionConfig, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let sql = tab.sql;
        let language = Rc::new(SqlLanguage::new(connection.id.clone(), connection.driver.clone()));
        let editor = cx.new(|cx| {
            let mut state =
                EditorState::new(window, cx).language("sql").line_number(true).folding(true).default_value(sql);
            state.lsp_mut().hover_provider = Some(language.clone());
            state
        });
        let completion = cx.new(|cx| Completion::new(editor.clone(), Some(language), window, cx));
        let results = cx.new(|_| ResultsPanel::default());
        let subscriptions = vec![
            cx.subscribe_in(&editor, window, |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Change) {
                    this.query_error = None;
                    this.schedule_diagnostics(window, cx);
                    cx.emit(QueryTabEvent::Edited);
                }
            }),
            cx.subscribe_in(&results, window, |this, _, event: &ResultsEvent, window, cx| match event {
                ResultsEvent::JumpToError { statement, position, message } => {
                    this.jump_to_error(statement, *position, message, window, cx)
                }
                ResultsEvent::FocusedRowChanged => cx.emit(QueryTabEvent::FocusedRowChanged),
            }),
            cx.observe_global_in::<Schemas>(window, |this, window, cx| this.schedule_diagnostics(window, cx)),
            cx.observe_global_in::<I18n>(window, |this, window, cx| this.schedule_diagnostics(window, cx)),
            cx.observe(&results, |_, _, cx| cx.notify()),
        ];
        schema::ensure_loaded(&connection.id, connection.driver.clone(), cx);
        let mut this = Self {
            id: tab.id,
            title: tab.title.into(),
            connection,
            color: tab.color,
            saved_query_id: tab.saved_query_id,
            saved_sql_baseline: tab.saved_sql_baseline,
            editor,
            completion,
            results,
            running: None,
            txn: TxnState::Idle,
            run_origin: None,
            query_error: None,
            diagnostics: Task::ready(()),
            _subscriptions: subscriptions,
        };
        this.schedule_diagnostics(window, cx);
        this
    }

    pub fn focused_row(&self, cx: &App) -> Option<RowRef> {
        self.results.read(cx).focused_row(cx)
    }

    pub fn results_focus(&self, cx: &App) -> Option<FocusHandle> {
        self.results.read(cx).grid_focus(cx)
    }

    pub fn focus_editor(&self, window: &mut Window, cx: &mut App) {
        self.editor.update(cx, |state, cx| state.focus(window, cx));
    }

    pub fn insert(&self, text: &str, window: &mut Window, cx: &mut App) {
        let text = text.to_string();
        self.editor.update(cx, |state, cx| {
            state.replace(text, window, cx);
            state.focus(window, cx);
        });
    }

    pub fn sql(&self, cx: &App) -> SharedString {
        self.editor.read(cx).value()
    }

    pub fn stored(&self, cx: &App) -> EditorTab {
        EditorTab {
            id: self.id.clone(),
            connection_id: self.connection.id.clone(),
            title: self.title.to_string(),
            sql: self.sql(cx).to_string(),
            color: self.color.clone(),
            saved_query_id: self.saved_query_id.clone(),
            saved_sql_baseline: self.saved_sql_baseline.clone(),
            table_view: None,
        }
    }

    pub fn saved_query_id(&self) -> &str {
        &self.saved_query_id
    }

    pub fn color(&self) -> &str {
        &self.color
    }

    pub fn is_dirty(&self, cx: &App) -> bool {
        !self.saved_query_id.is_empty() && *self.sql(cx) != *self.saved_sql_baseline
    }

    // Takes edits like name, colour or read-only. A changed driver is ignored.
    pub fn set_connection(&mut self, connection: ConnectionConfig, cx: &mut Context<Self>) {
        if connection.driver == self.connection.driver {
            self.connection = connection;
            cx.notify();
        }
    }

    pub fn set_title(&mut self, title: SharedString, cx: &mut Context<Self>) {
        self.title = title;
        cx.emit(QueryTabEvent::Edited);
        cx.notify();
    }

    // Undoable, for history entries and reopened saved queries.
    pub fn set_sql(&mut self, sql: String, window: &mut Window, cx: &mut Context<Self>) {
        self.editor.update(cx, |state, cx| state.replace_all(sql, window, cx));
    }

    pub fn load_saved(&mut self, saved: &SavedQuery, window: &mut Window, cx: &mut Context<Self>) {
        self.set_sql(saved.sql.clone(), window, cx);
        self.link(saved, saved.sql.clone(), cx);
    }

    fn link(&mut self, saved: &SavedQuery, baseline: String, cx: &mut Context<Self>) {
        self.saved_query_id = saved.id.clone();
        self.saved_sql_baseline = baseline;
        self.set_title(saved.name.clone().into(), cx);
    }

    // A linked tab updates its saved query. Any other tab asks for a name and gets linked.
    pub fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let sql = self.sql(cx).to_string();
        if !self.saved_query_id.is_empty() {
            self.persist(sql, window, cx);
            return;
        }
        let this = cx.entity().downgrade();
        let baseline = sql.clone();
        saved_queries::save_as_new(self.connection.id.clone(), sql, window, cx, move |saved, _, cx| {
            let _ = this.update(cx, |tab, cx| tab.link(&saved, baseline.clone(), cx));
        });
    }

    pub fn persist(&mut self, sql: String, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let created_at = saved_queries::get(cx, &self.saved_query_id).map(|q| q.created_at.clone()).unwrap_or_default();
        let query = SavedQuery {
            id: self.saved_query_id.clone(),
            name: self.title.to_string(),
            connection_id: self.connection.id.clone(),
            sql: sql.clone(),
            created_at,
            updated_at: String::new(),
        };
        match state::bar(cx).save_saved_query(query) {
            Ok(_) => {
                self.saved_sql_baseline = sql;
                saved_queries::refresh(cx);
                cx.emit(QueryTabEvent::Edited);
                cx.notify();
                true
            }
            Err(error) => {
                saved_queries::failed(error.message, "errors.saveQueryFailed", window, cx);
                false
            }
        }
    }

    fn rename_saved(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saved_query_id.is_empty() {
            return;
        }
        let prompt = Prompt {
            title: t(cx, "dialog.renameQueryTitle"),
            description: None,
            label: t(cx, "dialog.renameQueryLabel"),
            placeholder: SharedString::default(),
            initial: self.title.to_string(),
            confirm: t(cx, "common.rename"),
        };
        let this = cx.entity().downgrade();
        dialogs::prompt(prompt, window, cx, move |name, window, cx| {
            let _ = this.update(cx, |tab, cx| tab.apply_rename(name, window, cx));
        });
    }

    fn apply_rename(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        if *name == *self.title {
            return;
        }
        let Some(previous) = saved_queries::get(cx, &self.saved_query_id).cloned() else { return };
        match state::bar(cx).save_saved_query(SavedQuery { name, ..previous }) {
            Ok(saved) => {
                saved_queries::refresh(cx);
                self.set_title(saved.name.into(), cx);
            }
            Err(error) => saved_queries::failed(error.message, "errors.renameQueryFailed", window, cx),
        }
    }

    fn suggest(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.completion.update(cx, |completion, cx| completion.request(true, window, cx));
    }

    // Formats the whole text, undoably.
    fn format(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let formatted = barsql_io::format_query(&self.sql(cx), &self.connection.driver);
        self.editor.update(cx, |state, cx| state.replace_all(formatted, window, cx));
    }

    pub fn set_transaction_ended(&mut self, cx: &mut Context<Self>) {
        self.txn = TxnState::Idle;
        cx.notify();
    }

    fn schedule_diagnostics(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.diagnostics = cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(DIAGNOSTICS_DEBOUNCE).await;
            let _ = this.update(cx, |this, cx| this.refresh_diagnostics(cx));
        });
    }

    fn refresh_diagnostics(&mut self, cx: &mut Context<Self>) {
        let catalog = schema::catalog(cx, &self.connection.id);
        let labels = cx.global::<I18n>().sql_labels();
        let query_error = self.query_error.clone();
        self.editor.update(cx, |state, cx| {
            let rope = state.text().clone();
            let warnings = catalog.map_or_else(Vec::new, |catalog| {
                sql_language::diagnostics(&rope.to_string(), &catalog, &labels, state.cursor())
            });
            let Some(set) = state.diagnostics_mut() else { return };
            set.clear();
            for d in warnings {
                set.push(Diagnostic {
                    range: lsp_range(&rope, d.start..d.end),
                    severity: Some(DiagnosticSeverity::WARNING),
                    message: d.message,
                    ..Default::default()
                });
            }
            if let Some((range, message)) = query_error {
                set.push(Diagnostic {
                    range: lsp_range(&rope, range),
                    severity: Some(DiagnosticSeverity::ERROR),
                    message,
                    ..Default::default()
                });
            }
            cx.notify();
        });
    }

    // Finds the statement by the last run's split ranges, or by its text if the buffer changed since.
    // `position` counts characters, not bytes.
    fn jump_to_error(
        &mut self,
        statement: &str,
        position: u32,
        message: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let buffer = self.sql(cx);
        let at_run = self.run_origin.as_ref().and_then(|origin| {
            let found =
                split_statements(&self.connection.driver, &origin.text).into_iter().find(|s| s.text == statement);
            found.map(|s| origin.base + s.range.start)
        });
        let start = at_run
            .filter(|&start| buffer.get(start..start + statement.len()) == Some(statement))
            .or_else(|| buffer.find(statement));
        let offset = start.and_then(|start| char_offset(statement, position).map(|ix| start + ix));
        let Some(offset) = offset else {
            self.focus_editor(window, cx);
            return;
        };
        self.query_error = Some((word_range(&buffer, offset), message.to_string()));
        self.editor.update(cx, |state, cx| {
            let position = state.text().offset_to_position(offset);
            state.set_cursor_position(position, window, cx);
        });
        self.refresh_diagnostics(cx);
    }

    // Does nothing without a selection.
    fn run_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.editor.read(cx);
        let (selected, base) = (state.selected_text().to_string(), state.selected_range().start);
        if !selected.trim().is_empty() {
            self.run_sql(selected, base, window, cx);
        }
    }

    pub(crate) fn run_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let sql = self.sql(cx).to_string();
        if !sql.trim().is_empty() {
            self.run_sql(sql, 0, window, cx);
        }
    }

    // When several statements start on the line, runs the one holding the caret.
    fn run_line(&mut self, line: usize, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.editor.read(cx);
        let (sql, cursor) = (state.value().to_string(), state.cursor());
        let cursor_line = sql[..cursor.min(sql.len())].matches('\n').count() + 1;
        let statements = lang::parse_statements(&sql, Some(&self.connection.driver));
        let on_line: Vec<_> = statements.iter().filter(|s| s.run_line == line).collect();
        let at_cursor =
            on_line.iter().find(|s| cursor_line == line && cursor >= s.start && cursor < s.end).or(on_line.first());
        if let Some(statement) = at_cursor {
            let base = statement.text.as_ptr() as usize - sql.as_ptr() as usize;
            let text = statement.text.to_string();
            self.run_sql(text, base, window, cx);
        }
    }

    // Glyphs go in paint because the editor lays itself out in paint and this sibling paints after it.
    fn run_glyphs(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let editor = self.editor.clone();
        let tab = cx.entity().downgrade();
        let running = self.running.is_some();
        let driver = self.connection.driver.clone();
        let accent = cx.theme().primary;
        canvas(
            |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
            move |bounds, hitbox, window, cx| {
                let state = editor.read(cx);
                let Some(line_height) = state.line_height() else { return };
                let sql = state.value().to_string();
                let mut glyphs: Vec<(usize, Bounds<Pixels>)> = Vec::new();
                for statement in lang::parse_statements(&sql, Some(&driver)) {
                    if glyphs.last().is_some_and(|(line, _)| *line == statement.run_line) {
                        continue;
                    }
                    let offset = statement.text.as_ptr() as usize - sql.as_ptr() as usize;
                    let Some(at) = state.range_to_bounds(&(offset..offset)) else { continue };
                    let rect = Bounds::new(point(bounds.left(), at.top()), size(bounds.size.width, line_height));
                    if rect.bottom() > bounds.top() && rect.top() < bounds.bottom() {
                        glyphs.push((statement.run_line, rect));
                    }
                }
                let hovered = (!running && hitbox.is_hovered(window))
                    .then(|| glyphs.iter().position(|(_, rect)| rect.contains(&window.mouse_position())))
                    .flatten();
                window.with_content_mask(Some(ContentMask { bounds }), |window| {
                    let (width, height) = (px(7.), px(8.));
                    for (ix, (_, rect)) in glyphs.iter().enumerate() {
                        let opacity = if running {
                            0.2
                        } else if hovered == Some(ix) {
                            1.
                        } else {
                            0.35
                        };
                        let x = rect.right() - width - px(1.);
                        let y = rect.top() + (rect.size.height - height) / 2.;
                        let mut path = Path::new(point(x, y));
                        path.line_to(point(x, y + height));
                        path.line_to(point(x + width, y + height / 2.));
                        path.line_to(point(x, y));
                        window.paint_path(path, accent.opacity(opacity));
                    }
                });
                if hovered.is_some() {
                    window.set_cursor_style(CursorStyle::PointingHand, &hitbox);
                }
                let (lines, move_hitbox) = (glyphs.clone(), hitbox.clone());
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, _| {
                    if !phase.bubble() {
                        return;
                    }
                    let now = (!running && move_hitbox.is_hovered(window))
                        .then(|| lines.iter().position(|(_, rect)| rect.contains(&event.position)))
                        .flatten();
                    if now != hovered {
                        window.refresh();
                    }
                });
                window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                    if !phase.bubble() || event.button != MouseButton::Left || running || !hitbox.is_hovered(window) {
                        return;
                    }
                    if let Some(&(line, _)) = glyphs.iter().find(|(_, rect)| rect.contains(&event.position)) {
                        cx.stop_propagation();
                        let _ = tab.update(cx, |tab, cx| tab.run_line(line, window, cx));
                    }
                });
            },
        )
        .absolute()
        .top_0()
        .bottom_0()
        .left_0()
        .w(rems(GLYPH_MARGIN_REM))
    }

    fn run_sql(&mut self, sql: String, base: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.running.is_some() {
            return;
        }
        if self.query_error.take().is_some() {
            self.refresh_diagnostics(cx);
        }
        self.run_origin = Some(RunOrigin { base, text: sql.clone() });
        // A lone BEGIN, COMMIT or ROLLBACK drives the tab's transaction instead of running raw.
        if let Some(control) = detect_transaction_control(&sql) {
            self.transaction(control, true, window, cx);
            return;
        }
        self.results.update(cx, |results, cx| results.clear(cx));
        let bar = state::bar(cx);
        let (connection_id, tab_id) = (self.connection.id.clone(), self.id.clone());
        let start = state::spawn(cx, async move { bar.execute_query_stream(&connection_id, &tab_id, &sql).await });
        let results = self.results.downgrade();
        let work = cx.spawn_in(window, async move |this, cx| {
            match start.await {
                Some(Ok(handle)) => {
                    while let Ok(event) = handle.events.recv().await {
                        let done = matches!(event, RunEvent::Done { .. });
                        let Ok(failed) = results.update_in(cx, |results, window, cx| results.apply(event, window, cx))
                        else {
                            return;
                        };
                        if let Some(failed) = failed {
                            let _ = this.update(cx, |this, _| this.track_statement(failed));
                        }
                        if done {
                            break;
                        }
                    }
                }
                Some(Err(error)) => {
                    let _ = results.update(cx, |results, cx| results.show_error(error, cx));
                }
                None => {}
            }
            let _ = this.update(cx, |this, cx| this.finish_run(cx));
        });
        self.start_running(work, cx);
    }

    fn start_running(&mut self, work: Task<()>, cx: &mut Context<Self>) {
        let tick = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(STATUS_TICK).await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        });
        self.running = Some(Running { started: Instant::now(), _work: work, _tick: tick });
        cx.notify();
    }

    fn finish_run(&mut self, cx: &mut Context<Self>) {
        self.running = None;
        cx.emit(QueryTabEvent::RunFinished);
        // A tab opened while the server was down gets its schema once a run gets through.
        schema::ensure_loaded(&self.connection.id, self.connection.driver.clone(), cx);
        cx.notify();
    }

    fn track_statement(&mut self, failed: bool) {
        if self.txn != TxnState::Idle {
            self.txn = if failed { TxnState::Error } else { TxnState::Active };
        }
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        state::bar(cx).cancel_query(&self.id);
    }

    // `confirm` shows the result message a typed BEGIN, COMMIT or ROLLBACK gets. Toolbar buttons pass false.
    pub(crate) fn transaction(
        &mut self,
        control: TxnControl,
        confirm: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let bar = state::bar(cx);
        let (connection_id, tab_id) = (self.connection.id.clone(), self.id.clone());
        let work = state::spawn(cx, async move {
            match control {
                TxnControl::Begin => bar.begin_transaction(&connection_id, &tab_id).await,
                TxnControl::Commit => bar.commit_transaction(&tab_id).await,
                TxnControl::Rollback => bar.rollback_transaction(&tab_id).await,
            }
        });
        cx.spawn_in(window, async move |this, cx| {
            let Some(result) = work.await else { return };
            let _ = this.update(cx, |this, cx| {
                // Commit and rollback end the transaction whether or not they succeed.
                match (control, &result) {
                    (TxnControl::Begin, Ok(())) => this.txn = TxnState::Active,
                    (TxnControl::Begin, Err(_)) => {}
                    _ => this.txn = TxnState::Idle,
                }
                let message = match control {
                    TxnControl::Begin => "results.txnBegin",
                    TxnControl::Commit => "results.txnCommit",
                    TxnControl::Rollback => "results.txnRollback",
                };
                this.results.update(cx, |results, cx| match result {
                    Ok(()) if confirm => results.show_message(t(cx, message), cx),
                    Ok(()) => {}
                    Err(error) => results.show_error(error, cx),
                });
                cx.notify();
            });
        })
        .detach();
    }

    fn can_analyze(&self) -> bool {
        self.connection.driver != DriverType::Sqlite
    }

    // Uses the selection, else the statement at the caret, else the whole text.
    fn explain_text(&self, cx: &App) -> String {
        let state = self.editor.read(cx);
        let selected = state.selected_text().to_string();
        if !selected.trim().is_empty() {
            return selected.trim().to_string();
        }
        let sql = state.value();
        let offset = state.cursor();
        let statements = lang::parse_statements(&sql, Some(&self.connection.driver));
        let at_cursor = statements.iter().find(|s| offset >= s.start && offset <= s.end).map(|s| s.text.trim());
        match at_cursor.filter(|text| !text.is_empty()) {
            Some(text) => text.to_string(),
            None => sql.trim().to_string(),
        }
    }

    // Shares the running state because an analyze takes as long as the query and cancels the same way.
    pub(crate) fn explain(&mut self, analyze: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.running.is_some() || (analyze && !self.can_analyze()) {
            return;
        }
        let sql = self.explain_text(cx);
        if sql.is_empty() {
            return;
        }
        let bar = state::bar(cx);
        let (connection_id, tab_id) = (self.connection.id.clone(), self.id.clone());
        let plan = state::spawn(cx, async move { bar.explain_query(&connection_id, &tab_id, &sql, analyze).await });
        let work = cx.spawn_in(window, async move |this, cx| {
            let result = plan.await;
            let _ = this.update(cx, |this, cx| {
                this.results.update(cx, |results, cx| match result {
                    Some(Ok(plan)) => results.show_plan(plan, cx),
                    Some(Err(error)) => results.show_error(error, cx),
                    None => {}
                });
                this.running = None;
                cx.notify();
            });
        });
        self.start_running(work, cx);
    }

    pub fn status(&self, cx: &App) -> TabStatus {
        let result = self.results.read(cx).status(cx);
        let status = |text: SharedString| TabStatus { text, error: false };
        if let Some(running) = &self.running {
            let seconds = format!("{:.1}", running.started.elapsed().as_secs_f64());
            return status(match result {
                ResultStatus::Streaming(rows) if rows > 0 => {
                    t_with(cx, "app.statusStreaming", &[("count", &rows.to_string()), ("seconds", &seconds)])
                }
                _ => t_with(cx, "app.statusRunning", &[("seconds", &seconds)]),
            });
        }
        match result {
            ResultStatus::Error(message) => TabStatus { text: message, error: true },
            ResultStatus::Rows { count, ms } => {
                status(t_with(cx, "app.statusRows", &[("count", &count.to_string()), ("ms", &ms.to_string())]))
            }
            ResultStatus::Affected { count, ms } => {
                status(t_with(cx, "app.statusAffected", &[("count", &count.to_string()), ("ms", &ms.to_string())]))
            }
            ResultStatus::Empty | ResultStatus::Streaming(_) => status(t(cx, "app.statusReady")),
        }
    }

    fn toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let linked = !self.saved_query_id.is_empty();
        let run_buttons = |el: Div, cx: &mut Context<Self>| {
            if self.running.is_some() {
                return el.child(
                    Button::new("stop")
                        .debug_selector(|| "stop".into())
                        .danger()
                        .tool(Icon::new(Lucide::Square), ICON_SM, t(cx, "editor.stop"))
                        .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
                );
            }
            el.child(
                Button::new("run")
                    .debug_selector(|| "run".into())
                    .primary()
                    .tool(Icon::new(Lucide::Play), ICON_SM, t(cx, "editor.run"))
                    .on_click(cx.listener(|this, _, window, cx| this.run_selection(window, cx))),
            )
            .child(
                Button::new("run-all")
                    .debug_selector(|| "run-all".into())
                    .tool(Icon::new(Lucide::CirclePlay), ICON_SM, t(cx, "editor.runAll"))
                    .on_click(cx.listener(|this, _, window, cx| this.run_all(window, cx))),
            )
            .child(
                Button::new("explain")
                    .debug_selector(|| "explain".into())
                    .tool(Icon::new(Lucide::Route), ICON_SM, t(cx, "editor.explain"))
                    .on_click(cx.listener(|this, _, window, cx| this.explain(false, window, cx))),
            )
            .when(self.can_analyze(), |el| {
                el.child(
                    Button::new("explain-analyze")
                        .debug_selector(|| "explain-analyze".into())
                        .tool(Icon::new(Lucide::Gauge), ICON_SM, t(cx, "editor.explainAnalyze"))
                        .on_click(cx.listener(|this, _, window, cx| this.explain(true, window, cx))),
                )
            })
        };
        let txn_buttons =
            |el: Div, cx: &mut Context<Self>| match self.txn {
                TxnState::Idle if self.connection.read_only => el,
                TxnState::Idle => el.child(
                    Button::new("begin-txn")
                        .debug_selector(|| "begin-txn".into())
                        .tool(Icon::new(Lucide::GitBranch), ICON_SM, t(cx, "editor.beginTxn"))
                        .on_click(
                            cx.listener(|this, _, window, cx| this.transaction(TxnControl::Begin, false, window, cx)),
                        ),
                ),
                state => {
                    let (key, color) = match state {
                        TxnState::Error => ("editor.txnError", theme.danger),
                        _ => ("editor.txnActive", theme.warning),
                    };
                    el.child(
                        h_flex()
                            .gap_1p5()
                            .px_2()
                            .text_size(TEXT_XS)
                            .text_color(color)
                            .child(div().size(px(8.)).rounded_full().bg(color))
                            .child(t(cx, key)),
                    )
                    .child(
                        Button::new("commit-txn")
                            .debug_selector(|| "commit-txn".into())
                            .tool(Icon::new(IconName::Check), ICON_SM, t(cx, "editor.commitTxn"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.transaction(TxnControl::Commit, false, window, cx)
                            })),
                    )
                    .child(
                        Button::new("rollback-txn")
                            .debug_selector(|| "rollback-txn".into())
                            .tool(Icon::new(Lucide::X), ICON_SM, t(cx, "editor.rollbackTxn"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.transaction(TxnControl::Rollback, false, window, cx)
                            })),
                    )
                }
            };
        let row = h_flex()
            .flex_none()
            .h(rems(3.077))
            .px(rems(0.769))
            .gap(rems(0.462))
            .bg(theme.sidebar)
            .border_b_1()
            .border_color(theme.border);
        let row = run_buttons(row, cx)
            .child(
                Button::new("save-query")
                    .debug_selector(|| "save-query".into())
                    .tool(
                        Icon::new(Lucide::Bookmark),
                        ICON_SM,
                        t(cx, if linked { "editor.update" } else { "editor.save" }),
                    )
                    .on_click(cx.listener(|this, _, window, cx| this.save(window, cx))),
            )
            .when(linked, |el| {
                el.child(
                    Button::new("rename-query")
                        .debug_selector(|| "rename-query".into())
                        .tool(Icon::new(Lucide::Pencil), ICON_SM, t(cx, "editor.rename"))
                        .on_click(cx.listener(|this, _, window, cx| this.rename_saved(window, cx))),
                )
            });
        txn_buttons(row, cx)
    }
}

impl EventEmitter<QueryTabEvent> for QueryTab {}

fn editor_menu(focus: FocusHandle) -> impl Fn(NativeMenu, &mut Window, &mut App) -> NativeMenu + 'static {
    move |native, window, cx| {
        let entries = vec![
            Entry::item(t(cx, "editor.contextUndo"), Undo, false),
            Entry::item(t(cx, "editor.contextRedo"), Redo, false),
            Entry::Separator,
            Entry::item(t(cx, "editor.contextCut"), Cut, false),
            Entry::item(t(cx, "editor.contextCopy"), Copy, false),
            Entry::item(t(cx, "editor.contextPaste"), Paste, false),
            Entry::item(t(cx, "editor.contextDelete"), Delete, false),
            Entry::item(t(cx, "editor.contextSelectAll"), SelectAll, false),
            Entry::Separator,
            Entry::item(t(cx, "editor.contextFind"), Search, false),
            Entry::item(t(cx, "editor.contextFindReplace"), Replace, false),
            Entry::item(t(cx, "editor.contextFormat"), FormatQuery, false),
        ];
        context_menu::open(native, entries, &focus, window, cx)
    }
}

impl Render for QueryTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let font_size = px(theme::editor_font_size(cx));
        v_flex()
            .size_full()
            .on_action(cx.listener(|this, _: &RunSelection, window, cx| this.run_selection(window, cx)))
            .on_action(cx.listener(|this, _: &RunAll, window, cx| this.run_all(window, cx)))
            .on_action(cx.listener(|this, _: &ExplainQuery, window, cx| this.explain(false, window, cx)))
            .on_action(cx.listener(|this, _: &ExplainAnalyze, window, cx| this.explain(true, window, cx)))
            .on_action(cx.listener(|this, _: &FormatQuery, window, cx| this.format(window, cx)))
            .on_action(cx.listener(|this, _: &TriggerSuggest, window, cx| this.suggest(window, cx)))
            .on_action(cx.listener(|this, _: &SaveQuery, window, cx| this.save(window, cx)))
            .on_action(cx.listener(|this, _: &RenameSavedQuery, window, cx| this.rename_saved(window, cx)))
            // GPUI Kit's handle drags its value in an Rc.
            .on_drag_move(cx.listener(|_, event: &DragMoveEvent<Rc<SplitDrag>>, _, cx| {
                let bounds = event.bounds;
                if bounds.size.height > px(0.) {
                    let share = (bounds.bottom() - event.event.position.y) / bounds.size.height * 100.;
                    cx.set_global(ResultsShare(share.clamp(RESULTS_SHARE.1, RESULTS_SHARE.2)));
                    cx.notify();
                }
            }))
            .child(self.toolbar(cx))
            .child(
                completion::handlers(editor_commands::handlers(div(), &self.editor, Some("-- ")), &self.completion)
                    .key_context(if self.completion.read(cx).is_open(cx) { COMPLETING_CONTEXT } else { EDITOR_CONTEXT })
                    .debug_selector(|| "query-editor".into())
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(
                        Editor::new(&self.editor)
                            .size_full()
                            .bordered(false)
                            .pl(rems(GLYPH_MARGIN_REM))
                            .text_size(font_size)
                            .context_menu(editor_menu(self.editor.read(cx).focus_handle(cx)))
                            .on_paste({
                                let editor = self.editor.clone();
                                move |item, window, cx| {
                                    let Some(text) = copy::aligned_text(item) else { return false };
                                    editor.update(cx, |editor, cx| editor.replace(text, window, cx));
                                    true
                                }
                            })
                            .scrollbars_on_hover(),
                    )
                    .child(self.run_glyphs(cx))
                    .child(self.completion.clone()),
            )
            .child(
                div()
                    .debug_selector(|| "results-pane".into())
                    .relative()
                    .flex_none()
                    .min_h_0()
                    .h(relative(results_share(cx) / 100.))
                    .child(self.results.clone())
                    .child(resize_handle::<SplitDrag, SplitDrag>("results-splitter", Axis::Vertical).on_drag(
                        SplitDrag,
                        |drag, _, _, cx| {
                            cx.stop_propagation();
                            cx.new(|_| drag.as_ref().clone())
                        },
                    )),
            )
    }
}

// `position` is 1-based and counts characters. One past the end maps to the end, for "at end of input" errors.
fn char_offset(text: &str, position: u32) -> Option<usize> {
    let ix = (position as usize).checked_sub(1)?;
    text.char_indices().map(|(i, _)| i).chain(std::iter::once(text.len())).nth(ix)
}

// Word at or just before the offset, else the single character there.
fn word_range(text: &str, offset: usize) -> Range<usize> {
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let start = text[..offset].char_indices().rev().take_while(|(_, c)| is_word(*c)).last().map_or(offset, |(i, _)| i);
    let end = text[offset..].char_indices().find(|(_, c)| !is_word(*c)).map_or(text.len(), |(i, _)| offset + i);
    if start < end {
        return start..end;
    }
    offset..offset + text[offset..].chars().next().map_or(0, char::len_utf8)
}

impl QueryTab {
    pub(crate) fn completion(&self) -> Entity<Completion<EditorMode>> {
        self.completion.clone()
    }
}

#[cfg(any(test, feature = "snapshot"))]
impl QueryTab {
    pub(crate) fn is_running(&self) -> bool {
        self.running.is_some()
    }

    pub(crate) fn results(&self) -> Entity<ResultsPanel> {
        self.results.clone()
    }

    pub(crate) fn in_transaction(&self) -> bool {
        self.txn != TxnState::Idle
    }
}

#[cfg(test)]
impl QueryTab {
    pub(crate) fn editor(&self) -> Entity<EditorState> {
        self.editor.clone()
    }

    #[cfg(feature = "e2e")]
    pub(crate) fn error_squiggle(&self, cx: &App) -> Option<String> {
        let (range, _) = self.query_error.as_ref()?;
        Some(self.editor.read(cx).value()[range.clone()].to_string())
    }
}

#[cfg(test)]
mod tests;
