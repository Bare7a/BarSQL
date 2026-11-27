use std::collections::HashSet;

use barsql_sql::QueryPlan;
use barsql_sql::plan::PlanNode;
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::{ContextMenuExt, PopupMenu, PopupMenuItem};
use gpui_kit::component::scroll::ScrollbarAxis;
use gpui_kit::component::{ActiveTheme, Icon, Selectable, Sizable, StyledExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::form::{self, ToolButton as _};
use crate::i18n::{I18n, t, t_count, t_with};
use crate::plan_tree::{
    Metric, PlanRow, available_metrics, collect_parent_keys, default_metric, flatten, format_cost, format_factor,
    format_ms, format_rows, is_estimate_off, is_full_scan,
};
use crate::scrollbars::HoverScrollbar as _;
use crate::toast;
use crate::tokens::{ICON_XS, RADIUS, TEXT_SM, TEXT_XS, TINT};

const CONTEXT: &str = "PlanTree";
const INDENT_REM: f32 = 1.077;
const METRIC_REM: f32 = 10.;
const NODE_REM: f32 = 18.;
const DETAILS_REM: f32 = 23.;
const STAT_GAP_REM: f32 = 0.308;
const STATS_GAP_REM: f32 = 0.923;

actions!(plan, [SelectPrevious, SelectNext, Collapse, Expand, SelectFirst, SelectLast]);

type ViewAction = Box<dyn Fn(&mut PlanView, &mut Context<PlanView>)>;

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
    // Whether the details show the fields at zero or false too.
    all_fields: bool,
    fit: Fit,
    stats_room: Option<Pixels>,
    focus: FocusHandle,
    scroll: ScrollHandle,
    details_scroll: ScrollHandle,
    raw_scroll: ScrollHandle,
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
            all_fields: false,
            fit: Fit::All,
            stats_room: None,
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            details_scroll: ScrollHandle::new(),
            raw_scroll: ScrollHandle::new(),
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

    fn collapse_all(&mut self, collapsed: bool, cx: &mut Context<Self>) {
        self.collapsed = match collapsed {
            true => collect_parent_keys(&self.plan.nodes, "").into_iter().collect(),
            false => Default::default(),
        };
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

    // Ties go to the deepest node, which has the richest details. Its label registers as `plan-node-{key}`.
    #[cfg(feature = "snapshot")]
    pub(crate) fn hottest(&self) -> Option<String> {
        let rows = flatten(&self.plan, self.metric, &self.collapsed);
        rows.iter().rev().find(|row| row.hottest).map(|row| row.key.clone())
    }

    #[cfg(feature = "snapshot")]
    pub(crate) fn select_hottest(&mut self, cx: &mut Context<Self>) {
        if let Some(key) = self.hottest() {
            self.select(key, cx);
        }
    }

    pub(crate) fn copy_raw(&self, cx: &mut App) {
        cx.write_to_clipboard(ClipboardItem::new_string(self.plan.raw.clone()));
        toast::success(t(cx, "toast.copiedClipboard"), cx);
    }

    fn fit_for(&self, width: Pixels, rem: Pixels) -> Fit {
        let need = |columns: usize, details: f32| rem * (NODE_REM + METRIC_REM * columns as f32 + details);
        let columns = self.metrics.len();
        if width >= need(columns, DETAILS_REM) {
            Fit::All
        } else if width >= need(columns.min(1), DETAILS_REM) {
            Fit::Picked
        } else {
            Fit::TreeOnly
        }
    }

    pub(crate) fn columns(&self) -> Vec<Metric> {
        let picked = self.metric.or(self.metrics.first().copied());
        [Metric::Rows, Metric::Time, Metric::Cost]
            .into_iter()
            .filter(|&metric| self.metrics.contains(&metric) && (self.fit == Fit::All || Some(metric) == picked))
            .collect()
    }

    fn toolbar(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let lang = cx.global::<I18n>().lang();
        let (badge, color) = if self.plan.analyzed {
            (t(cx, "results.planAnalyzed"), theme.success)
        } else {
            (t(cx, "results.planEstimated"), theme.muted_foreground)
        };
        // Each with how long it stays when they don't all fit: the total cost goes first, the execution time last.
        let mut stats: Vec<(u8, &'static str, SharedString, SharedString)> = Vec::new();
        if self.plan.planning_ms.is_some() {
            let value = format_ms(self.plan.planning_ms, lang).into();
            stats.push((1, "plan-stat-planning", t(cx, "results.planPlanning"), value));
        }
        if self.plan.execution_ms.is_some() {
            let value = format_ms(self.plan.execution_ms, lang).into();
            stats.push((2, "plan-stat-execution", t(cx, "results.planExecution"), value));
        }
        if self.plan.total_cost.is_some() {
            let value = format_cost(self.plan.total_cost, lang).into();
            stats.push((0, "plan-stat-cost", t(cx, "results.planTotalCost"), value));
        }
        if let Some(room) = self.stats_room {
            let (rem, ui) = (window.rem_size(), font(theme.font_family.clone()));
            let width = |(_, _, label, value): &(u8, &str, SharedString, SharedString)| {
                text_width(window, label.clone(), &ui, TEXT_XS.to_pixels(rem))
                    + rem * STAT_GAP_REM
                    + text_width(window, value.clone(), &ui, TEXT_SM.to_pixels(rem))
            };
            // A quarter rem spare, as the layout rounds each edge to a whole pixel.
            let gaps = |count: usize| rem * (STATS_GAP_REM * count.saturating_sub(1) as f32 + 0.25);
            let total = |stats: &[(u8, &str, SharedString, SharedString)]| {
                stats.iter().map(width).fold(gaps(stats.len()), |sum, width| sum + width)
            };
            while !stats.is_empty() && total(&stats) > room {
                let first = (0..stats.len()).min_by_key(|&i| stats[i].0).unwrap_or(0);
                stats.remove(first);
            }
        }
        let stat = |id: &'static str, label: SharedString, value: SharedString| {
            h_flex()
                .debug_selector(move || id.into())
                .gap(rems(STAT_GAP_REM))
                .text_size(TEXT_SM)
                .whitespace_nowrap()
                .child(div().text_size(TEXT_XS).text_color(theme.muted_foreground).child(label))
                .child(value)
        };
        let metrics = self.metrics.clone();
        let current = self.metric;
        let switch = (metrics.len() > 1 && !self.raw).then(|| {
            form::toggle_group().flex_none().children(metrics.iter().map(|&metric| {
                let name = t(cx, &format!("results.planMetric.{}", metric.key()));
                let id = SharedString::from(format!("plan-metric-{}", metric.key()));
                form::toggle_small(id, name.clone(), current == Some(metric))
                    .debug_selector(move || format!("plan-metric-{}", metric.key()))
                    .tooltip(t_with(cx, "results.planHeatByMetric", &[("metric", &name)]))
                    .on_click(cx.listener(move |view, _, _, cx| {
                        view.metric = Some(metric);
                        cx.notify();
                    }))
            }))
        });
        let parents = collect_parent_keys(&self.plan.nodes, "");
        let all_collapsed = !parents.is_empty() && parents.iter().all(|key| self.collapsed.contains(key));
        let expand_label = t(cx, if all_collapsed { "results.planExpandAll" } else { "results.planCollapseAll" });
        h_flex()
            .flex_none()
            .gap(rems(0.615))
            .px(rems(0.769))
            .py(rems(0.462))
            .border_b_1()
            .border_color(theme.border)
            .overflow_hidden()
            .child(form::badge(badge.to_uppercase(), color))
            .child(
                h_flex()
                    .debug_selector(|| "plan-stats".into())
                    .flex_1()
                    .min_w_0()
                    .gap(rems(STATS_GAP_REM))
                    .overflow_hidden()
                    .children(stats.into_iter().map(|(_, id, label, value)| stat(id, label, value)))
                    .child(watch_width(cx.entity(), |view, width, _| view.stats_room.replace(width) != Some(width))),
            )
            .children(switch)
            .when(!self.raw && !parents.is_empty(), |el| {
                el.child(
                    Button::new("plan-toggle-all")
                        .ghost()
                        .tool_icon(Icon::new(Lucide::ListTree), ICON_XS)
                        .tooltip(expand_label)
                        .on_click(cx.listener(|view, _, _, cx| view.toggle_all(cx))),
                )
            })
            .child(
                Button::new("plan-raw")
                    .ghost()
                    .debug_selector(|| "plan-raw".into())
                    .tool_icon(Icon::new(Lucide::CodeXml), ICON_XS)
                    .selected(self.raw)
                    .tooltip(t(cx, "results.planRaw"))
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.raw = !view.raw;
                        cx.notify();
                    })),
            )
            .child(
                Button::new("plan-copy")
                    .ghost()
                    .tool_icon(Icon::new(Lucide::Copy), ICON_XS)
                    .tooltip(t(cx, "results.planCopy"))
                    .on_click(cx.listener(|view, _, _, cx| view.copy_raw(cx))),
            )
    }

    fn row(&self, row: &PlanRow, columns: &[Metric], cx: &mut Context<Self>) -> AnyElement {
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
            .id(SharedString::from(format!("plan-node-{key}")))
            .flex_1()
            .min_w(rems(NODE_REM))
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
            .when(is_full_scan(node), |el| {
                let hint = t(cx, "results.planFullScanHint");
                el.child(
                    form::badge(t(cx, "results.planFullScan"), theme.warning)
                        .id(SharedString::from(format!("plan-full-scan-{key}")))
                        .tooltip(move |window, cx| {
                            gpui_kit::component::tooltip::Tooltip::new(hint.clone()).build(window, cx)
                        }),
                )
            })
            .when(estimate_off, |el| {
                el.child(
                    form::icon_badge(Lucide::TriangleAlert, format_factor(row.estimate_factor, lang), theme.warning)
                        .id(SharedString::from(format!("plan-estimate-{key}")))
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
            })
            // The row has a context menu, which takes no test support after it.
            .test_support();
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
        let shown = |metric: Metric| columns.contains(&metric);
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
            .on_click(cx.listener({
                let key = key.clone();
                move |view, _, window, cx| {
                    window.focus(&view.focus, cx);
                    view.select(key.clone(), cx);
                }
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener({
                    let key = key.clone();
                    move |view, _, _, cx| view.select(key.clone(), cx)
                }),
            )
            .context_menu({
                let view = cx.entity().downgrade();
                let has_children = row.has_children;
                move |menu, _, cx| Self::row_menu(&view, key.clone(), has_children, menu, cx)
            })
            .into_any_element()
    }

    // On a node: opening and closing the tree, and the engine's own output.
    fn row_menu(
        view: &WeakEntity<Self>,
        key: String,
        has_children: bool,
        menu: PopupMenu,
        cx: &mut Context<PopupMenu>,
    ) -> PopupMenu {
        let Some(view) = view.upgrade() else { return menu };
        let collapsed = view.read(cx).collapsed.contains(&key);
        let run = |f: ViewAction| {
            let view = view.clone();
            move |_: &ClickEvent, _: &mut Window, cx: &mut App| view.update(cx, |view, cx| f(view, cx))
        };
        let node = if collapsed { "results.planExpandNode" } else { "results.planCollapseNode" };
        menu.item(
            PopupMenuItem::new(t(cx, node))
                .disabled(!has_children)
                .on_click(run(Box::new(move |view, cx| view.toggle(&key, cx)))),
        )
        .item(
            PopupMenuItem::new(t(cx, "results.planExpandAll"))
                .on_click(run(Box::new(|view, cx| view.collapse_all(false, cx)))),
        )
        .item(
            PopupMenuItem::new(t(cx, "results.planCollapseAll"))
                .on_click(run(Box::new(|view, cx| view.collapse_all(true, cx)))),
        )
        .separator()
        .item(PopupMenuItem::new(t(cx, "results.planCopy")).on_click(run(Box::new(|view, cx| view.copy_raw(cx)))))
    }

    fn details(&self, node: &PlanNode, window: &Window, cx: &Context<Self>) -> AnyElement {
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
        let fields: Vec<(SharedString, String)> = node
            .fields
            .iter()
            .filter(|field| self.all_fields || !quiet(&field.value))
            .map(|field| (field.key.clone().into(), field.value.clone()))
            .collect();
        // As wide as the longest label shown, so labels like "Shared Read Blocks" stay on one line.
        let rem = window.rem_size();
        let label_font = font(theme.font_family.clone());
        let label_width = stats
            .iter()
            .chain(&fields)
            .map(|(label, _)| text_width(window, label.clone(), &label_font, TEXT_XS.to_pixels(rem)))
            .fold(px(0.), |widest, width| if width > widest { width } else { widest })
            .min(rem * 11.);
        // Values are mono, measured or straight from EXPLAIN, so their digits line up.
        let grid = |entries: Vec<(SharedString, String)>| {
            v_flex().gap(rems(0.231)).text_size(TEXT_XS).children(entries.into_iter().map(move |(label, value)| {
                h_flex()
                    .gap(rems(0.615))
                    .items_start()
                    .child(div().flex_none().w(label_width).text_color(theme.muted_foreground).child(label))
                    .child(div().flex_1().min_w_0().font_family(theme.mono_font_family.clone()).child(value))
            }))
        };
        let hidden = node.fields.iter().filter(|field| quiet(&field.value)).count();
        let more = (hidden > 0).then(|| {
            let label = match self.all_fields {
                true => t(cx, "results.planHideQuiet"),
                false => t_count(cx, "results.planShowQuiet", hidden as i64, &[]),
            };
            Button::new("plan-quiet-fields")
                .debug_selector(|| "plan-quiet-fields".into())
                .ghost()
                .tool_label(label)
                // Its label lines up with the field names above.
                .ml(rems(-0.615))
                .on_click(cx.listener(|view, _, _, cx| {
                    view.all_fields = !view.all_fields;
                    cx.notify();
                }))
        });
        let details = v_flex()
            .id("plan-details")
            .debug_selector(|| "plan-details".into())
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.details_scroll)
            .p(rems(0.769))
            .gap(rems(0.615))
            .border_l_1()
            .border_color(theme.border)
            .bg(theme.sidebar)
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
            .when(!stats.is_empty(), |el| el.child(grid(stats)))
            .when(!fields.is_empty() || more.is_some(), |el| {
                el.child(
                    v_flex()
                        .items_start()
                        .gap(rems(0.462))
                        .pt(rems(0.769))
                        .border_t_1()
                        .border_color(theme.border)
                        .child(grid(fields))
                        .children(more),
                )
            });
        div()
            .relative()
            .flex_none()
            .w(rems(DETAILS_REM))
            .h_full()
            .child(details)
            .hover_scrollbar(&self.details_scroll, ScrollbarAxis::Vertical)
            .into_any_element()
    }
}

// Up to a whole pixel, as a text element lays itself out.
fn text_width(window: &Window, text: SharedString, font: &Font, size: Pixels) -> Pixels {
    let run = TextRun {
        len: text.len(),
        font: font.clone(),
        color: Hsla::default(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    window.text_system().shape_line(text, size, &[run], None).width().ceil()
}

// Paints nothing. Hands `store` its width and the rem at each paint, and draws again when `store` says the layout
// changes, since only the paint knows the width.
fn watch_width(
    view: Entity<PlanView>,
    store: impl FnOnce(&mut PlanView, Pixels, Pixels) -> bool + 'static,
) -> impl IntoElement {
    canvas(
        move |bounds, window, cx| {
            let rem = window.rem_size();
            if view.update(cx, |view, _| store(view, bounds.size.width, rem)) {
                window.defer(cx, |window, _| window.refresh());
            }
        },
        |_, _, _, _| {},
    )
    .absolute()
    .size_full()
}

// A field that only says nothing happened: no blocks hit, a feature off. They wait behind Show more, so the few that
// matter stand out.
fn quiet(value: &str) -> bool {
    let value = value.trim();
    // 0, 0.0 or 0.000: nothing but zeros and one point.
    let zero =
        value.starts_with('0') && value.bytes().all(|b| b == b'0' || b == b'.') && value.matches('.').count() <= 1;
    value.is_empty() || value == "false" || zero
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

// What fits beside the tree at the view's width when it was last painted.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Fit {
    All,
    // Only the column of the metric the switch picked. The details still give every number.
    Picked,
    // The details step aside too, as the JSON viewer does in a narrow window.
    TreeOnly,
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
            let raw = div()
                .id("plan-raw")
                .debug_selector(|| "plan-raw-text".into())
                .size_full()
                .overflow_scroll()
                .track_scroll(&self.raw_scroll)
                .p(rems(0.769))
                .text_size(TEXT_XS)
                .font_family(theme.mono_font_family.clone())
                .whitespace_nowrap()
                .child(self.plan.raw.clone());
            div()
                .relative()
                .flex_1()
                .min_h_0()
                .child(raw)
                .hover_scrollbar(&self.raw_scroll, ScrollbarAxis::Both)
                .into_any_element()
        } else {
            let plan = self.plan.clone();
            let rows = flatten(&plan, self.metric, &self.collapsed);
            let selected = rows.iter().find(|row| Some(&row.key) == self.selected.as_ref()).map(|row| row.node.clone());
            let head = |key: &str, width: Option<f32>| {
                div()
                    .when_some(width, |el, width| el.flex_none().w(rems(width)).text_right())
                    .when(width.is_none(), |el| el.flex_1().min_w(rems(NODE_REM)))
                    .px(rems(0.615))
                    .py(rems(0.385))
                    .child(t(cx, key))
            };
            let columns = self.columns();
            let shown = |metric: Metric| columns.contains(&metric);
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
            let items: Vec<AnyElement> = rows.iter().map(|row| self.row(row, &columns, cx)).collect();
            let tree = v_flex()
                .id("plan-tree")
                .key_context(CONTEXT)
                .track_focus(&self.focus)
                .size_full()
                .overflow_scroll()
                .track_scroll(&self.scroll)
                .on_action(cx.listener(|view, _: &SelectPrevious, _, cx| view.step(Step::Previous, cx)))
                .on_action(cx.listener(|view, _: &SelectNext, _, cx| view.step(Step::Next, cx)))
                .on_action(cx.listener(|view, _: &Collapse, _, cx| view.step(Step::Collapse, cx)))
                .on_action(cx.listener(|view, _: &Expand, _, cx| view.step(Step::Expand, cx)))
                .on_action(cx.listener(|view, _: &SelectFirst, _, cx| view.step(Step::First, cx)))
                .on_action(cx.listener(|view, _: &SelectLast, _, cx| view.step(Step::Last, cx)))
                .child(header)
                .children(items);
            h_flex()
                .flex_1()
                .min_h_0()
                .items_start()
                .child(
                    div()
                        .relative()
                        .flex_1()
                        .h_full()
                        .min_w_0()
                        .child(tree)
                        .hover_scrollbar(&self.scroll, ScrollbarAxis::Both),
                )
                .children(selected.filter(|_| self.fit != Fit::TreeOnly).map(|node| self.details(&node, window, cx)))
                .into_any_element()
        };
        v_flex()
            .size_full()
            .bg(theme.sidebar)
            .child(watch_width(cx.entity(), |view, width, rem| {
                let fit = view.fit_for(width, rem);
                std::mem::replace(&mut view.fit, fit) != fit
            }))
            .child(self.toolbar(window, cx))
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
    use gpui_kit::{Entity, TestAppContext, VisualTestContext, px, size};

    use super::{DETAILS_REM, METRIC_REM, NODE_REM, PlanView, Step, quiet};
    use crate::plan_tree::Metric;
    use crate::test_support::Env;

    #[test]
    fn fields_that_only_say_nothing_happened_are_quiet() {
        for value in ["0", "0.0", "0.000", "false", "", " "] {
            assert!(quiet(value), "{value:?}");
        }
        for value in ["24", "0.038", "Inner", "(score > 10)", "true", "-1", "00:01"] {
            assert!(!quiet(value), "{value:?}");
        }
    }

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

    // Short of room, the table keeps only the metric the switch picked, then the details step aside. The toolbar's
    // numbers go whole, the total cost first and the execution time last.
    #[gpui_kit::test]
    fn a_narrow_plan_keeps_the_picked_column_then_drops_the_details(cx: &mut TestAppContext) {
        let _env = Env::new(cx);
        let node = PlanNode {
            label: "Seq Scan".into(),
            rows_actual: Some(5.),
            self_time_ms: Some(0.1),
            cost_self: Some(1.),
            ..Default::default()
        };
        let plan = QueryPlan {
            nodes: vec![node],
            analyzed: true,
            planning_ms: Some(0.5),
            execution_ms: Some(0.4),
            total_cost: Some(48.34),
            ..Default::default()
        };
        let (view, cx) = cx.add_window_view(|_, cx| PlanView::new(plan, cx));
        let rem = cx.update(|window, _| window.rem_size());
        let (all, picked) = (NODE_REM + 3. * METRIC_REM + DETAILS_REM, NODE_REM + METRIC_REM + DETAILS_REM);
        let mut counts = Vec::new();
        for (width, columns, details) in [
            (all, &[Metric::Rows, Metric::Time, Metric::Cost][..], true),
            (all - 1., &[Metric::Time][..], true),
            (picked - 1., &[Metric::Time][..], false),
            (NODE_REM + METRIC_REM, &[Metric::Time][..], false),
        ] {
            cx.simulate_resize(size(rem * width, px(600.)));
            // One frame measures the widths, the next draws for them.
            for _ in 0..2 {
                cx.update(|window, cx| window.draw(cx).clear(cx));
            }
            assert_eq!(view.read_with(cx, |view, _| view.columns()), columns, "{width} rem");
            assert_eq!(cx.debug_bounds("plan-details").is_some(), details, "{width} rem");
            let room = cx.debug_bounds("plan-stats").expect("the toolbar's numbers");
            let stats =
                ["plan-stat-cost", "plan-stat-planning", "plan-stat-execution"].map(|stat| cx.debug_bounds(stat));
            for bounds in stats.iter().flatten() {
                assert!(bounds.right() <= room.right(), "{width} rem: a number is cut at {bounds:?} in {room:?}");
            }
            assert!(stats.windows(2).all(|pair| pair[0].is_none() || pair[1].is_some()), "{width} rem: {stats:?}");
            counts.push(stats.iter().flatten().count());
        }
        assert!(counts[0] == 3 && counts[3] < 3, "{counts:?}");
    }
}
