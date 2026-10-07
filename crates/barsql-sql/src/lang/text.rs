use crate::lex::prim;
pub use crate::lex::prim::{char_at, is_space};

// Returns the index of the '\n' itself, or None at EOF. `from` is the first byte after the marker.
pub fn line_comment_end(text: &str, from: usize) -> Option<usize> {
    prim::line_comment_end(text.as_bytes(), from)
}

// Index just past the closing `*/`, or None when unterminated. `from` points at the opening `/*`.
pub fn block_comment_end(text: &str, from: usize, nested: bool) -> Option<usize> {
    prim::block_comment_end(text.as_bytes(), from, nested)
}

// Index past the closing quote, or None when unterminated. A doubled quote stands for one.
pub fn quote_end(text: &str, from: usize, quote: u8, backslash_escapes: bool) -> Option<usize> {
    prim::quote_end(text.as_bytes(), from, quote, backslash_escapes)
}

// `$tag$` or `$$` at `from`. Tags can't start with a digit, so `$1$` isn't one.
pub fn dollar_tag(text: &str, from: usize) -> Option<&str> {
    prim::dollar_tag_len(text.as_bytes(), from).map(|len| &text[from..from + len])
}

// Position past the dollar quote. Returns `end` if unterminated and from + 1 if there's no dollar quote.
pub fn skip_dollar_quoted(text: &str, from: usize, end: usize) -> usize {
    match prim::dollar_tag_len(text.as_bytes(), from) {
        Some(_) => prim::dollar_quoted_end(text.as_bytes(), from).unwrap_or(end),
        None => from + 1,
    }
}

// A standalone E before the quote starts a Postgres escape string. `1e'...'` and `TABLE'...'` don't count.
pub fn is_escape_string_prefix(text: &str, quote_ix: usize) -> bool {
    prim::is_escape_string_prefix(text.as_bytes(), quote_ix)
}
