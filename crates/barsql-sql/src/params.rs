use std::ops::Range;

use barsql_core::DriverType;

use crate::lex::LexRules;
use crate::lex::boundary::{is_word_start, quoted_span_end, word_end};
use crate::lex::prim::{block_comment_end, dash_comment_at, hash_comment_at, line_comment_end};

// A `:name` placeholder. `range` covers the colon and the name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Param {
    pub name: String,
    pub range: Range<usize>,
}

// Placeholders outside strings, quoted identifiers, dollar quotes and comments. A `::` cast doesn't count, nor does
// a colon right after a word, a quote or a closing bracket: labels, ClickHouse's `{id:UInt32}`, `a[lo:hi]`.
pub fn find_params(driver: Option<&DriverType>, sql: &str) -> Vec<Param> {
    let rules = LexRules::for_driver(driver);
    let b = sql.as_bytes();
    let n = b.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        let c = b[i];
        if dash_comment_at(b, i, rules.dash_needs_space) {
            i = line_comment_end(b, i + 2).unwrap_or(n);
        } else if hash_comment_at(b, i, rules.hash_comments) {
            i = line_comment_end(b, i + 1).unwrap_or(n);
        } else if c == b'/' && b.get(i + 1) == Some(&b'*') {
            i = block_comment_end(b, i, rules.nested_block_comments).unwrap_or(n);
        } else if let Some(end) = quoted_span_end(b, i, &rules) {
            i = end.unwrap_or(n).max(i + 1);
        } else if is_word_start(c, &rules) {
            i = word_end(b, i, &rules);
        } else if c == b':' && b.get(i + 1) == Some(&b':') {
            i += 2;
        } else if c == b':' && opens_param(b, i) {
            let end =
                i + 1 + b[i + 1..].iter().position(|&w| !(w.is_ascii_alphanumeric() || w == b'_')).unwrap_or(n - i - 1);
            out.push(Param { name: sql[i + 1..end].to_string(), range: i..end });
            i = end;
        } else {
            i += 1;
        }
    }
    out
}

fn opens_param(b: &[u8], colon: usize) -> bool {
    let after_word = colon.checked_sub(1).is_some_and(|p| {
        b[p].is_ascii_alphanumeric() || matches!(b[p], b'_' | b']' | b')' | b'"' | b'`' | b'\'') || b[p] >= 0x80
    });
    !after_word && b.get(colon + 1).is_some_and(|&c| c.is_ascii_alphabetic() || c == b'_')
}

// Each name once, in the order they first appear.
pub fn param_names(params: &[Param]) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for param in params {
        if !names.contains(&param.name) {
            names.push(param.name.clone());
        }
    }
    names
}

// Puts each value's text in place of its placeholder, as typed: values are SQL, so text goes in quotes.
pub fn substitute(sql: &str, params: &[Param], value: impl Fn(&str) -> String) -> String {
    let mut out = String::with_capacity(sql.len());
    let mut last = 0;
    for param in params {
        out.push_str(&sql[last..param.range.start]);
        out.push_str(&value(&param.name));
        last = param.range.end;
    }
    out.push_str(&sql[last..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(driver: DriverType, sql: &str) -> Vec<String> {
        find_params(Some(&driver), sql).into_iter().map(|p| p.name).collect()
    }

    #[test]
    fn finds_placeholders_in_code_only() {
        let cases: &[(&str, DriverType, &str, &[&str])] = &[
            ("plain", DriverType::Postgres, "SELECT * FROM t WHERE id = :id AND n > :min_n", &["id", "min_n"]),
            ("after an operator", DriverType::Sqlite, "SELECT * FROM t WHERE id=:id", &["id"]),
            ("in a list", DriverType::Sqlite, "SELECT * FROM t WHERE id IN (:a,:b)", &["a", "b"]),
            ("a cast", DriverType::Postgres, "SELECT x::int, :v::text FROM t", &["v"]),
            ("a string", DriverType::Postgres, "SELECT ':no', 'a :b' FROM t", &[]),
            ("a quoted identifier", DriverType::Postgres, r#"SELECT ":no" FROM t"#, &[]),
            ("comments", DriverType::Postgres, "SELECT 1 -- :no\n/* :no */ , :yes", &["yes"]),
            ("a dollar quote", DriverType::Postgres, "SELECT $$ :no $$, $q$ :no $q$, :yes", &["yes"]),
            ("an assignment", DriverType::MySql, "SET @x := 1, @y = :y", &["y"]),
            ("a hash comment", DriverType::MySql, "SELECT 1 # :no\n, :yes", &["yes"]),
            ("ClickHouse's typed parameters", DriverType::ClickHouse, "SELECT {id:UInt32}, :yes", &["yes"]),
            ("a slice", DriverType::Postgres, "SELECT a[lo:hi], a[1:2] FROM t", &[]),
            ("a digit after the colon", DriverType::Postgres, "SELECT :1", &[]),
            ("a bracket identifier", DriverType::SqlServer, "SELECT [a:b], :yes", &["yes"]),
            ("a word with a dollar", DriverType::Postgres, "SELECT a$b$, :yes FROM t", &["yes"]),
        ];
        for (name, driver, sql, want) in cases {
            assert_eq!(names(driver.clone(), sql), *want, "{name}");
        }
    }

    #[test]
    fn substitutes_every_use_and_lists_each_name_once() {
        let sql = "SELECT * FROM t WHERE a = :id OR b = :id OR c = :name";
        let params = find_params(Some(&DriverType::Postgres), sql);
        assert_eq!(param_names(&params), ["id", "name"]);
        let filled = substitute(sql, &params, |name| if name == "id" { "7".into() } else { "'Ann'".into() });
        assert_eq!(filled, "SELECT * FROM t WHERE a = 7 OR b = 7 OR c = 'Ann'");
    }
}
