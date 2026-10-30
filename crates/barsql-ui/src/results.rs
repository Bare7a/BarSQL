use std::rc::Rc;

use barsql_app::{ApplyError, ResultTable, RunEvent};
use barsql_core::{ConnectionConfig, DriverType, QueryError, ResultSummary, SqlDialect};
use barsql_db::{Cancel, ResultSet};
use barsql_io::EXPORT_FORMATS;
use barsql_sql::QueryPlan;
use barsql_sql::lang::{self, UnknownKind, closest, find_name, unknown_name};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::{ContextMenuExt, PopupMenu, PopupMenuItem};
use gpui_kit::component::scroll::{Scrollbar, ScrollbarAxis};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme, Disableable, Icon, Selectable, Sizable, StyledExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::cell_viewer;
use crate::chart_view::ChartView;
use crate::dialogs;
use crate::form::{self, ToolButton};
use crate::grid::{self, Grid, GridEvent, RowRef};
use crate::i18n::{I18n, format_number, t, t_count, t_with};
use crate::list_nav::LIST_INSET;
use crate::message_log::{self, Heading, Line, MessageLog};
use crate::plan_view::PlanView;
use crate::result_edits::ResultEditor;
use crate::schema::{self, ColumnsLoad};
use crate::scrollbars::{HoverScrollbar as _, system_bar};
use crate::shortcuts;
use crate::state;
use crate::table_edits::Outcome;
use crate::toast;
use crate::tokens::{ICON_SM, ICON_XS, TEXT_MD, TEXT_SM, TEXT_XS};

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
        Self { message: error.message.clone().into(), info: Some(error), fix: None, scroll }
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
        let message = div()
            .id("error-message")
            .max_h(rems(16.))
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .font_family(theme.mono_font_family.clone())
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
    summary: Option<ResultSummary>,
    plan: Option<Entity<PlanView>>,
    error: Option<ShownError>,
    statement: Option<String>,
    // Set once the rows turn out to be one table's, primary key included.
    editor: Option<ResultEditor>,
    // Rows that may be one table's, looked up once the result shows: most of a long script's never do.
    lookup_due: bool,
    _resolving: Option<Lookup>,
}

// Cancelled when its result goes, so the lookup leaves the tab's queue instead of holding up the next run.
struct Lookup {
    _task: Task<()>,
    cancel: Cancel,
}

impl Drop for Lookup {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

impl ResultSetView {
    fn rows(&self, cx: &App) -> usize {
        self.grid.as_ref().map_or(0, |grid| grid.read(cx).set().rows())
    }

    // Nothing to look at but the summary line.
    fn is_plain(&self) -> bool {
        self.grid.is_none() && self.plan.is_none() && self.error.is_none()
    }

    // Row count for a grid, affected rows otherwise, and no badge for errors or plans.
    fn count(&self, cx: &App) -> Option<i64> {
        if self.error.is_some() || self.plan.is_some() {
            return None;
        }
        let summary = self.summary.as_ref();
        Some(match self.grid {
            Some(_) => summary.map_or(self.rows(cx) as i64, |s| s.row_count),
            None => summary.map_or(0, |s| s.affected_rows),
        })
    }
}

fn statement_tooltip(statement: &str) -> SharedString {
    let line = statement.split_whitespace().collect::<Vec<_>>().join(" ");
    line.chars().take(120).collect::<String>().into()
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
    messages: MessageLog,
    tab_scroll: ScrollHandle,
}

impl ResultsPanel {
    pub fn set_dialect(&mut self, dialect: Option<SqlDialect>) {
        self.dialect = dialect;
    }

    // Editors and lookups were for the old target, so a change to how it connects, read-only included, drops them.
    // Anything else leaves the overlays as they are.
    pub fn set_connection(&mut self, connection: ConnectionConfig, cx: &mut Context<Self>) {
        if self.connection.as_ref().is_some_and(|old| old.fingerprint() != connection.fingerprint()) {
            for set in &mut self.sets {
                (set.lookup_due, set._resolving) = (false, None);
                if let (Some(_), Some(grid)) = (set.editor.take(), &set.grid) {
                    grid.update(cx, |grid, cx| grid.clear_overlay(cx));
                }
            }
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
        self.active = 0;
        self.forget_messages();
        cx.emit(ResultsEvent::FocusedRowChanged);
        cx.notify();
    }

    fn show(&mut self, set: ResultSetView, cx: &mut Context<Self>) {
        self.sets = vec![set];
        self.active = 0;
        self.forget_messages();
        cx.emit(ResultsEvent::FocusedRowChanged);
        cx.notify();
    }

    fn forget_messages(&mut self) {
        self.messages = MessageLog::default();
        self.messages_open = false;
    }

    pub(crate) fn select_result(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.active = ix;
        self.resolve_editor(ix, cx);
        self.messages_open = false;
        cx.emit(ResultsEvent::FocusedRowChanged);
        cx.notify();
    }

    pub(crate) fn open_messages(&mut self, cx: &mut Context<Self>) {
        self.messages_open = true;
        cx.emit(ResultsEvent::FocusedRowChanged);
        cx.notify();
    }

    pub fn focused_row(&self, cx: &App) -> Option<RowRef> {
        self.shown_grid()?.read(cx).focused_row()
    }

    // The active result's grid, unless the Messages tab covers it.
    fn shown_grid(&self) -> Option<&Entity<Grid>> {
        self.sets.get(self.active).filter(|_| !self.messages_open)?.grid.as_ref()
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
            RunEvent::Meta { result_index, columns, .. } => {
                let dialect = self.dialect;
                let grid = cx.new(|cx| {
                    let mut grid = Grid::new(columns, cx);
                    grid.set_dialect(dialect);
                    grid
                });
                let subscriptions =
                    vec![cx.observe(&grid, |_, _, cx| cx.notify()), cx.subscribe_in(&grid, window, Self::grid_event)];
                *self.slot(result_index) = ResultSetView {
                    grid: Some(grid),
                    _subscriptions: subscriptions,
                    streaming: true,
                    ..Default::default()
                };
            }
            RunEvent::Rows { result_index, chunk } => {
                if let Some(grid) = self.sets.get(result_index).filter(|s| s.streaming).and_then(|s| s.grid.clone()) {
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
                // A batch's later results carry its whole text too, and only its first can be traced.
                let before = result.result_index.checked_sub(1).and_then(|prev| self.sets.get(prev));
                let first = before.and_then(|set| set.statement.as_deref()) != Some(result.statement.as_str());
                let (index, rows) = (result.result_index, first && shown.is_none() && plan.is_none());
                let set = self.slot(result.result_index);
                set.streaming = false;
                set.statement = Some(result.statement);
                set.lookup_due = rows;
                if let Some(error) = shown {
                    set.grid = None;
                    set._subscriptions.clear();
                    set.error = Some(error);
                } else {
                    failed = Some(false);
                    if let Some(plan) = plan {
                        set.grid = None;
                        set._subscriptions.clear();
                        set.plan = Some(plan);
                    } else {
                        set.summary = result.summary;
                    }
                }
                if self.active >= self.sets.len() {
                    self.active = 0;
                }
                if index == self.active {
                    self.resolve_editor(index, cx);
                }
            }
            // A batch-level failure, like no session, replaces every result.
            RunEvent::Done { error: Some(error), .. } => {
                let error = ShownError::new(error, cx);
                self.sets = vec![ResultSetView { error: Some(error), ..Default::default() }];
                self.active = 0;
            }
            // A run that only said something opens on what it said, like a DO block's notices.
            RunEvent::Done { .. } => {
                if self.messages.has_raised() && self.sets.iter().all(ResultSetView::is_plain) {
                    self.messages_open = true;
                }
            }
        }
        if self.shown_grid().cloned() != before {
            cx.emit(ResultsEvent::FocusedRowChanged);
        }
        cx.notify();
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
        self.sets.get(self.active)?.grid.clone()
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
                    form::badge(format_number(count as f64, 0, &lang), color).ml(rems(0.462))
                });
                let tooltip = set.statement.as_deref().map(statement_tooltip).filter(|tip| !tip.is_empty());
                h_flex()
                    .id(("result-tab", ix))
                    .debug_selector(move || format!("result-tab-{ix}"))
                    .relative()
                    .flex_none()
                    .gap(rems(0.462))
                    .pt(rems(0.462))
                    .pb(rems(0.615))
                    .px(rems(0.769))
                    .border_r_1()
                    .border_color(theme.border)
                    .whitespace_nowrap()
                    .text_size(TEXT_SM)
                    .text_color(color)
                    .cursor_pointer()
                    .when(!failed, |el| el.hover(|style| style.text_color(theme.foreground)))
                    .child(
                        h_flex()
                            .when(set.plan.is_some(), |el| {
                                el.child(Icon::new(Lucide::Route).size(ICON_XS).opacity(0.75))
                            })
                            .child(t_with(cx, key, &[("n", &n.to_string())]))
                            .children(badge),
                    )
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
            .child(
                h_flex()
                    .id("result-tabs")
                    .w_full()
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
            .gap(rems(0.462))
            .pt(rems(0.462))
            .pb(rems(0.615))
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
        range
            .filter_map(|ix| {
                let line = self.messages.row(ix)?;
                let heading = match line {
                    Line::Result(index) => Some(self.heading(index, cx)),
                    _ => None,
                };
                let text = match &line {
                    Line::Text { text, .. } | Line::Labeled { text, .. } => Some(text.to_string()),
                    _ => None,
                };
                let row = message_log::render_row(ix, line, heading, self.messages.has_codes(), cx);
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

    fn messages_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let lang = cx.global::<I18n>().lang().to_string();
        let count = t_count(cx, "results.messagesCount", self.messages.total() as i64, &[]);
        let count =
            count.replace(&self.messages.total().to_string(), &format_number(self.messages.total() as f64, 0, &lang));
        let copy = cx.listener(|this, _, _, cx| this.copy_messages(cx));
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
            .child(div().flex_1().min_w_0().truncate().child(count))
            .child(
                Button::new("copy-messages")
                    .debug_selector(|| "copy-messages".into())
                    .tool(Icon::new(Lucide::Copy), ICON_XS, t(cx, "common.copy"))
                    .tooltip(t(cx, "results.copyMessages"))
                    .on_click(copy),
            );
        let scroll = self.messages.scroll.clone();
        let list = uniform_list("messages", self.messages.rows(), cx.processor(Self::render_message_rows))
            .debug_selector(|| "messages".into())
            .track_scroll(&scroll)
            .size_full()
            .px(LIST_INSET)
            .pb(LIST_INSET);
        v_flex()
            .size_full()
            .child(toolbar)
            .child(div().relative().flex_1().min_h_0().child(list).hover_scrollbar(&scroll, ScrollbarAxis::Vertical))
            .into_any_element()
    }

    fn header(&self, left: SharedString, right: Option<SharedString>, cx: &App) -> impl IntoElement {
        h_flex()
            .h(px(28.))
            .flex_none()
            .px_3()
            .justify_between()
            .text_size(TEXT_SM)
            .text_color(cx.theme().muted_foreground)
            .gap_3()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(div().flex_1().min_w_0().truncate().child(left))
            .children(right.map(|right| div().flex_none().child(right)))
    }

    fn empty(&self, set: Option<&ResultSetView>, cx: &App) -> AnyElement {
        let summary = set.and_then(|s| s.summary.as_ref());
        // Nothing has run yet. Instead of a header saying so, the shortcuts that run something.
        if summary.is_none() {
            let hint = |id: &str, key: &str| (shortcuts::shown(id, cx), t(cx, key));
            return v_flex()
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
                .into_any_element();
        }
        let message = summary.map(|s| s.message.clone()).filter(|m| !m.is_empty());
        let meta = summary.map(|s| {
            let count = if s.affected_rows != 0 { s.affected_rows } else { s.row_count };
            t_count(cx, "results.metaRowsShort", count, &[("ms", &s.duration_ms.to_string())])
        });
        let title = message.clone().map_or_else(|| t(cx, "results.noResults"), SharedString::from);
        let body = message.map_or_else(|| t(cx, "results.runQueryHint"), SharedString::from);
        v_flex()
            .size_full()
            .child(self.header(title, meta, cx))
            .child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(cx.theme().muted_foreground)
                    .child(body),
            )
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
                .xsmall()
                .label(t_with(cx, "errors.useName", &[("name", &fix.meant)]))
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
            _ => {}
        }
    }

    fn index_of(&self, grid: &Entity<Grid>) -> Option<usize> {
        self.sets.iter().position(|set| set.grid.as_ref() == Some(grid))
    }

    fn editable(&self) -> bool {
        self.connection.as_ref().is_some_and(|connection| !connection.read_only)
    }

    fn resolve_editor(&mut self, index: usize, cx: &mut Context<Self>) {
        let editing = |c: &ConnectionConfig| c.driver.capabilities().row_editing && !c.read_only;
        let Some(connection) = self.connection.clone().filter(editing) else { return };
        let tab = self.tab_id.clone();
        let Some(set) = self.sets.get_mut(index).filter(|set| set.lookup_due) else { return };
        let (Some(grid), Some(statement)) = (set.grid.clone(), set.statement.clone()) else { return };
        set.lookup_due = false;
        let columns = grid.read(cx).set().columns.clone();
        let (bar, id, cancel) = (state::bar(cx), connection.id.clone(), Cancel::new());
        let lookup = state::spawn(cx, {
            let cancel = cancel.clone();
            async move { bar.result_table(&id, &tab, &statement, &columns, &cancel).await }
        });
        let task = cx.spawn(async move |this, cx| {
            let Some(Ok(Some(table))) = lookup.await else { return };
            let _ = this.update(cx, |this, cx| this.install_editor(index, &grid, table, cx));
        });
        set._resolving = Some(Lookup { _task: task, cancel });
    }

    fn install_editor(&mut self, index: usize, grid: &Entity<Grid>, table: ResultTable, cx: &mut Context<Self>) {
        let Some(set) = self.sets.get_mut(index).filter(|set| set.grid.as_ref() == Some(grid)) else { return };
        set.editor = ResultEditor::new(table, grid.read(cx).set());
        self.sync_overlay(index, cx);
    }

    fn sync_overlay(&mut self, index: usize, cx: &mut Context<Self>) {
        let editable = self.editable();
        let Some(set) = self.sets.get(index) else { return };
        let (Some(grid), Some(editor)) = (&set.grid, &set.editor) else { return };
        let overlay = editor.overlay(grid.read(cx).set(), editable && !editor.applying);
        grid.update(cx, |grid, cx| grid.set_overlay(overlay, cx));
        cx.notify();
    }

    fn stage(
        &mut self,
        grid: &Entity<Grid>,
        edit: impl FnOnce(&mut ResultEditor, &ResultSet) -> Outcome,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.index_of(grid).filter(|_| self.editable()) else { return };
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
        h_flex()
            .flex_none()
            .px(rems(0.923))
            .py(rems(0.615))
            .gap(rems(0.462))
            .border_t_1()
            .border_color(theme.border)
            .child(
                Button::new("result-revert")
                    .debug_selector(|| "result-revert".into())
                    .tool_label(t(cx, "tableView.reset"))
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
            })
            .child(div().flex_1())
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
            .child(grid::toolbar(grid, meta, None, Some(toggle), !set.charting, cx))
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
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match self.sets.get(self.active) {
            _ if self.messages_open => self.messages_view(cx),
            None => self.empty(None, cx),
            Some(set) => match (&set.error, &set.plan, &set.grid) {
                (Some(error), ..) => self.error(error, set.statement.as_deref(), cx),
                (None, Some(plan), _) => plan.clone().into_any_element(),
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
    use super::{ordinals, split_message};

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
    use gpui_kit::{AppContext as _, Entity, Modifiers, TestAppContext, VisualTestContext};

    use super::ResultsPanel;
    use crate::test_support::Env;

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
            RunEvent::Meta { result_index: 0, columns, schema_name: String::new(), table_name: String::new() },
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
    use std::sync::Arc;

    use barsql_app::{ResultTable, RunEvent};
    use barsql_core::{ColumnInfo, ConnectionConfig, DriverType};
    use barsql_db::{ChunkBuilder, ColumnMeta};
    use gpui_kit::TestAppContext;

    use super::ResultsPanel;
    use super::messages_tests::{apply, done, panel};
    use crate::test_support::Env;

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
            RunEvent::Meta { result_index: index, columns, schema_name: String::new(), table_name: String::new() },
            RunEvent::Rows { result_index: index, chunk: Arc::new(rows.finish()) },
            done(index, statement, 1),
        ]
    }

    fn looked_up(panel: &ResultsPanel) -> Vec<bool> {
        panel.sets.iter().map(|set| set._resolving.is_some()).collect()
    }

    // A SQL Server batch's results all carry its text, and only the first can be traced from it. A result that isn't
    // shown waits until it is.
    #[gpui_kit::test]
    fn only_a_statements_first_result_on_a_writable_connection_looks_up_its_table(cx: &mut TestAppContext) {
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

        panel.update(cx, |panel, cx| {
            panel.clear(cx);
            panel.set_connection(ConnectionConfig { read_only: true, ..connection }, cx);
        });
        apply(&panel, people(0, "SELECT id, name FROM c"), cx);
        assert_eq!(panel.read_with(cx, |panel, _| looked_up(panel)), [false], "its edits could never apply");
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
            panel.install_editor(
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
