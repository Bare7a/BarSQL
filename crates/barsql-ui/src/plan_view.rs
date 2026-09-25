use std::collections::HashSet;

use barsql_sql::QueryPlan;
use barsql_sql::plan::PlanNode;
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonGroup, ButtonVariants};
use gpui_kit::component::{ActiveTheme, Icon, Selectable, Sizable, StyledExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::form;
use crate::i18n::{I18n, t, t_with};
use crate::plan_tree::{
    Metric, PlanRow, available_metrics, collect_parent_keys, default_metric, flatten, format_cost, format_factor,
    format_ms, format_rows, is_estimate_off,
};
use crate::toast;
use crate::tokens::{ICON_2XS, ICON_SM, ICON_XS, RADIUS, RADIUS_SM, TEXT_2XS, TEXT_SM, TEXT_XS, TINT, TINT_BORDER};

const CONTEXT: &str = "PlanTree";
const INDENT_REM: f32 = 1.077;
const METRIC_REM: f32 = 10.;

actions!(plan, [SelectPrevious, SelectNext, Collapse, Expand, SelectFirst, SelectLast]);

pub fn init(cx: &mut App) {
    let context = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("up", SelectPrevious, context),
        KeyBinding::new("down", SelectNext, context),
        KeyBinding::new("left", Collapse, context),
        KeyBinding::new("right", Expand, context),
        KeyBinding::new("home", SelectFirst, context),
        KeyBinding::new("end", SelectLast, context),
    ]);
}

pub struct PlanView {
    plan: QueryPlan,
    metrics: Vec<Metric>,
    metric: Option<Metric>,
    collapsed: HashSet<String>,
    selected: Option<String>,
    raw: bool,
    focus: FocusHandle,
    scroll: ScrollHandle,
}

impl PlanView {
    pub fn new(plan: QueryPlan, cx: &mut Context<Self>) -> Self {
        Self {
            metrics: available_metrics(&plan),
            metric: default_metric(&plan),
            plan,
            collapsed: HashSet::new(),
            selected: Some("0".into()),
            raw: false,
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
        }
    }

    fn toggle(&mut self, key: &str, cx: &mut Context<Self>) {
        if !self.collapsed.remove(key) {
            self.collapsed.insert(key.to_string());
        }
        cx.notify();
    }

    fn toggle_all(&mut self, cx: &mut Context<Self>) {
        let parents = collect_parent_keys(&self.plan.nodes, "");
        if parents.iter().all(|key| self.collapsed.contains(key)) {
            self.collapsed.clear();
        } else {
            self.collapsed = parents.into_iter().collect();
        }
        cx.notify();
    }

    fn select(&mut self, key: String, cx: &mut Context<Self>) {
        self.selected = Some(key);
        cx.notify();
    }

    // Left collapses an open node or moves to its parent. Right expands a closed node or moves into it.
    fn step(&mut self, action: Step, cx: &mut Context<Self>) {
        let rows = flatten(&self.plan, self.metric, &self.collapsed);
        let Some(index) = rows.iter().position(|row| Some(&row.key) == self.selected.as_ref()) else { return };
        let row = &rows[index];
        let (key, has_children, open) = (row.key.clone(), row.has_children, !self.collapsed.contains(&row.key));
        let target = match action {
            Step::Next => rows.get(index + 1).map(|r| r.key.clone()),
            Step::Previous => index.checked_sub(1).map(|i| rows[i].key.clone()),
            Step::First => rows.first().map(|r| r.key.clone()),
            Step::Last => rows.last().map(|r| r.key.clone()),
            Step::Expand if has_children && !open => {
                self.toggle(&key, cx);
                None
            }
            Step::Expand if has_children => rows.get(index + 1).map(|r| r.key.clone()),
            Step::Collapse if has_children && open => {
                self.toggle(&key, cx);
                None
            }
            Step::Collapse => key.rfind('.').map(|dot| key[..dot].to_string()),
            _ => None,
        };
        if let Some(target) = target {
            self.select(target, cx);
        }
    }

    // README screenshots. Several nodes can tie for hottest, and the deepest one has the richest details.
    #[cfg(feature = "snapshot")]
    pub(crate) fn select_hottest(&mut self, cx: &mut Context<Self>) {
        let rows = flatten(&self.plan, self.metric, &self.collapsed);
        if let Some(key) = rows.iter().rev().find(|row| row.hottest).map(|row| row.key.clone()) {
            self.select(key, cx);
        }
    }

    fn copy_raw(&self, cx: &mut App) {
        cx.write_to_clipboard(ClipboardItem::new_string(self.plan.raw.clone()));
        toast::success(t(cx, "toast.copiedClipboard"), cx);
    }

    fn toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let lang = cx.global::<I18n>().lang();
        let (badge, color) = if self.plan.analyzed {
            (t(cx, "results.planAnalyzed"), theme.success)
        } else {
            (t(cx, "results.planEstimated"), theme.muted_foreground)
        };
        let stat = |label: SharedString, value: String| {
            h_flex()
                .gap(rems(0.308))
                .text_size(TEXT_SM)
                .whitespace_nowrap()
                .child(div().text_size(TEXT_XS).text_color(theme.muted_foreground).child(label))
                .child(value)
        };
        let mut stats = Vec::new();
        if self.plan.planning_ms.is_some() {
            stats.push(stat(t(cx, "results.planPlanning"), format_ms(self.plan.planning_ms, lang)));
        }
        if self.plan.execution_ms.is_some() {
            stats.push(stat(t(cx, "results.planExecution"), format_ms(self.plan.execution_ms, lang)));
        }
        if self.plan.total_cost.is_some() {
            stats.push(stat(t(cx, "results.planTotalCost"), format_cost(self.plan.total_cost, lang)));
        }
        let metrics = self.metrics.clone();
        let current = self.metric;
        let switch = (metrics.len() > 1 && !self.raw).then(|| {
            ButtonGroup::new("plan-metric")
                .small()
                .outline()
                .children(metrics.iter().map(|&metric| {
                    let name = t(cx, &format!("results.planMetric.{}", metric.key()));
                    Button::new(SharedString::from(format!("plan-metric-{}", metric.key())))
                        .debug_selector(move || format!("plan-metric-{}", metric.key()))
                        .label(name.clone())
                        .selected(current == Some(metric))
                        .tooltip(t_with(cx, "results.planHeatByMetric", &[("metric", &name)]))
                }))
                .on_click(cx.listener(move |view, clicked: &Vec<usize>, _, cx| {
                    if let Some(metric) = clicked.first().and_then(|ix| metrics.get(*ix)) {
                        view.metric = Some(*metric);
                        cx.notify();
                    }
                }))
        });
        let parents = collect_parent_keys(&self.plan.nodes, "");
        let all_collapsed = !parents.is_empty() && parents.iter().all(|key| self.collapsed.contains(key));
        let expand_label = t(cx, if all_collapsed { "results.planExpandAll" } else { "results.planCollapseAll" });
        h_flex()
            .flex_none()
            .gap(rems(0.615))
            .px(rems(0.615))
            .py(rems(0.308))
            .border_b_1()
            .border_color(theme.border)
            .overflow_hidden()
            .child(
                div()
                    .flex_none()
                    .px(rems(0.538))
                    .rounded_full()
                    .border_1()
                    .border_color(color.opacity(TINT_BORDER))
                    .bg(color.opacity(if self.plan.analyzed { TINT } else { 0. }))
                    .text_size(TEXT_2XS)
                    .font_semibold()
                    .text_color(color)
                    .child(badge.to_uppercase()),
            )
            .child(h_flex().gap(rems(0.923)).min_w_0().overflow_hidden().children(stats))
            .child(div().flex_1())
            .children(switch)
            .when(!self.raw && !parents.is_empty(), |el| {
                el.child(
                    Button::new("plan-toggle-all")
                        .small()
                        .ghost()
                        .w(rems(2.154))
                        .h(rems(1.846))
                        .p_0()
                        .child(Icon::new(Lucide::ListTree).size(ICON_SM))
                        .tooltip(expand_label)
                        .on_click(cx.listener(|view, _, _, cx| view.toggle_all(cx))),
                )
            })
            .child(
                Button::new("plan-raw")
                    .small()
                    .ghost()
                    .debug_selector(|| "plan-raw".into())
                    .w(rems(2.154))
                    .h(rems(1.846))
                    .p_0()
                    .child(Icon::new(Lucide::CodeXml).size(ICON_SM))
                    .selected(self.raw)
                    .tooltip(t(cx, "results.planRaw"))
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.raw = !view.raw;
                        cx.notify();
                    })),
            )
            .child(
                Button::new("plan-copy")
                    .small()
                    .ghost()
                    .w(rems(2.154))
                    .h(rems(1.846))
                    .p_0()
                    .child(Icon::new(Lucide::Copy).size(ICON_SM))
                    .tooltip(t(cx, "results.planCopy"))
                    .on_click(cx.listener(|view, _, _, cx| view.copy_raw(cx))),
            )
    }

    fn row(&self, row: &PlanRow, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let lang = cx.global::<I18n>().lang();
        let node = row.node;
        let selected = self.selected.as_ref() == Some(&row.key);
        let collapsed = self.collapsed.contains(&row.key);
        let estimate_off = is_estimate_off(row.estimate_factor, node.never_run);
        let key = row.key.clone();
        let twisty: AnyElement = if row.has_children {
            let key = key.clone();
            let selector = format!("plan-twisty-{key}");
            Button::new(SharedString::from(format!("plan-twisty-{key}")))
                .xsmall()
                .ghost()
                .debug_selector(move || selector)
                .child(Icon::new(if collapsed { Lucide::ChevronRight } else { Lucide::ChevronDown }).size(ICON_XS))
                .tooltip(t(cx, if collapsed { "results.planExpandNode" } else { "results.planCollapseNode" }))
                .on_click(cx.listener(move |view, _, _, cx| view.toggle(&key, cx)))
                .into_any_element()
        } else {
            div().flex_none().w(rems(1.231)).into_any_element()
        };
        let label_color = if node.never_run { theme.muted_foreground } else { theme.foreground };
        let main = h_flex()
            .flex_1()
            .min_w(rems(18.))
            .gap(rems(0.462))
            .pl(rems(0.308 + row.depth as f32 * INDENT_REM))
            .pr(rems(0.615))
            .py(rems(0.385))
            .overflow_hidden()
            .child(twisty)
            .child(div().flex_none().font_semibold().text_color(label_color).child(node.label.clone()))
            .when(!node.relation.is_empty(), |el| el.child(form::badge(node.relation.clone(), theme.primary)))
            .when(!node.index.is_empty(), |el| el.child(form::badge(node.index.clone(), theme.success)))
            .when(node.never_run, |el| el.child(form::badge(t(cx, "results.planNeverRun"), theme.muted_foreground)))
            .when(estimate_off, |el| {
                el.child(
                    h_flex()
                        .id(SharedString::from(format!("plan-estimate-{key}")))
                        .gap(rems(0.154))
                        .flex_none()
                        .px(rems(0.385))
                        .rounded(RADIUS_SM)
                        .text_size(TEXT_2XS)
                        .font_semibold()
                        .text_color(theme.warning)
                        .bg(theme.warning.opacity(TINT))
                        .child(Icon::new(Lucide::TriangleAlert).size(ICON_2XS))
                        .child(format_factor(row.estimate_factor, lang))
                        .tooltip({
                            let hint = t(cx, "results.planEstimateOffHint");
                            move |window, cx| gpui_kit::component::tooltip::Tooltip::new(hint.clone()).build(window, cx)
                        }),
                )
            })
            .when(!node.detail.is_empty(), |el| {
                el.child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(TEXT_XS)
                        .text_color(theme.muted_foreground)
                        .font_family(theme.mono_font_family.clone())
                        .child(node.detail.clone()),
                )
            });
        let cell =
            |main: String, sub: Option<String>| {
                h_flex()
                    .flex_none()
                    .w(rems(METRIC_REM))
                    .justify_end()
                    .gap(rems(0.385))
                    .px(rems(0.615))
                    .py(rems(0.385))
                    .whitespace_nowrap()
                    .child(main)
                    .children(sub.map(|sub| {
                        div().text_size(TEXT_XS).text_color(theme.muted_foreground).child(format!("/ {sub}"))
                    }))
            };
        let shown = |metric: Metric| self.metrics.contains(&metric);
        let rows = node.rows_actual.or(node.rows_planned);
        let rows_sub = node.rows_actual.and(node.rows_planned).map(|planned| format_rows(Some(planned), lang));
        let time_sub = match (node.time_ms, node.self_time_ms) {
            (Some(total), Some(own)) if total != own => Some(format_ms(Some(total), lang)),
            _ => None,
        };
        let cost_sub = match (node.cost_total, node.cost_self) {
            (Some(total), Some(own)) if total != own => Some(format_cost(Some(total), lang)),
            _ => None,
        };
        let heat_color = theme.danger;
        let selected_bg = theme.primary.opacity(TINT);
        let hover_bg = theme.table_hover;
        let divider = theme.border.opacity(0.45);
        let selector = format!("plan-row-{key}");
        h_flex()
            .id(SharedString::from(format!("plan-row-{key}")))
            .debug_selector(move || selector)
            .relative()
            .w_full()
            .border_b_1()
            .border_color(divider)
            .text_size(TEXT_SM)
            .when(selected, |el| el.bg(selected_bg))
            .when(!selected, |el| el.hover(move |style| style.bg(hover_bg)))
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left_0()
                    .w(relative(row.heat as f32))
                    .bg(heat_color.opacity(row.heat as f32 * 0.28)),
            )
            .when(row.hottest, |el| el.child(div().absolute().top_0().bottom_0().left_0().w(px(2.)).bg(heat_color)))
            .child(main)
            .when(shown(Metric::Rows), |el| el.child(cell(format_rows(rows, lang), rows_sub)))
            .when(shown(Metric::Time), |el| el.child(cell(format_ms(node.self_time_ms, lang), time_sub)))
            .when(shown(Metric::Cost), |el| el.child(cell(format_cost(node.cost_self, lang), cost_sub)))
            .on_click(cx.listener(move |view, _, window, cx| {
                window.focus(&view.focus, cx);
                view.select(key.clone(), cx);
            }))
            .into_any_element()
    }

    fn details(&self, node: &PlanNode, cx: &App) -> AnyElement {
        let theme = cx.theme();
        let lang = cx.global::<I18n>().lang();
        let mut stats: Vec<(SharedString, String)> = Vec::new();
        let mut push = |key: &str, value: Option<String>| {
            if let Some(value) = value {
                stats.push((t(cx, key), value));
            }
        };
        push("results.planRowsActual", node.rows_actual.map(|v| format_rows(Some(v), lang)));
        push("results.planRowsPlanned", node.rows_planned.map(|v| format_rows(Some(v), lang)));
        push("results.planLoops", node.loops.map(|v| format_rows(Some(v), lang)));
        push("results.planSelfTime", node.self_time_ms.map(|v| format_ms(Some(v), lang)));
        push("results.planTotalTime", node.time_ms.map(|v| format_ms(Some(v), lang)));
        push("results.planSelfCost", node.cost_self.map(|v| format_cost(Some(v), lang)));
        push("results.planTotalCost", node.cost_total.map(|v| format_cost(Some(v), lang)));
        let grid = |entries: Vec<(SharedString, String)>, mono: bool| {
            v_flex().gap(rems(0.231)).text_size(TEXT_XS).children(entries.into_iter().map(move |(label, value)| {
                h_flex()
                    .gap(rems(0.615))
                    .items_start()
                    .child(div().flex_none().w(rems(8.)).text_color(theme.muted_foreground).child(label))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .when(mono, |el| el.font_family(theme.mono_font_family.clone()))
                            .child(value),
                    )
            }))
        };
        let fields: Vec<(SharedString, String)> =
            node.fields.iter().map(|field| (field.key.clone().into(), field.value.clone())).collect();
        v_flex()
            .id("plan-details")
            .debug_selector(|| "plan-details".into())
            .flex_none()
            .w(rems(21.))
            .h_full()
            .overflow_y_scroll()
            .p(rems(0.769))
            .gap(rems(0.615))
            .border_l_1()
            .border_color(theme.border)
            .bg(theme.background)
            .child(
                h_flex()
                    .flex_wrap()
                    .gap(rems(0.462))
                    .child(div().font_semibold().child(node.label.clone()))
                    .when(!node.relation.is_empty(), |el| {
                        el.child(
                            div()
                                .text_size(TEXT_XS)
                                .font_family(theme.mono_font_family.clone())
                                .text_color(theme.primary)
                                .child(node.relation.clone()),
                        )
                    })
                    .when(!node.index.is_empty(), |el| {
                        el.child(
                            div()
                                .text_size(TEXT_XS)
                                .font_family(theme.mono_font_family.clone())
                                .text_color(theme.success)
                                .child(node.index.clone()),
                        )
                    }),
            )
            .when(!node.detail.is_empty(), |el| {
                el.child(
                    div()
                        .p(rems(0.462))
                        .rounded(RADIUS)
                        .bg(theme.sidebar)
                        .text_size(TEXT_XS)
                        .font_family(theme.mono_font_family.clone())
                        .child(node.detail.clone()),
                )
            })
            .when(!stats.is_empty(), |el| el.child(grid(stats, false)))
            .when(!fields.is_empty(), |el| {
                el.child(div().pt(rems(0.769)).border_t_1().border_color(theme.border).child(grid(fields, true)))
            })
            .into_any_element()
    }
}

#[cfg(test)]
impl PlanView {
    pub(crate) fn analyzed(&self) -> bool {
        self.plan.analyzed
    }

    pub(crate) fn notes(&self) -> Vec<String> {
        self.plan.notes.clone()
    }

    pub(crate) fn metrics(&self) -> Vec<Metric> {
        self.metrics.clone()
    }

    // Same order as the table's metric columns.
    #[cfg(feature = "e2e")]
    pub(crate) fn columns(&self) -> Vec<Metric> {
        [Metric::Rows, Metric::Time, Metric::Cost].into_iter().filter(|metric| self.metrics.contains(metric)).collect()
    }

    // Visible rows as (key, label, heat).
    pub(crate) fn rows(&self) -> Vec<(String, String, f64)> {
        flatten(&self.plan, self.metric, &self.collapsed)
            .iter()
            .map(|row| (row.key.clone(), row.node.label.clone(), row.heat))
            .collect()
    }

    pub(crate) fn raw_text(&self) -> Option<String> {
        self.raw.then(|| self.plan.raw.clone())
    }
}

#[derive(Clone, Copy)]
enum Step {
    Previous,
    Next,
    Collapse,
    Expand,
    First,
    Last,
}

impl Render for PlanView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let notes: Vec<SharedString> = self
            .plan
            .notes
            .iter()
            .map(|note| {
                let key = format!("results.planNote.{note}");
                let text = t(cx, &key);
                if *text == *key { note.clone().into() } else { text }
            })
            .collect();
        let body: AnyElement = if self.raw {
            div()
                .id("plan-raw")
                .debug_selector(|| "plan-raw-text".into())
                .flex_1()
                .min_h_0()
                .overflow_scroll()
                .p(rems(0.769))
                .text_size(TEXT_XS)
                .font_family(theme.mono_font_family.clone())
                .whitespace_nowrap()
                .child(self.plan.raw.clone())
                .into_any_element()
        } else {
            let plan = self.plan.clone();
            let rows = flatten(&plan, self.metric, &self.collapsed);
            let selected = rows.iter().find(|row| Some(&row.key) == self.selected.as_ref()).map(|row| row.node.clone());
            let head = |key: &str, width: Option<f32>| {
                div()
                    .when_some(width, |el, width| el.flex_none().w(rems(width)).text_right())
                    .when(width.is_none(), |el| el.flex_1().min_w(rems(18.)))
                    .px(rems(0.615))
                    .py(rems(0.385))
                    .child(t(cx, key))
            };
            let shown = |metric: Metric| self.metrics.contains(&metric);
            let header = h_flex()
                .flex_none()
                .border_b_1()
                .border_color(theme.border)
                .bg(theme.sidebar)
                .text_size(TEXT_XS)
                .text_color(theme.muted_foreground)
                .child(head("results.planNode", None))
                .when(shown(Metric::Rows), |el| el.child(head("results.planColRows", Some(METRIC_REM))))
                .when(shown(Metric::Time), |el| el.child(head("results.planColTime", Some(METRIC_REM))))
                .when(shown(Metric::Cost), |el| el.child(head("results.planColCost", Some(METRIC_REM))));
            let items: Vec<AnyElement> = rows.iter().map(|row| self.row(row, cx)).collect();
            h_flex()
                .flex_1()
                .min_h_0()
                .items_start()
                .child(
                    v_flex()
                        .id("plan-tree")
                        .key_context(CONTEXT)
                        .track_focus(&self.focus)
                        .flex_1()
                        .h_full()
                        .min_w_0()
                        .overflow_scroll()
                        .track_scroll(&self.scroll)
                        .on_action(cx.listener(|view, _: &SelectPrevious, _, cx| view.step(Step::Previous, cx)))
                        .on_action(cx.listener(|view, _: &SelectNext, _, cx| view.step(Step::Next, cx)))
                        .on_action(cx.listener(|view, _: &Collapse, _, cx| view.step(Step::Collapse, cx)))
                        .on_action(cx.listener(|view, _: &Expand, _, cx| view.step(Step::Expand, cx)))
                        .on_action(cx.listener(|view, _: &SelectFirst, _, cx| view.step(Step::First, cx)))
                        .on_action(cx.listener(|view, _: &SelectLast, _, cx| view.step(Step::Last, cx)))
                        .child(header)
                        .children(items),
                )
                .children(selected.map(|node| self.details(&node, cx)))
                .into_any_element()
        };
        v_flex()
            .size_full()
            .bg(theme.sidebar)
            .child(self.toolbar(cx))
            .when(!notes.is_empty(), |el| {
                el.child(
                    h_flex()
                        .flex_none()
                        .flex_wrap()
                        .gap(rems(0.462))
                        .px(rems(0.769))
                        .py(rems(0.385))
                        .border_b_1()
                        .border_color(theme.border)
                        .bg(theme.background)
                        .children(notes.into_iter().map(|note| {
                            h_flex()
                                .gap(rems(0.385))
                                .text_size(TEXT_XS)
                                .text_color(theme.muted_foreground)
                                .child(div().text_color(theme.warning).child("•"))
                                .child(note)
                        })),
                )
            })
            .child(body)
    }
}

#[cfg(test)]
mod tests {
    use barsql_sql::QueryPlan;
    use barsql_sql::plan::PlanNode;
    use gpui_kit::{Entity, TestAppContext, VisualTestContext};

    use super::{PlanView, Step};
    use crate::test_support::Env;

    fn plan() -> QueryPlan {
        let node =
            |label: &str, children: Vec<PlanNode>| PlanNode { label: label.into(), children, ..Default::default() };
        QueryPlan {
            nodes: vec![node(
                "Hash Join",
                vec![node("Seq Scan", vec![]), node("Hash", vec![node("Index Scan", vec![])])],
            )],
            ..Default::default()
        }
    }

    fn selected(view: &Entity<PlanView>, cx: &mut VisualTestContext) -> Option<String> {
        view.read_with(cx, |view, _| view.selected.clone())
    }

    #[gpui_kit::test]
    fn arrows_walk_open_and_close_the_tree(cx: &mut TestAppContext) {
        let _env = Env::new(cx);
        let (view, cx) = cx.add_window_view(|_, cx| PlanView::new(plan(), cx));
        let step = |step: Step, cx: &mut VisualTestContext| view.update(cx, |view, cx| view.step(step, cx));
        step(Step::Last, cx);
        assert_eq!(selected(&view, cx).as_deref(), Some("0.1.0"));
        step(Step::Collapse, cx);
        assert_eq!(selected(&view, cx).as_deref(), Some("0.1"), "a leaf climbs to its parent");
        step(Step::Collapse, cx);
        assert!(view.read_with(cx, |view, _| view.collapsed.contains("0.1")), "an open parent closes first");
        step(Step::Next, cx);
        assert_eq!(selected(&view, cx).as_deref(), Some("0.1"), "no hidden rows to step into");
        step(Step::Expand, cx);
        step(Step::Expand, cx);
        assert_eq!(selected(&view, cx).as_deref(), Some("0.1.0"));
        view.update(cx, |view, cx| view.toggle_all(cx));
        assert_eq!(view.read_with(cx, |view, _| view.collapsed.len()), 2);
        view.update(cx, |view, cx| view.toggle_all(cx));
        assert!(view.read_with(cx, |view, _| view.collapsed.is_empty()));
    }
}
