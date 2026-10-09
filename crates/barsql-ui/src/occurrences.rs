use std::ops::Range;
use std::time::Duration;

use gpui_kit::component::RopeExt;
use gpui_kit::component::input::{
    EditorState, InputEvent, RangeDecoration, RangeDecorationCollection, RangeDecorationStyle,
};
use gpui_kit::*;

// Marks wait for the caret to rest, so holding an arrow key doesn't flash every word it passes.
const MARK_DELAY: Duration = Duration::from_millis(100);
// VS Code's limit for marking where else a selection appears.
const MAX_SELECTION: usize = 200;
// So a huge script doesn't slow every caret move: past MAX_TEXT nothing is marked, and never more than MAX_MARKS.
const MAX_TEXT: usize = 8 << 20;
const MAX_MARKS: usize = 10_000;

fn is_word(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

// The word touching `at` on either side. Empty between non-word characters.
pub fn word_at(text: &str, at: usize) -> Range<usize> {
    let start = text[..at].char_indices().rev().take_while(|(_, ch)| is_word(*ch)).last().map_or(at, |(ix, _)| ix);
    let end = text[at..].char_indices().find(|(_, ch)| !is_word(*ch)).map_or(text.len(), |(ix, _)| at + ix);
    start..end
}

// What to look for, matched as VS Code does: a word taken from a caret only as a whole word in the same case,
// selected text anywhere and in any ASCII case.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Query {
    pub text: String,
    pub word: bool,
}

impl Query {
    pub fn word(text: &str) -> Self {
        Self { text: text.to_string(), word: true }
    }

    pub fn selection(text: &str) -> Self {
        Self { text: text.to_string(), word: false }
    }

    pub fn matches(&self, found: &str) -> bool {
        if self.word { found == self.text } else { found.eq_ignore_ascii_case(&self.text) }
    }
}

fn is_whole_word(text: &str, range: &Range<usize>) -> bool {
    !text[..range.start].chars().next_back().is_some_and(is_word)
        && !text[range.end..].chars().next().is_some_and(is_word)
}

// Every match in order, none overlapping. ASCII lowercasing keeps byte offsets, so they hold for `text`.
pub fn find_all(text: &str, query: &Query) -> Vec<Range<usize>> {
    if query.text.is_empty() {
        return Vec::new();
    }
    if query.word {
        let found = text.match_indices(query.text.as_str()).map(|(ix, _)| ix..ix + query.text.len());
        return found.filter(|range| is_whole_word(text, range)).collect();
    }
    let needle = query.text.to_ascii_lowercase();
    text.to_ascii_lowercase().match_indices(needle.as_str()).map(|(ix, _)| ix..ix + needle.len()).collect()
}

// The first match starting at or after `from`, wrapping round to the first one.
pub fn find_next(text: &str, query: &Query, from: usize) -> Option<Range<usize>> {
    let found = find_all(text, query);
    found.iter().find(|range| range.start >= from).or(found.first()).cloned()
}

pub fn overlaps(a: &Range<usize>, b: &Range<usize>) -> bool {
    a.start < b.end && b.start < a.end
}

// Select Next Occurrence's state between presses. GPUI Kit can add a selection but not list them, so the session
// keeps the ones it made, in order: the first is the editor's main selection, and the next search starts after the
// last.
#[derive(Clone)]
pub struct Session {
    pub editor: EntityId,
    pub query: Query,
    pub ranges: Vec<Range<usize>>,
}

impl Session {
    // False once an edit has moved or changed what the ranges hold.
    pub fn holds(&self, text: &str) -> bool {
        self.ranges.iter().all(|range| text.get(range.clone()).is_some_and(|found| self.query.matches(found)))
    }
}

struct CurrentSession(Option<Session>);

impl Global for CurrentSession {}

pub fn set_session(session: Session, cx: &mut App) {
    cx.set_global(CurrentSession(Some(session)));
}

pub fn end_session(cx: &mut App) {
    cx.set_global(CurrentSession(None));
}

// The session for this editor and main selection. Check `holds` before trusting its ranges.
pub fn session(editor: EntityId, main: &Range<usize>, cx: &App) -> Option<Session> {
    let session = cx.try_global::<CurrentSession>()?.0.as_ref()?;
    (session.editor == editor && session.ranges.first() == Some(main)).then(|| session.clone())
}

// Ends the editor's session unless its main selection is still `main`.
fn leave_session(editor: EntityId, main: Option<&Range<usize>>, cx: &mut App) {
    let Some(session) = cx.try_global::<CurrentSession>().and_then(|current| current.0.as_ref()) else { return };
    if session.editor == editor && session.ranges.first() != main {
        end_session(cx);
    }
}

#[derive(Clone, PartialEq)]
struct Target {
    query: Query,
    // The selection whose other occurrences are marked. A caret's word is marked where it stands too.
    selection: Option<Range<usize>>,
}

fn target(editor: &Entity<EditorState>, cx: &App) -> Option<Target> {
    let state = editor.read(cx);
    let selection = state.selected_range();
    let text = state.text();
    if selection.is_empty() {
        let row = text.offset_to_point(selection.start).row;
        let line = text.slice_line(row).to_string();
        let word = word_at(&line, selection.start - text.line_start_offset(row));
        return (!word.is_empty()).then(|| Target { query: Query::word(&line[word]), selection: None });
    }
    if selection.len() > MAX_SELECTION {
        return None;
    }
    let selected = state.selected_text().to_string();
    if selected.contains('\n') || selected.trim().is_empty() {
        return None;
    }
    // While Select Next Occurrence is adding cursors, the marks show what it will pick.
    let picking = session(editor.entity_id(), &selection, cx).filter(|session| session.holds(&text.to_string()));
    let query = picking.map_or_else(|| Query::selection(&selected), |session| session.query);
    Some(Target { query, selection: Some(selection) })
}

// VS Code's occurrence highlighting: the word at the caret, or the text selected on one line, gets a background
// wherever it appears. Typing clears the marks until the caret moves again.
//
// It also ends the editor's Select Next Occurrence session once an edit or anything else moves the main selection,
// since the session can't see that happen.
pub struct OccurrenceMarks {
    editor: WeakEntity<EditorState>,
    marks: RangeDecorationCollection,
    seen: Range<usize>,
    shown: Option<Target>,
    pending: Task<()>,
    _subscriptions: [Subscription; 2],
}

impl OccurrenceMarks {
    pub fn new(editor: &Entity<EditorState>, cx: &mut Context<Self>) -> Self {
        let marks = editor.update(cx, |state, cx| state.create_range_decorations_collection(Vec::new(), cx));
        let subscriptions = [
            cx.observe(editor, |this, editor, cx| this.editor_notified(&editor, cx)),
            cx.subscribe(editor, |this, editor, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.edited(&editor, cx);
                }
            }),
        ];
        // Nothing is marked until the caret first moves, so tabs restored at launch stay plain.
        let seen = editor.read(cx).selected_range();
        Self {
            editor: editor.downgrade(),
            marks,
            seen,
            shown: None,
            pending: Task::ready(()),
            _subscriptions: subscriptions,
        }
    }

    // The editor notifies for scrolling and hovering too, so this returns early unless the selection moved.
    fn editor_notified(&mut self, editor: &Entity<EditorState>, cx: &mut Context<Self>) {
        let selection = editor.read(cx).selected_range();
        if selection == self.seen {
            return;
        }
        leave_session(editor.entity_id(), Some(&selection), cx);
        self.seen = selection;
        // Still the same word, so the marks hold, as when the caret moves along it or onto another occurrence.
        let target = target(editor, cx);
        if self.shown.is_some() && target == self.shown {
            return;
        }
        self.clear(cx);
        if target.is_some() {
            self.pending = cx.spawn(async move |this, cx| {
                cx.background_executor().timer(MARK_DELAY).await;
                let _ = this.update(cx, |this, cx| this.mark(cx));
            });
        }
    }

    fn edited(&mut self, editor: &Entity<EditorState>, cx: &mut Context<Self>) {
        leave_session(editor.entity_id(), None, cx);
        self.seen = editor.read(cx).selected_range();
        self.clear(cx);
    }

    fn clear(&mut self, cx: &mut Context<Self>) {
        self.pending = Task::ready(());
        if self.shown.take().is_some() {
            self.marks.clear(cx);
        }
    }

    fn mark(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.upgrade() else { return };
        let Some(target) = target(&editor, cx) else { return };
        let text = editor.read(cx).text();
        if text.len() > MAX_TEXT {
            return;
        }
        let text = text.to_string();
        let found = find_all(&text, &target.query).into_iter().filter(|range| Some(range) != target.selection.as_ref());
        // GPUI Kit's default fill is the editor's text colour, faint, so it follows the theme.
        let marks = found
            .take(MAX_MARKS)
            .map(|range| RangeDecoration::new(range).with_style(RangeDecorationStyle::Fill))
            .collect();
        self.marks.set(marks, cx);
        self.shown = Some(target);
    }
}

#[cfg(test)]
impl OccurrenceMarks {
    pub fn ranges(&self, cx: &App) -> Vec<Range<usize>> {
        self.marks.get_ranges(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::{Query, find_all, find_next, word_at};

    #[test]
    fn a_caret_takes_the_word_on_either_side() {
        let text = "SELECT id, user_id FROM t";
        assert_eq!(&text[word_at(text, 8)], "id");
        assert_eq!(&text[word_at(text, 9)], "id", "just after the word");
        assert_eq!(&text[word_at(text, 11)], "user_id");
        assert!(word_at("a  b", 2).is_empty());
    }

    #[test]
    fn a_word_matches_whole_and_in_its_own_case() {
        let text = "id, user_id, ID, id_x, xid, (id)";
        let found: Vec<_> = find_all(text, &Query::word("id")).into_iter().map(|r| r.start).collect();
        assert_eq!(found, vec![0, 29]);
    }

    #[test]
    fn selected_text_matches_anywhere_in_any_case() {
        let text = "id, user_id, ID";
        let found: Vec<_> = find_all(text, &Query::selection("id")).into_iter().map(|r| r.start).collect();
        assert_eq!(found, vec![0, 9, 13]);
        assert!(Query::selection("Id").matches("ID"));
        assert!(!Query::word("Id").matches("ID"));
    }

    #[test]
    fn non_ascii_text_keeps_its_byte_offsets() {
        let text = "ä id Ä id";
        let found = find_all(text, &Query::selection("ID"));
        assert_eq!(found.iter().map(|r| &text[r.clone()]).collect::<Vec<_>>(), vec!["id", "id"]);
        assert_eq!(find_all("é_x éx", &Query::word("éx")), vec![5..8]);
    }

    #[test]
    fn the_next_match_wraps_round() {
        let text = "a b a b a";
        let query = Query::word("a");
        assert_eq!(find_next(text, &query, 1), Some(4..5));
        assert_eq!(find_next(text, &query, 5), Some(8..9));
        assert_eq!(find_next(text, &query, 9), Some(0..1));
        assert_eq!(find_next(text, &Query::word("c"), 0), None);
    }
}
