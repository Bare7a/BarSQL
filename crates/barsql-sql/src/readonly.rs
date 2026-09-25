use std::sync::LazyLock;

use barsql_core::DriverType;
use regex::Regex;

use crate::split::{dollar_quoted_end, dollar_tag_len};
use crate::sql_text::{first_keyword, to_upper};

pub const READ_ONLY_ERROR: &str = "connection is read-only: only read queries (SELECT, EXPLAIN, etc.) are allowed";

// \b is ASCII-only here, \s is spelled [\t\n\f\r ] and \w is spelled [0-9A-Za-z_].
static WITH_WRITE_AFTER_CTE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)\)[\t\n\f\r ]*(insert|update|delete|merge|replace)(?-u:\b)").unwrap());
static FORBIDDEN_IN_STATEMENT: LazyLock<[Regex; 4]> = LazyLock::new(|| {
    [
        // SELECT ... INTO writes (a new table on Postgres, OUTFILE on MySQL). Columns can sit in between.
        Regex::new(r"(?is)(?-u:\b)select(?-u:\b).*(?-u:\b)into(?-u:\b)").unwrap(),
        Regex::new(r"(?is)(?-u:\b)copy[\t\n\f\r ]+").unwrap(),
        // Only REPLACE INTO. The REPLACE() function is a read.
        Regex::new(r"(?is)(?-u:\b)replace[\t\n\f\r ]+into(?-u:\b)").unwrap(),
        Regex::new(
            r"(?is)(?-u:\b)(insert|update|delete|drop|create|alter|truncate|merge|grant|revoke|vacuum|reindex|attach|detach)(?-u:\b)",
        )
        .unwrap(),
    ]
});
// FOR UPDATE and FOR NO KEY UPDATE are locking reads. Blank them so the bare `update` rule skips them.
static LOCKING_CLAUSE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(?-u:\b)for[\t\n\f\r ]+(no[\t\n\f\r ]+key[\t\n\f\r ]+)?update(?-u:\b)").unwrap());
static PRAGMA_WITH_ARGUMENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?is)^[\t\n\f\r ]*pragma[\t\n\f\r ]+(?:[0-9A-Za-z_]+\.)?([0-9A-Za-z_]+)[\t\n\f\r ]*(?:=|\()").unwrap()
});

const READ_ONLY_PRAGMAS: &[&str] = &[
    "table_info",
    "table_xinfo",
    "table_list",
    "index_info",
    "index_xinfo",
    "index_list",
    "foreign_key_list",
    "foreign_key_check",
    "database_list",
    "collation_list",
    "function_list",
    "module_list",
    "pragma_list",
    "compile_options",
    "integrity_check",
    "quick_check",
];

const ALLOWED_FIRST_KEYWORDS: &[&str] = &[
    "SELECT",
    "WITH",
    "EXPLAIN",
    "PRAGMA",
    "SHOW",
    "DESCRIBE",
    "DESC",
    "TABLE",
    "BEGIN",
    "COMMIT",
    "END",
    "ROLLBACK",
    "START",
    "SAVEPOINT",
    "RELEASE",
];

const DENIED_FIRST_KEYWORDS: &[&str] = &[
    "INSERT", "UPDATE", "DELETE", "DROP", "CREATE", "ALTER", "TRUNCATE", "MERGE", "REPLACE", "GRANT", "REVOKE", "CALL",
    "EXEC", "EXECUTE", "VACUUM", "REINDEX", "ATTACH", "DETACH", "COMMENT", "COPY", "CLUSTER", "DISCARD", "DO", "LOCK",
    "REFRESH",
];

pub fn assert_read_only(driver: &DriverType, sql: &str) -> Result<(), String> {
    if is_read_only(driver, sql) { Ok(()) } else { Err(READ_ONLY_ERROR.into()) }
}

// Only MySQL gets # comments stripped. On Postgres # is an operator, and stripping to end of line could
// hide a write that follows.
pub fn is_read_only(driver: &DriverType, sql: &str) -> bool {
    let cleaned = mask_string_literals(&strip_sql_comments(sql, *driver == DriverType::MySql));
    split_on_semicolons(&cleaned).iter().all(|stmt| is_read_only_statement(stmt))
}

// Runs on writable connections too, since the table-data path must never escape its WHERE clause.
pub fn validate_table_filter(filter: &str) -> Result<(), String> {
    let filter = filter.trim();
    if filter.is_empty() {
        return Ok(());
    }
    // Mask strings but not comments, because a bare `--` or `/*` would comment out the LIMIT.
    let masked = mask_string_literals(filter);
    if masked.contains(';') {
        return Err("filter must be a single boolean expression: ';' is not allowed".into());
    }
    if masked.contains("--") || masked.contains("/*") || masked.contains("*/") {
        return Err("filter must not contain comment markers".into());
    }
    let upper = to_upper(&masked);
    let upper = LOCKING_CLAUSE.replace_all(&upper, " ");
    if FORBIDDEN_IN_STATEMENT.iter().any(|re| re.is_match(&upper)) {
        return Err("filter must not contain write keywords".into());
    }
    Ok(())
}

fn is_read_only_statement(stmt: &str) -> bool {
    let stmt = stmt.trim();
    if stmt.is_empty() {
        return true;
    }
    let first = first_keyword(stmt);
    if first.is_empty() {
        return true;
    }
    if DENIED_FIRST_KEYWORDS.contains(&first.as_str()) || !ALLOWED_FIRST_KEYWORDS.contains(&first.as_str()) {
        return false;
    }
    if first == "WITH" && WITH_WRITE_AFTER_CTE.is_match(stmt) {
        return false;
    }
    if first == "PRAGMA" && !is_read_only_pragma(stmt) {
        return false;
    }
    let upper = to_upper(stmt);
    let upper = LOCKING_CLAUSE.replace_all(&upper, " ");
    !FORBIDDEN_IN_STATEMENT.iter().any(|re| re.is_match(&upper))
}

fn is_read_only_pragma(stmt: &str) -> bool {
    match PRAGMA_WITH_ARGUMENT.captures(stmt) {
        None => true,
        Some(caps) => READ_ONLY_PRAGMAS.contains(&caps[1].to_lowercase().as_str()),
    }
}

// Blanks quoted and dollar-quoted spans so their keywords, quotes, comments and `;` can't confuse the
// classifier. Byte offsets stay the same.
pub fn mask_string_literals(sql: &str) -> String {
    let b = sql.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let end = match b[i] {
            quote @ (b'\'' | b'"' | b'`') => quoted_span_end(b, i, quote),
            b'$' if dollar_tag_len(b, i).is_some() => dollar_quoted_end(b, i),
            other => {
                out.push(other);
                i += 1;
                continue;
            }
        };
        out.resize(out.len() + (end - i), b' ');
        i = end;
    }
    String::from_utf8(out).unwrap_or_default()
}

// Quoted spans stay intact. MySQL runs the body of /*! ... */, so unwrap it instead of stripping it.
pub fn strip_sql_comments(sql: &str, hash_line_comments: bool) -> String {
    let b = sql.as_bytes();
    let n = b.len();
    let mut out = Vec::with_capacity(n);
    let mut i = 0;
    while i < n {
        let c = b[i];
        if (c == b'-' && b.get(i + 1) == Some(&b'-')) || (c == b'#' && hash_line_comments) {
            while i < n && b[i] != b'\n' {
                i += 1;
            }
        } else if c == b'/' && b.get(i + 1) == Some(&b'*') {
            if b.get(i + 2) == Some(&b'!') {
                i += 3;
                while i < n && b[i].is_ascii_digit() {
                    i += 1;
                }
                out.push(b' ');
                continue;
            }
            i += 2;
            while i + 1 < n && !(b[i] == b'*' && b[i + 1] == b'/') {
                i += 1;
            }
            i = if i + 1 < n { i + 2 } else { n };
        } else if matches!(c, b'\'' | b'"' | b'`') {
            let end = quoted_span_end(b, i, c);
            out.extend_from_slice(&b[i..end]);
            i = end;
        } else if c == b'$' && dollar_tag_len(b, i).is_some() {
            let end = dollar_quoted_end(b, i);
            out.extend_from_slice(&b[i..end]);
            i = end;
        } else {
            out.push(c);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap_or_default()
}

fn quoted_span_end(b: &[u8], mut i: usize, quote: u8) -> usize {
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

// Classifier-only splitter that ignores `;` inside quoted and dollar-quoted spans.
fn split_on_semicolons(sql: &str) -> Vec<&str> {
    let b = sql.as_bytes();
    let mut out = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            quote @ (b'\'' | b'"' | b'`') => i = quoted_span_end(b, i, quote),
            b'$' if dollar_tag_len(b, i).is_some() => i = dollar_quoted_end(b, i),
            b';' => {
                out.push(&sql[start..i]);
                start = i + 1;
                i += 1;
            }
            _ => i += 1,
        }
    }
    let tail = sql[start..].trim();
    if !tail.is_empty() {
        out.push(tail);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ro(sql: &str) -> bool {
        is_read_only(&DriverType::Unset, sql)
    }

    #[test]
    fn classifies_reads_and_writes() {
        let cases = [
            ("SELECT 1", true),
            ("SELECT * FROM users WHERE note = 'DELETE me'", true),
            ("WITH c AS (SELECT 1) SELECT * FROM c", true),
            ("EXPLAIN SELECT 1", true),
            ("PRAGMA table_info(users)", true),
            ("", true),
            ("INSERT INTO t VALUES (1)", false),
            ("UPDATE t SET x = 1", false),
            ("DELETE FROM t", false),
            ("DROP TABLE t", false),
            ("SELECT 1; DELETE FROM t", false),
            ("WITH c AS (SELECT 1) DELETE FROM t", false),
            ("SELECT INTO new_t FROM old_t", false),
            ("CREATE TABLE t (id int)", false),
        ];
        for (sql, want) in cases {
            assert_eq!(ro(sql), want, "{sql}");
        }
    }

    #[test]
    fn quoting_cannot_hide_a_write() {
        let cases = [
            ("WITH x AS (SELECT $$ ' $$) DELETE FROM t", false),
            ("SELECT $$it's$$; DELETE FROM users", false),
            ("SELECT * FROM `weird'name` ; DROP TABLE t", false),
            ("WITH x AS (SELECT 1 AS `a'b`) DELETE FROM t", false),
            ("WITH x AS (SELECT $tag$ ' $tag$) UPDATE t SET a = 1", false),
            ("SELECT $$ hello ; world $$", true),
            ("SELECT $tag$ a ' b $tag$ AS c", true),
            ("SELECT `select`, `from` FROM t", true),
            ("SELECT * FROM t WHERE note = $$DROP TABLE x$$", true),
            ("SELECT * FROM t WHERE id = $1", true),
        ];
        for (sql, want) in cases {
            assert_eq!(ro(sql), want, "{sql}");
        }
    }

    #[test]
    fn mysql_executable_comments_are_classified() {
        let cases = [
            ("/*!40000 DROP TABLE t */", false),
            ("/*! INSERT INTO t VALUES (1) */", false),
            ("/*!UPDATE t SET a=1*/", false),
            ("/*!40000 TRUNCATE t */", false),
            ("SELECT 1; /*! DELETE FROM t */", false),
            ("SELECT /*!40001 SQL_NO_CACHE */ * FROM t", true),
            ("SELECT /*! STRAIGHT_JOIN */ a FROM t", true),
        ];
        for (sql, want) in cases {
            assert_eq!(ro(sql), want, "{sql}");
        }
    }

    #[test]
    fn pragmas_with_arguments_are_writes_unless_inspection() {
        let cases = [
            ("PRAGMA foreign_keys", true),
            ("PRAGMA table_info(users)", true),
            ("PRAGMA index_list('t')", true),
            ("PRAGMA foreign_key_check", true),
            ("PRAGMA foreign_keys = on", false),
            ("PRAGMA foreign_keys(0)", false),
            ("PRAGMA journal_mode(WAL)", false),
            ("PRAGMA incremental_vacuum(10)", false),
        ];
        for (sql, want) in cases {
            assert_eq!(ro(sql), want, "{sql}");
        }
    }

    #[test]
    fn hash_comments_depend_on_the_driver() {
        let mysql_read = "SELECT 1 # insert note to self\nFROM t";
        assert!(is_read_only(&DriverType::MySql, mysql_read));
        assert!(!ro(mysql_read), "the driver-less classifier stays conservative");
        assert!(!is_read_only(&DriverType::Postgres, "SELECT a # b; DELETE FROM t"));
        assert!(!is_read_only(&DriverType::MySql, "SELECT 1; # note\nDROP TABLE t"));
    }

    #[test]
    fn edge_cases() {
        let cases = [
            ("SELECT ';DELETE FROM t;' FROM dual", true),
            ("SELECT 1 -- DELETE FROM t", true),
            ("SELECT 1 /* INSERT INTO t VALUES (1) */", true),
            ("DESCRIBE users", true),
            ("DESC users", true),
            ("SHOW TABLES", true),
            ("SELECT 1; SELECT 2; SELECT 3", true),
            ("Insert Into t Values(1)", false),
            ("TRUNCATE TABLE t", false),
            ("WITH c AS (SELECT 1) UPDATE t SET x=1", false),
            ("WITH c AS (SELECT 1) MERGE INTO t USING c", false),
            ("EXEC sp_who", false),
            ("COPY t TO 'x.csv'", false),
            (";", true),
            ("-- nothing here\n/* still nothing */", true),
            ("PRAGMA table_info(users)", true),
            ("PRAGMA journal_mode = WAL", false),
            ("PRAGMA writable_schema = 1", false),
            ("SELECT * INTO t2 FROM t1", false),
            ("SELECT a, b INTO t2 FROM t1", false),
            ("SELECT * FROM t INTO OUTFILE '/tmp/x'", false),
            ("SELECT REPLACE(name, 'a', 'b') FROM t", true),
            ("REPLACE INTO t (a) VALUES (1)", false),
            ("EXPLAIN REPLACE INTO t (a) VALUES (1)", false),
            ("SELECT * FROM t WHERE id = 1 FOR UPDATE", true),
            ("SELECT * FROM t FOR NO KEY UPDATE", true),
            ("SELECT * FROM t FOR UPDATE SKIP LOCKED", true),
            ("EXPLAIN ANALYZE UPDATE t SET x = 1", false),
            ("UPDATE t SET x = 1 RETURNING *", false),
            ("DELETE FROM t WHERE id = 1 RETURNING id, name", false),
            ("INSERT INTO t (a) VALUES (1) RETURNING *", false),
            ("WITH d AS (DELETE FROM t RETURNING *) SELECT * FROM d", false),
            ("WITH u AS (UPDATE t SET x=1 RETURNING *) SELECT * FROM u", false),
            (r"SELECT 'a\'; DELETE FROM t; --'", false),
            (r#"SELECT "a\"; DELETE FROM t; --""#, false),
            ("SELECT 1 /* don't worry */ FROM t", true),
            ("SELECT 1 -- don't worry\nFROM t", true),
            ("INSERT INTO t /* don't */ VALUES (1)", false),
            ("(SELECT 1) UNION (SELECT 2)", true),
            ("BEGIN; SELECT 1; COMMIT;", true),
            ("START TRANSACTION; SELECT 1; ROLLBACK;", true),
            ("SAVEPOINT s1; SELECT 1; RELEASE s1;", true),
            ("BEGIN; DELETE FROM t; COMMIT;", false),
        ];
        for (sql, want) in cases {
            assert_eq!(ro(sql), want, "{sql}");
        }
    }

    #[test]
    fn table_filters() {
        let cases = [
            ("", false),
            ("id > 5", false),
            ("name = 'O;Brien'", false),
            ("name = '-- safe'", false),
            ("name = 'O''Brien'", false),
            ("1=1; DELETE FROM users", true),
            ("1=1 -- AND active = false", true),
            ("1=1 /* AND active = false */", true),
            ("1=1 UNION SELECT 1 UPDATE t SET x=1", true),
            (r"name = 'a\'; DELETE FROM users; --'", true),
            ("name = REPLACE(other, 'a', 'b')", false),
        ];
        for (filter, want_err) in cases {
            assert_eq!(validate_table_filter(filter).is_err(), want_err, "{filter}");
        }
    }

    #[test]
    fn helpers_keep_strings() {
        assert_eq!(split_on_semicolons(r#"SELECT ';' FROM t; SELECT "a;b""#).len(), 2);
        let out = strip_sql_comments("SELECT '-- not a comment', 1 -- real comment\nFROM t", false);
        assert!(out.contains("'-- not a comment'"));
        assert!(!out.contains("real comment"));
        assert!(assert_read_only(&DriverType::Unset, "SELECT 1").is_ok());
        assert!(assert_read_only(&DriverType::Unset, "DROP TABLE t").is_err());
    }
}
