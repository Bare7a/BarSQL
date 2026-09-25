use std::sync::LazyLock;

use barsql_core::DriverType;
use regex::Regex;
use sqlformat::{Dialect, FormatOptions, QueryParams};

// Longer multi-word keywords first, so "LEFT JOIN" wins over "JOIN".
const KEYWORD_BREAKERS: &[&str] = &[
    "LEFT JOIN",
    "RIGHT JOIN",
    "INNER JOIN",
    "OUTER JOIN",
    "CROSS JOIN",
    "GROUP BY",
    "ORDER BY",
    "INSERT INTO",
    "DELETE FROM",
    "CREATE TABLE",
    "ALTER TABLE",
    "DROP TABLE",
    "UNION ALL",
    "SELECT",
    "FROM",
    "WHERE",
    "JOIN",
    "HAVING",
    "LIMIT",
    "OFFSET",
    "VALUES",
    "UPDATE",
    "SET",
    "UNION",
    "ON",
    "AND",
    "OR",
];

static KEYWORD_BREAK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r"(?i)(?-u:\b)({})(?-u:\b)", KEYWORD_BREAKERS.join("|"))).unwrap());

// Used by the editor's Format query.
pub fn format_query(sql: &str, driver: &DriverType) -> String {
    let dialect = if *driver == DriverType::Postgres { Dialect::PostgreSql } else { Dialect::Generic };
    let options = FormatOptions { uppercase: Some(true), dialect, ..Default::default() };
    sqlformat::format(sql, &QueryParams::None, &options)
}

// Puts upcased keywords on new lines and leaves literals untouched.
pub fn format_sql(sql: &str) -> String {
    let sql = sql.trim();
    if sql.is_empty() {
        return String::new();
    }
    break_keywords_outside_literals(sql)
        .split('\n')
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn break_keywords_outside_literals(sql: &str) -> String {
    let b = sql.as_bytes();
    let mut out = String::with_capacity(sql.len() + 16);
    let mut run_start = 0;
    let flush = |out: &mut String, start: usize, end: usize| {
        if start < end {
            out.push_str(&KEYWORD_BREAK.replace_all(&sql[start..end], |caps: &regex::Captures<'_>| {
                format!("\n{}", barsql_sql::to_upper(&caps[0]))
            }));
        }
    };
    let mut i = 0;
    while i < b.len() {
        let end = match b[i] {
            quote @ (b'\'' | b'"' | b'`') => Some(quoted_end(b, i, quote)),
            b'$' => dollar_end(b, i),
            _ => None,
        };
        match end {
            Some(end) => {
                flush(&mut out, run_start, i);
                out.push_str(&sql[i..end]);
                i = end;
                run_start = i;
            }
            None => i += 1,
        }
    }
    flush(&mut out, run_start, b.len());
    out
}

fn quoted_end(b: &[u8], mut i: usize, quote: u8) -> usize {
    i += 1;
    while i < b.len() {
        if b[i] != quote {
            i += 1;
        } else if b.get(i + 1) == Some(&quote) {
            i += 2;
        } else {
            return i + 1;
        }
    }
    i
}

fn dollar_end(b: &[u8], i: usize) -> Option<usize> {
    let mut j = i + 1;
    if j < b.len() && (b[j].is_ascii_alphabetic() || b[j] == b'_') {
        j += 1;
        while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
            j += 1;
        }
    }
    if b.get(j) != Some(&b'$') {
        return None;
    }
    let tag = &b[i..=j];
    let body = j + 1;
    Some(match b[body..].windows(tag.len()).position(|w| w == tag) {
        Some(ix) => body + ix + tag.len(),
        None => b.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_query_upcases_keywords_and_keeps_literals() {
        let pg = format_query("select id, 'from x' as t from users where id::text = $1", &DriverType::Postgres);
        assert_eq!(pg, "SELECT\n  id,\n  'from x' AS t\nFROM\n  users\nWHERE\n  id::text = $1");
        let my = format_query("select `order` from t;select 2", &DriverType::MySql);
        assert_eq!(my, "SELECT\n  `order`\nFROM\n  t;\nSELECT\n  2");
    }

    #[test]
    fn basic_formatter_breaks_keywords_and_keeps_literals() {
        assert_eq!(format_sql(""), "");
        assert_eq!(format_sql("   "), "");
        let lines: Vec<String> =
            format_sql("select id from users where active = 1 order by id").split('\n').map(String::from).collect();
        assert!(lines.len() >= 4);
        assert_eq!(lines[0], "SELECT id");
        for want in ["FROM users", "WHERE active = 1", "ORDER BY id"] {
            assert!(lines.iter().any(|l| l == want), "{want}: {lines:?}");
        }
        let kept = format_sql("SELECT UserId, FullName FROM AppUsers WHERE FullName = 'Alice'");
        assert!(kept.contains("UserId") && kept.contains("AppUsers") && kept.contains("'Alice'"));
        assert!(!format_sql("SELECT 1").starts_with('\n'));
        assert!(
            format_sql("SELECT * FROM t WHERE note = 'shipped and ready or not'")
                .contains("'shipped and ready or not'")
        );
        assert!(format_sql("SELECT $$a or b and c$$ FROM t").contains("$$a or b and c$$"));
        assert!(format_sql("SELECT `from` FROM t").contains("`from`"));
    }
}
