// The values a column takes from a fixed list: an enum type's labels, or a CHECK that lists them.

use barsql_core::{DriverType, SqlDialect};

use crate::lang::tokens::{Token, TokenKind, tokenize};
use crate::lex::LexRules;

// The labels of a MySQL enum('a','b') or a ClickHouse Enum8('a' = 1, 'b' = 2), in order.
pub fn from_type(type_name: &str, driver: &DriverType) -> Vec<String> {
    let tokens = code_tokens(type_name, driver);
    let mut p = Parser::new(&tokens, driver);
    let mut wrappers = 0;
    while p.keyword("nullable") || p.keyword("lowcardinality") {
        if !p.eat("(") {
            return Vec::new();
        }
        wrappers += 1;
    }
    if !(p.keyword("enum") || p.keyword("enum8") || p.keyword("enum16")) || !p.eat("(") {
        return Vec::new();
    }
    let mut labels = Vec::new();
    loop {
        let Some(label) = p.string() else { return Vec::new() };
        labels.push(label);
        // ClickHouse numbers each label.
        if p.eat("=") && p.number().is_none() {
            return Vec::new();
        }
        if p.eat(")") {
            break;
        }
        if !p.eat(",") {
            return Vec::new();
        }
    }
    let closed = (0..wrappers).all(|_| p.eat(")"));
    if closed && p.done() { labels } else { Vec::new() }
}

// The values a CHECK allows its column, when all it does is list them: `kind IN ('a', 'b')`, an OR of
// equalities as SQL Server keeps it, or `kind = ANY (ARRAY['a'::text, 'b'::text])` as Postgres does.
pub fn from_check(column: &str, check: &str, driver: &DriverType) -> Option<Vec<String>> {
    let tokens = code_tokens(check, driver);
    let mut tokens = tokens.as_slice();
    // A check Postgres hasn't validated, or one MySQL doesn't enforce, still lists the values.
    if let [rest @ .., not, flag] = tokens
        && not.is_keyword("not")
        && (flag.is_keyword("valid") || flag.is_keyword("enforced"))
    {
        tokens = rest;
    }
    let mut p = Parser::new(tokens, driver);
    p.keyword("check");
    let mut values = Vec::new();
    if !p.any_of(column, &mut values) || !p.done() || values.is_empty() {
        return None;
    }
    let mut seen = std::collections::HashSet::new();
    values.retain(|value| seen.insert(value.clone()));
    Some(values)
}

// The first of a table's checks that lists the column's values.
pub fn from_checks<S: AsRef<str>>(column: &str, checks: &[S], driver: &DriverType) -> Vec<String> {
    checks.iter().find_map(|check| from_check(column, check.as_ref(), driver)).unwrap_or_default()
}

// The expression of each CHECK in a CREATE TABLE, on a column or the table.
pub fn ddl_checks<'a>(ddl: &'a str, driver: &DriverType) -> Vec<&'a str> {
    let tokens = code_tokens(ddl, driver);
    // The others want the expression in parentheses. ClickHouse leaves it bare, up to the next column or the end of
    // the list, and may still open it with one, as it wraps OR's operands.
    let bare = driver.dialect() == Some(SqlDialect::ClickHouse);
    let mut out = Vec::new();
    for (at, token) in tokens.iter().enumerate() {
        let rest = &tokens[at + 1..];
        let Some(first) = rest.first().filter(|_| token.is_keyword("check")) else { continue };
        let parenthesized = !bare && first.is_punct("(");
        let mut depth = 0;
        let mut end = None;
        for (i, t) in rest.iter().enumerate() {
            if t.is_punct("(") {
                depth += 1;
            } else if t.is_punct(")") && depth > 0 {
                depth -= 1;
                if parenthesized && depth == 0 {
                    end = Some(i);
                    break;
                }
            } else if (t.is_punct(")") || t.is_punct(",")) && depth == 0 {
                end = Some(i);
                break;
            }
        }
        let text = match (parenthesized, end) {
            (true, Some(close)) => &ddl[first.end..rest[close].start],
            (false, Some(0)) | (true, None) => continue,
            (false, Some(next)) => &ddl[first.start..rest[next - 1].end],
            (false, None) => &ddl[first.start..rest[rest.len() - 1].end],
        };
        out.push(text.trim());
    }
    out
}

fn code_tokens<'a>(text: &'a str, driver: &DriverType) -> Vec<Token<'a>> {
    let mut tokens = tokenize(text, Some(driver));
    tokens.retain(|t| t.kind != TokenKind::Comment);
    tokens
}

// Recursive descent over a check's few shapes. Each rule moves past what it matched, or nowhere.
struct Parser<'t, 'a> {
    tokens: &'t [Token<'a>],
    at: usize,
    rules: LexRules,
    // Postgres and ClickHouse keep a name's case, where the others don't.
    exact_names: bool,
}

impl<'t, 'a> Parser<'t, 'a> {
    fn new(tokens: &'t [Token<'a>], driver: &DriverType) -> Self {
        let exact_names = matches!(driver.dialect(), Some(SqlDialect::Postgres | SqlDialect::ClickHouse));
        Self { tokens, at: 0, rules: LexRules::for_driver(Some(driver)), exact_names }
    }

    fn done(&self) -> bool {
        self.at == self.tokens.len()
    }

    fn peek(&self) -> Option<&'t Token<'a>> {
        self.tokens.get(self.at)
    }

    fn advance_if(&mut self, matches: impl FnOnce(&Token) -> bool) -> bool {
        let hit = self.peek().is_some_and(matches);
        self.at += usize::from(hit);
        hit
    }

    // Punctuation or an operator.
    fn eat(&mut self, text: &str) -> bool {
        self.advance_if(|t| matches!(t.kind, TokenKind::Punct | TokenKind::Op) && t.text == text)
    }

    fn keyword(&mut self, word: &str) -> bool {
        self.advance_if(|t| t.is_keyword(word))
    }

    // Moves back to `at` unless the rule matched.
    fn attempt(&mut self, rule: impl FnOnce(&mut Self) -> bool) -> bool {
        let at = self.at;
        let matched = rule(self);
        if !matched {
            self.at = at;
        }
        matched
    }

    // term (OR term)*
    fn any_of(&mut self, column: &str, values: &mut Vec<String>) -> bool {
        loop {
            if !self.term(column, values) {
                return false;
            }
            if !self.keyword("or") {
                return true;
            }
        }
    }

    fn term(&mut self, column: &str, values: &mut Vec<String>) -> bool {
        let before = values.len();
        if self.attempt(|p| p.eat("(") && p.any_of(column, values) && p.eat(")")) {
            return true;
        }
        values.truncate(before);
        let matched = self.attempt(|p| p.comparison(column, values));
        if !matched {
            values.truncate(before);
        }
        matched
    }

    fn comparison(&mut self, column: &str, values: &mut Vec<String>) -> bool {
        if !self.attempt(|p| p.column(column)) {
            // 'like' = kind
            let Some(value) = self.value() else { return false };
            values.push(value);
            return self.eat("=") && self.column(column);
        }
        if self.keyword("in") {
            return self.eat("(") && self.list(")", values);
        }
        if self.keyword("is") {
            return self.keyword("null");
        }
        if !self.eat("=") {
            return false;
        }
        if self.keyword("any") {
            return self.eat("(") && self.array(values) && self.eat(")");
        }
        let Some(value) = self.value() else { return false };
        values.push(value);
        true
    }

    // The column, maybe parenthesized and cast: `(kind)::text`.
    fn column(&mut self, column: &str) -> bool {
        let named = if self.eat("(") {
            self.column(column) && self.eat(")")
        } else {
            let exact = self.exact_names;
            self.advance_if(|t| {
                let name = t.ident_text();
                t.is_ident_like() && (name == column || (!exact && name.eq_ignore_ascii_case(column)))
            })
        };
        named && self.casts()
    }

    // ARRAY['a'::text, 'b'::text], maybe parenthesized and cast.
    fn array(&mut self, values: &mut Vec<String>) -> bool {
        let inner = if self.eat("(") {
            self.array(values) && self.eat(")")
        } else {
            self.keyword("array") && self.eat("[") && self.list("]", values)
        };
        inner && self.casts()
    }

    // value (, value)* close
    fn list(&mut self, close: &str, values: &mut Vec<String>) -> bool {
        loop {
            let Some(value) = self.value() else { return false };
            values.push(value);
            if self.eat(close) {
                return true;
            }
            if !self.eat(",") {
                return false;
            }
        }
    }

    // A string or a number, maybe parenthesized and cast: `'a'::character varying`.
    fn value(&mut self) -> Option<String> {
        let at = self.at;
        let value = if self.eat("(") {
            self.value().filter(|_| self.eat(")"))
        } else {
            self.string().or_else(|| self.number())
        };
        match value {
            Some(value) if self.casts() => Some(value),
            _ => {
                self.at = at;
                None
            }
        }
    }

    // A string literal, with MySQL's charset introducer (_utf8mb4'a') or Postgres' E'a' before it.
    fn string(&mut self) -> Option<String> {
        let token = self.peek()?;
        let (literal, escapes) = match token.kind {
            TokenKind::String => (token, self.rules.backslash_escapes),
            TokenKind::Ident if token.text.starts_with('_') || token.lower == "e" => {
                let literal =
                    self.tokens.get(self.at + 1).filter(|t| t.kind == TokenKind::String && t.start == token.end)?;
                self.at += 1;
                (literal, self.rules.backslash_escapes || token.lower == "e")
            }
            _ => return None,
        };
        self.at += 1;
        Some(unquote(literal.text, escapes))
    }

    fn number(&mut self) -> Option<String> {
        let minus = self.attempt(|p| p.eat("-") && p.peek().is_some_and(|t| t.kind == TokenKind::Number));
        let token = self.peek().filter(|t| t.kind == TokenKind::Number && !t.text.starts_with('$'))?;
        self.at += 1;
        Some(if minus { format!("-{}", token.text) } else { token.text.to_string() })
    }

    // Postgres casts: `::text`, `::character varying(10)`, `::text[]`.
    fn casts(&mut self) -> bool {
        while self.eat("::") {
            let words = self.at;
            while self.advance_if(|t| {
                t.is_ident_like() && !["in", "or", "and", "is", "not", "any", "null"].contains(&t.lower.as_str())
            }) {}
            if self.at == words {
                return false;
            }
            self.attempt(|p| p.eat("(") && p.number().is_some() && (!p.eat(",") || p.number().is_some()) && p.eat(")"));
            while self.attempt(|p| p.eat("[") && p.eat("]")) {}
        }
        true
    }
}

// The text of a string literal: 'it''s', N'x' and, where backslashes escape, 'it\'s'.
fn unquote(literal: &str, backslash_escapes: bool) -> String {
    let literal = literal.strip_prefix(['N', 'n']).unwrap_or(literal);
    let mut chars = literal.chars();
    let Some(quote) = chars.next() else { return String::new() };
    let inner = chars.as_str();
    let inner = inner.strip_suffix(quote).unwrap_or(inner);
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        if c == quote && chars.peek() == Some(&quote) {
            chars.next();
            out.push(quote);
        } else if c == '\\' && backslash_escapes {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('0') => out.push('\0'),
                Some(other) => out.push(other),
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(list: &[&str]) -> Option<Vec<String>> {
        Some(list.iter().map(|v| v.to_string()).collect())
    }

    #[test]
    fn enum_types_list_their_labels() {
        let my = DriverType::MySql;
        assert_eq!(from_type("enum('ok','meh','it''s')", &my), ["ok", "meh", "it's"]);
        assert_eq!(from_type(r"enum('a\\b','x,y)')", &my), [r"a\b", "x,y)"]);
        let ch = DriverType::ClickHouse;
        assert_eq!(from_type("Enum8('a' = 1, 'b' = 2)", &ch), ["a", "b"]);
        assert_eq!(from_type(r"Nullable(Enum16('it\'s' = -1, 'x' = 300))", &ch), ["it's", "x"]);
        assert_eq!(from_type("LowCardinality(Enum('a', 'b'))", &ch), ["a", "b"]);
    }

    #[test]
    fn other_types_list_nothing() {
        for (type_name, driver) in [
            ("enum", DriverType::MySql),
            ("set('a','b')", DriverType::MySql),
            ("varchar(10)", DriverType::MySql),
            ("Array(Enum8('a' = 1))", DriverType::ClickHouse),
            ("String", DriverType::ClickHouse),
        ] {
            assert!(from_type(type_name, &driver).is_empty(), "{type_name}");
        }
    }

    #[test]
    fn a_check_that_lists_values_gives_them() {
        let love = values(&["like", "love"]);
        let cases = [
            (DriverType::Sqlite, "kind IN ('like', 'love')"),
            (DriverType::Sqlite, "CHECK (Kind in ('like','love'))"),
            (DriverType::Sqlite, "\"kind\" = 'like' OR kind = 'love'"),
            (DriverType::Sqlite, "((kind = 'like') OR (kind = 'love'))"),
            (DriverType::Sqlite, "kind IS NULL OR kind IN ('like', 'love', 'like')"),
            (DriverType::Postgres, "CHECK (kind = ANY (ARRAY['like'::text, 'love'::text]))"),
            (
                DriverType::Postgres,
                "CHECK (kind::text = ANY (ARRAY['like'::character varying, 'love'::character varying]::text[]))",
            ),
            (
                DriverType::Postgres,
                "CHECK (((kind)::text = ANY ((ARRAY['like'::character varying(10), 'love'::character varying(10)])::text[]))) NOT VALID",
            ),
            (DriverType::Postgres, "CHECK (kind = 'like'::text OR 'love'::text = kind)"),
            (DriverType::MySql, "(`kind` in (_utf8mb4'like',_utf8mb4'love'))"),
            (DriverType::MySql, "`kind` in ('like','love')"),
            (DriverType::SqlServer, "([kind]=N'like' OR [kind]=N'love')"),
            (DriverType::ClickHouse, "kind IN ('like', 'love')"),
        ];
        for (driver, check) in cases {
            assert_eq!(from_check("kind", check, &driver), love, "{check}");
        }
        assert_eq!(from_check("Kind", "CHECK (\"Kind\" = 'it''s')", &DriverType::Postgres), values(&["it's"]));
        assert_eq!(
            from_check("stars", "CHECK (stars = ANY (ARRAY[1, 2, -3]))", &DriverType::Postgres),
            values(&["1", "2", "-3"])
        );
    }

    #[test]
    fn a_check_that_does_more_than_list_values_gives_none() {
        for check in [
            "n > 0",
            "kind NOT IN ('a', 'b')",
            "other IN ('a', 'b')",
            "kind IN ('a') AND length(kind) > 0",
            "kind IN ('a') OR other = 'b'",
            "kind = other",
            "kind IS NULL",
            "lower(kind) IN ('a', 'b')",
            "kind IN ('a', upper('b'))",
            "kind IN ('a', 'b'",
        ] {
            assert_eq!(from_check("kind", check, &DriverType::Sqlite), None, "{check}");
        }
        // Postgres keeps a quoted name's case.
        assert_eq!(from_check("kind", "CHECK (\"Kind\" = 'a'::text)", &DriverType::Postgres), None);
    }

    #[test]
    fn a_create_table_gives_each_check() {
        let lite = "CREATE TABLE posts (id INTEGER PRIMARY KEY, kind TEXT NOT NULL CHECK (kind IN ('like', 'love')), \
            stars INT CHECK(stars BETWEEN 1 AND 5) /* CHECK (no) */, note TEXT DEFAULT 'CHECK (x)', \
            CONSTRAINT one CHECK ((kind <> 'x')))";
        assert_eq!(
            ddl_checks(lite, &DriverType::Sqlite),
            ["kind IN ('like', 'love')", "stars BETWEEN 1 AND 5", "(kind <> 'x')"]
        );
        // ClickHouse leaves the expression bare.
        let ch = "CREATE TABLE t (`kind` String, CONSTRAINT kinds CHECK kind IN ('a', 'b'), CONSTRAINT big CHECK n > 1) \
            ENGINE = MergeTree ORDER BY kind";
        assert_eq!(ddl_checks(ch, &DriverType::ClickHouse), ["kind IN ('a', 'b')", "n > 1"]);
        // It wraps OR's operands, so a parenthesis can open the expression without closing it.
        let ored = "CREATE TABLE t (kind String, CONSTRAINT c CHECK (kind = 'a') OR (kind = 'b')) ENGINE = Memory";
        assert_eq!(ddl_checks(ored, &DriverType::ClickHouse), ["(kind = 'a') OR (kind = 'b')"]);
        assert_eq!(from_checks("kind", &ddl_checks(lite, &DriverType::Sqlite), &DriverType::Sqlite), ["like", "love"]);
        assert!(from_checks("note", &ddl_checks(lite, &DriverType::Sqlite), &DriverType::Sqlite).is_empty());
    }
}
