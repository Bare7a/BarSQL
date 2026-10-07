// A run's server messages for the results' Messages tab, grouped by the result they came with. The view is a
// uniform list, so a message takes one row per line of its text, detail and hint.
use barsql_core::{MessageLevel, ServerMessage};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme, StyledExt, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::form;
use crate::i18n::{t, t_count};
use crate::tokens::{TEXT_SM, TEXT_XS};

// Rows the log keeps. Messages past it are only counted.
pub(crate) const MAX_LINES: usize = 5000;
const ROW_HEIGHT: Rems = rems(1.846);
// Longer lines get the whole line as a tooltip, since the row cuts them off.
const TOOLTIP_CHARS: usize = 80;

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Line {
    // Opens the messages of a result.
    Result(usize),
    // A message's first line carries its level and code.
    Text { level: Option<MessageLevel>, code: SharedString, text: SharedString },
    // Detail and hint, labelled on their first line.
    Labeled { label: Option<&'static str>, text: SharedString },
    Dropped(usize),
}

struct Group {
    result_index: usize,
    messages: Vec<ServerMessage>,
    dropped: usize,
    // Its heading row made it into the log, so the log's last rows are its own.
    shown: bool,
}

#[derive(Default)]
pub(crate) struct MessageLog {
    lines: Vec<Line>,
    groups: Vec<Group>,
    // Messages the server sent, kept or not.
    total: usize,
    warnings: bool,
    // A notice or warning, which a run that returned nothing opens on. Info lines, like MySQL's "Rows matched",
    // only report how a statement went.
    raised: bool,
    // Some message has a code, so every row leaves room for one.
    coded: bool,
    // Dropped once the log is full, shown after its last row.
    overflow: usize,
    pub(crate) scroll: UniformListScrollHandle,
}

impl MessageLog {
    pub(crate) fn is_empty(&self) -> bool {
        self.total == 0
    }

    pub(crate) fn total(&self) -> usize {
        self.total
    }

    pub(crate) fn has_warnings(&self) -> bool {
        self.warnings
    }

    pub(crate) fn has_codes(&self) -> bool {
        self.coded
    }

    pub(crate) fn has_raised(&self) -> bool {
        self.raised
    }

    // Rows to draw, the overflow note included.
    pub(crate) fn rows(&self) -> usize {
        self.lines.len() + usize::from(self.overflow > 0)
    }

    pub(crate) fn row(&self, ix: usize) -> Option<Line> {
        match self.lines.get(ix) {
            Some(line) => Some(line.clone()),
            None if ix == self.lines.len() && self.overflow > 0 => Some(Line::Dropped(self.overflow)),
            None => None,
        }
    }

    pub(crate) fn add(&mut self, result_index: usize, messages: Vec<ServerMessage>, dropped: usize) {
        self.total += messages.len() + dropped;
        if self.groups.last().map(|g| g.result_index) != Some(result_index) {
            let shown = self.lines.len() < MAX_LINES;
            if shown {
                self.lines.push(Line::Result(result_index));
            }
            self.groups.push(Group { result_index, messages: Vec::new(), dropped: 0, shown });
        }
        let mut lost = dropped;
        let mut kept = Vec::new();
        for message in messages {
            let lines = lines_of(&message);
            if self.lines.len() + lines.len() > MAX_LINES {
                lost += 1;
                continue;
            }
            self.warnings |= message.level == MessageLevel::Warning;
            self.raised |= message.level != MessageLevel::Info;
            self.coded |= !message.code.is_empty();
            self.lines.extend(lines);
            kept.push(message);
        }
        let group = self.groups.last_mut().expect("pushed above");
        group.messages.extend(kept);
        group.dropped += lost;
        if lost == 0 {
            return;
        }
        let room = self.lines.len() < MAX_LINES;
        match self.lines.last_mut() {
            _ if !group.shown => self.overflow += lost,
            Some(Line::Dropped(n)) => *n += lost,
            _ if room => self.lines.push(Line::Dropped(lost)),
            _ => self.overflow += lost,
        }
    }

    // Plain text for the clipboard. `heading` names a result, like "Result 2: SELECT …".
    pub(crate) fn copy_text(&self, heading: impl Fn(usize) -> String, cx: &App) -> String {
        let mut out = Vec::new();
        for group in &self.groups {
            out.push(format!("-- {}", heading(group.result_index)));
            for message in &group.messages {
                let level = t(cx, level_key(message.level));
                match message.code.as_str() {
                    "" => out.push(format!("{level}: {}", message.text)),
                    code => out.push(format!("{level} {code}: {}", message.text)),
                }
                for (key, text) in [("errors.detailLabel", &message.detail), ("errors.hintLabel", &message.hint)] {
                    if !text.is_empty() {
                        out.push(format!("{}: {text}", t(cx, key)));
                    }
                }
            }
            if group.dropped > 0 {
                out.push(t_count(cx, "results.messagesDropped", group.dropped as i64, &[]).to_string());
            }
        }
        out.join("\n")
    }
}

fn lines_of(message: &ServerMessage) -> Vec<Line> {
    let text = |line: &str| SharedString::from(line.replace('\t', "    "));
    let mut lines = Vec::new();
    for (ix, line) in message.text.lines().chain(message.text.is_empty().then_some("")).enumerate() {
        let (level, code) =
            if ix == 0 { (Some(message.level), message.code.clone().into()) } else { (None, "".into()) };
        lines.push(Line::Text { level, code, text: text(line) });
    }
    for (key, value) in [("errors.detailLabel", &message.detail), ("errors.hintLabel", &message.hint)] {
        for (ix, line) in value.lines().enumerate() {
            lines.push(Line::Labeled { label: (ix == 0).then_some(key), text: text(line) });
        }
    }
    lines
}

pub(crate) fn level_key(level: MessageLevel) -> &'static str {
    match level {
        MessageLevel::Info => "results.level.info",
        MessageLevel::Notice => "results.level.notice",
        MessageLevel::Warning => "results.level.warning",
    }
}

pub(crate) fn level_color(level: MessageLevel, cx: &App) -> Hsla {
    let theme = cx.theme();
    match level {
        MessageLevel::Info => theme.muted_foreground,
        MessageLevel::Notice => theme.primary,
        MessageLevel::Warning => theme.warning,
    }
}

// What a result's heading row shows: its tab's label, the statement and how it ended.
pub(crate) struct Heading {
    pub label: SharedString,
    pub statement: SharedString,
    pub outcome: SharedString,
    pub failed: bool,
}

// Levels and codes get fixed columns, so the text of every row starts at the same place. `coded` leaves room for
// a code on every row.
pub(crate) fn render_row(ix: usize, line: Line, heading: Option<Heading>, coded: bool, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let row = h_flex()
        .id(("message-row", ix))
        .h(ROW_HEIGHT)
        .w_full()
        .gap(rems(0.615))
        .px(rems(0.462))
        .text_size(TEXT_SM)
        .whitespace_nowrap();
    let level_column = || div().flex_none().w(rems(5.385));
    let code_column = || div().flex_none().w(rems(3.846));
    let long = |text: &SharedString| (text.chars().count() > TOOLTIP_CHARS).then(|| text.clone());
    match line {
        Line::Result(_) => {
            let heading = heading.expect("a heading for every result row");
            let outcome_color = if heading.failed { theme.danger } else { theme.muted_foreground };
            row.when(ix > 0, |el| el.border_t_1().border_color(theme.border))
                .child(div().flex_none().font_semibold().text_color(theme.foreground).child(heading.label))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(theme.muted_foreground)
                        .font_family(theme.mono_font_family.clone())
                        .text_size(TEXT_XS)
                        .child(heading.statement),
                )
                .child(div().flex_none().text_size(TEXT_XS).text_color(outcome_color).child(heading.outcome))
                .into_any_element()
        }
        Line::Text { level, code, text } => {
            let tooltip = long(&text);
            row.child(level_column().children(level.map(|level| {
                div()
                    .font_semibold()
                    .text_size(TEXT_XS)
                    .text_color(level_color(level, cx))
                    .child(t(cx, level_key(level)))
            })))
            .when(coded, |el| {
                el.child(code_column().when(!code.is_empty(), |el| {
                    el.child(
                        h_flex().child(
                            form::badge(code, theme.muted_foreground)
                                .font_family(theme.mono_font_family.clone())
                                .debug_selector(move || format!("message-code-{ix}")),
                        ),
                    )
                }))
            })
            .child(
                div()
                    .debug_selector(move || format!("message-text-{ix}"))
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(theme.foreground)
                    .child(text),
            )
            .when_some(tooltip, |el, tip| el.tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx)))
            .into_any_element()
        }
        Line::Labeled { label, text } => {
            let tooltip = long(&text);
            // Inline, since some languages' labels are longer than any code.
            row.child(level_column())
                .when(coded, |el| el.child(code_column()))
                .children(
                    label.map(|key| {
                        div().flex_none().font_semibold().text_color(theme.muted_foreground).child(t(cx, key))
                    }),
                )
                .child(div().flex_1().min_w_0().truncate().text_color(theme.muted_foreground).child(text))
                .when_some(tooltip, |el, tip| el.tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx)))
                .into_any_element()
        }
        Line::Dropped(count) => row
            .child(level_column())
            .when(coded, |el| el.child(code_column()))
            .child(div().flex_1().min_w_0().truncate().italic().text_color(theme.muted_foreground).child(t_count(
                cx,
                "results.messagesDropped",
                count as i64,
                &[],
            )))
            .into_any_element(),
    }
}

#[cfg(test)]
mod tests {
    use barsql_core::{MessageLevel, ServerMessage};

    use super::{Line, MAX_LINES, MessageLog};

    fn notice(text: &str) -> ServerMessage {
        ServerMessage::new(MessageLevel::Notice, text)
    }

    #[test]
    fn messages_group_under_their_result_with_a_row_per_line() {
        let mut log = MessageLog::default();
        let warning = ServerMessage {
            level: MessageLevel::Warning,
            code: "01000".into(),
            text: "two\nlines".into(),
            detail: "why".into(),
            hint: "fix it".into(),
        };
        log.add(0, vec![notice("hi"), warning], 0);
        log.add(0, vec![notice("later")], 0);
        log.add(2, vec![], 3);
        let rows: Vec<Line> = (0..log.rows()).filter_map(|ix| log.row(ix)).collect();
        assert_eq!(rows.len(), 9, "{rows:?}");
        assert_eq!(rows[0], Line::Result(0));
        assert!(matches!(&rows[2], Line::Text { level: Some(MessageLevel::Warning), code, .. } if code == "01000"));
        assert!(matches!(&rows[3], Line::Text { level: None, text, .. } if text == "lines"));
        assert!(matches!(&rows[4], Line::Labeled { label: Some("errors.detailLabel"), .. }));
        assert!(matches!(&rows[5], Line::Labeled { label: Some("errors.hintLabel"), .. }));
        assert_eq!(rows[7..], [Line::Result(2), Line::Dropped(3)]);
        assert_eq!((log.total(), log.has_warnings()), (6, true));
    }

    #[test]
    fn a_full_log_counts_the_rest() {
        let mut log = MessageLog::default();
        log.add(0, (0..MAX_LINES + 10).map(|n| notice(&n.to_string())).collect(), 0);
        assert_eq!(log.rows(), MAX_LINES + 1, "the log, then a note of what didn't fit");
        assert_eq!(log.row(MAX_LINES), Some(Line::Dropped(11)));
        log.add(0, vec![notice("one more")], 5);
        assert_eq!(log.row(MAX_LINES), Some(Line::Dropped(17)));
        assert_eq!(log.total(), MAX_LINES + 16);
    }
}
