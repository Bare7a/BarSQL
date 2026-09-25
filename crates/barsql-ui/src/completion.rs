use std::ops::Range;
use std::rc::Rc;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::input::{InputBaseState, InputModeKind};
use gpui_kit::component::input::{CompletionProvider, InputEvent};
use gpui_kit::component::{ActiveTheme, Icon, Rope, RopeExt, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use lsp_types::{
    CompletionContext, CompletionItem, CompletionItemKind, CompletionResponse, CompletionTextEdit,
    CompletionTriggerKind,
};

use crate::sql_language::word_start;
use crate::theme;

// GPUI Kit's completion menu uses tiny text, has no kind icons and can't be restyled, so the editors draw their
// own list.
actions!(
    completion,
    [
        SelectPrevSuggestion,
        SelectNextSuggestion,
        SelectPrevSuggestionPage,
        SelectNextSuggestionPage,
        AcceptSuggestion,
        HideSuggestions,
    ]
);

// Set on the editor's parent while the list is open, so the list gets these keys.
pub const OPEN_CONTEXT: &str = "completing";
const VISIBLE_ROWS: usize = 12;
const WIDTH: Pixels = px(430.);

pub fn init(cx: &mut App) {
    let context = Some("completing > Input");
    cx.bind_keys([
        KeyBinding::new("up", SelectPrevSuggestion, context),
        KeyBinding::new("down", SelectNextSuggestion, context),
        KeyBinding::new("pageup", SelectPrevSuggestionPage, context),
        KeyBinding::new("pagedown", SelectNextSuggestionPage, context),
        KeyBinding::new("enter", AcceptSuggestion, context),
        KeyBinding::new("tab", AcceptSuggestion, context),
        KeyBinding::new("escape", HideSuggestions, context),
    ]);
}

pub fn handlers<E: InteractiveElement, M: InputModeKind>(el: E, completion: &Entity<Completion<M>>) -> E {
    let on = || completion.clone();
    el.on_action({
        let completion = on();
        move |_: &SelectPrevSuggestion, _, cx| completion.update(cx, |this, cx| this.step(-1, cx))
    })
    .on_action({
        let completion = on();
        move |_: &SelectNextSuggestion, _, cx| completion.update(cx, |this, cx| this.step(1, cx))
    })
    .on_action({
        let completion = on();
        move |_: &SelectPrevSuggestionPage, _, cx| {
            completion.update(cx, |this, cx| this.step(-(VISIBLE_ROWS as isize), cx))
        }
    })
    .on_action({
        let completion = on();
        move |_: &SelectNextSuggestionPage, _, cx| {
            completion.update(cx, |this, cx| this.step(VISIBLE_ROWS as isize, cx))
        }
    })
    .on_action({
        let completion = on();
        move |_: &AcceptSuggestion, window, cx| completion.update(cx, |this, cx| this.accept(this.selected, window, cx))
    })
    .on_action({
        let completion = on();
        move |_: &HideSuggestions, _, cx| completion.update(cx, |this, cx| this.hide(cx))
    })
    // A click in the editor closes the list. Row clicks stop propagation and accept the row instead.
    .on_any_mouse_down({
        let completion = on();
        move |_, _, cx| completion.update(cx, |this, cx| this.hide(cx))
    })
}

struct Suggestion {
    item: CompletionItem,
    matched: Vec<Range<usize>>,
}

pub struct Completion<M: InputModeKind> {
    editor: Entity<InputBaseState<M>>,
    provider: Option<Rc<dyn CompletionProvider>>,
    items: Vec<Suggestion>,
    selected: usize,
    start: usize,
    open: bool,
    text: String,
    // Font size and row height. None means the SQL editor's.
    size: Option<(Rems, Rems)>,
    scroll: UniformListScrollHandle,
    request: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl<M: InputModeKind> Completion<M> {
    pub fn new(
        editor: Entity<InputBaseState<M>>,
        provider: Option<Rc<dyn CompletionProvider>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = editor.read(cx).focus_handle(cx);
        let subscriptions = vec![
            cx.subscribe_in(&editor, window, |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Change) {
                    this.changed(window, cx);
                }
            }),
            cx.on_blur(&focus, window, |this, _, cx| this.hide(cx)),
        ];
        let text = editor.read(cx).value().to_string();
        Self {
            editor,
            provider,
            items: Vec::new(),
            selected: 0,
            start: 0,
            open: false,
            text,
            size: None,
            scroll: UniformListScrollHandle::new(),
            request: Task::ready(()),
            _subscriptions: subscriptions,
        }
    }

    pub fn sized(mut self, font_size: Rems, row_height: Rems) -> Self {
        self.size = Some((font_size, row_height));
        self
    }

    pub fn set_provider(&mut self, provider: Rc<dyn CompletionProvider>) {
        self.provider = Some(provider);
    }

    // Also false once the caret moves before the word start or onto another line.
    pub fn is_open(&self, cx: &App) -> bool {
        let state = self.editor.read(cx);
        let cursor = state.cursor();
        self.open && state.value().get(self.start..cursor).is_some_and(|word| !word.contains('\n'))
    }

    // A typed trigger character opens the list. Any edit while it's open refilters it.
    fn changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (text, cursor) = {
            let state = self.editor.read(cx);
            (state.value().to_string(), state.cursor())
        };
        let grown = text.len().saturating_sub(self.text.len());
        let inserted =
            (grown > 0).then(|| text.get(cursor.saturating_sub(grown)..cursor)).flatten().map(str::to_string);
        self.text = text;
        let trigger = match (&self.provider, inserted) {
            (Some(provider), Some(inserted)) => provider.is_completion_trigger(cursor, &inserted, cx),
            _ => false,
        };
        if self.open || trigger {
            self.request(false, window, cx);
        }
    }

    // `invoked` means Ctrl+Space rather than typing.
    pub fn request(&mut self, invoked: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(provider) = self.provider.clone() else { return };
        let (text, cursor) = {
            let state = self.editor.read(cx);
            (state.value().to_string(), state.cursor())
        };
        let trigger_kind =
            if invoked { CompletionTriggerKind::INVOKED } else { CompletionTriggerKind::TRIGGER_CHARACTER };
        let context = CompletionContext { trigger_kind, trigger_character: None };
        let task = provider.completions(&Rope::from(text.as_str()), cursor, context, window, cx);
        self.request = cx.spawn(async move |this, cx| {
            let items = match task.await {
                Ok(CompletionResponse::Array(items)) => items,
                Ok(CompletionResponse::List(list)) => list.items,
                Err(_) => Vec::new(),
            };
            let _ = this.update(cx, |this, cx| this.show(&text, cursor, items, cx));
        });
    }

    fn show(&mut self, text: &str, cursor: usize, items: Vec<CompletionItem>, cx: &mut Context<Self>) {
        let state = self.editor.read(cx);
        if state.value().as_ref() != text || state.cursor() != cursor {
            return;
        }
        let start = word_start(text, cursor);
        let word = &text[start..cursor];
        let mut ranked: Vec<(u8, usize, Suggestion)> = items
            .into_iter()
            .enumerate()
            .filter_map(|(ix, item)| {
                let (rank, matched) = fuzzy(&item.label, word)?;
                Some((rank, ix, Suggestion { item, matched }))
            })
            .collect();
        ranked.sort_by_key(|(rank, ix, _)| (*rank, *ix));
        self.items = ranked.into_iter().map(|(.., suggestion)| suggestion).collect();
        self.start = start;
        self.selected = 0;
        self.open = !self.items.is_empty();
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
        cx.notify();
    }

    pub fn hide(&mut self, cx: &mut Context<Self>) {
        if self.open {
            self.open = false;
            self.request = Task::ready(());
            cx.notify();
        }
    }

    fn step(&mut self, by: isize, cx: &mut Context<Self>) {
        let count = self.items.len();
        if count == 0 {
            return;
        }
        self.selected = if by.unsigned_abs() == 1 {
            (self.selected as isize + by).rem_euclid(count as isize) as usize
        } else {
            self.selected.saturating_add_signed(by).min(count - 1)
        };
        self.scroll.scroll_to_item(self.selected, ScrollStrategy::Nearest);
        cx.notify();
    }

    fn accept(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = self.items.get(ix).map(|suggestion| suggestion.item.clone()) else { return };
        let (text, cursor) = {
            let state = self.editor.read(cx);
            (state.value(), state.cursor())
        };
        let (range, new_text) = edit(&item, self.start.min(cursor)..cursor, &text);
        self.hide(cx);
        self.editor.update(cx, |state, cx| {
            state.set_selected_range(range, cx);
            state.replace(new_text, window, cx);
        });
    }

    fn row(&self, ix: usize, metrics: &Metrics, cx: &mut Context<Self>) -> AnyElement {
        let suggestion = &self.items[ix];
        let selected = ix == self.selected;
        let palette = &metrics.palette;
        let (icon, color) = kind_icon(suggestion.item.kind, cx.theme().is_dark());
        let highlight = if selected { palette.selected_highlight } else { palette.highlight };
        let style =
            HighlightStyle { color: Some(highlight), font_weight: Some(FontWeight::BOLD), ..Default::default() };
        let label = StyledText::new(suggestion.item.label.clone())
            .with_highlights(suggestion.matched.iter().map(|range| (range.clone(), style)));
        h_flex()
            .id(ix)
            .w_full()
            .h(metrics.row_height)
            .pl(px(2.))
            .pr(px(10.))
            .gap(px(4.))
            .text_color(if selected { palette.selected_text } else { palette.text })
            .when(selected, |el| el.bg(palette.selected))
            .when(!selected, |el| el.hover(|style| style.bg(palette.hover)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.accept(ix, window, cx);
                }),
            )
            .child(
                div().flex_none().w(px(16.)).flex().justify_center().child(
                    Icon::new(icon).size(px(14.)).text_color(if selected { palette.selected_text } else { color }),
                ),
            )
            .child(div().min_w_0().truncate().child(label))
            .children(suggestion.item.detail.clone().filter(|_| selected).map(|detail| {
                let size = metrics.font_size * 0.85;
                div().flex_none().ml_auto().pl(size * 1.1).text_size(size).child(detail)
            }))
            .into_any_element()
    }
}

struct Palette {
    background: Hsla,
    border: Hsla,
    text: Hsla,
    selected: Hsla,
    selected_text: Hsla,
    hover: Hsla,
    highlight: Hsla,
    selected_highlight: Hsla,
}

struct Metrics {
    palette: Palette,
    font_size: Pixels,
    row_height: Pixels,
}

fn palette(cx: &App) -> Palette {
    let theme = cx.theme();
    let rgb_hsla = |value: u32| -> Hsla { rgb(value).into() };
    if theme.is_dark() {
        Palette {
            background: theme.sidebar,
            border: hsla(0., 0., 0.8, 0.2),
            text: theme.foreground,
            selected: rgb_hsla(0x04395e),
            selected_text: rgb_hsla(0xffffff),
            hover: rgb_hsla(0x2a2d2e),
            highlight: rgb_hsla(0x2aaaff),
            selected_highlight: rgb_hsla(0x2aaaff),
        }
    } else {
        Palette {
            background: theme.background,
            border: hsla(0., 0., 0.38, 0.2),
            text: theme.foreground,
            selected: rgb_hsla(0x0060c0),
            selected_text: rgb_hsla(0xffffff),
            hover: rgb_hsla(0xe8e8e8),
            highlight: rgb_hsla(0x0066bf),
            selected_highlight: rgb_hsla(0xbbe7ff),
        }
    }
}

fn kind_icon(kind: Option<CompletionItemKind>, dark: bool) -> (Lucide, Hsla) {
    let (icon, dark_color, light_color) = match kind {
        Some(CompletionItemKind::CLASS) => (Lucide::Workflow, 0xee9d28, 0xd67e00),
        Some(CompletionItemKind::FIELD) => (Lucide::Box, 0x75beff, 0x007acc),
        Some(CompletionItemKind::MODULE) => (Lucide::Braces, 0xcccccc, 0x616161),
        Some(CompletionItemKind::FUNCTION | CompletionItemKind::METHOD) => (Lucide::Box, 0xb180d7, 0x652d90),
        _ => (Lucide::TextAlignStart, 0xcccccc, 0x616161),
    };
    (icon, rgb(if dark { dark_color } else { light_color }).into())
}

// First letter must start a word (label start, after a separator, or a capital after a lowercase letter) and
// the rest follow in order. Rank 0 is a prefix, 1 the whole word at a later word start, 2 spread-out letters.
fn fuzzy(label: &str, word: &str) -> Option<(u8, Vec<Range<usize>>)> {
    let wanted: Vec<char> = word.chars().collect();
    let Some(&first) = wanted.first() else { return Some((0, Vec::new())) };
    let same = |a: char, b: char| a.to_lowercase().eq(b.to_lowercase());
    let chars: Vec<(usize, char)> = label.char_indices().collect();
    let span = |at: usize, len: usize| chars[at].0..chars.get(at + len).map_or(label.len(), |(ix, _)| *ix);
    let separator = |c: char| c.is_whitespace() || "_-./\\'\":$<>()[]{}".contains(c);
    let starts_word = |at: usize| match at.checked_sub(1).map(|prev| chars[prev].1) {
        None => true,
        Some(prev) => {
            let c = chars[at].1;
            separator(prev) || (c.is_uppercase() && !prev.is_uppercase()) || (separator(c) && !separator(prev))
        }
    };
    let starts: Vec<usize> = (0..chars.len()).filter(|&at| same(chars[at].1, first) && starts_word(at)).collect();
    let whole =
        |at: usize| at + wanted.len() <= chars.len() && wanted.iter().zip(&chars[at..]).all(|(w, (_, c))| same(*c, *w));
    if let Some(&at) = starts.iter().find(|&&at| whole(at)) {
        return Some((u8::from(at > 0), vec![span(at, wanted.len())]));
    }
    starts.iter().find_map(|&at| {
        let mut matched = vec![span(at, 1)];
        let mut rest = wanted[1..].iter().peekable();
        for (ix, (_, c)) in chars.iter().enumerate().skip(at + 1) {
            if rest.next_if(|w| same(*c, **w)).is_some() {
                matched.push(span(ix, 1));
            }
        }
        rest.peek().is_none().then_some((2, matched))
    })
}

// Matches how GPUI Kit's editor applies a completion. Text edit first, else insert text at the caret, else the
// label over the word typed so far.
fn edit(item: &CompletionItem, word: Range<usize>, text: &str) -> (Range<usize>, String) {
    let rope = Rope::from(text);
    let span = |range: &lsp_types::Range| rope.position_to_offset(&range.start)..rope.position_to_offset(&range.end);
    match (&item.text_edit, &item.insert_text) {
        (Some(CompletionTextEdit::Edit(edit)), _) => (span(&edit.range), edit.new_text.clone()),
        (Some(CompletionTextEdit::InsertAndReplace(edit)), _) => (span(&edit.replace), edit.new_text.clone()),
        (None, Some(insert)) => (word.end..word.end, insert.clone()),
        (None, None) => (word, item.label.clone()),
    }
}

impl<M: InputModeKind> Render for Completion<M> {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.editor.read(cx);
        let anchor = self
            .is_open(cx)
            .then(|| state.range_to_bounds(&(state.cursor()..state.cursor())))
            .flatten()
            .map(|bounds| point(bounds.left(), bounds.bottom()));
        let Some(anchor) = anchor else { return div().absolute().into_any_element() };
        // Match the editor's line height, 20px at the SQL editor's 13px.
        let (font_size, row_height) = match self.size {
            Some((font, row)) => (font.to_pixels(window.rem_size()), row.to_pixels(window.rem_size())),
            None => (px(theme::editor_font_size(cx)), px(theme::editor_font_size(cx) + 7.)),
        };
        let metrics = Metrics { palette: palette(cx), font_size, row_height };
        let count = self.items.len();
        let height = metrics.row_height * count.min(VISIBLE_ROWS) as f32;
        let (background, border) = (metrics.palette.background, metrics.palette.border);
        let mono = cx.theme().mono_font_family.clone();
        let list = uniform_list(
            "completion-rows",
            count,
            cx.processor(move |this, range: Range<usize>, _, cx| {
                let metrics = Metrics { palette: palette(cx), font_size, row_height };
                range.map(|ix| this.row(ix, &metrics, cx)).collect()
            }),
        )
        .track_scroll(&self.scroll)
        .h(height);
        deferred(
            anchored().position(anchor).snap_to_window_with_margin(px(8.)).child(
                div()
                    .id("completion")
                    .debug_selector(|| "completion".into())
                    .occlude()
                    .w(WIDTH)
                    .bg(background)
                    .border_1()
                    .border_color(border)
                    .font_family(mono)
                    .text_size(font_size)
                    .child(list),
            ),
        )
        .with_priority(1)
        .into_any_element()
    }
}

impl<M: InputModeKind> Completion<M> {
    // For BARSQL_SNAPSHOT_PANEL=suggest=<text>. Types the text, then asks for suggestions as Ctrl+Space does.
    pub fn preview(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.editor.update(cx, |state, cx| {
            state.set_value("", window, cx);
            state.insert(text.to_string(), window, cx);
            state.focus_handle(cx).focus(window, cx);
        });
        self.request(true, window, cx);
    }
}

#[cfg(test)]
impl<M: InputModeKind> Completion<M> {
    pub(crate) fn labels(&self, cx: &App) -> Vec<String> {
        if self.is_open(cx) {
            self.items.iter().map(|suggestion| suggestion.item.label.clone()).collect()
        } else {
            Vec::new()
        }
    }

    pub(crate) fn provider(&self) -> Option<Rc<dyn CompletionProvider>> {
        self.provider.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::fuzzy;

    fn bold<'a>(label: &'a str, word: &str) -> Option<(u8, Vec<&'a str>)> {
        fuzzy(label, word).map(|(rank, spans)| (rank, spans.into_iter().map(|span| &label[span]).collect()))
    }

    #[test]
    fn prefixes_beat_word_starts_beat_spread_letters() {
        assert_eq!(bold("posts", "po"), Some((0, vec!["po"])));
        assert_eq!(bold("post_reactions", "RE"), Some((1, vec!["re"])));
        assert_eq!(bold("parent_id", "pid"), Some((2, vec!["p", "i", "d"])));
        assert_eq!(bold("users", "x"), None);
        assert_eq!(bold("anything", ""), Some((0, Vec::new())));
    }

    #[test]
    fn the_first_letter_must_start_a_word() {
        assert_eq!(bold("is_locked", "s"), None);
        assert_eq!(bold("EXISTS", "s"), None);
        assert_eq!(bold("IS NULL", "n"), Some((1, vec!["N"])));
        assert_eq!(bold("createdAt", "at"), Some((1, vec!["At"])));
        assert_eq!(bold("is_score", "sc"), Some((1, vec!["sc"])));
    }
}
