use std::collections::VecDeque;
use std::ops::Range;
use std::sync::{Arc, LazyLock, Mutex};

use barsql_core::DriverType;

use super::text::{block_comment_end, char_at, is_space, line_comment_end};
use crate::lex::LexRules;
use crate::lex::boundary::{Boundary, Mode, boundaries};
use crate::lex::prim::{dash_comment_at, hash_comment_at};

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
    // Ends with `;` or a GO line, so the text after it starts a new region.
    pub terminated: bool,
}

pub(crate) fn trim_spaces(s: &str) -> &str {
    s.trim_matches(is_space)
}

fn first_code_offset(text: &str, from: usize, to: usize, rules: &LexRules) -> Option<usize> {
    let b = text.as_bytes();
    let mut i = from;
    while i < to && i < b.len() {
        let c = char_at(text, i)?;
        if is_space(c) {
            i += c.len_utf8();
            continue;
        }
        let dash = dash_comment_at(b, i, rules.dash_needs_space);
        if dash || hash_comment_at(b, i, rules.hash_comments) {
            let nl = line_comment_end(text, i + if dash { 2 } else { 1 })?;
            if nl >= to {
                return None;
            }
            i = nl;
            continue;
        }
        if c == '/' && b.get(i + 1) == Some(&b'*') {
            i = block_comment_end(text, i, rules.nested_block_comments)?;
            continue;
        }
        return Some(i);
    }
    None
}

#[derive(Clone)]
struct Span {
    text: Range<usize>,
    run_line: usize,
    start: usize,
    end: usize,
    terminated: bool,
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
                Span {
                    text: at..at + s.text.len(),
                    run_line: s.run_line,
                    start: s.start,
                    end: s.end,
                    terminated: s.terminated,
                }
            })
            .collect();
        let mut recent = lock();
        recent.push_front((driver.cloned(), sql.to_string(), spans.clone()));
        recent.truncate(RECENT_TEXTS);
        spans
    });
    spans
        .iter()
        .map(|s| EditorStatement {
            text: &sql[s.text.clone()],
            run_line: s.run_line,
            start: s.start,
            end: s.end,
            terminated: s.terminated,
        })
        .collect()
}

fn split<'a>(sql: &'a str, driver: Option<&DriverType>) -> Vec<EditorStatement<'a>> {
    let rules = LexRules::for_driver(driver);
    let b = sql.as_bytes();
    let len = b.len();
    let mut statements = Vec::new();
    let mut stmt_start = 0;

    // Statements come in source order, so line numbers are counted incrementally.
    let (mut line_pos, mut line) = (0, 1);
    let mut line_at = |offset: usize| {
        line += b[line_pos..offset].iter().filter(|&&c| c == b'\n').count();
        line_pos = offset;
        line
    };

    // Text is stmt_start..content_end, but the statement's region runs to slice_end.
    let mut push = |content_end: usize, slice_end: usize, stmt_start: &mut usize, batch_end: bool| {
        let start = *stmt_start;
        *stmt_start = slice_end;
        let text = trim_spaces(&sql[start..content_end]);
        if text.is_empty() {
            return;
        }
        if let Some(code_at) = first_code_offset(sql, start, content_end, &rules) {
            let terminated = batch_end || text.ends_with(';');
            statements.push(EditorStatement { text, run_line: line_at(code_at), start, end: slice_end, terminated });
        }
    };

    for boundary in boundaries(sql, &rules, Mode::Statements) {
        match boundary {
            Boundary::Terminator { at, custom: false, .. } => push(at + 1, at + 1, &mut stmt_start, false),
            // A custom DELIMITER isn't part of the statement's text, but its region covers it.
            Boundary::Terminator { at, len, custom: true } => push(at, at + len, &mut stmt_start, false),
            // DELIMITER and GO lines belong to no statement.
            Boundary::DelimiterLine { at, end } => {
                push(at, at, &mut stmt_start, false);
                stmt_start = end;
            }
            Boundary::BatchSeparator { at, end, .. } => {
                push(at, at, &mut stmt_start, true);
                stmt_start = end;
            }
        }
    }

    let trailing = trim_spaces(&sql[stmt_start..]);
    if !trailing.is_empty()
        && let Some(code_at) = first_code_offset(sql, stmt_start, len, &rules)
    {
        let terminated = trailing.ends_with(';');
        statements.push(EditorStatement {
            text: trailing,
            run_line: line_at(code_at),
            start: stmt_start,
            end: len,
            terminated,
        });
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
        } else if s.terminated {
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
