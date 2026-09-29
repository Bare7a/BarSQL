use barsql_app::RunEvent;
use barsql_core::{QueryError, ResultSummary};
use barsql_sql::QueryPlan;
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme, Icon, StyledExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::form::{self, ToolButton};
use crate::grid::{self, Grid, GridEvent, RowRef};
use crate::i18n::{I18n, format_number, t, t_with};
use crate::plan_view::PlanView;
use crate::toast;
use crate::tokens::{ICON_SM, ICON_XS, TEXT_MD, TEXT_SM, TEXT_XS};

// A cancelled run shows only a message, without error details.
#[derive(Debug, Clone)]
pub struct ShownError {
    pub message: SharedString,
    pub info: Option<QueryError>,
    scroll: ScrollHandle,
}

impl ShownError {
    pub fn new(error: QueryError, cx: &App) -> Self {
        if error.cancelled {
            return Self { message: t(cx, "dialog.queryCancelled"), info: None, scroll: ScrollHandle::new() };
        }
        Self { message: error.message.clone().into(), info: Some(error), scroll: ScrollHandle::new() }
    }

    // Centred in the pane. `action` sits next to the copy button.
    pub fn card(&self, action: Option<Button>, cx: &App) -> Div {
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
            .when_some(info.map(|i| i.code.clone()).filter(|code| !code.is_empty()), |el, code| {
                el.child(form::badge(code, danger).text_size(TEXT_XS).font_family(theme.mono_font_family.clone()))
            })
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
        let message = div()
            .id("error-message")
            .max_h(rems(16.))
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .font_family(theme.mono_font_family.clone())
            .text_color(theme.foreground)
            .child(self.message.clone());
        let card = form::alert(danger)
            .w_full()
            .max_w(rems(40.))
            .flex()
            .flex_col()
            .gap(rems(0.462))
            .child(header)
            .child(div().relative().child(message).vertical_scrollbar(&self.scroll))
            .when_some(info.filter(|i| !i.detail.is_empty()), |el, info| {
                el.child(meta("errors.detailLabel", &info.detail, theme.foreground))
            })
            .when_some(info.filter(|i| !i.hint.is_empty()), |el, info| {
                el.child(meta("errors.hintLabel", &info.hint, theme.primary))
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

#[derive(Default)]
struct ResultSetView {
    grid: Option<Entity<Grid>>,
    _subscriptions: Vec<Subscription>,
    streaming: bool,
    summary: Option<ResultSummary>,
    plan: Option<Entity<PlanView>>,
    error: Option<ShownError>,
    statement: Option<String>,
}

impl ResultSetView {
    fn rows(&self, cx: &App) -> usize {
        self.grid.as_ref().map_or(0, |grid| grid.read(cx).set().rows())
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
    // Also sent when there is no focused row anymore.
    FocusedRowChanged,
}

pub enum ResultStatus {
    Empty,
    Streaming(usize),
    Rows { count: i64, ms: i64 },
    Affected { count: i64, ms: i64 },
    Error(SharedString),
}

#[derive(Default)]
pub struct ResultsPanel {
    sets: Vec<ResultSetView>,
    active: usize,
    tab_scroll: ScrollHandle,
}

impl ResultsPanel {
    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.sets.clear();
        self.active = 0;
        cx.emit(ResultsEvent::FocusedRowChanged);
        cx.notify();
    }

    fn show(&mut self, set: ResultSetView, cx: &mut Context<Self>) {
        self.sets = vec![set];
        self.active = 0;
        cx.emit(ResultsEvent::FocusedRowChanged);
        cx.notify();
    }

    pub(crate) fn select_result(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.active = ix;
        cx.emit(ResultsEvent::FocusedRowChanged);
        cx.notify();
    }

    pub fn focused_row(&self, cx: &App) -> Option<RowRef> {
        self.sets.get(self.active)?.grid.as_ref()?.read(cx).focused_row()
    }

    fn is_active(&self, grid: &Entity<Grid>) -> bool {
        self.sets.get(self.active).and_then(|set| set.grid.as_ref()) == Some(grid)
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
        let before = self.sets.get(self.active).and_then(|set| set.grid.clone());
        match event {
            RunEvent::Meta { result_index, columns, .. } => {
                let grid = cx.new(|cx| Grid::new(columns, cx));
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
            RunEvent::Result(result) => {
                let result = *result;
                let shown = result.error.map(|error| {
                    failed = (!error.cancelled).then_some(true);
                    ShownError::new(error, cx)
                });
                let plan = result.plan.map(|plan| cx.new(|cx| PlanView::new(plan, cx)));
                let set = self.slot(result.result_index);
                set.streaming = false;
                set.statement = Some(result.statement);
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
            }
            // A batch-level failure, like no session, replaces every result.
            RunEvent::Done { error: Some(error), .. } => {
                let error = ShownError::new(error, cx);
                self.sets = vec![ResultSetView { error: Some(error), ..Default::default() }];
                self.active = 0;
            }
            RunEvent::Done { .. } => {}
        }
        if self.sets.get(self.active).and_then(|set| set.grid.clone()) != before {
            cx.emit(ResultsEvent::FocusedRowChanged);
        }
        cx.notify();
        failed
    }

    #[cfg(any(test, feature = "snapshot"))]
    pub fn result_count(&self) -> usize {
        self.sets.len()
    }

    pub fn status(&self, cx: &App) -> ResultStatus {
        let Some(set) = self.sets.get(self.active) else { return ResultStatus::Empty };
        if let Some(error) = &set.error {
            return ResultStatus::Error(error.message.clone());
        }
        if set.plan.is_some() {
            return ResultStatus::Empty;
        }
        if set.streaming {
            return ResultStatus::Streaming(set.rows(cx));
        }
        let ms = set.summary.as_ref().map_or(0, |s| s.duration_ms);
        let count = set.count(cx).unwrap_or(0);
        if set.grid.is_some() { ResultStatus::Rows { count, ms } } else { ResultStatus::Affected { count, ms } }
    }

    #[cfg(test)]
    pub fn tab_counts(&self, cx: &App) -> Vec<Option<i64>> {
        self.sets.iter().map(|set| set.count(cx)).collect()
    }

    #[cfg(any(test, feature = "snapshot"))]
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
        let tabs: Vec<Stateful<Div>> = self
            .sets
            .iter()
            .zip(ordinals)
            .enumerate()
            .map(|(ix, (set, n))| {
                let active = ix == self.active;
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
            })
            .collect();
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
            .horizontal_scrollbar(&self.tab_scroll)
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
        let message = summary.map(|s| s.message.clone()).filter(|m| !m.is_empty());
        let meta = summary.map(|s| {
            let count = if s.affected_rows != 0 { s.affected_rows } else { s.row_count };
            t_with(cx, "results.metaRowsShort", &[("ms", &s.duration_ms.to_string()), ("count", &count.to_string())])
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
        error.card(jump_button, cx).into_any_element()
    }

    // For the snapshot hook.
    pub fn grid_focus(&self, cx: &App) -> Option<FocusHandle> {
        Some(self.sets.get(self.active)?.grid.as_ref()?.focus_handle(cx))
    }

    fn grid_event(&mut self, grid: &Entity<Grid>, event: &GridEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            GridEvent::ViewCell { row, column } => {
                let grid = grid.read(cx);
                let name = grid.set().columns[*column].name.clone();
                let value = grid.set().display(*row, *column).map(str::to_string);
                crate::cell_viewer::open(name, value, window, cx);
            }
            GridEvent::Export => crate::export_dialog::open(grid.read(cx).export_source(), window, cx),
            GridEvent::FocusedRowChanged if self.is_active(grid) => cx.emit(ResultsEvent::FocusedRowChanged),
            _ => {}
        }
    }

    fn grid(&self, set: &ResultSetView, grid: &Entity<Grid>, cx: &mut Context<Self>) -> AnyElement {
        let rows =
            set.summary.as_ref().filter(|_| !set.streaming).map_or(grid.read(cx).set().rows() as i64, |s| s.row_count);
        let ms = set.summary.as_ref().map_or(0, |s| s.duration_ms);
        let meta = t_with(cx, "results.metaRows", &[("count", &rows.to_string()), ("ms", &ms.to_string())]);
        v_flex()
            .size_full()
            .child(grid::toolbar(grid, meta, cx))
            .child(div().flex_1().min_h_0().child(grid.clone()))
            .into_any_element()
    }
}

impl EventEmitter<ResultsEvent> for ResultsPanel {}

impl Render for ResultsPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match self.sets.get(self.active) {
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
            .when(self.sets.len() > 1, |el| el.child(self.tabs(cx)))
            .child(div().flex_1().min_h_0().child(body))
    }
}

#[cfg(test)]
mod label_tests {
    use super::ordinals;

    #[test]
    fn grids_and_plans_are_numbered_apart() {
        assert_eq!(ordinals([false, true, false, true]), [1, 1, 2, 2]);
        assert_eq!(ordinals([true, false]), [1, 1]);
        assert!(ordinals([]).is_empty());
    }
}
