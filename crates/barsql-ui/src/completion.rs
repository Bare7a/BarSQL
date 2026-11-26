use std::ops::Range;
use std::rc::Rc;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::input::{InputBaseState, InputModeKind};
use gpui_kit::component::input::{CompletionProvider, InputEvent};
use gpui_kit::component::kbd::Kbd;
use gpui_kit::component::scroll::ScrollbarAxis;
use gpui_kit::component::{ActiveTheme, Icon, Rope, RopeExt, Theme, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use lsp_types::{
    CompletionContext, CompletionItem, CompletionItemKind, CompletionResponse, CompletionTextEdit,
    CompletionTriggerKind, InsertTextFormat,
};

use crate::scrollbars::HoverScrollbar as _;
use crate::sql_language::{continues_word, word_start};
use crate::{form, theme};

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
        ConfirmSuggestion,
        HideSuggestions,
    ]
);

// Set on the editor's parent while the list is open, so the list gets these keys.
pub const OPEN_CONTEXT: &str = "completing";
pub const ENTER_KEY: &str = "barsql-enter-accepts-suggestions";
const VISIBLE_ROWS: usize = 12;
const WIDTH: Pixels = px(430.);

// On: Enter takes the highlighted row whenever that changes the text. Off: only once a row was picked.
pub fn enter_accepts_suggestions(cx: &App) -> bool {
    crate::state::setting_bool(cx, ENTER_KEY, true)
}

pub fn init(cx: &mut App) {
    let context = Some("completing > Input");
    cx.bind_keys([
        KeyBinding::new("up", SelectPrevSuggestion, context),
        KeyBinding::new("down", SelectNextSuggestion, context),
        KeyBinding::new("pageup", SelectPrevSuggestionPage, context),
        KeyBinding::new("pagedown", SelectNextSuggestionPage, context),
        KeyBinding::new("enter", ConfirmSuggestion, context),
        KeyBinding::new("tab", AcceptSuggestion, context),
        KeyBinding::new("escape", HideSuggestions, context),
    ]);
}

// A key the list doesn't use, or one that arrives after it closed, goes on to the editor's own binding.
fn when_open<M: InputModeKind>(
    completion: &Entity<Completion<M>>,
    window: &mut Window,
    cx: &mut App,
    act: impl FnOnce(&mut Completion<M>, &mut Window, &mut Context<Completion<M>>) -> bool,
) {
    if !completion.update(cx, |this, cx| this.is_open(cx) && act(this, window, cx)) {
        cx.propagate();
    }
}

pub fn handlers<E: InteractiveElement, M: InputModeKind>(el: E, completion: &Entity<Completion<M>>) -> E {
    let on = || completion.clone();
    let step = |by: isize| {
        let completion = on();
        move |window: &mut Window, cx: &mut App| {
            when_open(&completion, window, cx, |this, _, cx| {
                this.step(by, cx);
                true
            })
        }
    };
    let page = VISIBLE_ROWS as isize;
    el.on_action({
        let step = step(-1);
        move |_: &SelectPrevSuggestion, window, cx| step(window, cx)
    })
    .on_action({
        let step = step(1);
        move |_: &SelectNextSuggestion, window, cx| step(window, cx)
    })
    .on_action({
        let step = step(-page);
        move |_: &SelectPrevSuggestionPage, window, cx| step(window, cx)
    })
    .on_action({
        let step = step(page);
        move |_: &SelectNextSuggestionPage, window, cx| step(window, cx)
    })
    .on_action({
        let completion = on();
        move |_: &AcceptSuggestion, window, cx| {
            when_open(&completion, window, cx, |this, window, cx| {
                this.accept(this.selected, window, cx);
                true
            })
        }
    })
    .on_action({
        let completion = on();
        move |_: &ConfirmSuggestion, window, cx| {
            when_open(&completion, window, cx, |this, window, cx| {
                let take = this.enter_accepts(cx);
                if take {
                    this.accept(this.selected, window, cx);
                }
                take
            })
        }
    })
    .on_action({
        let completion = on();
        move |_: &HideSuggestions, window, cx| {
            when_open(&completion, window, cx, |this, _, cx| {
                this.hide(cx);
                true
            })
        }
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
    // The caret after the last edit. Moving it any other way closes the list.
    cursor: usize,
    shown_at: usize,
    // The start of a word nothing matched, so Backspace in it asks again.
    no_match: Option<usize>,
    // Enter takes a row only once one was picked with the arrows, or the list came from Ctrl+Space.
    picked: bool,
    invoked: bool,
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
            cx.observe(&editor, |this, editor, cx| {
                let state = editor.read(cx);
                if this.open && state.text().len() == this.text.len() && state.cursor() != this.cursor {
                    this.hide(cx);
                }
            }),
            cx.on_blur(&focus, window, |this, _, cx| this.hide(cx)),
        ];
        let (text, cursor) = {
            let state = editor.read(cx);
            (state.value().to_string(), state.cursor())
        };
        Self {
            editor,
            provider,
            items: Vec::new(),
            selected: 0,
            start: 0,
            open: false,
            text,
            cursor,
            shown_at: cursor,
            no_match: None,
            picked: false,
            invoked: false,
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

    // With the setting on, also a row that changes the text. Case alone isn't a change, so `desc` Enter ends the line.
    fn enter_accepts(&self, cx: &App) -> bool {
        self.picked || self.invoked || (enter_accepts_suggestions(cx) && self.changes_text(cx))
    }

    fn changes_text(&self, cx: &App) -> bool {
        let Some((range, new_text, _)) = self.replacement(self.selected, cx) else { return false };
        let text = self.editor.read(cx).value();
        text.get(range).is_none_or(|old| old.to_lowercase() != new_text.to_lowercase())
    }

    // False as soon as the text or caret changes without this list seeing it.
    pub fn is_open(&self, cx: &App) -> bool {
        let state = self.editor.read(cx);
        self.open && state.cursor() == self.cursor && state.text().len() == self.text.len()
    }

    // A name starting opens the list and typing it refilters. Backspace inside it does too. Anything else closes it.
    fn changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (text, cursor) = {
            let state = self.editor.read(cx);
            (state.value().to_string(), state.cursor())
        };
        if text == self.text && cursor == self.cursor {
            return;
        }
        let grown = text.len().saturating_sub(self.text.len());
        let removed = self.text.len().saturating_sub(text.len());
        let was_open = self.open
            && if grown > 0 {
                cursor.checked_sub(grown) == Some(self.cursor)
            } else {
                removed > 0 && (self.cursor == cursor + removed || self.cursor == cursor)
            };
        let typed = (grown > 0).then(|| text.get(cursor.saturating_sub(grown)..cursor)).flatten();
        let single = typed.and_then(|typed| {
            let mut chars = typed.chars();
            chars.next().filter(|_| chars.next().is_none())
        });
        let refresh = match typed {
            Some(typed) => {
                self.provider.as_ref().is_some_and(|provider| provider.is_completion_trigger(cursor, typed, cx))
                    || (was_open && single.is_some_and(continues_word))
            }
            None if removed > 0 => {
                let start = word_start(&text, cursor);
                let in_word = start < cursor || text[..cursor].ends_with('.');
                in_word && (was_open || self.no_match == Some(start))
            }
            None => false,
        };
        // A character that ends the word drops its list before anything new is asked for.
        let ended = single.filter(|c| !continues_word(*c));
        self.text = text;
        self.cursor = cursor;
        if ended.is_some() || !refresh {
            self.hide(cx);
        }
        if refresh {
            self.ask(false, ended, window, cx);
        }
    }

    // `invoked` means Ctrl+Space rather than typing.
    pub fn request(&mut self, invoked: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.ask(invoked, None, window, cx);
    }

    // `typed` is the character that asked when it isn't part of a name, like `.` or a space.
    fn ask(&mut self, invoked: bool, typed: Option<char>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(provider) = self.provider.clone() else { return };
        self.invoked |= invoked;
        let (text, cursor) = {
            let state = self.editor.read(cx);
            (state.value().to_string(), state.cursor())
        };
        let trigger_kind =
            if invoked { CompletionTriggerKind::INVOKED } else { CompletionTriggerKind::TRIGGER_CHARACTER };
        let context = CompletionContext { trigger_kind, trigger_character: typed.map(String::from) };
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
        self.no_match = (!self.open && start < cursor).then_some(start);
        self.picked = false;
        self.invoked &= self.open;
        self.text = text.to_string();
        self.cursor = cursor;
        self.shown_at = cursor;
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
        cx.notify();
    }

    pub fn hide(&mut self, cx: &mut Context<Self>) {
        self.request = Task::ready(());
        self.no_match = None;
        self.picked = false;
        self.invoked = false;
        if self.open {
            self.open = false;
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
        self.picked = true;
        self.scroll.scroll_to_item(self.selected, ScrollStrategy::Nearest);
        cx.notify();
    }

    // The range row `ix` replaces, its text, and where the caret lands in that text.
    fn replacement(&self, ix: usize, cx: &App) -> Option<(Range<usize>, String, usize)> {
        let item = &self.items.get(ix)?.item;
        let state = self.editor.read(cx);
        let (text, cursor) = (state.value(), state.cursor());
        let (range, new_text) = edit(item, self.start.min(cursor)..cursor, &text);
        // Items a key behind end where they were asked for, so the word now runs to the caret.
        let range = range.start.min(cursor)..if range.end == self.shown_at { cursor } else { range.end };
        let (new_text, caret) = if item.insert_text_format == Some(InsertTextFormat::SNIPPET) {
            expand_snippet(&new_text)
        } else {
            let end = new_text.len();
            (new_text, end)
        };
        Some((range, new_text, caret))
    }

    fn accept(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some((range, new_text, caret)) = self.replacement(ix, cx) else { return };
        self.hide(cx);
        let at = range.start + caret;
        self.editor.update(cx, |state, cx| {
            state.set_selected_range(range, cx);
            state.replace(new_text, window, cx);
            state.set_selected_range(at..at, cx);
        });
        self.sync(cx);
    }

    // After an edit of the list's own, so the change it causes doesn't read as typing.
    fn sync(&mut self, cx: &App) {
        let state = self.editor.read(cx);
        (self.text, self.cursor) = (state.value().to_string(), state.cursor());
    }

    fn row(&self, ix: usize, metrics: &Metrics, cx: &mut Context<Self>) -> AnyElement {
        let suggestion = &self.items[ix];
        let selected = ix == self.selected;
        let palette = &metrics.palette;
        let (icon, color) = kind_icon(suggestion.item.kind, cx.theme());
        let style = match_style(if selected { palette.selected_highlight } else { palette.highlight });
        let label = StyledText::new(suggestion.item.label.clone())
            .with_highlights(suggestion.matched.iter().map(|range| (range.clone(), style)));
        let args = suggestion.item.label_details.as_ref().and_then(|details| details.detail.clone());
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
            // A function's arguments follow its name, dimmed, and give way first when the row runs out of room.
            .map(|row| match args {
                Some(args) => row.child(
                    h_flex().min_w_0().child(div().flex_none().max_w(px(210.)).truncate().child(label)).child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_color(if selected { palette.selected_text.opacity(0.7) } else { palette.dim })
                            .child(args),
                    ),
                ),
                None => row.child(div().min_w_0().truncate().child(label)),
            })
            .children(suggestion.item.detail.clone().filter(|_| selected).map(|detail| {
                let size = metrics.font_size * 0.85;
                div().flex_none().max_w(px(170.)).truncate().ml_auto().pl(size * 1.1).text_size(size).child(detail)
            }))
            // With the setting off, only Tab takes a row until one is picked, so say so.
            .when(selected && !enter_accepts_suggestions(cx) && !self.picked && !self.invoked, |row| {
                let hint = form::key_chip(metrics.tab.clone(), palette.selected_text)
                    .debug_selector(|| "completion-tab-hint".into());
                row.child(hint.when(suggestion.item.detail.is_none(), |hint| hint.ml_auto()))
            })
            .into_any_element()
    }
}

struct Palette {
    background: Hsla,
    border: Hsla,
    text: Hsla,
    dim: Hsla,
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
    tab: SharedString,
}

fn palette(theme: &Theme) -> Palette {
    Palette {
        background: theme.popover,
        border: theme.border,
        text: theme.foreground,
        dim: theme.muted_foreground,
        selected: theme.list_active,
        selected_text: theme.foreground,
        hover: theme.list_hover,
        highlight: theme.primary,
        selected_highlight: theme.primary,
    }
}

// Color only. The bundled mono font stops at SemiBold, so bold matches read as another font.
fn match_style(color: Hsla) -> HighlightStyle {
    HighlightStyle { color: Some(color), ..Default::default() }
}

// The list's background and border, which the editor's hover card shares.
pub(crate) fn surface(cx: &App) -> (Hsla, Hsla) {
    let palette = palette(cx.theme());
    (palette.background, palette.border)
}

fn kind_icon(kind: Option<CompletionItemKind>, theme: &Theme) -> (Lucide, Hsla) {
    match kind {
        Some(CompletionItemKind::CLASS) => (Lucide::Workflow, theme.yellow),
        Some(CompletionItemKind::FIELD) => (Lucide::Box, theme.blue),
        Some(CompletionItemKind::MODULE) => (Lucide::Braces, theme.muted_foreground),
        Some(CompletionItemKind::FUNCTION | CompletionItemKind::METHOD) => (Lucide::SquareFunction, theme.magenta),
        _ => (Lucide::TextAlignStart, theme.muted_foreground),
    }
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

// Only the final tab stop, `$0`, which marks the caret. `\$`, `\\` and `\}` are escapes. The caret is a byte
// offset into the expanded text, at its end when there's no `$0`.
fn expand_snippet(snippet: &str) -> (String, usize) {
    let mut out = String::with_capacity(snippet.len());
    let mut caret = None;
    let mut chars = snippet.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek().is_some_and(|n| matches!(n, '$' | '\\' | '}')) => out.extend(chars.next()),
            '$' if chars.peek() == Some(&'0') => {
                chars.next();
                caret.get_or_insert(out.len());
            }
            _ => out.push(c),
        }
    }
    let caret = caret.unwrap_or(out.len());
    (out, caret)
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
        if !self.is_open(cx) {
            return div().absolute().into_any_element();
        }
        // Match the editor's line height, 20px at the SQL editor's 13px.
        let (font_size, row_height) = match self.size {
            Some((font, row)) => (font.to_pixels(window.rem_size()), row.to_pixels(window.rem_size())),
            None => (px(theme::editor_font_size(cx)), px(theme::editor_font_size(cx) + 7.)),
        };
        let tab: SharedString =
            Kbd::format(&Keystroke { modifiers: Modifiers::none(), key: "tab".into(), key_char: None }).into();
        let metrics = Metrics { palette: palette(cx.theme()), font_size, row_height, tab: tab.clone() };
        let count = self.items.len();
        let height = metrics.row_height * count.min(VISIBLE_ROWS) as f32;
        let (background, border) = (metrics.palette.background, metrics.palette.border);
        let mono = cx.theme().mono_font_family.clone();
        let list = uniform_list(
            "completion-rows",
            count,
            cx.processor(move |this, range: Range<usize>, _, cx| {
                let metrics = Metrics { palette: palette(cx.theme()), font_size, row_height, tab: tab.clone() };
                range.map(|ix| this.row(ix, &metrics, cx)).collect()
            }),
        )
        .track_scroll(&self.scroll)
        .h(height);
        let list = div()
            .id("completion")
            .debug_selector(|| "completion".into())
            .occlude()
            .w(WIDTH)
            .bg(background)
            .border_1()
            .border_color(border)
            .font_family(mono)
            .text_size(font_size)
            .relative()
            .child(list)
            .hover_scrollbar(&self.scroll, ScrollbarAxis::Vertical);
        let at_cursor = AtCursor { editor: self.editor.clone(), list: Some(list.into_any_element()) };
        deferred(at_cursor).with_priority(1).into_any_element()
    }
}

// Placed in a deferred prepaint, which runs after the editor lays out the frame, so it's never a keystroke behind.
// It follows the cursor as each character is typed, as VS Code's list does.
struct AtCursor<M: InputModeKind> {
    editor: Entity<InputBaseState<M>>,
    list: Option<AnyElement>,
}

impl<M: InputModeKind> Element for AtCursor<M> {
    type RequestLayoutState = ();
    type PrepaintState = Option<AnyElement>;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        (window.request_layout(Style::default(), [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        let state = self.editor.read(cx);
        let caret = state.range_to_bounds(&(state.cursor()..state.cursor()))?;
        let at = point(caret.left(), caret.bottom());
        #[cfg(test)]
        PLACED.with(|placed| placed.borrow_mut().push(at.x));
        let list = self.list.take()?;
        let mut list = anchored().position(at).snap_to_window_with_margin(px(8.)).child(list).into_any_element();
        list.layout_as_root(window.viewport_size().map(AvailableSpace::Definite), window, cx);
        list.prepaint_at(Point::default(), window, cx);
        Some(list)
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        list: &mut Option<AnyElement>,
        window: &mut Window,
        cx: &mut App,
    ) {
        if let Some(list) = list {
            list.paint(window, cx);
        }
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
        self.sync(cx);
        self.request(true, window, cx);
    }
}

#[cfg(test)]
thread_local! {
    // The list's left edge each time a frame placed it.
    static PLACED: std::cell::RefCell<Vec<Pixels>> = const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(test)]
pub(crate) fn take_placements() -> Vec<Pixels> {
    PLACED.with(|placed| std::mem::take(&mut *placed.borrow_mut()))
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
    use gpui_kit::component::{Theme, ThemeMode};
    use lsp_types::CompletionItemKind;

    use super::{expand_snippet, fuzzy, kind_icon, match_style, palette};

    #[test]
    fn matched_letters_keep_the_rows_font() {
        let color = gpui_kit::hsla(0.6, 0.5, 0.5, 1.);
        let style = match_style(color);
        assert_eq!((style.color, style.font_weight), (Some(color), None));
    }

    fn barsql(mode: ThemeMode) -> Theme {
        let mut theme = Theme::default();
        theme.apply_config(&crate::theme_files::built_in().barsql(mode).config);
        theme
    }

    #[test]
    fn the_completion_list_takes_its_colors_from_the_theme() {
        for mode in [ThemeMode::Dark, ThemeMode::Light] {
            let theme = barsql(mode);
            let p = palette(&theme);
            let surface = (theme.popover, theme.border, theme.foreground, theme.muted_foreground);
            assert_eq!((p.background, p.border, p.text, p.dim), surface, "{mode:?}");
            let rows = (theme.list_active, theme.foreground, theme.list_hover, theme.primary, theme.primary);
            assert_eq!((p.selected, p.selected_text, p.hover, p.highlight, p.selected_highlight), rows, "{mode:?}");
        }
    }

    #[test]
    fn completion_icons_use_the_theme_hues() {
        for mode in [ThemeMode::Dark, ThemeMode::Light] {
            let theme = barsql(mode);
            let color = |kind: Option<CompletionItemKind>| kind_icon(kind, &theme).1;
            assert_eq!(color(Some(CompletionItemKind::CLASS)), theme.yellow);
            assert_eq!(color(Some(CompletionItemKind::FIELD)), theme.blue);
            assert_eq!(color(Some(CompletionItemKind::FUNCTION)), theme.magenta);
            assert_eq!(color(Some(CompletionItemKind::METHOD)), theme.magenta);
            assert_eq!(color(Some(CompletionItemKind::MODULE)), theme.muted_foreground);
            assert_eq!(color(None), theme.muted_foreground);
        }
    }

    #[test]
    fn snippets_place_the_caret_and_unescape() {
        assert_eq!(expand_snippet("count($0)"), ("count()".to_string(), 6));
        assert_eq!(expand_snippet("now()"), ("now()".to_string(), 5));
        assert_eq!(expand_snippet("a\\$b($0)"), ("a$b()".to_string(), 4));
        assert_eq!(expand_snippet("x\\}\\\\($0)"), ("x}\\()".to_string(), 4));
        assert_eq!(expand_snippet("é($0)"), ("é()".to_string(), 3), "byte offsets");
    }

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
