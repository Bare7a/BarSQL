use barsql_core::DriverType;

use super::text::{
    block_comment_end, char_at, dollar_tag, is_escape_string_prefix, is_space, line_comment_end, quote_end,
};
use crate::lex::LexRules;
use crate::lex::prim::{bracket_end, dash_comment_at, hash_comment_at};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    // Bare identifier or keyword. `lower` has the lowercased text.
    Ident,
    // "ident", `ident` or [ident]
    Quoted,
    // '...', and "..." on MySQL where it's a literal
    String,
    // Numeric literal or $n placeholder
    Number,
    Op,
    // . , ( ) ;
    Punct,
    Comment,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token<'a> {
    pub kind: TokenKind,
    pub text: &'a str,
    // Empty unless the token is an Ident.
    pub lower: String,
    pub start: usize,
    pub end: usize,
    // A string, quoted identifier or comment cut off by the end of input.
    pub unterminated: bool,
}

impl Token<'_> {
    pub fn is_ident_like(&self) -> bool {
        matches!(self.kind, TokenKind::Ident | TokenKind::Quoted)
    }

    pub fn is_keyword(&self, word: &str) -> bool {
        self.kind == TokenKind::Ident && self.lower == word
    }

    pub fn is_punct(&self, ch: &str) -> bool {
        self.kind == TokenKind::Punct && self.text == ch
    }

    // "a""b" -> a"b, `x` -> x and [a]]b] -> a]b. Bare idents pass through.
    pub fn ident_text(&self) -> String {
        if self.kind != TokenKind::Quoted {
            return self.text.to_string();
        }
        let quote = self.text.as_bytes()[0];
        let close = if self.unterminated { self.text.len() } else { self.text.len().saturating_sub(1).max(1) };
        let inner = &self.text[1..close];
        match quote {
            b'"' => inner.replace("\"\"", "\""),
            b'`' => inner.replace("``", "`"),
            b'[' => inner.replace("]]", "]"),
            _ => inner.to_string(),
        }
    }
}

pub fn is_ident_like(t: Option<&Token>) -> bool {
    t.is_some_and(Token::is_ident_like)
}

pub fn is_keyword(t: Option<&Token>, word: &str) -> bool {
    t.is_some_and(|t| t.is_keyword(word))
}

pub fn is_punct(t: Option<&Token>, ch: &str) -> bool {
    t.is_some_and(|t| t.is_punct(ch))
}

const TWO_CHAR_OPS: [&str; 7] = ["<=", ">=", "<>", "!=", "::", "||", ":="];

fn is_ident_start(c: u8, rules: &LexRules) -> bool {
    c.is_ascii_alphabetic() || c == b'_' || (rules.at_hash_words && matches!(c, b'@' | b'#'))
}

fn is_ident_char(c: u8, rules: &LexRules) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'$' || (rules.at_hash_words && matches!(c, b'@' | b'#'))
}

// $tag$ delimiters are ops, so dollar-quoted bodies stay tokenized and completion works inside them.
pub fn tokenize<'a>(text: &'a str, driver: Option<&DriverType>) -> Vec<Token<'a>> {
    let rules = LexRules::for_driver(driver);
    let b = text.as_bytes();
    let len = b.len();
    let mut tokens = Vec::new();
    let mut push = |kind: TokenKind, start: usize, end: usize, unterminated: bool| {
        let raw = &text[start..end];
        let lower = if kind == TokenKind::Ident { raw.to_ascii_lowercase() } else { String::new() };
        tokens.push(Token { kind, text: raw, lower, start, end, unterminated });
    };

    let mut i = 0;
    while i < len {
        let c = b[i];
        if c >= 0x80 {
            let ch = char_at(text, i).unwrap_or('\u{fffd}');
            let width = ch.len_utf8();
            if !is_space(ch) {
                push(TokenKind::Op, i, i + width, false);
            }
            i += width;
            continue;
        }
        if is_space(c as char) {
            i += 1;
            continue;
        }
        let next = b.get(i + 1).copied();

        let dash = dash_comment_at(b, i, rules.dash_needs_space);
        if dash || hash_comment_at(b, i, rules.hash_comments) {
            let nl = line_comment_end(text, if dash { i + 2 } else { i + 1 });
            let end = nl.unwrap_or(len);
            push(TokenKind::Comment, i, end, nl.is_none());
            i = end;
            continue;
        }
        if c == b'/' && next == Some(b'*') {
            let close = block_comment_end(text, i, rules.nested_block_comments);
            push(TokenKind::Comment, i, close.unwrap_or(len), close.is_none());
            i = close.unwrap_or(len);
            continue;
        }
        if c == b'\'' || (c == b'"' && rules.double_quote_strings) {
            let escapes = rules.backslash_escapes || (rules.escape_strings && is_escape_string_prefix(text, i));
            let close = quote_end(text, i, c, escapes).map(|e| e.min(len));
            push(TokenKind::String, i, close.unwrap_or(len), close.is_none());
            i = close.unwrap_or(len);
            continue;
        }
        // T-SQL N'...' is one string.
        if rules.national_strings && matches!(c, b'N' | b'n') && next == Some(b'\'') {
            let close = quote_end(text, i + 1, b'\'', false);
            push(TokenKind::String, i, close.unwrap_or(len), close.is_none());
            i = close.unwrap_or(len);
            continue;
        }
        if c == b'"' || c == b'`' {
            let close = quote_end(text, i, c, rules.ident_backslash);
            push(TokenKind::Quoted, i, close.unwrap_or(len), close.is_none());
            i = close.unwrap_or(len);
            continue;
        }
        if c == b'[' && rules.bracket_idents {
            let close = bracket_end(b, i);
            push(TokenKind::Quoted, i, close.unwrap_or(len), close.is_none());
            i = close.unwrap_or(len);
            continue;
        }
        if c == b'$' {
            if rules.dollar_quotes
                && let Some(tag) = dollar_tag(text, i)
            {
                push(TokenKind::Op, i, i + tag.len(), false);
                i += tag.len();
                continue;
            }
            if next.is_some_and(|n| n.is_ascii_digit()) {
                let mut j = i + 1;
                while j < len && b[j].is_ascii_digit() {
                    j += 1;
                }
                push(TokenKind::Number, i, j, false);
                i = j;
                continue;
            }
            push(TokenKind::Op, i, i + 1, false);
            i += 1;
            continue;
        }
        if is_ident_start(c, &rules) {
            let mut j = i + 1;
            while j < len && is_ident_char(b[j], &rules) {
                j += 1;
            }
            push(TokenKind::Ident, i, j, false);
            i = j;
            continue;
        }
        if c.is_ascii_digit() {
            let mut j = i + 1;
            while j < len && (b[j].is_ascii_alphanumeric() || b[j] == b'.' || b[j] == b'_') {
                j += 1;
            }
            push(TokenKind::Number, i, j, false);
            i = j;
            continue;
        }
        if matches!(c, b'.' | b',' | b'(' | b')' | b';') {
            push(TokenKind::Punct, i, i + 1, false);
            i += 1;
            continue;
        }
        let two_char_op = |op: &str| TWO_CHAR_OPS.contains(&op) || (rules.arrow_op && op == "->");
        if next.is_some_and(|n| n < 0x80 && two_char_op(&text[i..i + 2])) {
            push(TokenKind::Op, i, i + 2, false);
            i += 2;
            continue;
        }
        push(TokenKind::Op, i, i + 1, false);
        i += 1;
    }
    tokens
}

pub fn prev_code_token(tokens: &[Token], from: usize) -> Option<usize> {
    (0..from.min(tokens.len())).rev().find(|&i| tokens[i].kind != TokenKind::Comment)
}

pub fn next_code_token(tokens: &[Token], from: Option<usize>) -> Option<usize> {
    let start = from.map_or(0, |f| f + 1);
    (start..tokens.len()).find(|&i| tokens[i].kind != TokenKind::Comment)
}
