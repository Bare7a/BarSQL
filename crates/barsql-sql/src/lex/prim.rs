// Byte-level lexing primitives. Offsets are byte offsets into the text; helpers return None when the input
// ends inside the construct.

use super::rules::HashComment;

// Unicode whitespace: \t \n \v \f \r, the space separators (Zs), U+2028, U+2029 and U+FEFF.
pub fn is_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}'
    )
}

pub fn char_at(text: &str, i: usize) -> Option<char> {
    text.get(i..).and_then(|rest| rest.chars().next())
}

pub(crate) fn trim_spaces(s: &str) -> &str {
    s.trim_matches(is_space)
}

// MySQL only opens a `--` comment when whitespace, a control character or the end follows the dashes, so
// `1--1` is arithmetic there. Everywhere else `--` always starts a comment.
pub(crate) fn dash_comment_at(b: &[u8], i: usize, needs_space: bool) -> bool {
    b.get(i) == Some(&b'-')
        && b.get(i + 1) == Some(&b'-')
        && (!needs_space || b.get(i + 2).is_none_or(|&c| c <= b' ' || c == 0x7f))
}

pub(crate) fn hash_comment_at(b: &[u8], i: usize, rule: HashComment) -> bool {
    b.get(i) == Some(&b'#')
        && match rule {
            HashComment::Never => false,
            HashComment::Always => true,
            HashComment::SpaceOrBang => matches!(b.get(i + 1), Some(b' ' | b'!')),
        }
}

// Index of the terminating '\n' (left unconsumed), or None at the end. `from` is the first byte after the marker.
pub(crate) fn line_comment_end(b: &[u8], from: usize) -> Option<usize> {
    b.get(from..).and_then(|rest| rest.iter().position(|&c| c == b'\n')).map(|ix| from + ix)
}

// Index just past the closing `*/`, or None when unterminated. `from` points at the opening `/*`.
pub(crate) fn block_comment_end(b: &[u8], from: usize, nested: bool) -> Option<usize> {
    let mut i = from + 2;
    let mut depth = 1;
    while i < b.len() {
        if b[i] == b'*' && b.get(i + 1) == Some(&b'/') {
            i += 2;
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        } else if nested && b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
            depth += 1;
            i += 2;
        } else {
            i += 1;
        }
    }
    None
}

// Index past the closing quote, or None when unterminated. A doubled quote stands for one.
pub(crate) fn quote_end(b: &[u8], from: usize, quote: u8, backslash_escapes: bool) -> Option<usize> {
    let mut i = from + 1;
    while i < b.len() {
        let c = b[i];
        if backslash_escapes && c == b'\\' {
            i += 2;
            continue;
        }
        if c == quote {
            if b.get(i + 1) == Some(&quote) {
                i += 2;
                continue;
            }
            return Some(i + 1);
        }
        i += 1;
    }
    None
}

// [ident] with ]] standing for a `]`.
pub(crate) fn bracket_end(b: &[u8], from: usize) -> Option<usize> {
    let mut i = from + 1;
    while i < b.len() {
        if b[i] == b']' {
            if b.get(i + 1) == Some(&b']') {
                i += 2;
                continue;
            }
            return Some(i + 1);
        }
        i += 1;
    }
    None
}

// A standalone E before the quote starts a Postgres escape string. `1e'...'` and `TABLE'...'` don't count.
pub(crate) fn is_escape_string_prefix(b: &[u8], quote_ix: usize) -> bool {
    if quote_ix < 1 || !matches!(b[quote_ix - 1], b'e' | b'E') {
        return false;
    }
    if quote_ix < 2 {
        return true;
    }
    let before = b[quote_ix - 2];
    !(before.is_ascii_alphanumeric() || matches!(before, b'_' | b'$' | b'\'' | b'"' | b'`'))
}

// Length of a `$tag$` or `$$` at `i`. Tags can't start with a digit, so `$1$` is a placeholder, not a tag.
pub(crate) fn dollar_tag_len(b: &[u8], i: usize) -> Option<usize> {
    let mut j = i + 1;
    if j < b.len() && (b[j].is_ascii_alphabetic() || b[j] == b'_') {
        j += 1;
        while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
            j += 1;
        }
    }
    (b.get(i) == Some(&b'$') && b.get(j) == Some(&b'$')).then_some(j + 1 - i)
}

// Index past the closing tag of the dollar quote opening at `i`, or None when unterminated. `i` must open one.
pub(crate) fn dollar_quoted_end(b: &[u8], i: usize) -> Option<usize> {
    let tag_len = dollar_tag_len(b, i)?;
    let tag = &b[i..i + tag_len];
    let body = i + tag_len;
    b[body..].windows(tag_len).position(|w| w == tag).map(|ix| body + ix + tag_len)
}

pub(crate) fn at_line_start(sql: &str, i: usize) -> bool {
    let line_start = sql[..i].rfind('\n').map_or(0, |p| p + 1);
    trim_spaces(&sql[line_start..i]).is_empty()
}

// mysql client `DELIMITER xx` line at the start of `rest`. Returns the new terminator and the line's length,
// including its newline.
pub(crate) fn delimiter_line(rest: &str) -> Option<(&str, usize)> {
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

// sqlcmd-style `GO [count]` line at the start of `rest`, with an optional trailing `--` comment. Returns the
// repeat count and the line's length, including its newline.
pub(crate) fn go_line(rest: &str) -> Option<(u32, usize)> {
    let b = rest.as_bytes();
    if b.len() < 2 || !b[..2].eq_ignore_ascii_case(b"GO") {
        return None;
    }
    let mut i = 2;
    let blank = |c: u8| matches!(c, b' ' | b'\t');
    let digits_start = {
        while i < b.len() && blank(b[i]) {
            i += 1;
        }
        i
    };
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    let repeat = if i > digits_start {
        // `GO2` isn't a separator, `GO 2` is.
        if digits_start == 2 {
            return None;
        }
        rest[digits_start..i].parse::<u32>().ok().filter(|&n| n > 0)?
    } else {
        1
    };
    while i < b.len() && blank(b[i]) {
        i += 1;
    }
    if dash_comment_at(b, i, false) {
        i = line_comment_end(b, i + 2).unwrap_or(b.len());
    }
    if i == b.len() {
        return Some((repeat, i));
    }
    if b[i] == b'\r' {
        i += 1;
    }
    (b.get(i) == Some(&b'\n')).then_some((repeat, i + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn go_lines() {
        let cases: &[(&str, Option<(u32, usize)>)] = &[
            ("GO", Some((1, 2))),
            ("go\nSELECT 1", Some((1, 3))),
            ("GO 3\n", Some((3, 5))),
            ("GO  2  -- twice\r\nx", Some((2, 17))),
            ("GO -- end", Some((1, 9))),
            ("GOTO done", None),
            ("GO2", None),
            ("GO 0", None),
            ("GO x", None),
            ("G", None),
        ];
        for (rest, want) in cases {
            assert_eq!(go_line(rest), *want, "{rest:?}");
        }
    }

    #[test]
    fn brackets_and_dashes() {
        assert_eq!(bracket_end(b"[a]]b] x", 0), Some(6));
        assert_eq!(bracket_end(b"[a", 0), None);
        assert!(dash_comment_at(b"--1", 0, false));
        assert!(!dash_comment_at(b"--1", 0, true));
        assert!(dash_comment_at(b"--", 0, true));
        assert!(hash_comment_at(b"# x", 0, HashComment::SpaceOrBang));
        assert!(hash_comment_at(b"#!x", 0, HashComment::SpaceOrBang));
        assert!(!hash_comment_at(b"#x", 0, HashComment::SpaceOrBang));
    }
}
