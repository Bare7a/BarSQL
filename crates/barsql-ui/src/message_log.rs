// A run's server messages for the results' Messages tab, grouped by the result they came with. The view is a
// uniform list, so a message takes one row per line of its text, detail and hint.
use std::ops::Range;

use barsql_core::{MessageLevel, ServerMessage};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme, Icon, StyledExt, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::form;
use crate::i18n::{t, t_count, t_with};
use crate::tokens::{ICON_SM, ICON_XS, TEXT_SM, TEXT_XS};

// Rows the log keeps. Messages past it are only counted.
pub(crate) const MAX_LINES: usize = 5000;
const ROW_HEIGHT: Rems = rems(1.846);
// An icon and the longest level name, Spanish "Advertencia".
const LEVEL_REM: f32 = 6.615;
const CODE_REM: f32 = 3.846;
const GAP_REM: f32 = 0.615;
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
    heading: usize,
    messages: Vec<ServerMessage>,
    dropped: usize,
    // Its heading row made it into the log, so the log's last rows are its own.
    shown: bool,
}

struct Span {
    heading: usize,
    rows: Range<usize>,
    level: MessageLevel,
    // Its text, detail, hint and code, lowercased.
    haystack: String,
}

#[derive(Clone, Default)]
struct Filter {
    text: String,
    level: Option<MessageLevel>,
}

impl Filter {
    fn is_set(&self) -> bool {
        !self.text.is_empty() || self.level.is_some()
    }

    fn lets(&self, span: &Span) -> bool {
        self.level.is_none_or(|level| level == span.level) && span.haystack.contains(&self.text)
    }
}

// What a filter lets through: its messages' rows, under their results' headings.
#[derive(Default)]
struct Filtered {
    rows: Vec<usize>,
    messages: usize,
}

impl Filtered {
    fn push(&mut self, span: &Span) {
        if self.rows.last().is_none_or(|&last| last < span.heading) {
            self.rows.push(span.heading);
        }
        self.rows.extend(span.rows.clone());
        self.messages += 1;
    }
}

#[derive(Default)]
pub(crate) struct MessageLog {
    lines: Vec<Line>,
    groups: Vec<Group>,
    spans: Vec<Span>,
    // Messages the server sent, kept or not.
    total: usize,
    // Kept messages by level.
    levels: [usize; 3],
    // Some message has a code, so every row leaves room for one.
    coded: bool,
    // Dropped once the log is full, shown after its last row.
    overflow: usize,
    filter: Filter,
    filtered: Option<Filtered>,
    pub(crate) scroll: UniformListScrollHandle,
}

impl MessageLog {
    // An empty log for the next run, filtered as this one is.
    pub(crate) fn fresh(&self) -> Self {
        let filtered = self.filter.is_set().then(Filtered::default);
        Self { filter: self.filter.clone(), filtered, ..Self::default() }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.total == 0
    }

    pub(crate) fn total(&self) -> usize {
        self.total
    }

    pub(crate) fn kept(&self) -> usize {
        self.levels.iter().sum()
    }

    pub(crate) fn count(&self, level: MessageLevel) -> usize {
        self.levels[level as usize]
    }

    pub(crate) fn has_warnings(&self) -> bool {
        self.count(MessageLevel::Warning) > 0
    }

    pub(crate) fn has_codes(&self) -> bool {
        self.coded
    }

    // A notice or warning, which a run that returned nothing opens on. Info lines, like MySQL's "Rows matched",
    // only report how a statement went.
    pub(crate) fn has_raised(&self) -> bool {
        self.count(MessageLevel::Notice) + self.count(MessageLevel::Warning) > 0
    }

    // Levels its messages come in. Filtering by one takes two.
    pub(crate) fn level_kinds(&self) -> usize {
        self.levels.iter().filter(|&&n| n > 0).count()
    }

    pub(crate) fn level_filter(&self) -> Option<MessageLevel> {
        self.filter.level
    }

    pub(crate) fn matched(&self) -> Option<usize> {
        self.filtered.as_ref().map(|filtered| filtered.messages)
    }

    pub(crate) fn filter_by_text(&mut self, text: &str) {
        let text = text.trim().to_lowercase();
        if text != self.filter.text {
            self.filter.text = text;
            self.refilter();
        }
    }

    pub(crate) fn filter_by_level(&mut self, level: Option<MessageLevel>) {
        if level != self.filter.level {
            self.filter.level = level;
            self.refilter();
        }
    }

    fn refilter(&mut self) {
        self.filtered = self.filter.is_set().then(|| {
            let mut filtered = Filtered::default();
            self.spans.iter().filter(|span| self.filter.lets(span)).for_each(|span| filtered.push(span));
            filtered
        });
    }

    // Rows to draw: the filter's, or all of them and the overflow note.
    pub(crate) fn rows(&self) -> usize {
        match &self.filtered {
            Some(filtered) => filtered.rows.len(),
            None => self.lines.len() + usize::from(self.overflow > 0),
        }
    }

    pub(crate) fn row(&self, ix: usize) -> Option<Line> {
        if let Some(filtered) = &self.filtered {
            return filtered.rows.get(ix).map(|&line| self.lines[line].clone());
        }
        match self.lines.get(ix) {
            Some(line) => Some(line.clone()),
            None if ix == self.lines.len() && self.overflow > 0 => Some(Line::Dropped(self.overflow)),
            None => None,
        }
    }

    pub(crate) fn add(&mut self, result_index: usize, messages: Vec<ServerMessage>, dropped: usize) {
        self.total += messages.len() + dropped;
        if self.groups.last().map(|g| g.result_index) != Some(result_index) {
            let (heading, shown) = (self.lines.len(), self.lines.len() < MAX_LINES);
            if shown {
                self.lines.push(Line::Result(result_index));
            }
            self.groups.push(Group { result_index, heading, messages: Vec::new(), dropped: 0, shown });
        }
        let heading = self.groups.last().expect("pushed above").heading;
        let first_new = self.spans.len();
        let mut lost = dropped;
        let mut kept = Vec::new();
        for message in messages {
            let lines = lines_of(&message);
            if self.lines.len() + lines.len() > MAX_LINES {
                lost += 1;
                continue;
            }
            self.levels[message.level as usize] += 1;
            self.coded |= !message.code.is_empty();
            let start = self.lines.len();
            self.lines.extend(lines);
            let haystack = [&message.text, &message.detail, &message.hint, &message.code].map(|s| s.to_lowercase());
            let rows = start..self.lines.len();
            self.spans.push(Span { heading, rows, level: message.level, haystack: haystack.join("\n") });
            kept.push(message);
        }
        if let Some(filtered) = &mut self.filtered {
            self.spans[first_new..].iter().filter(|span| self.filter.lets(span)).for_each(|span| filtered.push(span));
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

fn label(text: SharedString, color: Hsla) -> Div {
    div().font_semibold().text_size(TEXT_XS).text_color(color).child(text)
}

// The icon's column is as wide as the toolbar's icon above, so their centres line up.
fn level_cell(ix: usize, level: Option<MessageLevel>, cx: &App) -> Div {
    let theme = cx.theme();
    h_flex().flex_none().w(rems(LEVEL_REM)).gap(form::ICON_GAP).when_some(level, |el, level| {
        let (icon, color, name) = match level {
            MessageLevel::Info => (Lucide::Info, theme.muted_foreground, theme.muted_foreground),
            MessageLevel::Notice => (Lucide::Info, theme.info, theme.muted_foreground),
            MessageLevel::Warning => (Lucide::TriangleAlert, theme.warning, theme.warning),
        };
        el.child(
            h_flex()
                .debug_selector(move || format!("message-level-{ix}"))
                .flex_none()
                .w(ICON_SM)
                .justify_center()
                .child(Icon::new(icon).size(ICON_XS).text_color(color)),
        )
        .child(label(t(cx, level_key(level)), name).min_w_0().truncate())
    })
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
        .gap(rems(GAP_REM))
        .px(rems(0.462))
        .text_size(TEXT_SM)
        .whitespace_nowrap();
    // Spans the level and code columns, so what follows starts where a message's text does.
    let lead = || div().flex_none().min_w(rems(if coded { LEVEL_REM + GAP_REM + CODE_REM } else { LEVEL_REM }));
    let text = |text: SharedString, color: Hsla| {
        let tooltip = (text.chars().count() > TOOLTIP_CHARS).then(|| text.clone());
        div()
            .id(("message-text", ix))
            .debug_selector(move || format!("message-text-{ix}"))
            .flex_1()
            .min_w_0()
            .truncate()
            .text_color(color)
            .child(text)
            .when_some(tooltip, |el, tip| el.tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx)))
    };
    match line {
        Line::Result(_) => {
            let heading = heading.expect("a heading for every result row");
            let outcome_color = if heading.failed { theme.danger } else { theme.muted_foreground };
            row.when(ix > 0, |el| el.border_t_1().border_color(theme.border))
                .child(lead().font_semibold().text_color(theme.foreground).child(heading.label))
                .child(
                    div()
                        .debug_selector(move || format!("message-statement-{ix}"))
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
        Line::Text { level, code, text: line } => {
            let code_cell = div().flex_none().w(rems(CODE_REM)).when(!code.is_empty(), |el| {
                let tip = t_with(cx, "errors.code", &[("code", &code)]);
                el.child(
                    div()
                        .id(("message-code", ix))
                        .debug_selector(move || format!("message-code-{ix}"))
                        .w_full()
                        .truncate()
                        .font_family(theme.mono_font_family.clone())
                        .text_size(TEXT_XS)
                        .text_color(theme.muted_foreground)
                        .child(code)
                        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx)),
                )
            });
            row.child(level_cell(ix, level, cx))
                .when(coded, |el| el.child(code_cell))
                .child(text(line, theme.foreground))
                .into_any_element()
        }
        // Right-aligned, as some languages' labels run longer than a level column.
        Line::Labeled { label: key, text: line } => row
            .child(lead().flex().justify_end().children(key.map(|key| label(t(cx, key), theme.muted_foreground))))
            .child(text(line, theme.muted_foreground))
            .into_any_element(),
        Line::Dropped(count) => row
            .child(lead())
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
        assert_eq!((log.total(), log.count(MessageLevel::Warning), log.count(MessageLevel::Notice)), (6, 1, 2));
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

    // A message shows whole, under its result's heading, when its text, detail, hint or code matches.
    #[test]
    fn a_filter_keeps_whole_messages_under_their_headings() {
        let mut log = MessageLog::default();
        let warning = ServerMessage {
            level: MessageLevel::Warning,
            code: "01000".into(),
            text: "Slow path".into(),
            detail: "No INDEX on author_id".into(),
            ..Default::default()
        };
        log.add(0, vec![notice("skipping"), warning], 0);
        log.add(1, vec![notice("Checked 150 users")], 2);
        let texts = |log: &MessageLog| -> Vec<String> {
            (0..log.rows())
                .filter_map(|ix| match log.row(ix)? {
                    Line::Result(index) => Some(format!("#{index}")),
                    Line::Text { text, .. } | Line::Labeled { text, .. } => Some(text.to_string()),
                    Line::Dropped(n) => Some(format!("{n} dropped")),
                })
                .collect()
        };
        log.filter_by_text(" index ");
        assert_eq!(texts(&log), ["#0", "Slow path", "No INDEX on author_id"]);
        assert_eq!(log.matched(), Some(1));
        log.filter_by_text("01000");
        assert_eq!(log.matched(), Some(1), "codes match too");
        log.filter_by_text("");
        log.filter_by_level(Some(MessageLevel::Notice));
        assert_eq!(texts(&log), ["#0", "skipping", "#1", "Checked 150 users"]);
        log.add(1, vec![notice("later"), ServerMessage::new(MessageLevel::Warning, "careful")], 0);
        assert_eq!(texts(&log)[4..], ["later"], "new messages go through the filter as they come");
        log.filter_by_level(None);
        assert_eq!(log.matched(), None);
        assert_eq!(texts(&log).last().map(String::as_str), Some("careful"));
        assert_eq!((log.level_kinds(), log.kept(), log.total()), (2, 5, 7));

        log.filter_by_text("nothing like it");
        let next = log.fresh();
        assert_eq!((next.rows(), next.matched()), (0, Some(0)), "the next run's log keeps the filter");
    }
}
