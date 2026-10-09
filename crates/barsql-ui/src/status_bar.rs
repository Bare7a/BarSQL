use crate::grid::Aggregate;
use crate::i18n::{I18n, format_number, t, t_count, t_with};
use crate::tokens::{ICON_XS, TEXT_XS};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::{ActiveTheme, Icon, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

// 1-based, as editors show it. `selections` counts what Select Next Occurrence holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caret {
    pub line: usize,
    pub column: usize,
    pub selected: usize,
    pub selections: usize,
}

pub struct StatusBarState {
    pub connection: Option<(SharedString, bool)>,
    pub read_only: bool,
    // The connection asks before changes.
    pub protected: bool,
    // An open transaction, and whether a statement in it failed.
    pub transaction: Option<bool>,
    pub caret: Option<Caret>,
    pub aggregate: Option<Aggregate>,
    pub status: SharedString,
    pub error: bool,
}

fn caret_label(caret: Caret, cx: &App) -> String {
    let position =
        t_with(cx, "status.caret", &[("line", &caret.line.to_string()), ("column", &caret.column.to_string())]);
    match (caret.selections, caret.selected) {
        (n, _) if n > 1 => format!("{position} · {}", t_count(cx, "status.selections", n as i64, &[])),
        (_, 0) => position.to_string(),
        (_, chars) => format!("{position} · {}", t_count(cx, "status.selected", chars as i64, &[])),
    }
}

// Count, then the sum and average of the numbers among the cells, if any.
fn aggregate_label(total: Aggregate, cx: &App) -> String {
    let lang = cx.global::<I18n>().lang();
    let count = t_with(cx, "status.count", &[("count", &format_number(total.count as f64, 0, lang))]);
    if total.numbers == 0 {
        return count.to_string();
    }
    let sum = t_with(cx, "status.sum", &[("sum", &format_number(total.sum, 6, lang))]);
    let average = t_with(cx, "status.average", &[("avg", &format_number(total.sum / total.numbers as f64, 4, lang))]);
    format!("{count} · {sum} · {average}")
}

pub fn render(state: StatusBarState, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    let left = match state.connection {
        Some((label, connected)) => h_flex()
            .gap_1p5()
            .child(div().size(px(8.)).rounded_full().bg(if connected { theme.success } else { theme.muted_foreground }))
            .child(label)
            .when(state.read_only, |el| {
                el.child(
                    h_flex()
                        .gap_1()
                        .ml_1()
                        .text_color(theme.warning)
                        .child(Icon::new(Lucide::Lock).size(ICON_XS))
                        .child(t(cx, "app.readOnly")),
                )
            })
            .when(state.protected && !state.read_only, |el| {
                el.child(
                    h_flex()
                        .gap_1()
                        .ml_1()
                        .text_color(theme.warning)
                        .child(Icon::new(Lucide::ShieldCheck).size(ICON_XS))
                        .child(t(cx, "status.protected")),
                )
            })
            .when_some(state.transaction, |el, failed| {
                let (key, color) =
                    if failed { ("editor.txnError", theme.danger) } else { ("editor.txnActive", theme.warning) };
                el.child(
                    h_flex()
                        .gap_1()
                        .ml_1()
                        .text_color(color)
                        .child(div().size(px(6.)).rounded_full().bg(color))
                        .child(t(cx, key)),
                )
            }),
        None => h_flex().child(t(cx, "app.statusNoConnection")),
    };
    let segment = |text: String| div().flex_none().whitespace_nowrap().child(text);
    h_flex()
        .h(px(22.))
        .flex_none()
        .px_3()
        .gap_4()
        .justify_between()
        .text_size(TEXT_XS)
        .text_color(theme.muted_foreground)
        .bg(theme.status_bar)
        .border_t_1()
        .border_color(theme.status_bar_border)
        .child(left.flex_none())
        .child(
            h_flex()
                .min_w_0()
                .gap_4()
                .children(state.aggregate.map(|total| segment(aggregate_label(total, cx)).text_color(theme.foreground)))
                .children(state.caret.map(|caret| segment(caret_label(caret, cx))))
                .child(
                    h_flex()
                        .min_w_0()
                        .gap_1()
                        .when(state.error, |el| {
                            el.text_color(theme.danger).child(Icon::new(Lucide::CircleAlert).size(ICON_XS).flex_none())
                        })
                        .child(div().truncate().child(state.status)),
                ),
        )
}
