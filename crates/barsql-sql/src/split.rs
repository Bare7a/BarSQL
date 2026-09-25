use std::ops::Range;

use barsql_core::DriverType;

#[derive(Debug, Clone, Copy, Default)]
pub struct Dialect {
    pub hash_line_comments: bool,
    pub backslash_escapes: bool,
    pub double_quote_strings: bool,
    pub nested_block_comments: bool,
    pub dollar_quotes: bool,
    pub client_delimiters: bool,
    // SQLite trigger bodies contain `;`. Like sqlite3_complete, a CREATE TRIGGER ends at `; END ;`.
    pub trigger_bodies: bool,
}

impl Dialect {
    pub fn for_driver(driver: &DriverType) -> Self {
        let mysql = *driver == DriverType::MySql;
        Self {
            hash_line_comments: mysql,
            backslash_escapes: mysql,
            double_quote_strings: mysql,
            nested_block_comments: *driver == DriverType::Postgres,
            dollar_quotes: !mysql,
            client_delimiters: mysql,
            trigger_bodies: *driver == DriverType::Sqlite,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Statement<'a> {
    pub text: &'a str,
    pub range: Range<usize>,
}

// Drops terminators and comment-only chunks. Ranges point at the trimmed statement in `sql`.
pub fn split_statements<'a>(driver: &DriverType, sql: &'a str) -> Vec<Statement<'a>> {
    let dialect = Dialect::for_driver(driver);
    let b = sql.as_bytes();
    let n = b.len();
    let mut out = Vec::new();
    let mut start = 0;
    let mut delimiter: &str = ";";
    let mut state = Complete::Start;
    let mut i = 0;

    let push = |start: usize, end: usize, out: &mut Vec<Statement<'a>>| {
        let chunk = &sql[start..end];
        if has_code(chunk.as_bytes(), dialect) {
            let lead = chunk.len() - chunk.trim_start().len();
            let text = chunk.trim();
            out.push(Statement { text, range: start + lead..start + lead + text.len() });
        }
    };

    while i < n {
        let c = b[i];
        if dialect.client_delimiters
            && (c == b'd' || c == b'D')
            && at_line_start(sql, i)
            && let Some((consumed, next)) = delimiter_line(&sql[i..])
        {
            push(start, i, &mut out);
            delimiter = next;
            i += consumed;
            start = i;
            continue;
        }
        match c {
            b'-' if b.get(i + 1) == Some(&b'-') => i = line_comment_end(b, i + 2),
            b'#' if dialect.hash_line_comments => i = line_comment_end(b, i + 1),
            b'/' if b.get(i + 1) == Some(&b'*') => i = block_comment_end(b, i, dialect.nested_block_comments),
            b'\'' => {
                i = quote_end(b, i, b'\'', dialect.backslash_escapes || is_escape_string_prefix(b, i));
                state = state.next(CompleteToken::Other);
            }
            b'"' => {
                i = quote_end(b, i, b'"', dialect.backslash_escapes && dialect.double_quote_strings);
                state = state.next(CompleteToken::Other);
            }
            b'`' => {
                i = quote_end(b, i, b'`', false);
                state = state.next(CompleteToken::Other);
            }
            b'$' if dialect.dollar_quotes => {
                i = dollar_quoted_end(b, i);
                state = state.next(CompleteToken::Other);
            }
            _ if c == delimiter.as_bytes()[0] && b[i..].starts_with(delimiter.as_bytes()) => {
                state = state.next(CompleteToken::Semi);
                if !dialect.trigger_bodies || state == Complete::Start {
                    push(start, i, &mut out);
                    start = i + delimiter.len();
                    state = Complete::Start;
                }
                i += delimiter.len();
            }
            _ if dialect.trigger_bodies => (state, i) = state.scan(b, i),
            _ => i += 1,
        }
    }
    push(start, n, &mut out);
    out
}

// Mirrors sqlite3_complete's states. Whitespace and comments don't change the state.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Complete {
    Start,
    Normal,
    Explain,
    Create,
    Trigger,
    Semi,
    End,
}

#[derive(Clone, Copy)]
pub(crate) enum CompleteToken {
    Semi,
    Other,
    Explain,
    Create,
    Temp,
    Trigger,
    End,
}

impl CompleteToken {
    pub(crate) fn word(w: &[u8]) -> Self {
        let is = |k: &[u8]| w.eq_ignore_ascii_case(k);
        if is(b"END") {
            Self::End
        } else if is(b"CREATE") {
            Self::Create
        } else if is(b"TEMP") || is(b"TEMPORARY") {
            Self::Temp
        } else if is(b"TRIGGER") {
            Self::Trigger
        } else if is(b"EXPLAIN") {
            Self::Explain
        } else {
            Self::Other
        }
    }
}

impl Complete {
    // Start after a Semi means the statement is complete.
    pub(crate) fn next(self, token: CompleteToken) -> Self {
        match (self, token) {
            (Self::Trigger | Self::Semi, CompleteToken::Semi) => Self::Semi,
            (_, CompleteToken::Semi) => Self::Start,
            (Self::Semi, CompleteToken::End) => Self::End,
            (Self::Trigger | Self::Semi | Self::End, _) => Self::Trigger,
            (Self::Start, CompleteToken::Explain) => Self::Explain,
            (Self::Explain, CompleteToken::Other) => Self::Explain,
            (Self::Start | Self::Explain, CompleteToken::Create) => Self::Create,
            (Self::Create, CompleteToken::Temp) => Self::Create,
            (Self::Create, CompleteToken::Trigger) => Self::Trigger,
            _ => Self::Normal,
        }
    }

    // Consumes one word or one character at `i`. Returns where scanning resumes.
    pub(crate) fn scan(self, b: &[u8], i: usize) -> (Self, usize) {
        let c = b[i];
        if c.is_ascii_alphabetic() || c == b'_' || c >= 0x80 {
            let end = i + b[i..]
                .iter()
                .position(|&w| !(w.is_ascii_alphanumeric() || matches!(w, b'_' | b'$') || w >= 0x80))
                .unwrap_or(b.len() - i);
            return (self.next(CompleteToken::word(&b[i..end])), end);
        }
        let state = if c.is_ascii_whitespace() { self } else { self.next(CompleteToken::Other) };
        (state, i + 1)
    }
}

pub fn split_statement_texts(driver: &DriverType, sql: &str) -> Vec<String> {
    split_statements(driver, sql).into_iter().map(|s| s.text.to_string()).collect()
}

pub(crate) fn has_code(s: &[u8], dialect: Dialect) -> bool {
    let n = s.len();
    let mut i = 0;
    while i < n {
        match s[i] {
            b' ' | b'\t' | b'\n' | b'\r' => i += 1,
            b'-' if s.get(i + 1) == Some(&b'-') => i = line_comment_end(s, i + 2),
            b'#' if dialect.hash_line_comments => i = line_comment_end(s, i + 1),
            b'/' if s.get(i + 1) == Some(&b'*') => i = block_comment_end(s, i, dialect.nested_block_comments),
            _ => return true,
        }
    }
    false
}

fn at_line_start(sql: &str, i: usize) -> bool {
    let line_start = sql[..i].rfind('\n').map_or(0, |p| p + 1);
    sql[line_start..i].trim().is_empty()
}

// mysql client `DELIMITER xx` line. Switching the terminator lets procedure bodies contain `;`.
fn delimiter_line(rest: &str) -> Option<(usize, &str)> {
    let b = rest.as_bytes();
    if b.len() < 9 || !b[..9].eq_ignore_ascii_case(b"DELIMITER") {
        return None;
    }
    let mut i = 9;
    let gap = i;
    while i < b.len() && matches!(b[i], b' ' | b'\t') {
        i += 1;
    }
    if i == gap {
        return None;
    }
    let token_start = i;
    while i < b.len() && !matches!(b[i], b'\t' | b'\n' | 0x0c | b'\r' | b' ') {
        i += 1;
    }
    if i == token_start {
        return None;
    }
    let token = &rest[token_start..i];
    while i < b.len() && matches!(b[i], b' ' | b'\t') {
        i += 1;
    }
    if i == b.len() {
        return Some((i, token));
    }
    if b[i] == b'\r' {
        i += 1;
    }
    (b.get(i) == Some(&b'\n')).then_some((i + 1, token))
}

// Index of the terminating '\n' (left unconsumed), or the end.
pub(crate) fn line_comment_end(s: &[u8], from: usize) -> usize {
    match s.get(from..).and_then(|rest| rest.iter().position(|&c| c == b'\n')) {
        Some(ix) => from + ix,
        None => s.len(),
    }
}

pub(crate) fn block_comment_end(s: &[u8], mut i: usize, nested: bool) -> usize {
    let n = s.len();
    i += 2;
    let mut depth = 1;
    while i < n {
        if s[i] == b'*' && s.get(i + 1) == Some(&b'/') {
            i += 2;
            depth -= 1;
            if depth == 0 {
                return i;
            }
        } else if nested && s[i] == b'/' && s.get(i + 1) == Some(&b'*') {
            depth += 1;
            i += 2;
        } else {
            i += 1;
        }
    }
    n
}

pub(crate) fn quote_end(s: &[u8], mut i: usize, quote: u8, backslash_escapes: bool) -> usize {
    let n = s.len();
    i += 1;
    while i < n {
        if backslash_escapes && s[i] == b'\\' {
            i += 2;
        } else if s[i] == quote {
            if s.get(i + 1) == Some(&quote) {
                i += 2;
                continue;
            }
            return i + 1;
        } else {
            i += 1;
        }
    }
    i.min(n)
}

// A standalone E before the quote starts a Postgres escape string. `1e'...'` and `TABLE'...'` don't count.
fn is_escape_string_prefix(s: &[u8], quote_ix: usize) -> bool {
    if quote_ix < 1 || !matches!(s[quote_ix - 1], b'e' | b'E') {
        return false;
    }
    if quote_ix < 2 {
        return true;
    }
    let before = s[quote_ix - 2];
    !(matches!(before, b'_' | b'$' | b'\'' | b'"' | b'`') || before.is_ascii_alphanumeric())
}

// Tags can't start with a digit, so `$1$` is a placeholder, not a tag.
pub(crate) fn dollar_tag_len(s: &[u8], i: usize) -> Option<usize> {
    let mut j = i + 1;
    if j < s.len() && (s[j].is_ascii_alphabetic() || s[j] == b'_') {
        j += 1;
        while j < s.len() && (s[j].is_ascii_alphanumeric() || s[j] == b'_') {
            j += 1;
        }
    }
    (s.get(j) == Some(&b'$')).then_some(j + 1 - i)
}

pub(crate) fn dollar_quoted_end(s: &[u8], i: usize) -> usize {
    let Some(tag_len) = dollar_tag_len(s, i) else {
        return i + 1;
    };
    let tag = &s[i..i + tag_len];
    let body = i + tag_len;
    match s[body..].windows(tag_len).position(|w| w == tag) {
        Some(ix) => body + ix + tag_len,
        None => s.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(driver: DriverType, sql: &str) -> Vec<String> {
        split_statement_texts(&driver, sql)
    }

    #[test]
    fn splits_at_top_level_semicolons() {
        let cases: &[(&str, &str, &[&str])] = &[
            ("empty", "", &[]),
            ("whitespace only", "   \n\t ", &[]),
            ("comment only", "-- just a comment\n/* and a block */", &[]),
            ("single, no semicolon", "SELECT 1", &["SELECT 1"]),
            ("single, trailing semicolon", "SELECT 1;", &["SELECT 1"]),
            ("two statements", "SELECT 1; SELECT 2;", &["SELECT 1", "SELECT 2"]),
            ("two, no trailing", "SELECT 1; SELECT 2", &["SELECT 1", "SELECT 2"]),
            ("blank chunks dropped", "SELECT 1;; ;SELECT 2;", &["SELECT 1", "SELECT 2"]),
            ("semicolon in single quotes", "SELECT ';' AS a; SELECT 2", &["SELECT ';' AS a", "SELECT 2"]),
            (
                "semicolon in double-quoted ident",
                r#"SELECT 1 AS "a;b"; SELECT 2"#,
                &[r#"SELECT 1 AS "a;b""#, "SELECT 2"],
            ),
            ("semicolon in backtick ident", "SELECT 1 AS `a;b`; SELECT 2", &["SELECT 1 AS `a;b`", "SELECT 2"]),
            ("escaped quote inside string", "SELECT 'it''s; fine'; SELECT 2", &["SELECT 'it''s; fine'", "SELECT 2"]),
            ("semicolon in line comment", "SELECT 1 -- a; b\n; SELECT 2", &["SELECT 1 -- a; b", "SELECT 2"]),
            ("semicolon in block comment", "SELECT 1 /* a; b */; SELECT 2", &["SELECT 1 /* a; b */", "SELECT 2"]),
            (
                "dollar-quoted function body keeps inner semicolons",
                "CREATE FUNCTION f() RETURNS int AS $$ BEGIN RETURN 1; END; $$ LANGUAGE plpgsql; SELECT f()",
                &["CREATE FUNCTION f() RETURNS int AS $$ BEGIN RETURN 1; END; $$ LANGUAGE plpgsql", "SELECT f()"],
            ),
            ("tagged dollar quote", "SELECT $tag$ a; b $tag$; SELECT 2", &["SELECT $tag$ a; b $tag$", "SELECT 2"]),
            ("trims surrounding whitespace", "  SELECT 1  ;\n\n  SELECT 2  ", &["SELECT 1", "SELECT 2"]),
            ("leading comment retained on statement", "-- note\nSELECT 1;", &["-- note\nSELECT 1"]),
        ];
        for (name, sql, want) in cases {
            assert_eq!(split(DriverType::Unset, sql), *want, "{name}");
        }
    }

    #[test]
    fn honours_each_dialect() {
        let cases: &[(&str, DriverType, &str, &[&str])] = &[
            (
                "mysql: backslash-escaped quote does not end the string",
                DriverType::MySql,
                r"SELECT 'it\'s a; test'; SELECT 2",
                &[r"SELECT 'it\'s a; test'", "SELECT 2"],
            ),
            (
                "mysql: # starts a line comment",
                DriverType::MySql,
                "SELECT 1 # ; not a split\n; SELECT 2",
                &["SELECT 1 # ; not a split", "SELECT 2"],
            ),
            (
                "mysql: $ is an identifier character",
                DriverType::MySql,
                "SELECT a$tag$ FROM t; SELECT b$tag$ FROM u",
                &["SELECT a$tag$ FROM t", "SELECT b$tag$ FROM u"],
            ),
            (
                "mysql: comment-only # chunk is dropped",
                DriverType::MySql,
                "# just a note\nSELECT 1;",
                &["# just a note\nSELECT 1"],
            ),
            (
                "postgres: block comments nest",
                DriverType::Postgres,
                "/* outer /* inner */ ; still comment */ SELECT 1",
                &["/* outer /* inner */ ; still comment */ SELECT 1"],
            ),
            (
                "postgres: E'…' honours backslash escapes",
                DriverType::Postgres,
                r"SELECT E'a\'b; not a split'; SELECT 2",
                &[r"SELECT E'a\'b; not a split'", "SELECT 2"],
            ),
            (
                "sqlite: backslash is a plain character",
                DriverType::Sqlite,
                r"SELECT 'path\'; SELECT 2",
                &[r"SELECT 'path\'", "SELECT 2"],
            ),
            (
                "mysql: DELIMITER keeps procedure bodies whole and strips the terminator",
                DriverType::MySql,
                "DELIMITER //\nCREATE PROCEDURE p()\nBEGIN\n  SELECT 1;\n  SELECT 2;\nEND//\nDELIMITER ;\nSELECT 3;",
                &["CREATE PROCEDURE p()\nBEGIN\n  SELECT 1;\n  SELECT 2;\nEND", "SELECT 3"],
            ),
            (
                "sqlite: a trigger body keeps its semicolons",
                DriverType::Sqlite,
                "CREATE TRIGGER t AFTER INSERT ON x BEGIN SELECT 1; SELECT 2; END; SELECT 3",
                &["CREATE TRIGGER t AFTER INSERT ON x BEGIN SELECT 1; SELECT 2; END", "SELECT 3"],
            ),
            (
                "sqlite: CASE … END and comments inside a temporary trigger",
                DriverType::Sqlite,
                "create temp trigger t after insert on x begin\n  update y set a = case when 1 then 2 end; -- end;\n  select 'end;';\nend\n; select 3",
                &[
                    "create temp trigger t after insert on x begin\n  update y set a = case when 1 then 2 end; -- end;\n  select 'end;';\nend",
                    "select 3",
                ],
            ),
            (
                "sqlite: a trigger without its END runs to the end of the input",
                DriverType::Sqlite,
                "CREATE TRIGGER t AFTER INSERT ON x BEGIN SELECT 1; SELECT 2",
                &["CREATE TRIGGER t AFTER INSERT ON x BEGIN SELECT 1; SELECT 2"],
            ),
            (
                "sqlite: END outside a trigger and words ending in END still split",
                DriverType::Sqlite,
                "BEGIN; SELECT legend FROM t; END; SELECT 2",
                &["BEGIN", "SELECT legend FROM t", "END", "SELECT 2"],
            ),
            (
                "postgres: trigger bodies are not SQLite's",
                DriverType::Postgres,
                "CREATE TRIGGER t AFTER INSERT ON x BEGIN SELECT 1; END",
                &["CREATE TRIGGER t AFTER INSERT ON x BEGIN SELECT 1", "END"],
            ),
            (
                "mysql: DELIMITER only counts at the start of a line",
                DriverType::MySql,
                "SELECT delimiter FROM t; SELECT 2",
                &["SELECT delimiter FROM t", "SELECT 2"],
            ),
        ];
        for (name, driver, sql, want) in cases {
            assert_eq!(split(driver.clone(), sql), *want, "{name}");
        }
    }

    #[test]
    fn ranges_point_at_the_trimmed_statement() {
        let sql = "  SELECT 'é';\n -- x\n SELECT 2 ";
        let stmts = split_statements(&DriverType::Postgres, sql);
        for stmt in &stmts {
            assert_eq!(&sql[stmt.range.clone()], stmt.text);
        }
        assert_eq!(stmts[1].text, "-- x\n SELECT 2");
    }
}
