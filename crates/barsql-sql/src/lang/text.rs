use barsql_core::DriverType;

// Dialect lexing rules. No driver means the conservative common subset.
#[derive(Debug, Clone, Copy)]
pub struct LexOptions {
    pub hash_line_comments: bool,
    pub backslash_escapes: bool,
    pub double_quote_strings: bool,
    pub nested_block_comments: bool,
    pub dollar_quotes: bool,
}

impl LexOptions {
    pub fn for_driver(driver: Option<&DriverType>) -> Self {
        let mysql = driver == Some(&DriverType::MySql);
        Self {
            hash_line_comments: mysql,
            backslash_escapes: mysql,
            double_quote_strings: mysql,
            nested_block_comments: driver == Some(&DriverType::Postgres),
            dollar_quotes: !mysql,
        }
    }
}

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

// Returns the index of the '\n' itself, or None at EOF. `from` is the first byte after the marker.
pub fn line_comment_end(text: &str, from: usize) -> Option<usize> {
    text.as_bytes().get(from..).and_then(|rest| rest.iter().position(|&c| c == b'\n')).map(|ix| from + ix)
}

// Index just past the closing `*/`, or None when unterminated. `from` points at the opening `/*`.
pub fn block_comment_end(text: &str, from: usize, nested: bool) -> Option<usize> {
    let b = text.as_bytes();
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

// Index past the closing quote, or None when unterminated. Doubled quotes and backslash escapes are opt-in.
pub fn quote_end(text: &str, from: usize, quote: u8, double_escape: bool, backslash_escapes: bool) -> Option<usize> {
    let b = text.as_bytes();
    let mut i = from + 1;
    while i < b.len() {
        let c = b[i];
        if backslash_escapes && c == b'\\' {
            i += 2;
            continue;
        }
        if c == quote {
            if double_escape && b.get(i + 1) == Some(&quote) {
                i += 2;
                continue;
            }
            return Some(i + 1);
        }
        i += 1;
    }
    None
}

// `$tag$` or `$$` at `from`. Tags can't start with a digit, so `$1$` isn't one.
pub fn dollar_tag(text: &str, from: usize) -> Option<&str> {
    let b = text.as_bytes();
    let mut j = from + 1;
    if j < b.len() && (b[j].is_ascii_alphabetic() || b[j] == b'_') {
        j += 1;
        while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
            j += 1;
        }
    }
    (b.get(from) == Some(&b'$') && b.get(j) == Some(&b'$')).then(|| &text[from..=j])
}

// Position past the dollar quote. Returns `end` if unterminated and from + 1 if there's no dollar quote.
pub fn skip_dollar_quoted(text: &str, from: usize, end: usize) -> usize {
    let Some(tag) = dollar_tag(text, from) else { return from + 1 };
    match text[from + tag.len()..].find(tag) {
        Some(ix) => from + tag.len() + ix + tag.len(),
        None => end,
    }
}

// A standalone E before the quote starts a Postgres escape string. `1e'...'` and `TABLE'...'` don't count.
pub fn is_escape_string_prefix(text: &str, quote_ix: usize) -> bool {
    let b = text.as_bytes();
    if quote_ix < 1 || !matches!(b[quote_ix - 1], b'e' | b'E') {
        return false;
    }
    if quote_ix < 2 {
        return true;
    }
    let before = b[quote_ix - 2];
    !(before.is_ascii_alphanumeric() || matches!(before, b'_' | b'$' | b'\'' | b'"' | b'`'))
}
