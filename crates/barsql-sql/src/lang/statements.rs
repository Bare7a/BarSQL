use std::collections::VecDeque;
use std::ops::Range;
use std::sync::{Arc, LazyLock, Mutex};

use barsql_core::DriverType;

use super::text::{
    LexOptions, block_comment_end, char_at, is_escape_string_prefix, is_space, line_comment_end, quote_end,
    skip_dollar_quoted,
};
use crate::split::{Complete, CompleteToken};

// Used for run glyphs, run-at-cursor and completion scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorStatement<'a> {
    // Trimmed. Keeps a trailing `;` but not a custom DELIMITER.
    pub text: &'a str,
    // 1-based line of the first code character, where the run glyph goes.
    pub run_line: usize,
    // Contiguous with the previous statement's end.
    pub start: usize,
    pub end: usize,
}

pub(crate) fn trim_spaces(s: &str) -> &str {
    s.trim_matches(is_space)
}

fn first_code_offset(text: &str, from: usize, to: usize, opts: LexOptions) -> Option<usize> {
    let b = text.as_bytes();
    let mut i = from;
    while i < to && i < b.len() {
        let c = char_at(text, i)?;
        if is_space(c) {
            i += c.len_utf8();
            continue;
        }
        if (c == '-' && b.get(i + 1) == Some(&b'-')) || (c == '#' && opts.hash_line_comments) {
            let nl = line_comment_end(text, i + if c == '#' { 1 } else { 2 })?;
            if nl >= to {
                return None;
            }
            i = nl;
            continue;
        }
        if c == '/' && b.get(i + 1) == Some(&b'*') {
            i = block_comment_end(text, i, opts.nested_block_comments)?;
            continue;
        }
        return Some(i);
    }
    None
}

// mysql client `DELIMITER xx` line. Returns the new terminator and the length of the line.
fn delimiter_line(rest: &str) -> Option<(&str, usize)> {
    let b = rest.as_bytes();
    if b.len() < 9 || !b[..9].eq_ignore_ascii_case(b"DELIMITER") {
        return None;
    }
    let mut i = 9;
    while i < b.len() && matches!(b[i], b' ' | b'\t') {
        i += 1;
    }
    if i == 9 {
        return None;
    }
    let token_start = i;
    while let Some(c) = char_at(rest, i).filter(|&c| !is_space(c)) {
        i += c.len_utf8();
    }
    if i == token_start {
        return None;
    }
    let token = &rest[token_start..i];
    while i < b.len() && matches!(b[i], b' ' | b'\t') {
        i += 1;
    }
    if i == b.len() {
        return Some((token, i));
    }
    if b[i] == b'\r' {
        i += 1;
    }
    (b.get(i) == Some(&b'\n')).then_some((token, i + 1))
}

#[derive(Clone)]
struct Span {
    text: Range<usize>,
    run_line: usize,
    start: usize,
    end: usize,
}

type Recent = VecDeque<(Option<DriverType>, String, Arc<[Span]>)>;

// Glyphs, completion, hover and diagnostics split the same buffer for each keystroke.
static RECENT: LazyLock<Mutex<Recent>> = LazyLock::new(|| Mutex::new(VecDeque::new()));
const RECENT_TEXTS: usize = 8;

pub fn parse_statements<'a>(sql: &'a str, driver: Option<&DriverType>) -> Vec<EditorStatement<'a>> {
    let lock = || RECENT.lock().unwrap_or_else(|e| e.into_inner());
    let hit = lock().iter().find(|(d, text, _)| d.as_ref() == driver && text == sql).map(|(_, _, spans)| spans.clone());
    let spans = hit.unwrap_or_else(|| {
        let spans: Arc<[Span]> = split(sql, driver)
            .into_iter()
            .map(|s| {
                let at = s.text.as_ptr() as usize - sql.as_ptr() as usize;
                Span { text: at..at + s.text.len(), run_line: s.run_line, start: s.start, end: s.end }
            })
            .collect();
        let mut recent = lock();
        recent.push_front((driver.cloned(), sql.to_string(), spans.clone()));
        recent.truncate(RECENT_TEXTS);
        spans
    });
    spans
        .iter()
        .map(|s| EditorStatement { text: &sql[s.text.clone()], run_line: s.run_line, start: s.start, end: s.end })
        .collect()
}

fn split<'a>(sql: &'a str, driver: Option<&DriverType>) -> Vec<EditorStatement<'a>> {
    let opts = LexOptions::for_driver(driver);
    let client_delimiters = driver == Some(&DriverType::MySql);
    let trigger_bodies = driver == Some(&DriverType::Sqlite);
    let b = sql.as_bytes();
    let len = b.len();
    let mut statements = Vec::new();
    let mut stmt_start = 0;
    let mut delimiter = ";";
    let mut state = Complete::Start;
    let mut i = 0;

    // Statements come in source order, so line numbers are counted incrementally.
    let (mut line_pos, mut line) = (0, 1);
    let mut line_at = |offset: usize| {
        line += b[line_pos..offset].iter().filter(|&&c| c == b'\n').count();
        line_pos = offset;
        line
    };

    // Text is stmt_start..content_end, but the statement's region runs to slice_end.
    let mut push = |content_end: usize, slice_end: usize, stmt_start: &mut usize| {
        let start = *stmt_start;
        *stmt_start = slice_end;
        let text = trim_spaces(&sql[start..content_end]);
        if text.is_empty() {
            return;
        }
        if let Some(code_at) = first_code_offset(sql, start, content_end, opts) {
            statements.push(EditorStatement { text, run_line: line_at(code_at), start, end: slice_end });
        }
    };

    while i < len {
        let c = b[i];
        let next = b.get(i + 1).copied();
        if client_delimiters
            && (c == b'd' || c == b'D')
            && trim_spaces(&sql[sql[..i].rfind('\n').map_or(0, |p| p + 1)..i]).is_empty()
            && let Some((token, consumed)) = delimiter_line(&sql[i..])
        {
            push(i, i, &mut stmt_start);
            delimiter = token;
            i += consumed;
            stmt_start = i;
            continue;
        }
        if (c == b'-' && next == Some(b'-')) || (c == b'#' && opts.hash_line_comments) {
            i = line_comment_end(sql, i + if c == b'#' { 1 } else { 2 }).unwrap_or(len);
            continue;
        }
        if c == b'/' && next == Some(b'*') {
            i = block_comment_end(sql, i, opts.nested_block_comments).unwrap_or(len);
            continue;
        }
        let quoted = match c {
            b'\'' => Some(quote_end(sql, i, c, true, opts.backslash_escapes || is_escape_string_prefix(sql, i))),
            b'"' => Some(quote_end(sql, i, c, true, opts.backslash_escapes && opts.double_quote_strings)),
            b'`' => Some(quote_end(sql, i, c, true, false)),
            b'$' if opts.dollar_quotes => Some(Some(skip_dollar_quoted(sql, i, len))),
            _ => None,
        };
        if let Some(end) = quoted {
            i = end.unwrap_or(len).min(len);
            state = state.next(CompleteToken::Other);
            continue;
        }
        let at_delimiter = if delimiter == ";" { c == b';' } else { sql[i..].starts_with(delimiter) };
        if at_delimiter {
            state = state.next(CompleteToken::Semi);
            if trigger_bodies && state != Complete::Start {
                i += 1;
                continue;
            }
            state = Complete::Start;
            if delimiter == ";" {
                push(i + 1, i + 1, &mut stmt_start);
            } else {
                push(i, i + delimiter.len(), &mut stmt_start);
            }
            i += delimiter.len();
            continue;
        }
        if trigger_bodies {
            (state, i) = state.scan(b, i);
        } else if c < 0x80 {
            i += 1;
        } else {
            i += char_at(sql, i).map_or(1, char::len_utf8);
        }
    }

    let trailing = trim_spaces(&sql[stmt_start..]);
    if !trailing.is_empty()
        && let Some(code_at) = first_code_offset(sql, stmt_start, len, opts)
    {
        statements.push(EditorStatement { text: trailing, run_line: line_at(code_at), start: stmt_start, end: len });
    }
    statements
}

pub fn statement_at_run_line<'s, 'a>(
    statements: &'s [EditorStatement<'a>],
    line: usize,
) -> Option<&'s EditorStatement<'a>> {
    statements.iter().find(|s| s.run_line == line)
}

pub fn statement_at_offset<'s, 'a>(
    statements: &'s [EditorStatement<'a>],
    offset: usize,
) -> Option<&'s EditorStatement<'a>> {
    statements.iter().find(|s| offset >= s.start && offset < s.end)
}

// Keeps completion context from leaking across `;`. In a fresh region after a terminator this is the
// previous statement's end.
pub fn current_statement_start(statements: &[EditorStatement], offset: usize) -> usize {
    let mut start = 0;
    for s in statements {
        if s.start > offset {
            break;
        }
        start = if offset < s.end {
            s.start
        } else if s.text.ends_with(';') {
            // Right after a `;` a new region starts, but an unterminated trailing statement is still
            // being edited.
            s.end
        } else {
            s.start
        };
    }
    start
}

// In a fresh region after a terminator the range runs to the next statement or the end of the text.
pub fn current_statement_range(statements: &[EditorStatement], offset: usize, text_len: usize) -> Range<usize> {
    let start = current_statement_start(statements, offset);
    if let Some(containing) = statement_at_offset(statements, offset) {
        return start..containing.end;
    }
    let next = statements.iter().find(|s| s.start >= offset);
    start..next.map_or(text_len, |s| s.start)
}
