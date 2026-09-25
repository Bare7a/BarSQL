use std::ops::Range;

use gpui_kit::base::input::{AddCursorAbove, AddCursorBelow};
use gpui_kit::component::input::{Copy, Cut, EditorState, Paste};
use gpui_kit::*;

// Line commands from VS Code and Zed that GPUI Kit's editor lacks. GPUI Kit keeps extra cursors private, so these
// act on the main selection and Select Next Occurrence moves it instead of adding a cursor.
actions!(
    editor_commands,
    [
        CutLine,
        CopyLine,
        PasteLine,
        SelectNextOccurrence,
        SelectLine,
        MoveLineUp,
        MoveLineDown,
        DuplicateLineUp,
        DuplicateLineDown,
        DeleteLine,
        ToggleComment,
    ]
);

pub const CONTEXT: &str = "CodeEditor";
// Clipboard metadata for a line copied or cut with no selection, so it pastes as a whole line.
const WHOLE_LINE: &str = "barsql-whole-line";

pub fn init(cx: &mut App) {
    let context = Some("CodeEditor > Input");
    let m = if cfg!(target_os = "macos") { "cmd" } else { "ctrl" };
    cx.bind_keys([
        KeyBinding::new(&format!("{m}-x"), CutLine, context),
        KeyBinding::new(&format!("{m}-c"), CopyLine, context),
        KeyBinding::new(&format!("{m}-v"), PasteLine, context),
        KeyBinding::new(&format!("{m}-d"), SelectNextOccurrence, context),
        KeyBinding::new(&format!("{m}-l"), SelectLine, context),
        KeyBinding::new("alt-up", MoveLineUp, context),
        KeyBinding::new("alt-down", MoveLineDown, context),
        KeyBinding::new("shift-alt-up", DuplicateLineUp, context),
        KeyBinding::new("shift-alt-down", DuplicateLineDown, context),
        KeyBinding::new(&format!("{m}-shift-d"), DuplicateLineDown, context),
        KeyBinding::new(&format!("{m}-shift-k"), DeleteLine, context),
        KeyBinding::new(&format!("{m}-/"), ToggleComment, context),
    ]);
    // Alt+Shift+Up/Down duplicate lines here, so Linux gets VS Code's add-cursor keys instead of GPUI Kit's.
    if !cfg!(any(target_os = "macos", target_os = "windows")) {
        cx.bind_keys([
            KeyBinding::new("ctrl-shift-up", AddCursorAbove, context),
            KeyBinding::new("ctrl-shift-down", AddCursorBelow, context),
        ]);
    }
}

// `comment` is the language's line comment prefix. None disables Toggle Comment.
pub fn handlers<E: InteractiveElement>(el: E, editor: &Entity<EditorState>, comment: Option<&'static str>) -> E {
    let on = || editor.clone();
    el.on_action({
        let editor = on();
        move |_: &CutLine, window, cx| cut_line(&editor, window, cx)
    })
    .on_action({
        let editor = on();
        move |_: &CopyLine, window, cx| copy_line(&editor, window, cx)
    })
    .on_action({
        let editor = on();
        move |_: &PasteLine, window, cx| paste_line(&editor, window, cx)
    })
    .on_action({
        let editor = on();
        move |_: &SelectNextOccurrence, _, cx| select(&editor, cx, next_occurrence)
    })
    .on_action({
        let editor = on();
        move |_: &SelectLine, _, cx| select(&editor, cx, select_line)
    })
    .on_action({
        let editor = on();
        move |_: &MoveLineUp, window, cx| edit(&editor, window, cx, |text, sel| move_lines(text, sel, true))
    })
    .on_action({
        let editor = on();
        move |_: &MoveLineDown, window, cx| edit(&editor, window, cx, |text, sel| move_lines(text, sel, false))
    })
    .on_action({
        let editor = on();
        move |_: &DuplicateLineUp, window, cx| {
            edit(&editor, window, cx, |text, sel| Some(duplicate_lines(text, sel, false)))
        }
    })
    .on_action({
        let editor = on();
        move |_: &DuplicateLineDown, window, cx| {
            edit(&editor, window, cx, |text, sel| Some(duplicate_lines(text, sel, true)))
        }
    })
    .on_action({
        let editor = on();
        move |_: &DeleteLine, window, cx| edit(&editor, window, cx, |text, sel| Some(delete_lines(text, sel)))
    })
    .on_action({
        let editor = on();
        move |_: &ToggleComment, window, cx| {
            if let Some(prefix) = comment {
                edit(&editor, window, cx, |text, sel| Some(toggle_comment(text, sel, prefix)))
            }
        }
    })
}

// Range to replace, replacement text, selection afterwards.
type Change = (Range<usize>, String, Range<usize>);

fn read(editor: &Entity<EditorState>, cx: &App) -> (String, Range<usize>, bool) {
    let state = editor.read(cx);
    (state.value().to_string(), state.selected_range(), state.is_editable())
}

fn edit(
    editor: &Entity<EditorState>,
    window: &mut Window,
    cx: &mut App,
    change: impl FnOnce(&str, Range<usize>) -> Option<Change>,
) {
    let (text, sel, editable) = read(editor, cx);
    let Some((range, new, select)) = change(&text, sel).filter(|_| editable) else { return };
    editor.update(cx, |state, cx| {
        state.set_selected_range(range, cx);
        state.replace(new, window, cx);
        state.set_selected_range(select, cx);
    });
}

fn select(editor: &Entity<EditorState>, cx: &mut App, pick: impl FnOnce(&str, Range<usize>) -> Option<Range<usize>>) {
    let (text, sel, _) = read(editor, cx);
    if let Some(range) = pick(&text, sel) {
        editor.update(cx, |state, cx| state.set_selected_range(range, cx));
    }
}

// With no selection, cuts the whole line including its newline.
fn cut_line(editor: &Entity<EditorState>, window: &mut Window, cx: &mut App) {
    let (text, sel, editable) = read(editor, cx);
    if !sel.is_empty() {
        window.dispatch_action(Box::new(Cut), cx);
    } else if editable {
        cx.write_to_clipboard(ClipboardItem::new_string_with_metadata(line_text(&text, sel.start), WHOLE_LINE.into()));
        let (range, _, select) = delete_lines(&text, sel);
        editor.update(cx, |state, cx| {
            state.set_selected_range(range, cx);
            state.replace("", window, cx);
            state.set_selected_range(select, cx);
        });
    }
}

fn copy_line(editor: &Entity<EditorState>, window: &mut Window, cx: &mut App) {
    let (text, sel, _) = read(editor, cx);
    if sel.is_empty() {
        cx.write_to_clipboard(ClipboardItem::new_string_with_metadata(line_text(&text, sel.start), WHOLE_LINE.into()));
    } else {
        window.dispatch_action(Box::new(Copy), cx);
    }
}

// Like VS Code, a whole-line copy pastes above the caret's line and the caret keeps its place. Anything else, or a
// paste over a selection, is a normal paste.
fn paste_line(editor: &Entity<EditorState>, window: &mut Window, cx: &mut App) {
    let (text, sel, editable) = read(editor, cx);
    let line = cx
        .read_from_clipboard()
        .filter(|item| editable && sel.is_empty() && item.metadata().is_some_and(|meta| meta == WHOLE_LINE))
        .and_then(|item| item.text());
    let Some(line) = line else {
        window.dispatch_action(Box::new(Paste), cx);
        return;
    };
    let (start, caret) = (line_start(&text, sel.start), sel.start + line.len());
    editor.update(cx, |state, cx| {
        state.set_selected_range(start..start, cx);
        state.replace(line, window, cx);
        state.set_selected_range(caret..caret, cx);
    });
}

fn line_start(text: &str, at: usize) -> usize {
    text[..at].rfind('\n').map_or(0, |ix| ix + 1)
}

fn line_end(text: &str, at: usize) -> usize {
    text[at..].find('\n').map_or(text.len(), |ix| at + ix)
}

// Whole lines the selection touches, minus the last newline. Like VS Code, a selection ending at a line start
// leaves that line out.
fn line_block(text: &str, sel: Range<usize>) -> Range<usize> {
    let start = line_start(text, sel.start);
    let last = if sel.end > sel.start && text[..sel.end].ends_with('\n') { sel.end - 1 } else { sel.end };
    start..line_end(text, last.max(start))
}

fn line_text(text: &str, at: usize) -> String {
    format!("{}\n", &text[line_block(text, at..at)])
}

fn shift(range: &Range<usize>, by: isize) -> Range<usize> {
    let move_by = |at: usize| at.saturating_add_signed(by);
    move_by(range.start)..move_by(range.end)
}

fn move_lines(text: &str, sel: Range<usize>, up: bool) -> Option<Change> {
    let block = line_block(text, sel.clone());
    let lines = &text[block.clone()];
    if up {
        let above = line_start(text, block.start.checked_sub(1)?)..block.start - 1;
        let new = format!("{lines}\n{}", &text[above.clone()]);
        Some((above.start..block.end, new, shift(&sel, -(above.len() as isize + 1))))
    } else {
        let below = (block.end < text.len()).then(|| block.end + 1..line_end(text, block.end + 1))?;
        let new = format!("{}\n{lines}", &text[below.clone()]);
        Some((block.start..below.end, new, shift(&sel, below.len() as isize + 1)))
    }
}

// Both give two copies. `down` moves the selection to the lower one, otherwise it stays on the upper one.
fn duplicate_lines(text: &str, sel: Range<usize>, down: bool) -> Change {
    let block = line_block(text, sel.clone());
    let lines = &text[block.clone()];
    let select = if down { shift(&sel, lines.len() as isize + 1) } else { sel };
    (block, format!("{lines}\n{lines}"), select)
}

fn delete_lines(text: &str, sel: Range<usize>) -> Change {
    let block = line_block(text, sel);
    let range = if block.end < text.len() {
        block.start..block.end + 1
    } else if block.start > 0 {
        block.start - 1..block.end
    } else {
        block.clone()
    };
    let caret = if range.start < block.start { line_start(text, range.start) } else { range.start };
    (range, String::new(), caret..caret)
}

fn toggle_comment(text: &str, sel: Range<usize>, prefix: &str) -> Change {
    let block = line_block(text, sel.clone());
    let marker = prefix.trim_end();
    let lines: Vec<&str> = text[block.clone()].split('\n').collect();
    let filled = || lines.iter().copied().filter(|line| !line.trim().is_empty());
    let indent = |line: &str| line.len() - line.trim_start().len();
    let commented = filled().all(|line| line.trim_start().starts_with(marker));
    let column = filled().map(indent).min().unwrap_or(0);
    let new = lines
        .iter()
        .copied()
        .map(|line| match line.trim().is_empty() {
            true => line.to_string(),
            false if commented => {
                let rest = &line[indent(line) + marker.len()..];
                format!("{}{}", &line[..indent(line)], rest.strip_prefix(' ').unwrap_or(rest))
            }
            false => format!("{}{prefix}{}", &line[..column], &line[column..]),
        })
        .collect::<Vec<_>>()
        .join("\n");
    let grown = new.len() as isize - block.len() as isize;
    let select = if sel.is_empty() {
        let caret = sel.start.saturating_add_signed(grown).max(block.start);
        caret..caret
    } else {
        block.start..block.end.saturating_add_signed(grown)
    };
    (block, new, select)
}

fn is_word(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

fn word_at(text: &str, at: usize) -> Range<usize> {
    let start = text[..at].char_indices().rev().take_while(|(_, ch)| is_word(*ch)).last().map_or(at, |(ix, _)| ix);
    let end = text[at..].char_indices().find(|(_, ch)| !is_word(*ch)).map_or(text.len(), |(ix, _)| at + ix);
    start..end
}

// With no selection, picks the word at the caret. Otherwise finds the next match, wrapping around.
fn next_occurrence(text: &str, sel: Range<usize>) -> Option<Range<usize>> {
    if sel.is_empty() {
        return Some(word_at(text, sel.start)).filter(|word| !word.is_empty());
    }
    let needle = &text[sel.clone()];
    let at = text[sel.end..].find(needle).map(|ix| sel.end + ix).or_else(|| text[..sel.start].find(needle))?;
    Some(at..at + needle.len())
}

// Selects the line with its newline. Repeating extends to the next line.
fn select_line(text: &str, sel: Range<usize>) -> Option<Range<usize>> {
    let block = line_block(text, sel.clone());
    let whole = block.start..(block.end + 1).min(text.len());
    if sel == whole && whole.end < text.len() {
        return Some(whole.start..(line_end(text, whole.end) + 1).min(text.len()));
    }
    Some(whole)
}

#[cfg(test)]
mod tests {
    use super::{
        Change, delete_lines, duplicate_lines, line_block, line_text, move_lines, next_occurrence, select_line,
        toggle_comment,
    };

    fn apply(text: &str, (range, new, _): Change) -> String {
        format!("{}{new}{}", &text[..range.start], &text[range.end..])
    }

    #[test]
    fn a_selection_ending_at_a_line_start_leaves_that_line_out() {
        let text = "one\ntwo\nthree";
        assert_eq!(&text[line_block(text, 5..5)], "two");
        assert_eq!(&text[line_block(text, 1..8)], "one\ntwo");
        assert_eq!(&text[line_block(text, 1..9)], "one\ntwo\nthree");
    }

    #[test]
    fn lines_move_past_their_neighbours_and_keep_the_selection() {
        let text = "one\ntwo\nthree";
        let up = move_lines(text, 5..6, true).unwrap();
        assert_eq!(up.2, 1..2);
        assert_eq!(apply(text, up), "two\none\nthree");
        let down = move_lines(text, 5..6, false).unwrap();
        assert_eq!(down.2, 11..12);
        assert_eq!(apply(text, down), "one\nthree\ntwo");
        assert!(move_lines(text, 0..0, true).is_none());
        assert!(move_lines(text, 9..9, false).is_none());
    }

    #[test]
    fn duplicates_and_deletes_take_whole_lines() {
        let text = "one\ntwo\nthree";
        let copy = duplicate_lines(text, 4..4, true);
        assert_eq!(copy.2, 8..8);
        assert_eq!(apply(text, copy), "one\ntwo\ntwo\nthree");
        assert_eq!(apply(text, delete_lines(text, 5..5)), "one\nthree");
        assert_eq!(apply(text, delete_lines(text, 10..10)), "one\ntwo");
        assert_eq!(delete_lines(text, 10..10).2, 4..4);
        assert_eq!(line_text(text, 10), "three\n");
    }

    #[test]
    fn comments_toggle_at_the_shallowest_indent() {
        let text = "SELECT 1\n  FROM t\n\nWHERE x";
        let on = apply(text, toggle_comment(text, 0..text.len(), "-- "));
        assert_eq!(on, "-- SELECT 1\n--   FROM t\n\n-- WHERE x");
        assert_eq!(apply(&on, toggle_comment(&on, 0..on.len(), "-- ")), text);
        let indented = "  a\n    b";
        assert_eq!(apply(indented, toggle_comment(indented, 0..9, "-- ")), "  -- a\n  --   b");
        assert_eq!(toggle_comment("abc", 1..1, "-- ").2, 4..4);
    }

    #[test]
    fn select_next_takes_the_word_then_the_following_match() {
        let text = "SELECT id FROM t WHERE id > 1 AND ids";
        assert_eq!(next_occurrence(text, 8..8), Some(7..9));
        assert_eq!(next_occurrence(text, 7..9), Some(23..25));
        assert_eq!(next_occurrence(text, 23..25), Some(34..36));
        assert_eq!(next_occurrence(text, 34..36), Some(7..9));
        assert_eq!(next_occurrence(text, 6..6), Some(0..6));
        assert_eq!(next_occurrence("a  b", 2..2), None);
    }

    #[test]
    fn select_line_grows_a_line_at_a_time() {
        let text = "one\ntwo\nthree";
        assert_eq!(select_line(text, 1..1), Some(0..4));
        assert_eq!(select_line(text, 0..4), Some(0..8));
        assert_eq!(select_line(text, 9..9), Some(8..13));
    }
}
