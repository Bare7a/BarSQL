use std::rc::Rc;
use std::time::{Duration, Instant};

use barsql_app::{ApplyError, ResultTable, RunEvent, traced_beside_run, traced_select};
use barsql_core::{ConnectionConfig, DriverType, MessageLevel, QueryError, ResultSummary, SqlDialect};
use barsql_db::{Cancel, ResultSet};
use barsql_io::EXPORT_FORMATS;
use barsql_sql::lang::{self, UnknownKind, closest, find_name, unknown_name};
use barsql_sql::{QueryPlan, first_keyword, strip_leading_comments};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt, PopupMenu, PopupMenuItem};
use gpui_kit::component::scroll::{Scrollbar, ScrollbarAxis};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme, Disableable, Icon, Selectable, StyledExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::cell_viewer;
use crate::chart_view::ChartView;
use crate::dialogs;
use crate::form::{self, ToolButton};
use crate::grid::{self, Grid, GridEvent, RowRef};
use crate::i18n::{self, I18n, format_number, t, t_count, t_with};
use crate::list_nav::LIST_INSET;
use crate::message_log::{self, Heading, Line, MessageLog};
use crate::plan_view::PlanView;
use crate::result_edits::{self, ResultEditor};
use crate::schema::{self, ColumnsLoad};
use crate::scrollbars::{HoverScrollbar as _, system_bar};
use crate::shortcuts;
use crate::spinner::Spinner;
use crate::state;
use crate::table_edits::{Outcome, OwnedCell};
use crate::table_tab::referenced_row;
use crate::toast;
use crate::tokens::{ICON_SM, ICON_XS, STRIP, TEXT_MD, TEXT_SM, TEXT_XS, TOP_BAR};

// How long a result's rows wait for its keys before they show without them.
const KEYS_WAIT: Duration = Duration::from_secs(3);

// A cancelled run shows only a message, without error details.
#[derive(Debug, Clone)]
pub struct ShownError {
    pub message: SharedString,
    pub info: Option<QueryError>,
    // Set once a name close to the unknown one turns up.
    pub fix: Option<Fix>,
    scroll: ScrollHandle,
}

// A name an error calls unknown, where it stands in its statement, and the one it likely meant.
#[derive(Debug, Clone, PartialEq)]
pub struct Fix {
    pub statement: String,
    // In characters into the statement, from 1.
    pub position: u32,
    pub name: String,
    pub meant: String,
}

impl ShownError {
    pub fn new(error: QueryError, cx: &App) -> Self {
        let scroll = ScrollHandle::new();
        if error.cancelled {
            return Self { message: t(cx, "dialog.queryCancelled"), info: None, fix: None, scroll };
        }
        let message = if error.bug { t(cx, "errors.bug") } else { error.message.clone().into() };
        Self { message, info: Some(error), fix: None, scroll }
    }

    // Centred in the pane. `action` sits next to the copy button, and `fix` next to the hint.
    #[track_caller]
    pub fn card(&self, action: Option<Button>, fix: Option<Button>, cx: &App) -> Div {
        let theme = cx.theme();
        let danger = theme.danger;
        let info = self.info.as_ref();
        let title = if info.is_some() { t(cx, "errors.queryFailed") } else { t(cx, "errors.generic") };
        let copy_text = self.copy_text(cx);
        let meta = |label: &str, text: &str, color: Hsla| {
            h_flex()
                .items_start()
                .gap(rems(0.462))
                .child(div().flex_none().font_semibold().text_color(color).child(t(cx, label)))
                .child(div().flex_1().min_w_0().text_color(theme.muted_foreground).child(text.to_string()))
        };
        let header = h_flex()
            .gap(rems(0.615))
            .child(Icon::new(Lucide::CircleAlert).size(ICON_SM).text_color(danger))
            .child(div().min_w_0().truncate().text_size(TEXT_MD).font_semibold().text_color(danger).child(title))
            .child(div().flex_1())
            .children(action)
            .child(
                Button::new("copy-error")
                    .ghost()
                    .tool_icon(Icon::new(Lucide::Copy), ICON_XS)
                    .tooltip(t(cx, "common.copy"))
                    .on_click(move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(copy_text.clone()));
                        toast::success(t(cx, "toast.copiedClipboard"), cx);
                    }),
            );
        let (headline, category) = match info {
            Some(info) => split_message(&self.message, &info.code),
            None => (self.message.as_ref(), None),
        };
        // Labelled, so a bare `1` doesn't read as a count.
        let code = info.filter(|i| !i.code.is_empty()).map(|i| t_with(cx, "errors.code", &[("code", &i.code)]));
        let quiet = [category.map(SharedString::from), code].into_iter().flatten().collect::<Vec<_>>().join(" · ");
        // Server text is mono, the app's own messages aren't.
        let message = div()
            .id("error-message")
            .max_h(rems(16.))
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .when(info.is_some_and(|info| !info.bug), |el| el.font_family(theme.mono_font_family.clone()))
            .text_color(theme.foreground)
            .child(SharedString::from(headline.to_string()));
        let card = form::alert(danger)
            .w_full()
            .max_w(rems(40.))
            .flex()
            .flex_col()
            .gap(rems(0.462))
            .child(header)
            .child(div().relative().child(message).hover_scrollbar(&self.scroll, ScrollbarAxis::Vertical))
            .when(!quiet.is_empty(), |el| {
                el.child(
                    div()
                        .debug_selector(|| "error-quiet".into())
                        .text_size(TEXT_XS)
                        .text_color(theme.muted_foreground)
                        .child(quiet),
                )
            })
            .when_some(info.filter(|i| !i.detail.is_empty()), |el, info| {
                el.child(meta("errors.detailLabel", &info.detail, theme.foreground))
            })
            .when_some(info.filter(|i| !i.hint.is_empty()), |el, info| {
                el.child(meta("errors.hintLabel", &info.hint, theme.primary).children(fix))
            });
        div().size_full().flex().items_center().justify_center().p(rems(1.846)).child(card)
    }

    fn copy_text(&self, cx: &App) -> String {
        let info = self.info.as_ref();
        let mut lines = vec![match info.filter(|i| !i.code.is_empty()) {
            Some(info) => format!("{}: {}", info.code, self.message),
            None => self.message.to_string(),
        }];
        if let Some(info) = info {
            for (label, text) in [("errors.detailLabel", &info.detail), ("errors.hintLabel", &info.hint)] {
                if !text.is_empty() {
                    lines.push(format!("{}: {text}", t(cx, label)));
                }
            }
        }
        lines.join("\n")
    }
}

// An error that names an unknown column or table without saying where gets the name's place in the statement, so
// Jump to error and the squiggle find it, as they do for engines that report one.
fn locate_unknown(error: &mut QueryError, statement: &str, driver: Option<&DriverType>) {
    if error.position == 0
        && let Some((_, name)) = unknown_name(&error.message)
        && let Some(position) = find_name(statement, &name, driver)
    {
        error.position = position;
    }
}

// SQLite's messages read "category: message (code)". The message leads, and the category goes under it with the
// code. Other engines' messages are just the message.
fn split_message<'a>(message: &'a str, code: &str) -> (&'a str, Option<&'a str>) {
    let tail = format!(" ({code})");
    match (!code.is_empty()).then(|| message.strip_suffix(tail.as_str())).flatten() {
        Some(body) => match body.split_once(": ") {
            Some((category, rest)) => (rest, Some(category)),
            None => (body, None),
        },
        None => (message, None),
    }
}

#[derive(Default)]
struct ResultSetView {
    grid: Option<Entity<Grid>>,
    // Shown in place of the grid while charting. Built on the first toggle and kept, so its picks survive going
    // back to the grid.
    chart: Option<Entity<ChartView>>,
    charting: bool,
    _subscriptions: Vec<Subscription>,
    streaming: bool,
    rows_in: bool,
    summary: Option<ResultSummary>,
    plan: Option<Entity<PlanView>>,
    error: Option<ShownError>,
    statement: Option<String>,
    // Set once the rows turn out to be one table's, for its keys.
    table: Option<ResultTable>,
    // Also needs the whole primary key and a writable connection.
    editor: Option<ResultEditor>,
    // Rows that may be one table's, looked up once the result shows: most of a long script's never do.
    lookup_due: bool,
    resolving: Option<Lookup>,
}

// Cancelled when its result goes, so the lookup leaves the tab's queue instead of holding up the next run.
struct Lookup {
    _task: Task<()>,
    cancel: Cancel,
    // While set, the rows wait for their keys so the icons don't pop in after them, KEYS_WAIT at most.
    hold: Option<Task<()>>,
}

impl Drop for Lookup {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

impl ResultSetView {
    // An error or a plan takes the grid's place, and what was traced of its rows goes with it.
    fn drop_grid(&mut self) {
        self.grid = None;
        self._subscriptions.clear();
        (self.lookup_due, self.resolving, self.table, self.editor) = (false, None, None, None);
    }

    fn awaits_keys(&self) -> bool {
        self.resolving.as_ref().is_some_and(|lookup| lookup.hold.is_some())
    }

    // A grid shows with its rows and their keys, never as headers alone.
    fn grid_waits(&self) -> bool {
        self.awaits_keys() || self.streaming && !self.rows_in
    }

    fn rows(&self, cx: &App) -> usize {
        self.grid.as_ref().map_or(0, |grid| grid.read(cx).set().rows())
    }

    // Nothing to look at but the summary line.
    fn is_plain(&self) -> bool {
        self.grid.is_none() && self.plan.is_none() && self.error.is_none()
    }

    // Row count for a grid, affected rows for a write, and no badge for errors, plans or DDL.
    fn count(&self, cx: &App) -> Option<i64> {
        if self.error.is_some() || self.plan.is_some() {
            return None;
        }
        let summary = self.summary.as_ref();
        match self.grid {
            Some(_) => Some(summary.map_or(self.rows(cx) as i64, |s| s.row_count)),
            None => {
                let affected = summary.map_or(0, |s| s.affected_rows);
                (affected != 0 || self.statement.as_deref().is_some_and(counts_rows)).then_some(affected)
            }
        }
    }
}

fn statement_tooltip(statement: &str) -> SharedString {
    let line = code_of(statement).split_whitespace().collect::<Vec<_>>().join(" ");
    line.chars().take(120).collect::<String>().into()
}

// Leading comments would fill a preview, so it starts at the code unless there's none.
fn code_of(statement: &str) -> &str {
    let code = strip_leading_comments(statement);
    if code.trim().is_empty() { statement } else { code }
}

// Servers report 0 affected rows for DDL too, so only these show a count.
fn counts_rows(statement: &str) -> bool {
    matches!(
        first_keyword(code_of(statement)).as_str(),
        "INSERT" | "UPDATE" | "DELETE" | "MERGE" | "COPY" | "REPLACE" | "LOAD" | "WITH" | "SELECT"
    )
}

// t_count, with the count grouped as the language writes numbers.
fn counted(cx: &App, key: &str, n: usize, vars: &[(&str, &str)]) -> String {
    t_count(cx, key, n as i64, vars).replace(&n.to_string(), &i18n::count(cx, n))
}

// Grids and plans get separate numbers, like Result 1, Plan 1, Result 2. A failed EXPLAIN has no plan, so
// it counts as a grid.
fn ordinals(plans: impl IntoIterator<Item = bool>) -> Vec<usize> {
    let (mut grids, mut plan_count) = (0, 0);
    plans
        .into_iter()
        .map(|plan| {
            let counter = if plan { &mut plan_count } else { &mut grids };
            *counter += 1;
            *counter
        })
        .collect()
}

pub enum ResultsEvent {
    // `position` counts characters into the statement, starting at 1.
    JumpToError { statement: String, position: u32, message: String },
    // A run failed at a known place, for the editor to underline.
    Located { statement: String, position: u32, message: String },
    // The error card's "Did you mean" button.
    Fix(Fix),
    // Also sent when there is no focused row anymore.
    FocusedRowChanged,
    // Edits to a result's rows ran on the tab's session, which may hold a transaction.
    EditsApplied { failed: bool },
    // A foreign key's jump to the row it references.
    OpenTable { schema: String, table: String, filter: String },
    Stop,
}

#[cfg(test)]
pub enum ResultStatus {
    Empty,
    Streaming,
    Rows { count: i64 },
    Affected { count: i64 },
    Error(SharedString),
}

#[derive(Default)]
pub struct ResultsPanel {
    sets: Vec<ResultSetView>,
    active: usize,
    // The tab's connection's, for the grids' SQL copies and exports.
    dialect: Option<SqlDialect>,
    // The tab's connection, where a name an error calls unknown may have a near one, and where edits go.
    connection: Option<ConnectionConfig>,
    tab_id: String,
    // The Messages tab is showing, in place of the active result.
    messages_open: bool,
    // A tab was picked during this run, so its end leaves the choice alone.
    picked: bool,
    messages: MessageLog,
    message_filter: Option<Entity<InputState>>,
    _message_filter_subscriptions: Vec<Subscription>,
    tab_scroll: ScrollHandle,
    running: Option<Instant>,
}

impl ResultsPanel {
    pub fn set_running(&mut self, started: Option<Instant>, cx: &mut Context<Self>) {
        self.running = started;
        cx.notify();
    }

    pub fn set_dialect(&mut self, dialect: Option<SqlDialect>) {
        self.dialect = dialect;
    }

    // Editors and lookups were for the old target, so a change to how it connects, read-only included, drops them.
    // Anything else leaves the overlays as they are.
    pub fn set_connection(&mut self, connection: ConnectionConfig, cx: &mut Context<Self>) {
        if self.connection.as_ref().is_some_and(|old| old.fingerprint() != connection.fingerprint()) {
            let shown = self.shown_grid().cloned();
            for set in &mut self.sets {
                (set.lookup_due, set.resolving, set.editor) = (false, None, None);
                if let (Some(_), Some(grid)) = (set.table.take(), &set.grid) {
                    grid.update(cx, |grid, cx| grid.clear_overlay(cx));
                }
            }
            self.shown_changed(shown, cx);
        }
        self.connection = Some(connection);
    }

    pub fn set_tab(&mut self, tab_id: String) {
        self.tab_id = tab_id;
    }

    pub fn pending_edits(&self) -> usize {
        self.sets.iter().filter_map(|set| set.editor.as_ref()).map(|editor| editor.counts()).map(|(e, d)| e + d).sum()
    }

    // Without counting them, as it's asked whenever the tab changes.
    pub fn has_pending_edits(&self) -> bool {
        self.sets.iter().filter_map(|set| set.editor.as_ref()).any(ResultEditor::has_pending)
    }

    // A name the error calls unknown may be a typo of one the schema has: a column of the statement's tables, or a
    // table. Their columns may still be loading, so the hint follows the error by a moment.
    fn suggest(&self, index: usize, statement: String, error: &QueryError, cx: &mut Context<Self>) {
        let Some((kind, name)) = unknown_name(&error.message) else { return };
        let Some(id) = self.connection.as_ref().map(|connection| connection.id.as_str()) else { return };
        let Some(catalog) = schema::catalog(cx, id) else { return };
        let (tables, loads): (Vec<String>, Vec<ColumnsLoad>) = match kind {
            UnknownKind::Table => (catalog.tables.iter().map(|table| table.name.clone()).collect(), Vec::new()),
            UnknownKind::Column => {
                let parsed = lang::parse_query(&statement, &catalog);
                let loads = parsed.bindings.values().map(|b| schema::columns(id, &b.schema, &b.table, cx)).collect();
                (Vec::new(), loads)
            }
        };
        cx.spawn(async move |this, cx| {
            let mut candidates = tables;
            for load in loads {
                if let Ok(columns) = load.await {
                    candidates.extend(columns.iter().map(|column| column.name.clone()));
                }
            }
            // Only a different name is a suggestion. The same one means the error lies elsewhere.
            let Some(meant) = closest(&name, candidates.iter().map(String::as_str)).filter(|meant| *meant != name)
            else {
                return;
            };
            let meant = meant.to_string();
            let _ = this.update(cx, |this, cx| {
                let set = this.sets.get_mut(index).filter(|set| set.statement.as_deref() == Some(statement.as_str()));
                let Some(error) = set.and_then(|set| set.error.as_mut()) else { return };
                let Some(info) = error.info.as_mut().filter(|info| info.hint.is_empty()) else { return };
                info.hint = t_with(cx, "errors.didYouMean", &[("name", &meant)]).to_string();
                let position = info.position;
                if position > 0 {
                    error.fix = Some(Fix { statement, position, name, meant });
                }
                cx.notify();
            });
        })
        .detach();
    }

    #[cfg(test)]
    pub(crate) fn shown_error(&self) -> Option<QueryError> {
        self.sets.get(self.active)?.error.as_ref()?.info.clone()
    }

    #[cfg(test)]
    pub(crate) fn shown_fix(&self) -> Option<Fix> {
        self.sets.get(self.active)?.error.as_ref()?.fix.clone()
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.sets.clear();
        self.forget_run();
        cx.emit(ResultsEvent::FocusedRowChanged);
        cx.notify();
    }

    fn show(&mut self, set: ResultSetView, cx: &mut Context<Self>) {
        self.sets = vec![set];
        self.forget_run();
        cx.emit(ResultsEvent::FocusedRowChanged);
        cx.notify();
    }

    // The message filter stays for the next run, as its field still shows it.
    fn forget_run(&mut self) {
        self.active = 0;
        self.messages = self.messages.fresh();
        self.messages_open = false;
        self.picked = false;
    }

    pub(crate) fn select_result(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.active = ix;
        self.resolve_table(ix, cx);
        self.messages_open = false;
        self.picked = true;
        cx.emit(ResultsEvent::FocusedRowChanged);
        cx.notify();
    }

    pub(crate) fn open_messages(&mut self, cx: &mut Context<Self>) {
        self.messages_open = true;
        self.picked = true;
        cx.emit(ResultsEvent::FocusedRowChanged);
        cx.notify();
    }

    pub fn focused_row(&self, cx: &App) -> Option<RowRef> {
        self.shown_grid()?.read(cx).focused_row()
    }

    // The active result's grid, unless the Messages tab covers it or its rows wait for their keys.
    pub(crate) fn shown_grid(&self) -> Option<&Entity<Grid>> {
        self.sets.get(self.active).filter(|set| !self.messages_open && !set.grid_waits())?.grid.as_ref()
    }

    fn is_active(&self, grid: &Entity<Grid>) -> bool {
        self.shown_grid() == Some(grid)
    }

    pub fn show_message(&mut self, message: SharedString, cx: &mut Context<Self>) {
        let summary = ResultSummary { message: message.to_string(), ..Default::default() };
        self.show(ResultSetView { summary: Some(summary), ..Default::default() }, cx);
    }

    pub fn show_error(&mut self, error: QueryError, cx: &mut Context<Self>) {
        let error = ShownError::new(error, cx);
        self.show(ResultSetView { error: Some(error), ..Default::default() }, cx);
    }

    pub fn show_plan(&mut self, plan: QueryPlan, cx: &mut Context<Self>) {
        let plan = cx.new(|cx| PlanView::new(plan, cx));
        self.show(ResultSetView { plan: Some(plan), ..Default::default() }, cx);
    }

    // A batch's later results carry its whole text too, and only its first can be traced.
    fn first_of_statement(&self, index: usize, statement: &str) -> bool {
        let before = index.checked_sub(1).and_then(|prev| self.sets.get(prev));
        before.and_then(|set| set.statement.as_deref()) != Some(statement)
    }

    fn slot(&mut self, index: usize) -> &mut ResultSetView {
        if self.sets.len() <= index {
            self.sets.resize_with(index + 1, Default::default);
        }
        &mut self.sets[index]
    }

    // Returns whether a finished statement failed, for the tab's transaction badge. None for a cancelled
    // statement or any other event.
    pub fn apply(&mut self, event: RunEvent, window: &mut Window, cx: &mut Context<Self>) -> Option<bool> {
        let mut failed = None;
        let before = self.shown_grid().cloned();
        match event {
            RunEvent::Meta { result_index, columns, statement, .. } => {
                let dialect = self.dialect;
                let grid = cx.new(|cx| {
                    let mut grid = Grid::new(columns, cx);
                    grid.set_dialect(dialect);
                    grid
                });
                let subscriptions =
                    vec![cx.observe(&grid, |_, _, cx| cx.notify()), cx.subscribe_in(&grid, window, Self::grid_event)];
                let first = self.first_of_statement(result_index, &statement);
                *self.slot(result_index) = ResultSetView {
                    grid: Some(grid),
                    _subscriptions: subscriptions,
                    streaming: true,
                    statement: Some(statement),
                    lookup_due: first,
                    ..Default::default()
                };
                // While the rows stream, so their keys can show with them.
                if result_index == self.active {
                    self.resolve_table(result_index, cx);
                }
            }
            RunEvent::Rows { result_index, chunk } => {
                if let Some(set) = self.sets.get_mut(result_index).filter(|s| s.streaming)
                    && let Some(grid) = set.grid.clone()
                {
                    set.rows_in |= chunk.rows() > 0;
                    grid.update(cx, |grid, cx| grid.push(chunk, cx));
                }
            }
            RunEvent::Messages { result_index, messages, dropped } => {
                self.messages.add(result_index, messages, dropped)
            }
            RunEvent::Result(result) => {
                let result = *result;
                let driver = self.connection.as_ref().map(|connection| connection.driver.clone());
                let error = result.error.map(|mut error| {
                    failed = (!error.cancelled).then_some(true);
                    locate_unknown(&mut error, &result.statement, driver.as_ref());
                    error
                });
                if let Some(error) = &error {
                    self.suggest(result.result_index, result.statement.clone(), error, cx);
                    if error.position > 0 {
                        let (statement, position) = (result.statement.clone(), error.position);
                        cx.emit(ResultsEvent::Located { statement, position, message: error.message.clone() });
                    }
                }
                let shown = error.map(|error| ShownError::new(error, cx));
                let plan = result.plan.map(|plan| cx.new(|cx| PlanView::new(plan, cx)));
                let index = result.result_index;
                let set = self.slot(index);
                set.streaming = false;
                set.statement = Some(result.statement);
                if let Some(error) = shown {
                    set.drop_grid();
                    set.error = Some(error);
                } else {
                    failed = Some(false);
                    if let Some(plan) = plan {
                        set.drop_grid();
                        set.plan = Some(plan);
                    } else {
                        set.summary = result.summary;
                    }
                }
                if self.active >= self.sets.len() {
                    self.active = 0;
                }
                // All its rows are in, so a table found while they streamed can take edits.
                self.sync_overlay(index, cx);
                if index == self.active {
                    self.resolve_table(index, cx);
                }
            }
            // A batch-level failure, like no session, replaces every result.
            RunEvent::Done { error: Some(error), .. } => {
                let error = ShownError::new(error, cx);
                self.sets = vec![ResultSetView { error: Some(error), ..Default::default() }];
                self.active = 0;
            }
            // A run that only said something opens on what it said, like a DO block's notices. One that starts with
            // setup, like DDL, opens on its first result with more to show than an outcome, or else its last.
            RunEvent::Done { .. } if !self.picked => {
                if self.messages.has_raised() && self.sets.iter().all(ResultSetView::is_plain) {
                    self.messages_open = true;
                } else if self.sets.get(self.active).is_some_and(ResultSetView::is_plain) {
                    let ix = self.sets.iter().position(|set| !set.is_plain()).unwrap_or(self.sets.len() - 1);
                    self.active = ix;
                    self.resolve_table(ix, cx);
                }
            }
            RunEvent::Done { .. } => {}
        }
        self.shown_changed(before, cx);
        failed
    }

    #[cfg(any(test, feature = "snapshot"))]
    pub fn result_count(&self) -> usize {
        self.sets.len()
    }

    #[cfg(test)]
    pub(crate) fn messages_shown(&self) -> bool {
        self.messages_open
    }

    // Each kept message as (level, code, text), for tests.
    #[cfg(test)]
    pub(crate) fn message_texts(&self) -> Vec<(barsql_core::MessageLevel, String, String)> {
        (0..self.messages.rows())
            .filter_map(|ix| match self.messages.row(ix) {
                Some(Line::Text { level: Some(level), code, text }) => {
                    Some((level, code.to_string(), text.to_string()))
                }
                _ => None,
            })
            .collect()
    }

    // Rows streamed so far, while the shown result is still streaming.
    pub fn streamed_rows(&self, cx: &App) -> Option<usize> {
        let set = self.sets.get(self.active)?;
        (set.streaming && set.error.is_none() && set.plan.is_none()).then(|| set.rows(cx))
    }

    #[cfg(test)]
    pub fn status(&self, cx: &App) -> ResultStatus {
        let Some(set) = self.sets.get(self.active) else { return ResultStatus::Empty };
        if let Some(error) = &set.error {
            return ResultStatus::Error(error.message.clone());
        }
        if set.plan.is_some() {
            return ResultStatus::Empty;
        }
        if set.streaming {
            return ResultStatus::Streaming;
        }
        let count = set.count(cx).unwrap_or(0);
        if set.grid.is_some() { ResultStatus::Rows { count } } else { ResultStatus::Affected { count } }
    }

    #[cfg(test)]
    pub fn tab_counts(&self, cx: &App) -> Vec<Option<i64>> {
        self.sets.iter().map(|set| set.count(cx)).collect()
    }

    pub(crate) fn active_grid(&self) -> Option<Entity<Grid>> {
        self.sets.get(self.active).filter(|set| !set.grid_waits())?.grid.clone()
    }

    pub(crate) fn awaits_keys(&self) -> bool {
        self.sets.get(self.active).is_some_and(ResultSetView::awaits_keys)
    }

    #[cfg(any(test, feature = "snapshot"))]
    pub(crate) fn active_plan(&self) -> Option<Entity<PlanView>> {
        self.sets.get(self.active)?.plan.clone()
    }

    #[cfg(test)]
    pub(crate) fn active_error(&self) -> Option<ShownError> {
        self.sets.get(self.active)?.error.clone()
    }

    // GPUI scrolls an x-only overflow sideways with the vertical wheel.
    fn tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let lang = cx.global::<I18n>().lang().to_string();
        let ordinals = ordinals(self.sets.iter().map(|set| set.plan.is_some()));
        let panel = cx.entity().downgrade();
        let mut tabs: Vec<AnyElement> = self
            .sets
            .iter()
            .zip(ordinals)
            .enumerate()
            .map(|(ix, (set, n))| {
                let active = ix == self.active && !self.messages_open;
                let failed = set.error.is_some();
                let key = if set.plan.is_some() { "results.planLabel" } else { "results.resultLabel" };
                let color = match (failed, active) {
                    (true, _) => theme.danger,
                    (false, true) => theme.foreground,
                    (false, false) => theme.muted_foreground,
                };
                let badge = set.count(cx).map(|count| {
                    let color = if active { theme.primary } else { theme.muted_foreground };
                    form::badge(format_number(count as f64, 0, &lang), color)
                });
                let tooltip = set.statement.as_deref().map(statement_tooltip).filter(|tip| !tip.is_empty());
                h_flex()
                    .id(("result-tab", ix))
                    .debug_selector(move || format!("result-tab-{ix}"))
                    .relative()
                    .flex_none()
                    .h_full()
                    .gap(rems(0.462))
                    .pb(rems(0.154))
                    .px(rems(0.769))
                    .border_r_1()
                    .border_color(theme.border)
                    .whitespace_nowrap()
                    .text_size(TEXT_SM)
                    .text_color(color)
                    .cursor_pointer()
                    .when(!failed, |el| el.hover(|style| style.text_color(theme.foreground)))
                    .when(set.plan.is_some(), |el| el.child(Icon::new(Lucide::Route).size(ICON_XS).opacity(0.75)))
                    .child(t_with(cx, key, &[("n", &n.to_string())]))
                    .children(badge)
                    .when(failed, |el| el.child(Icon::new(Lucide::CircleAlert).size(ICON_XS)))
                    .when(active, |el| {
                        let underline = if failed { theme.danger } else { theme.primary };
                        el.child(div().absolute().left_0().right_0().bottom_0().h(rems(0.154)).bg(underline))
                    })
                    .when_some(tooltip, |el, tip| {
                        el.tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                    })
                    .on_click(cx.listener(move |this, _, _, cx| this.select_result(ix, cx)))
                    .context_menu({
                        let panel = panel.clone();
                        move |menu, window, cx| Self::result_menu(&panel, ix, menu, window, cx)
                    })
                    .into_any_element()
            })
            .collect();
        if !self.messages.is_empty() {
            let copy = panel.clone();
            tabs.push(
                self.messages_tab(&lang, cx)
                    .context_menu(move |menu, _, cx| {
                        let copy = copy.clone();
                        menu.item(PopupMenuItem::new(t(cx, "results.copyMessages")).on_click(move |_, _, cx| {
                            let _ = copy.update(cx, |this, cx| this.copy_messages(cx));
                        }))
                    })
                    .into_any_element(),
            );
        }
        div()
            .relative()
            .flex_none()
            .w_full()
            .h(TOP_BAR)
            .child(
                h_flex()
                    .id("result-tabs")
                    .size_full()
                    .border_b_1()
                    .border_color(theme.border)
                    .overflow_x_scroll()
                    .track_scroll(&self.tab_scroll)
                    .children(tabs),
            )
            // Not on hover: its track would take clicks on the tabs' lower half. The mode is set here, so it stays the
            // system's even inside an area that shows its bars on hover.
            .child(div().absolute().inset_0().child(system_bar(Scrollbar::horizontal(&self.tab_scroll), cx)))
    }

    fn messages_tab(&self, lang: &str, cx: &mut Context<Self>) -> Stateful<Div> {
        let theme = cx.theme();
        let active = self.messages_open;
        let color = if active { theme.foreground } else { theme.muted_foreground };
        let badge_color = match (self.messages.has_warnings(), active) {
            (true, _) => theme.warning,
            (false, true) => theme.primary,
            (false, false) => theme.muted_foreground,
        };
        let hover = theme.foreground;
        let underline = theme.primary;
        h_flex()
            .id("result-tab-messages")
            .debug_selector(|| "result-tab-messages".into())
            .relative()
            .flex_none()
            .h_full()
            .gap(rems(0.462))
            .pb(rems(0.154))
            .px(rems(0.769))
            .border_r_1()
            .border_color(theme.border)
            .whitespace_nowrap()
            .text_size(TEXT_SM)
            .text_color(color)
            .cursor_pointer()
            .hover(move |style| style.text_color(hover))
            .child(Icon::new(Lucide::MessageSquareText).size(ICON_XS).opacity(0.75))
            .child(t(cx, "results.messages"))
            .child(form::badge(format_number(self.messages.total() as f64, 0, lang), badge_color))
            .when(active, |el| el.child(div().absolute().left_0().right_0().bottom_0().h(rems(0.154)).bg(underline)))
            .on_click(cx.listener(|this, _, _, cx| this.open_messages(cx)))
    }

    // The statement and outcome a result's messages are listed under.
    fn heading(&self, index: usize, cx: &App) -> Heading {
        let set = self.sets.get(index);
        let n = ordinals(self.sets.iter().map(|set| set.plan.is_some())).get(index).copied().unwrap_or(index + 1);
        let key = if set.is_some_and(|set| set.plan.is_some()) { "results.planLabel" } else { "results.resultLabel" };
        let outcome = match set {
            None => SharedString::default(),
            Some(set) if set.error.is_some() => t(cx, "results.outcomeError"),
            Some(set) if set.plan.is_some() => t(cx, "results.outcomePlan"),
            Some(set) if set.streaming => t(cx, "results.outcomeRunning"),
            Some(set) => match (set.grid.is_some(), set.count(cx)) {
                (true, Some(count)) => t_count(cx, "results.outcomeRows", count, &[]),
                (false, Some(count)) => t_count(cx, "results.outcomeAffected", count, &[]),
                (_, None) => SharedString::default(),
            },
        };
        Heading {
            label: t_with(cx, key, &[("n", &n.to_string())]),
            statement: set.and_then(|set| set.statement.as_deref()).map(statement_tooltip).unwrap_or_default(),
            outcome,
            failed: set.is_some_and(|set| set.error.is_some()),
        }
    }

    fn render_message_rows(
        &mut self,
        range: std::ops::Range<usize>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let hover = cx.theme().sidebar_accent;
        range
            .filter_map(|ix| {
                let line = self.messages.row(ix)?;
                let (heading, opens) = match line {
                    Line::Result(index) => (Some(self.heading(index, cx)), (index < self.sets.len()).then_some(index)),
                    _ => (None, None),
                };
                let text = match &line {
                    Line::Text { text, .. } | Line::Labeled { text, .. } => Some(text.to_string()),
                    _ => None,
                };
                let row = message_log::render_row(ix, line, heading, self.messages.has_codes(), cx);
                if let Some(index) = opens {
                    return Some(
                        div()
                            .id(("message-heading", ix))
                            .debug_selector(move || format!("message-heading-{ix}"))
                            .w_full()
                            .cursor_pointer()
                            .hover(move |style| style.bg(hover))
                            .child(row)
                            .on_click(cx.listener(move |this, _, _, cx| this.select_result(index, cx)))
                            .into_any_element(),
                    );
                }
                let Some(text) = text else { return Some(row) };
                let panel = cx.entity().downgrade();
                Some(
                    div()
                        .id(("message-row", ix))
                        .w_full()
                        .child(row)
                        .context_menu(move |menu, _, cx| {
                            let (text, panel) = (text.clone(), panel.clone());
                            menu.item(
                                PopupMenuItem::new(t(cx, "results.copyMessage"))
                                    .on_click(move |_, _, cx| copy_text(text.clone(), cx)),
                            )
                            .item(
                                PopupMenuItem::new(t(cx, "results.copyMessages")).on_click(move |_, _, cx| {
                                    let _ = panel.update(cx, |this, cx| this.copy_messages(cx));
                                }),
                            )
                        })
                        .into_any_element(),
                )
            })
            .collect()
    }

    fn copy_messages(&mut self, cx: &mut Context<Self>) {
        let text = self.messages.copy_text(
            |index| {
                let heading = self.heading(index, cx);
                match heading.statement.is_empty() {
                    true => heading.label.to_string(),
                    false => format!("{}: {}", heading.label, heading.statement),
                }
            },
            cx,
        );
        copy_text(text, cx);
    }

    // On a result tab: what it shows, and the statement it came from.
    fn result_menu(
        this: &WeakEntity<Self>,
        ix: usize,
        menu: PopupMenu,
        window: &mut Window,
        cx: &mut Context<PopupMenu>,
    ) -> PopupMenu {
        let Some(panel) = this.upgrade() else { return menu };
        let Some(set) = panel.read(cx).sets.get(ix) else { return menu };
        let (grid, plan) = (set.grid.clone(), set.plan.clone());
        let error = set.error.as_ref().map(|error| error.message.to_string());
        let statement = set.statement.clone().filter(|statement| !statement.trim().is_empty());
        let mut menu = menu;
        if let Some(grid) = grid {
            let (all, export, formats) = (grid.clone(), grid.clone(), grid);
            menu = menu
                .item(PopupMenuItem::new(t(cx, "results.copyAllRows")).on_click(move |_, window, cx| {
                    all.update(cx, |grid, cx| grid.copy_all(grid::copy_format(cx), window, cx));
                }))
                .submenu(t(cx, "grid.copyAs"), window, cx, move |menu, _, cx| {
                    EXPORT_FORMATS.into_iter().fold(menu, |menu, format| {
                        let grid = formats.clone();
                        menu.item(PopupMenuItem::new(grid::format_label(format, cx)).on_click(move |_, window, cx| {
                            grid.update(cx, |grid, cx| grid.copy_all(format, window, cx));
                        }))
                    })
                })
                .item(PopupMenuItem::new(t(cx, "results.contextExportAs")).on_click(move |_, window, cx| {
                    crate::export_dialog::open(export.read(cx).export_source(), window, cx);
                }));
        }
        if let Some(plan) = plan {
            menu = menu.item(
                PopupMenuItem::new(t(cx, "results.planCopy"))
                    .on_click(move |_, _, cx| plan.update(cx, |plan, cx| plan.copy_raw(cx))),
            );
        }
        if let Some(error) = error {
            menu = menu.item(PopupMenuItem::new(t(cx, "results.copyError")).on_click(move |_, _, cx| {
                copy_text(error.clone(), cx);
            }));
        }
        match statement {
            Some(statement) => menu.separator().item(
                PopupMenuItem::new(t(cx, "results.copyStatement"))
                    .on_click(move |_, _, cx| copy_text(statement.clone(), cx)),
            ),
            None => menu,
        }
    }

    // Made when the tab first shows, as it needs a window.
    fn message_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        if let Some(filter) = &self.message_filter {
            return filter.clone();
        }
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder(t(cx, "results.filterMessages")));
        self._message_filter_subscriptions = vec![
            cx.subscribe_in(&filter, window, |this, filter, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.messages.filter_by_text(&filter.read(cx).value());
                    cx.notify();
                }
            }),
            cx.observe_global_in::<I18n>(window, |this, window, cx| {
                if let Some(filter) = this.message_filter.clone() {
                    filter.update(cx, |state, cx| state.set_placeholder(t(cx, "results.filterMessages"), window, cx));
                }
            }),
        ];
        self.message_filter = Some(filter.clone());
        filter
    }

    // A level's toggle shows once the log has two levels, or while it's picked.
    fn level_toggles(&self, cx: &mut Context<Self>) -> Option<Div> {
        let picked = self.messages.level_filter();
        if self.messages.level_kinds() < 2 && picked.is_none() {
            return None;
        }
        let levels = [
            (None, "message-level-all", "results.filterAll"),
            (Some(MessageLevel::Warning), "message-level-warning", "results.filterWarnings"),
            (Some(MessageLevel::Notice), "message-level-notice", "results.filterNotices"),
            (Some(MessageLevel::Info), "message-level-info", "results.filterInfo"),
        ];
        let toggles = levels.into_iter().filter_map(|(level, id, key)| {
            let n = level.map_or(self.messages.kept(), |level| self.messages.count(level));
            (level.is_none() || n > 0 || level == picked).then(|| {
                form::filter_toggle(id, format!("{} {}", t(cx, key), i18n::count(cx, n)), level == picked)
                    .debug_selector(move || id.into())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.messages.filter_by_level(level);
                        cx.notify();
                    }))
            })
        });
        Some(form::toggle_group().flex_none().children(toggles))
    }

    fn messages_view(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let filter = self.message_filter(window, cx);
        let theme = cx.theme().clone();
        let total = self.messages.total();
        let count = match self.messages.matched() {
            Some(matched) => counted(cx, "results.messagesMatched", total, &[("matched", &i18n::count(cx, matched))]),
            None => counted(cx, "results.messagesCount", total, &[]),
        };
        let warnings = self.messages.count(MessageLevel::Warning);
        let warned = (warnings > 0 && self.messages.matched().is_none()).then(|| {
            let text = counted(cx, "results.messagesWarnings", warnings, &[]);
            div().text_color(theme.warning).child(format!(" · {text}"))
        });
        let strip = h_flex()
            .flex_none()
            .h(STRIP)
            .px(rems(0.769))
            .gap(rems(0.615))
            .justify_between()
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .debug_selector(|| "messages-filter".into())
                    .flex_1()
                    .min_w_0()
                    .max_w(rems(24.))
                    .child(form::search_input(&filter, window, cx).cleanable(true)),
            )
            .children(self.level_toggles(cx));
        let copy = cx.listener(|this, _, _, cx| this.copy_messages(cx));
        // Its icon sits under the filter's, as a table view's row count does.
        let toolbar = h_flex()
            .flex_none()
            .px(rems(0.769))
            .py(rems(0.462))
            .gap(rems(0.615))
            .justify_between()
            .border_b_1()
            .border_color(theme.border)
            .text_size(TEXT_SM)
            .text_color(theme.muted_foreground)
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .pl(form::ICON_INSET)
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(
                        Icon::new(Lucide::MessageSquareText)
                            .size(ICON_SM)
                            .text_color(theme.muted_foreground)
                            .mr(form::ICON_GAP),
                    )
                    .child(div().debug_selector(|| "messages-count".into()).child(count))
                    .children(warned),
            )
            .child(
                Button::new("copy-messages")
                    .debug_selector(|| "copy-messages".into())
                    .tool(Icon::new(Lucide::Copy), ICON_XS, t(cx, "common.copy"))
                    .tooltip(t(cx, "results.copyMessages"))
                    .on_click(copy),
            );
        let area = div().relative().flex_1().min_h_0();
        let body = match self.messages.rows() {
            0 => area.child(form::empty_state(None, t(cx, "results.noMessagesMatch"), cx)).into_any_element(),
            rows => {
                let scroll = self.messages.scroll.clone();
                let list = uniform_list("messages", rows, cx.processor(Self::render_message_rows))
                    .debug_selector(|| "messages".into())
                    .track_scroll(&scroll)
                    .size_full()
                    .pl(rems(0.769))
                    .pr(LIST_INSET)
                    .pb(LIST_INSET);
                area.child(list).hover_scrollbar(&scroll, ScrollbarAxis::Vertical).into_any_element()
            }
        };
        v_flex().size_full().child(strip).child(toolbar).child(body).into_any_element()
    }

    // Built like the grid's toolbar, with the icon where its row count's is, so switching tabs moves nothing.
    fn header(&self, icon: Lucide, text: SharedString, cx: &App) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        h_flex()
            .flex_none()
            .px(rems(0.769))
            .py(rems(0.462))
            .border_b_1()
            .border_color(cx.theme().border)
            .text_size(TEXT_SM)
            .text_color(muted)
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .h(rems(form::TOOL_REM))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(Icon::new(icon).size(ICON_SM).text_color(muted).mr(form::ICON_GAP))
                    .child(div().debug_selector(|| "results-outcome".into()).child(text)),
            )
    }

    // What a result without rows did, over its statement. The app's own messages, like a typed BEGIN's, have none.
    fn outcome(&self, set: &ResultSetView, summary: &ResultSummary, cx: &App) -> AnyElement {
        let theme = cx.theme();
        let ms = summary.duration_ms.to_string();
        let (icon, text) = match (set.count(cx), set.statement.as_deref()) {
            (Some(count), _) => (Lucide::Rows3, t_count(cx, "results.metaAffected", count, &[("ms", &ms)])),
            (None, Some(_)) => (Lucide::Check, t_with(cx, "results.metaDone", &[("ms", &ms)])),
            (None, None) => (Lucide::Check, summary.message.clone().into()),
        };
        let statement = set.statement.as_deref().map(|statement| {
            div()
                .min_w_0()
                .truncate()
                .font_family(theme.mono_font_family.clone())
                .text_size(TEXT_SM)
                .text_color(theme.muted_foreground)
                .child(statement_tooltip(statement))
        });
        v_flex()
            .size_full()
            .child(self.header(icon, text, cx))
            .child(div().flex_1().min_h_0().flex().items_center().justify_center().p(rems(1.231)).children(statement))
            .into_any_element()
    }

    fn running_state(&self, started: Instant, cx: &mut Context<Self>) -> AnyElement {
        let seconds = format!("{:.1}", started.elapsed().as_secs_f64());
        v_flex()
            .debug_selector(|| "results-running".into())
            .size_full()
            .items_center()
            .justify_center()
            .gap(rems(0.923))
            .p(rems(1.231))
            .text_color(cx.theme().muted_foreground)
            .child(h_flex().gap(rems(0.462)).child(Spinner::new(ICON_SM)).child(t_with(
                cx,
                "app.statusRunning",
                &[("seconds", &seconds)],
            )))
            .child(
                Button::new("results-stop")
                    .debug_selector(|| "results-stop".into())
                    .tool(Icon::new(Lucide::Square), ICON_XS, t(cx, "editor.stop"))
                    .tooltip(t(cx, "tooltip.stopQuery"))
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(ResultsEvent::Stop))),
            )
            .into_any_element()
    }

    fn empty(&self, set: Option<&ResultSetView>, cx: &mut Context<Self>) -> AnyElement {
        if let Some((set, summary)) = set.and_then(|set| Some((set, set.summary.as_ref()?))) {
            return self.outcome(set, summary, cx);
        }
        if let Some(started) = self.running {
            return self.running_state(started, cx);
        }
        // Nothing has run yet. Instead of a header saying so, the shortcuts that run something.
        let hint = |id: &str, key: &str| (shortcuts::shown(id, cx), t(cx, key));
        v_flex()
            .debug_selector(|| "results-empty".into())
            .size_full()
            .items_center()
            .justify_center()
            .gap(rems(0.923))
            .p(rems(1.231))
            .text_color(cx.theme().muted_foreground)
            .child(t(cx, "results.runQueryHint"))
            .child(form::shortcut_hints(
                [
                    hint("runSelection", "shortcuts.items.runSelection"),
                    hint("runAll", "shortcuts.items.runAll"),
                    hint("explainQuery", "shortcuts.items.explainQuery"),
                ],
                cx,
            ))
            .into_any_element()
    }

    fn error(&self, error: &ShownError, statement: Option<&str>, cx: &mut Context<Self>) -> AnyElement {
        let jump = match (&error.info, statement) {
            (Some(info), Some(statement)) if info.position > 0 => {
                Some((statement.to_string(), info.position, info.message.clone()))
            }
            _ => None,
        };
        let jump_button = jump.map(|(statement, position, message)| {
            Button::new("jump-to-error")
                .danger()
                .outline()
                .debug_selector(|| "jump-to-error".into())
                .tool(Icon::new(Lucide::Crosshair), ICON_XS, t(cx, "errors.jumpToError"))
                .on_click(cx.listener(move |_, _, _, cx| {
                    let (statement, message) = (statement.clone(), message.clone());
                    cx.emit(ResultsEvent::JumpToError { statement, position, message });
                }))
        });
        let fix_button = error.fix.clone().map(|fix| {
            Button::new("use-meant-name")
                .debug_selector(|| "use-meant-name".into())
                .outline()
                .tool_label(t_with(cx, "errors.useName", &[("name", &fix.meant)]))
                .on_click(cx.listener(move |_, _, _, cx| cx.emit(ResultsEvent::Fix(fix.clone()))))
        });
        error.card(jump_button, fix_button, cx).into_any_element()
    }

    // For the snapshot hook.
    pub fn grid_focus(&self, cx: &App) -> Option<FocusHandle> {
        Some(self.sets.get(self.active)?.grid.as_ref()?.focus_handle(cx))
    }

    fn grid_event(&mut self, grid: &Entity<Grid>, event: &GridEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            GridEvent::ViewCell { row, column } => {
                let grid = grid.read(cx);
                let meta = &grid.set().columns[*column];
                let (name, type_name) = (meta.name.clone(), meta.type_name.clone());
                let value = grid.set().display(*row, *column).map(str::to_string);
                cell_viewer::open(name, type_name, value, window, cx);
            }
            GridEvent::Export => crate::export_dialog::open(grid.read(cx).export_source(), window, cx),
            GridEvent::FocusedRowChanged if self.is_active(grid) => cx.emit(ResultsEvent::FocusedRowChanged),
            GridEvent::EditCell { row, column, value } => {
                let cell = [(*row, *column, value.clone())];
                self.stage(grid, |editor, set| editor.stage(set, &cell), cx);
            }
            GridEvent::PasteCells(cells) => self.stage(grid, |editor, set| editor.stage(set, cells), cx),
            GridEvent::ToggleDelete(row) => self.stage(grid, |editor, set| editor.toggle_deletes(set, &[*row]), cx),
            GridEvent::DeleteRows(rows) => self.stage(grid, |editor, set| editor.toggle_deletes(set, rows), cx),
            GridEvent::Undo => self.stage(grid, |editor, _| changed(editor.undo()), cx),
            GridEvent::Redo => self.stage(grid, |editor, _| changed(editor.redo()), cx),
            GridEvent::Apply => {
                if let Some(index) = self.index_of(grid) {
                    self.apply_edits(index, window, cx);
                }
            }
            GridEvent::EditInViewer { row, column } => {
                let state = grid.read(cx);
                let meta = &state.set().columns[*column];
                let (name, type_name) = (meta.name.clone(), meta.type_name.clone());
                let value = state.shown(*row, *column).display().map(str::to_string);
                let (grid, row, column) = (grid.downgrade(), *row, *column);
                let on_save: cell_viewer::OnSave = Rc::new(move |value, _, cx| {
                    grid.update(cx, |_, cx| cx.emit(GridEvent::EditCell { row, column, value })).ok();
                });
                cell_viewer::open_editor(name, type_name, value, on_save, window, cx);
            }
            GridEvent::OpenForeignKey { row, column } => self.open_foreign_key(grid, *row, *column, window, cx),
            _ => {}
        }
    }

    fn index_of(&self, grid: &Entity<Grid>) -> Option<usize> {
        self.sets.iter().position(|set| set.grid.as_ref() == Some(grid))
    }

    fn editable(&self) -> bool {
        self.connection.as_ref().is_some_and(|connection| !connection.read_only)
    }

    // On a read-only connection too, for the keys and foreign-key jumps.
    fn resolve_table(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(connection) = self.connection.clone() else { return };
        let tab = self.tab_id.clone();
        let Some(set) = self.sets.get_mut(index).filter(|set| set.lookup_due) else { return };
        let (Some(grid), Some(statement)) = (set.grid.clone(), set.statement.clone()) else { return };
        set.lookup_due = false;
        if traced_select(&connection.driver, &statement).is_none() {
            return;
        }
        let columns = grid.read(cx).set().columns.clone();
        let (bar, id, cancel) = (state::bar(cx), connection.id.clone(), Cancel::new());
        let lookup = state::spawn(cx, {
            let cancel = cancel.clone();
            async move { bar.result_table(&id, &tab, &statement, &columns, &cancel).await }
        });
        let found = grid.clone();
        let task = cx.spawn(async move |this, cx| {
            let table = lookup.await;
            let _ = this.update(cx, |this, cx| {
                if let Some(Ok(Some(table))) = table {
                    this.install_table(index, &found, table, cx);
                }
                this.show_rows(index, &found, cx);
            });
        });
        // A lookup that must wait for the run to end would hold the rows for all of it.
        let hold = traced_beside_run(&connection.driver).then(|| {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(KEYS_WAIT).await;
                let _ = this.update(cx, |this, cx| this.show_rows(index, &grid, cx));
            })
        });
        set.resolving = Some(Lookup { _task: task, cancel, hold });
    }

    fn show_rows(&mut self, index: usize, grid: &Entity<Grid>, cx: &mut Context<Self>) {
        let shown = self.shown_grid().cloned();
        let set = self.sets.get_mut(index).filter(|set| set.grid.as_ref() == Some(grid));
        if set.and_then(|set| set.resolving.as_mut()).and_then(|lookup| lookup.hold.take()).is_some() {
            self.shown_changed(shown, cx);
        }
    }

    // The row viewer follows the grid that shows.
    fn shown_changed(&self, before: Option<Entity<Grid>>, cx: &mut Context<Self>) {
        if self.shown_grid().cloned() != before {
            cx.emit(ResultsEvent::FocusedRowChanged);
        }
        cx.notify();
    }

    fn install_table(&mut self, index: usize, grid: &Entity<Grid>, table: ResultTable, cx: &mut Context<Self>) {
        let writable = self.editable();
        let Some(set) = self.sets.get_mut(index).filter(|set| set.grid.as_ref() == Some(grid)) else { return };
        set.editor = writable.then(|| ResultEditor::new(table.clone(), grid.read(cx).set())).flatten();
        set.table = Some(table);
        self.sync_overlay(index, cx);
    }

    // Edits wait for the last row.
    fn sync_overlay(&mut self, index: usize, cx: &mut Context<Self>) {
        let editable = self.editable();
        let Some(set) = self.sets.get(index) else { return };
        let (Some(grid), Some(table)) = (&set.grid, &set.table) else { return };
        let rows = grid.read(cx).set();
        let overlay = match &set.editor {
            Some(editor) => editor.overlay(rows, editable && !editor.applying && !set.streaming),
            None => result_edits::table_overlay(table, rows),
        };
        grid.update(cx, |grid, cx| grid.set_overlay(overlay, cx));
        cx.notify();
    }

    fn open_foreign_key(
        &mut self,
        grid: &Entity<Grid>,
        row: usize,
        column: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(connection) = self.connection.clone() else { return };
        let Some(table) = self.index_of(grid).and_then(|index| self.sets[index].table.as_ref()) else { return };
        let Some(info) = result_edits::column_info(table, column) else { return };
        let (schema, target) = (table.schema.clone(), info.foreign_table.clone());
        let value = OwnedCell::new(grid.read(cx).shown(row, column));
        let filter = referenced_row(&connection, &schema, &target, &info.foreign_column, value, cx);
        cx.spawn_in(window, async move |this, cx| {
            let filter = filter.await;
            this.update(cx, |_, cx| match filter {
                Some(filter) => cx.emit(ResultsEvent::OpenTable { schema, table: target, filter }),
                None => toast::error(t(cx, "tableView.foreignKeyUnresolved"), cx),
            })
            .ok();
        })
        .detach();
    }

    fn stage(
        &mut self,
        grid: &Entity<Grid>,
        edit: impl FnOnce(&mut ResultEditor, &ResultSet) -> Outcome,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.index_of(grid).filter(|&index| self.editable() && !self.sets[index].streaming) else {
            return;
        };
        let rows = grid.read(cx).set().clone();
        let Some(editor) = self.sets[index].editor.as_mut().filter(|editor| !editor.applying) else { return };
        if edit(editor, &rows) == Outcome::Ambiguous {
            toast::error(t(cx, "tableView.ambiguousRow"), cx);
        }
        let editor = &*editor;
        grid.update(cx, |grid, cx| grid.patch_overlay(|overlay| editor.patch(overlay), cx));
        cx.notify();
    }

    fn revert_edits(&mut self, cx: &mut Context<Self>) {
        let index = self.active;
        if let Some(editor) = self.sets.get_mut(index).and_then(|set| set.editor.as_mut()).filter(|e| !e.applying) {
            editor.revert();
        }
        self.sync_overlay(index, cx);
    }

    // A connection that asks before changes gets a confirmation first.
    fn apply_edits(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(connection) = self.connection.clone().filter(|connection| !connection.read_only) else { return };
        let Some(editor) = self.sets.get(index).and_then(|set| set.editor.as_ref()) else { return };
        if !editor.has_pending() || editor.applying {
            return;
        }
        if !connection.confirm_changes {
            return self.apply_now(index, window, cx);
        }
        let (edits, deletes) = editor.counts();
        let confirm = dialogs::Confirm::save_rows(&connection, edits + deletes, cx);
        let panel = cx.entity().downgrade();
        dialogs::confirm(confirm, window, cx, move |window, cx| {
            let _ = panel.update(cx, |panel, cx| panel.apply_now(index, window, cx));
        });
    }

    fn apply_now(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(connection) = self.connection.clone().filter(|connection| !connection.read_only) else { return };
        let tab = self.tab_id.clone();
        let Some(set) = self.sets.get_mut(index) else { return };
        let (Some(grid), Some(editor)) = (set.grid.clone(), set.editor.as_mut()) else { return };
        if !editor.has_pending() || editor.applying {
            return;
        }
        editor.applying = true;
        let edits = editor.edits();
        let bar = state::bar(cx);
        let work = state::spawn(cx, async move { bar.apply_result_edits(&connection.id, &tab, &edits).await });
        cx.spawn_in(window, async move |this, cx| {
            let result = work.await;
            let _ = this.update_in(cx, |this, window, cx| this.finish_apply(index, &grid, result, window, cx));
        })
        .detach();
        self.sync_overlay(index, cx);
    }

    // A failure keeps the edits staged: outside the tab's transaction, none landed.
    fn finish_apply(
        &mut self,
        index: usize,
        grid: &Entity<Grid>,
        result: Option<Result<i64, ApplyError>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let dialect = self.dialect;
        if let Some(set) = self.sets.get_mut(index).filter(|set| set.grid.as_ref() == Some(grid))
            && let Some(editor) = set.editor.as_mut()
        {
            editor.applying = false;
            if let Some(Ok(_)) = &result {
                let (applied, removed) = editor.apply_to(grid.read(cx).set(), dialect);
                if let Some(summary) = set.summary.as_mut() {
                    summary.row_count = applied.rows() as i64;
                }
                grid.update(cx, |grid, cx| grid.replace_set(applied, &removed, cx));
            }
        }
        // A refusal ran nothing, so the tab's transaction is as it was.
        let failed = match &result {
            Some(Ok(_)) => Some(false),
            Some(Err(error)) => error.ran.then_some(true),
            None => None,
        };
        match result {
            Some(Ok(missing)) if missing > 0 => toast::info(t_count(cx, "tableView.deletesNotFound", missing, &[]), cx),
            Some(Err(ApplyError { error, .. })) => {
                dialogs::alert(t(cx, "errors.generic"), error.message.into(), window, cx)
            }
            _ => {}
        }
        self.sync_overlay(index, cx);
        if let Some(failed) = failed {
            cx.emit(ResultsEvent::EditsApplied { failed });
        }
    }

    fn edit_bar(&self, editor: &ResultEditor, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (edits, deletes) = editor.counts();
        let busy = editor.applying;
        let actions = h_flex()
            .flex_none()
            .gap(rems(0.462))
            .child(
                Button::new("result-revert")
                    .debug_selector(|| "result-revert".into())
                    .tool_label(t(cx, "structure.revert"))
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, _, cx| this.revert_edits(cx))),
            )
            .child(
                Button::new("result-apply")
                    .debug_selector(|| "result-apply".into())
                    .primary()
                    .tool_label(t(cx, "tableView.apply"))
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, window, cx| this.apply_edits(this.active, window, cx))),
            )
            .when(edits > 0, |el| {
                let text = t_with(cx, "tableView.pendingUpdates", &[("count", &edits.to_string())]);
                el.child(form::badge(text, theme.warning))
            })
            .when(deletes > 0, |el| {
                let text = t_with(cx, "tableView.pendingDeletes", &[("count", &deletes.to_string())]);
                el.child(form::badge(text, theme.danger))
            });
        h_flex()
            .flex_none()
            .justify_between()
            .px(rems(0.769))
            .py(rems(0.615))
            .gap(rems(0.923))
            .border_t_1()
            .border_color(theme.border)
            .child(actions)
            .child(div().min_w_0().truncate().text_size(TEXT_SM).text_color(theme.muted_foreground).child(t_with(
                cx,
                "results.editsTo",
                &[("table", editor.table())],
            )))
    }

    fn grid(&self, set: &ResultSetView, grid: &Entity<Grid>, cx: &mut Context<Self>) -> AnyElement {
        let rows =
            set.summary.as_ref().filter(|_| !set.streaming).map_or(grid.read(cx).set().rows() as i64, |s| s.row_count);
        let ms = set.summary.as_ref().map_or(0, |s| s.duration_ms);
        let meta = t_count(cx, "results.metaRows", rows, &[("ms", &ms.to_string())]);
        let toggle = Button::new("chart-toggle")
            .debug_selector(|| "chart-toggle".into())
            .tool(Icon::new(Lucide::ChartColumn), ICON_XS, t(cx, "chart.title"))
            .selected(set.charting)
            .tooltip(t(cx, "chart.tooltip"))
            .on_click(cx.listener(|this, _, window, cx| this.toggle_chart(window, cx)))
            .into_any_element();
        let body = match &set.chart {
            Some(chart) if set.charting => chart.clone().into_any_element(),
            _ => grid.clone().into_any_element(),
        };
        let edit_bar = set.editor.as_ref().filter(|editor| editor.has_pending() || editor.applying);
        v_flex()
            .size_full()
            .child(grid::toolbar(grid, meta, None, Some(toggle), !set.charting, grid::CountAlign::Edge, cx))
            .child(div().flex_1().min_h_0().child(body))
            .children(edit_bar.map(|editor| self.edit_bar(editor, cx)))
            .into_any_element()
    }

    // Swaps the active result's grid for a chart of it, and back.
    pub(crate) fn toggle_chart(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(set) = self.sets.get_mut(self.active) else { return };
        let Some(grid) = set.grid.clone() else { return };
        set.charting = !set.charting;
        if set.charting && set.chart.is_none() {
            set.chart = Some(cx.new(|cx| ChartView::new(grid, window, cx)));
        }
        cx.notify();
    }

    #[cfg(test)]
    pub(crate) fn editing_table(&self) -> Option<String> {
        Some(self.sets.get(self.active)?.editor.as_ref()?.table().to_string())
    }

    #[cfg(any(test, feature = "snapshot"))]
    pub(crate) fn chart(&self) -> Option<Entity<ChartView>> {
        self.sets.get(self.active).filter(|set| set.charting)?.chart.clone()
    }
}

impl EventEmitter<ResultsEvent> for ResultsPanel {}

impl Render for ResultsPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match self.sets.get(self.active) {
            _ if self.messages_open => self.messages_view(window, cx),
            None => self.empty(None, cx),
            Some(set) => match (&set.error, &set.plan, &set.grid) {
                (Some(error), ..) => self.error(error, set.statement.as_deref(), cx),
                (None, Some(plan), _) => plan.clone().into_any_element(),
                // As before any result, while the tab's progress line says more is coming.
                (None, None, Some(_)) if set.grid_waits() => self.empty(None, cx),
                (None, None, Some(grid)) => self.grid(set, grid, cx),
                (None, None, None) => self.empty(Some(set), cx),
            },
        };
        // Panel colour behind the tabs, the grid toolbar and everything around the grid.
        v_flex()
            .size_full()
            .bg(cx.theme().sidebar)
            .when(self.sets.len() > 1 || !self.messages.is_empty(), |el| el.child(self.tabs(cx)))
            .child(div().flex_1().min_h_0().child(body))
    }
}

#[cfg(test)]
mod label_tests {
    use super::{counts_rows, ordinals, split_message, statement_tooltip};

    #[test]
    fn previews_start_at_the_code_and_only_writes_count_rows() {
        assert_eq!(statement_tooltip("-- why\n/* more */\nALTER TABLE t\n  ADD c int"), "ALTER TABLE t ADD c int");
        assert_eq!(statement_tooltip("-- only a note"), "-- only a note");
        assert!(counts_rows("-- backfill\nUPDATE t SET a = 1"));
        assert!(counts_rows("WITH gone AS (DELETE FROM t RETURNING *) SELECT 1"));
        assert!(!counts_rows("ALTER TABLE t ADD c int"));
        assert!(!counts_rows("DO $$ BEGIN END $$"));
    }

    #[test]
    fn grids_and_plans_are_numbered_apart() {
        assert_eq!(ordinals([false, true, false, true]), [1, 1, 2, 2]);
        assert_eq!(ordinals([true, false]), [1, 1]);
        assert!(ordinals([]).is_empty());
    }

    // SQLite's "category: message (code)" leads with its message. The category goes under it, with the code.
    #[test]
    fn an_error_leads_with_what_went_wrong() {
        let sqlite = "SQL logic error: no such column: usernme (1)";
        assert_eq!(split_message(sqlite, "1"), ("no such column: usernme", Some("SQL logic error")));
        let unique = "constraint failed: UNIQUE constraint failed: t.id (2067)";
        assert_eq!(split_message(unique, "2067"), ("UNIQUE constraint failed: t.id", Some("constraint failed")));
        let postgres = "column \"usernme\" does not exist";
        assert_eq!(split_message(postgres, "42703"), (postgres, None), "the code isn't in it");
        assert_eq!(split_message("no such table (12)", "1"), ("no such table (12)", None), "another number");
        assert_eq!(split_message("Query cancelled.", ""), ("Query cancelled.", None));
    }
}

#[cfg(test)]
mod messages_tests {
    use std::sync::Arc;

    use barsql_app::{RunEvent, RunResult};
    use barsql_core::{MessageLevel, ResultSummary, ServerMessage};
    use barsql_db::{ChunkBuilder, ColumnMeta};
    use gpui_kit::component::Root;
    use gpui_kit::{AppContext as _, Entity, Modifiers, Pixels, TestAppContext, VisualTestContext, px};

    use super::ResultsPanel;
    use crate::form::{ICON_GAP, ICON_INSET};
    use crate::test_support::Env;
    use crate::tokens::ICON_SM;

    pub(super) fn panel(cx: &mut TestAppContext) -> (Entity<ResultsPanel>, &mut VisualTestContext) {
        let mut panel = None;
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|_| ResultsPanel::default());
            panel = Some(view.clone());
            Root::new(view, window, cx)
        });
        let cx = VisualTestContext::from_window(window.into(), cx).into_mut();
        (panel.unwrap(), cx)
    }

    pub(super) fn apply(panel: &Entity<ResultsPanel>, events: Vec<RunEvent>, cx: &mut VisualTestContext) {
        for event in events {
            panel.update_in(cx, |panel, window, cx| panel.apply(event, window, cx));
        }
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
    }

    fn messages(index: usize, messages: Vec<ServerMessage>) -> RunEvent {
        RunEvent::Messages { result_index: index, messages, dropped: 0 }
    }

    pub(super) fn done(index: usize, statement: &str, rows: i64) -> RunEvent {
        let summary = ResultSummary { row_count: rows, ..Default::default() };
        let result = RunResult {
            result_index: index,
            statement: statement.into(),
            summary: Some(summary),
            ..Default::default()
        };
        RunEvent::Result(Box::new(result))
    }

    fn click(selector: &'static str, cx: &mut VisualTestContext) {
        let at = cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} is not drawn")).center();
        cx.simulate_click(at, Modifiers::none());
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
    }

    #[gpui_kit::test]
    fn a_run_that_only_said_something_opens_on_its_messages(cx: &mut TestAppContext) {
        let _env = Env::new(cx);
        let (panel, cx) = panel(cx);
        let warning = ServerMessage {
            level: MessageLevel::Warning,
            code: "01000".into(),
            text: "careful".into(),
            hint: "look closer".into(),
            ..Default::default()
        };
        let events = vec![
            messages(0, vec![ServerMessage::new(MessageLevel::Notice, "hi"), warning]),
            done(0, "DO $$ BEGIN RAISE NOTICE 'hi'; END $$", 0),
            RunEvent::Done { result_count: 1, error: None },
        ];
        apply(&panel, events, cx);
        assert!(panel.read_with(cx, |panel, _| panel.messages_shown()));
        assert!(cx.debug_bounds("result-tab-messages").is_some(), "one result still gets the tab strip");
        let texts = panel.read_with(cx, |panel, _| panel.message_texts());
        assert_eq!(
            texts,
            [
                (MessageLevel::Notice, String::new(), "hi".into()),
                (MessageLevel::Warning, "01000".into(), "careful".into())
            ]
        );
        click("copy-messages", cx);
        let copied = cx.read_from_clipboard().and_then(|item| item.text()).unwrap_or_default();
        assert_eq!(
            copied,
            "-- Result 1: DO $$ BEGIN RAISE NOTICE 'hi'; END $$\nNotice: hi\nWarning 01000: careful\nHint: look closer"
        );
    }

    // A result's statement, its message and the message's detail and hint all start at one x.
    #[gpui_kit::test]
    fn message_texts_share_one_column(cx: &mut TestAppContext) {
        let _env = Env::new(cx);
        let (panel, cx) = panel(cx);
        let warning = ServerMessage {
            level: MessageLevel::Warning,
            code: "01000".into(),
            text: "careful".into(),
            detail: "why".into(),
            hint: "look closer".into(),
        };
        let events = vec![
            messages(0, vec![warning]),
            done(0, "-- a note\nDO $$ BEGIN END $$", 0),
            RunEvent::Done { result_count: 1, error: None },
        ];
        apply(&panel, events, cx);
        let left = |selector: &'static str, cx: &mut VisualTestContext| {
            cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} is not drawn")).left()
        };
        let statement = left("message-statement-0", cx);
        for selector in ["message-text-1", "message-text-2", "message-text-3"] {
            assert_eq!(left(selector, cx), statement, "{selector}");
        }
    }

    // A level's icon sits under the toolbar's, which sits under the filter's, and the count starts where level names do.
    #[gpui_kit::test]
    fn levels_line_up_under_the_filter(cx: &mut TestAppContext) {
        let _env = Env::new(cx);
        let (panel, cx) = panel(cx);
        let events = vec![
            messages(0, vec![ServerMessage::new(MessageLevel::Notice, "hi")]),
            done(0, "DO $$ BEGIN END $$", 0),
            RunEvent::Done { result_count: 1, error: None },
        ];
        apply(&panel, events, cx);
        let rem = cx.update(|window, _| window.rem_size());
        let left = |selector: &'static str, cx: &mut VisualTestContext| {
            cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} is not drawn")).left()
        };
        // Layout rounds to whole pixels.
        let near = |a: Pixels, b: Pixels| (a - b).abs() < px(0.5);
        let icon = left("messages-filter", cx) + ICON_INSET.to_pixels(rem);
        assert!(near(left("message-level-1", cx), icon));
        assert!(near(left("messages-count", cx), icon + ICON_SM.to_pixels(rem) + ICON_GAP));
        assert!(cx.debug_bounds("message-level-all").is_none(), "one level leaves nothing to pick");
    }

    #[gpui_kit::test]
    fn the_filter_narrows_the_log_and_a_heading_opens_its_result(cx: &mut TestAppContext) {
        let _env = Env::new(cx);
        let (panel, cx) = panel(cx);
        let warning = ServerMessage {
            level: MessageLevel::Warning,
            code: "01000".into(),
            text: "Slow path".into(),
            hint: "Add an INDEX".into(),
            ..Default::default()
        };
        let events = vec![
            messages(0, vec![ServerMessage::new(MessageLevel::Notice, "skipping")]),
            done(0, "ALTER TABLE t ADD c int", 0),
            messages(1, vec![ServerMessage::new(MessageLevel::Notice, "checked"), warning]),
            done(1, "DO $$ BEGIN END $$", 0),
            RunEvent::Done { result_count: 2, error: None },
        ];
        apply(&panel, events, cx);
        let texts = |cx: &mut VisualTestContext| {
            panel.read_with(cx, |panel, _| {
                panel.message_texts().into_iter().map(|(_, _, text)| text).collect::<Vec<_>>()
            })
        };
        click("message-level-notice", cx);
        assert_eq!(texts(cx), ["skipping", "checked"]);
        click("message-level-all", cx);
        click("messages-filter", cx);
        cx.simulate_input("index");
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert_eq!(texts(cx), ["Slow path"], "its hint matched");

        click("message-heading-0", cx);
        assert_eq!(panel.read_with(cx, |panel, _| (panel.messages_shown(), panel.active)), (false, 1));

        // The next run's log is filtered as this one was, as the field still says.
        panel.update(cx, |panel, cx| panel.clear(cx));
        let events = vec![
            messages(0, vec![ServerMessage::new(MessageLevel::Notice, "no index used"), ServerMessage::default()]),
            RunEvent::Done { result_count: 1, error: None },
        ];
        apply(&panel, events, cx);
        assert_eq!(texts(cx), ["no index used"]);
    }

    // Until a result comes, a running tab says so in place of the hints, with a way to stop it.
    #[gpui_kit::test]
    fn a_run_without_results_yet_shows_it_runs(cx: &mut TestAppContext) {
        let _env = Env::new(cx);
        let (panel, cx) = panel(cx);
        let stopped = std::rc::Rc::new(std::cell::Cell::new(false));
        let seen = stopped.clone();
        cx.update(|_, cx| {
            cx.subscribe(&panel, move |_, event: &super::ResultsEvent, _| {
                seen.set(seen.get() || matches!(event, super::ResultsEvent::Stop))
            })
            .detach()
        });
        panel.update(cx, |panel, cx| panel.set_running(Some(std::time::Instant::now()), cx));
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(cx.debug_bounds("results-running").is_some() && cx.debug_bounds("results-empty").is_none());
        click("results-stop", cx);
        assert!(stopped.get());
        apply(&panel, vec![done(0, "CREATE TABLE t (x int)", 0)], cx);
        assert!(cx.debug_bounds("results-running").is_none() && cx.debug_bounds("results-outcome").is_some());
    }

    #[gpui_kit::test]
    fn messages_wait_behind_rows_until_their_tab_is_picked(cx: &mut TestAppContext) {
        let _env = Env::new(cx);
        let (panel, cx) = panel(cx);
        let columns: Arc<[ColumnMeta]> =
            vec![ColumnMeta { name: "n".into(), type_name: "int4".into(), ..Default::default() }].into();
        let mut rows = ChunkBuilder::new(1, 1);
        rows.push_number(|s| s.push('1'));
        rows.end_row();
        let events = vec![
            RunEvent::Meta { result_index: 0, columns, statement: "SELECT 1".into(), catalog: Default::default() },
            RunEvent::Rows { result_index: 0, chunk: Arc::new(rows.finish()) },
            done(0, "SELECT 1", 1),
            messages(0, vec![ServerMessage::new(MessageLevel::Warning, "1292: truncated")]),
            RunEvent::Done { result_count: 1, error: None },
        ];
        apply(&panel, events, cx);
        assert!(!panel.read_with(cx, |panel, _| panel.messages_shown()), "the rows come first");
        click("result-tab-messages", cx);
        assert!(panel.read_with(cx, |panel, _| panel.messages_shown()));
        assert!(cx.debug_bounds("messages").is_some());
        click("result-tab-0", cx);
        assert!(!panel.read_with(cx, |panel, _| panel.messages_shown()));

        // MySQL's info line only reports how an UPDATE went, so its run stays on the result.
        panel.update(cx, |panel, cx| panel.clear(cx));
        let info = ServerMessage::new(MessageLevel::Info, "Rows matched: 3  Changed: 1  Warnings: 0");
        apply(
            &panel,
            vec![
                messages(0, vec![info]),
                done(0, "UPDATE t SET a = 1", 0),
                RunEvent::Done { result_count: 1, error: None },
            ],
            cx,
        );
        assert!(!panel.read_with(cx, |panel, _| panel.messages_shown()));
        assert!(cx.debug_bounds("result-tab-messages").is_some());
        panel.update(cx, |panel, cx| panel.clear(cx));
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(cx.debug_bounds("result-tab-messages").is_none(), "a new run starts without them");
    }
}

#[cfg(test)]
mod editor_tests {
    use std::cell::RefCell;
    use std::net::TcpListener;
    use std::rc::Rc;
    use std::sync::Arc;

    use barsql_app::{EditorTab, ResultTable, RunEvent, RunResult};
    use barsql_core::{ColumnInfo, ConnectionConfig, DriverType, QueryError};
    use barsql_db::{ChunkBuilder, ColumnMeta};
    use gpui_kit::assets::IconName as Lucide;
    use gpui_kit::{TestAppContext, VisualTestContext};

    use super::messages_tests::{apply, done, panel};
    use super::{ResultsEvent, ResultsPanel, ShownError};
    use crate::grid::GridEvent;
    use crate::i18n::I18n;
    use crate::query_tab::{QueryTab, new_tab_id};
    use crate::test_support::{Env, settle};

    #[gpui_kit::test]
    fn a_bug_reads_in_the_ui_language_and_keeps_the_panic_as_detail(cx: &mut TestAppContext) {
        cx.update(|cx| {
            cx.set_global(I18n::new("de"));
            let shown = ShownError::new(QueryError::from_panic(Box::new("boom")), cx);
            let said = "In BarSQL ist ein interner Fehler aufgetreten. Details stehen in crash.log im Datenordner.";
            assert_eq!(shown.message, said);
            assert_eq!(shown.info.map(|info| info.detail), Some("boom".to_string()));
        });
    }

    // Result `index`: one row of (id, name).
    fn people(index: usize, statement: &str) -> Vec<RunEvent> {
        let columns: Arc<[ColumnMeta]> = [("id", "int4"), ("name", "text")]
            .map(|(name, type_name)| ColumnMeta {
                name: name.into(),
                type_name: type_name.into(),
                ..Default::default()
            })
            .to_vec()
            .into();
        let mut rows = ChunkBuilder::new(2, 1);
        rows.push_number(|s| s.push('1'));
        rows.push_text(|s| s.push_str("ann"));
        rows.end_row();
        vec![
            RunEvent::Meta { result_index: index, columns, statement: statement.into(), catalog: Default::default() },
            RunEvent::Rows { result_index: index, chunk: Arc::new(rows.finish()) },
            done(index, statement, 1),
        ]
    }

    // A script that starts with setup opens on its first result with rows, or on its last when none has any, unless
    // a tab was picked while it ran.
    #[gpui_kit::test]
    fn a_script_opens_past_its_setup(cx: &mut TestAppContext) {
        let _env = Env::new(cx);
        let (panel, cx) = panel(cx);
        let active = |cx: &mut VisualTestContext| panel.read_with(cx, |panel, _| panel.active);
        let script = || {
            let mut events = vec![done(0, "CREATE TABLE people (id int, name text)", 0)];
            events.extend(people(1, "SELECT id, name FROM people"));
            events
        };
        let end = || RunEvent::Done { result_count: 2, error: None };
        apply(&panel, script().into_iter().chain([end()]).collect(), cx);
        assert_eq!(active(cx), 1);

        panel.update(cx, |panel, cx| panel.clear(cx));
        apply(&panel, script(), cx);
        panel.update(cx, |panel, cx| panel.select_result(0, cx));
        apply(&panel, vec![end()], cx);
        assert_eq!(active(cx), 0, "the picked tab stays");

        panel.update(cx, |panel, cx| panel.clear(cx));
        let events = vec![done(0, "CREATE TABLE t (x int)", 0), done(1, "INSERT INTO t VALUES (1), (2)", 2), end()];
        apply(&panel, events, cx);
        assert_eq!(active(cx), 1);
    }

    // A grid shows with its first rows, or once its result ends without any, never as headers waiting for rows.
    #[gpui_kit::test]
    fn a_grid_shows_with_its_first_rows(cx: &mut TestAppContext) {
        let _env = Env::new(cx);
        let (panel, cx) = panel(cx);
        let shown = |cx: &mut VisualTestContext| panel.read_with(cx, |panel, _| panel.shown_grid().is_some());
        let mut events = people(0, "SELECT id, name FROM people");
        let rest = events.split_off(1);
        apply(&panel, events, cx);
        assert!(!shown(cx) && cx.debug_bounds("results-empty").is_some(), "its columns alone");
        apply(&panel, rest, cx);
        assert!(shown(cx));

        panel.update(cx, |panel, cx| panel.clear(cx));
        let statement = "SELECT id, name FROM people WHERE false";
        let meta = people(0, statement).into_iter().next().expect("its columns");
        apply(&panel, vec![meta], cx);
        assert!(!shown(cx));
        apply(&panel, vec![done(0, statement, 0)], cx);
        assert!(shown(cx), "no rows, once that's sure");
    }

    fn looked_up(panel: &ResultsPanel) -> Vec<bool> {
        panel.sets.iter().map(|set| set.resolving.is_some()).collect()
    }

    // A SQL Server batch's results all carry its text, and only the first can be traced from it. A result that isn't
    // shown waits until it is.
    #[gpui_kit::test]
    fn only_a_statements_first_result_looks_up_its_table(cx: &mut TestAppContext) {
        let _env = Env::new(cx);
        let (panel, cx) = panel(cx);
        let connection = ConnectionConfig { driver: DriverType::SqlServer, ..Default::default() };
        panel.update(cx, |panel, cx| panel.set_connection(connection.clone(), cx));
        let batch = "SELECT id, name FROM a WHERE id = 1\nUPDATE b SET name = name OUTPUT inserted.id, inserted.name";
        let runs = [people(0, batch), people(1, batch), people(2, "SELECT id, name FROM c")];
        apply(&panel, runs.into_iter().flatten().collect(), cx);
        assert_eq!(panel.read_with(cx, |panel, _| looked_up(panel)), [true, false, false]);
        panel.update(cx, |panel, cx| {
            panel.select_result(1, cx);
            panel.select_result(2, cx);
        });
        assert_eq!(panel.read_with(cx, |panel, _| looked_up(panel)), [true, false, true]);

        // A read-only connection's rows get their keys too, and as they stream.
        panel.update(cx, |panel, cx| {
            panel.clear(cx);
            panel.set_connection(ConnectionConfig { read_only: true, ..connection }, cx);
        });
        let mut streaming = people(0, "SELECT id, name FROM c");
        streaming.pop();
        apply(&panel, streaming, cx);
        assert_eq!(panel.read_with(cx, |panel, _| looked_up(panel)), [true]);
        let failed = RunResult {
            statement: "SELECT id, name FROM c".into(),
            error: Some(QueryError::message("lost")),
            ..Default::default()
        };
        apply(&panel, vec![RunEvent::Result(Box::new(failed))], cx);
        assert_eq!(panel.read_with(cx, |panel, _| looked_up(panel)), [false], "the error took the rows' place");
    }

    fn seed(env: &Env, sql: &str) {
        for statement in sql.split(';').map(str::trim).filter(|s| !s.is_empty()) {
            env.runtime.block_on(env.bar.execute_query(&env.connection.id, statement)).unwrap();
        }
    }

    // `sql`'s events as a query tab gets them, up to its Result, and the rest.
    fn run(env: &Env, tab: &str, sql: &str) -> (Vec<RunEvent>, Vec<RunEvent>) {
        let mut events = env.runtime.block_on(async {
            let handle = env.bar.execute_query_stream(&env.connection.id, tab, sql).await.unwrap();
            let mut events = Vec::new();
            while let Ok(event) = handle.events.recv().await {
                events.push(event);
            }
            events
        });
        let result = events.iter().position(|event| matches!(event, RunEvent::Result(_))).expect("a result");
        let rest = events.split_off(result);
        (events, rest)
    }

    // SQLite's one connection is the run's until it ends, so the rows show at once and their keys follow. Edits wait for
    // the last row. A foreign key's jump finds its row, through the target's primary key where SQLite leaves the
    // referenced column out.
    #[gpui_kit::test]
    fn a_sqlite_results_rows_show_at_once_and_its_foreign_keys_jump(cx: &mut TestAppContext) {
        let env = Env::new(cx);
        seed(
            &env,
            "CREATE TABLE authors (id INTEGER PRIMARY KEY, name TEXT);
             CREATE TABLE books (id INTEGER PRIMARY KEY, author_id INTEGER REFERENCES authors(id), editor INTEGER REFERENCES authors);
             INSERT INTO authors VALUES (7, 'ann');
             INSERT INTO books VALUES (1, 7, 7)",
        );
        let (panel, cx) = panel(cx);
        panel.update(cx, |panel, cx| {
            panel.set_connection(env.connection.clone(), cx);
            panel.set_tab("results-tab".into());
        });
        let (streaming, rest) = run(&env, "results-tab", "SELECT * FROM books");
        // Without a turn of the executor, so the lookup can't have answered yet.
        for event in streaming {
            panel.update_in(cx, |panel, window, cx| panel.apply(event, window, cx));
        }
        let grid = panel.read_with(cx, |panel, _| panel.shown_grid().cloned()).expect("its rows show at once");
        settle(cx, |cx| cx.update(|_, cx| grid.read(cx).key_icons().iter().all(Option::is_some)));
        let keys = vec![Some(Lucide::KeyRound), Some(Lucide::Link), Some(Lucide::Link)];
        assert_eq!(cx.update(|_, cx| (grid.read(cx).key_icons(), grid.read(cx).rows_shown())), (keys, 1));
        assert!(!cx.update(|_, cx| grid.read(cx).takes_edits()), "not while rows may still come");
        apply(&panel, rest, cx);
        assert!(cx.update(|_, cx| grid.read(cx).takes_edits()));

        let opened = Rc::new(RefCell::new(Vec::new()));
        let sink = opened.clone();
        cx.update(|_, cx| {
            cx.subscribe(&panel, move |_, event: &ResultsEvent, _| {
                if let ResultsEvent::OpenTable { schema, table, filter } = event {
                    sink.borrow_mut().push(format!("{schema}.{table} {filter}"));
                }
            })
            .detach()
        });
        for column in [1, 2] {
            grid.update(cx, |_, cx| cx.emit(GridEvent::OpenForeignKey { row: 0, column }));
        }
        settle(cx, |_| opened.borrow().len() == 2);
        assert_eq!(*opened.borrow(), ["main.authors id = 7", "main.authors id = 7"]);
    }

    // A lookup that fails doesn't hold the rows. One that doesn't come back holds them only so long, the tab's hints
    // staying and its progress line running meanwhile. This server takes the connection and never answers.
    #[gpui_kit::test]
    fn rows_wait_for_their_keys_only_while_they_may_come(cx: &mut TestAppContext) {
        let env = Env::new(cx);
        let tab = EditorTab { id: new_tab_id(), connection_id: env.connection.id.clone(), ..Default::default() };
        let connection = env.connection.clone();
        let (tab, cx) = cx.add_window_view(|window, cx| QueryTab::new(tab, connection, window, cx));
        let panel = cx.update(|_, cx| tab.read(cx).results());
        let unsaved = ConnectionConfig { driver: DriverType::Postgres, ..Default::default() };
        panel.update(cx, |panel, cx| panel.set_connection(unsaved, cx));
        apply(&panel, people(0, "SELECT id, name FROM people"), cx);
        settle(cx, |cx| panel.read_with(cx, |panel, _| panel.shown_grid().is_some()));

        let silent = TcpListener::bind("127.0.0.1:0").unwrap();
        let config = ConnectionConfig {
            name: "Silent".into(),
            driver: DriverType::Postgres,
            host: "127.0.0.1".into(),
            port: silent.local_addr().unwrap().port().into(),
            database: "shop".into(),
            username: "ann".into(),
            ..Default::default()
        };
        let connection = env.runtime.block_on(env.bar.save_connection(config)).unwrap();
        panel.update(cx, |panel, cx| {
            panel.clear(cx);
            panel.set_connection(connection, cx);
        });
        apply(&panel, people(0, "SELECT id, name FROM people"), cx);
        assert!(panel.read_with(cx, |panel, _| panel.shown_grid().is_none()), "the keys may still come");
        assert!(cx.debug_bounds("results-empty").is_some() && cx.debug_bounds("run-progress").is_some());
        cx.executor().advance_clock(super::KEYS_WAIT);
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(panel.read_with(cx, |panel, _| panel.shown_grid().is_some()));
        assert!(cx.debug_bounds("run-progress").is_none());
    }

    // Edits staged against one server don't follow the tab to another, though a rename keeps them.
    #[gpui_kit::test]
    fn a_connection_pointed_elsewhere_drops_staged_edits(cx: &mut TestAppContext) {
        let _env = Env::new(cx);
        let (panel, cx) = panel(cx);
        let staging = ConnectionConfig { driver: DriverType::Postgres, host: "staging".into(), ..Default::default() };
        panel.update(cx, |panel, cx| panel.set_connection(staging.clone(), cx));
        apply(&panel, people(0, "SELECT id, name FROM people"), cx);
        panel.update(cx, |panel, cx| {
            let grid = panel.sets[0].grid.clone().expect("a grid");
            let catalog = ["id", "name"]
                .map(|name| ColumnInfo { name: name.into(), is_primary: name == "id", ..Default::default() })
                .to_vec();
            let columns = vec![Some("id".into()), Some("name".into())];
            panel.install_table(
                0,
                &grid,
                ResultTable { table: "people".into(), columns, catalog, ..Default::default() },
                cx,
            );
            panel.stage(&grid, |editor, set| editor.stage(set, &[(0, 1, Some("anna".into()))]), cx);
        });
        let renamed = ConnectionConfig { name: "Staging".into(), ..staging.clone() };
        panel.update(cx, |panel, cx| panel.set_connection(renamed, cx));
        assert_eq!(panel.read_with(cx, |panel, _| panel.pending_edits()), 1, "a rename keeps them");
        let prod = ConnectionConfig { host: "prod".into(), ..staging };
        panel.update(cx, |panel, cx| panel.set_connection(prod, cx));
        assert!(panel.read_with(cx, |panel, _| panel.sets[0].editor.is_none() && panel.pending_edits() == 0));
    }
}

fn changed(changed: bool) -> Outcome {
    if changed { Outcome::Changed } else { Outcome::Unchanged }
}

fn copy_text(text: String, cx: &mut App) {
    cx.write_to_clipboard(ClipboardItem::new_string(text));
    toast::success(t(cx, "toast.copiedClipboard"), cx);
}
