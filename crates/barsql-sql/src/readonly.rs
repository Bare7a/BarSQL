use std::sync::LazyLock;

use barsql_core::{DriverType, SqlDialect};
use regex::Regex;

use crate::dialect::Dialect;
use crate::lex::LexRules;
use crate::lex::boundary::{Boundary, Mode, boundaries, is_word_start, quoted_span_end as lexed_span_end, word_end};
use crate::lex::prim::{block_comment_end, dash_comment_at, hash_comment_at, line_comment_end};

pub const READ_ONLY_ERROR: &str = "connection is read-only: only read queries (SELECT, EXPLAIN, etc.) are allowed";

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

// Which statements a read-only connection may run, per dialect.
#[derive(Debug)]
pub struct ReadOnlyRules {
    pub allowed_first: &'static [&'static str],
    pub denied_first: &'static [&'static str],
    // Words that make a statement a write wherever they appear. T-SQL statements need no separator, so
    // `SELECT 1 EXEC p` runs the procedure.
    pub denied_anywhere: &'static [&'static str],
    // Whether a denied word right before a `.` only names a database, as `system` does in ClickHouse.
    pub qualifier_names: bool,
}

// A statement holding any of these as a word writes, unless the UPDATE is a FOR UPDATE lock.
const WRITE_WORDS: &[&str] = &[
    "INSERT", "UPDATE", "DELETE", "DROP", "CREATE", "ALTER", "TRUNCATE", "MERGE", "GRANT", "REVOKE", "VACUUM",
    "REINDEX", "ATTACH", "DETACH",
];
const WRITES_AFTER_CTE: &[&str] = &["INSERT", "UPDATE", "DELETE", "MERGE", "REPLACE"];

pub static STANDARD: ReadOnlyRules = ReadOnlyRules {
    allowed_first: ALLOWED_FIRST_KEYWORDS,
    denied_first: DENIED_FIRST_KEYWORDS,
    denied_anywhere: &[],
    qualifier_names: false,
};

pub static TSQL: ReadOnlyRules = ReadOnlyRules {
    allowed_first: &[
        "SELECT",
        "WITH",
        "DECLARE",
        "SET",
        "PRINT",
        "RAISERROR",
        "THROW",
        "WAITFOR",
        "IF",
        "WHILE",
        "BEGIN",
        "COMMIT",
        "ROLLBACK",
        "SAVE",
        "END",
    ],
    denied_first: DENIED_FIRST_KEYWORDS,
    denied_anywhere: &[
        "EXEC",
        "EXECUTE",
        "BULK",
        "BACKUP",
        "RESTORE",
        "DBCC",
        "KILL",
        "SHUTDOWN",
        "RECONFIGURE",
        "OPENROWSET",
        "OPENDATASOURCE",
        "OPENQUERY",
        "WRITETEXT",
        "UPDATETEXT",
        "DENY",
        "DISABLE",
        "ENABLE",
        "SEND",
        "RECEIVE",
        "CHECKPOINT",
        "SETUSER",
    ],
    qualifier_names: false,
};

// The server enforces readonly=2 as well. Settings may change, data may not. None of the denied words can come
// right before a `.` as a keyword, so `system.tables` reads.
pub static CLICKHOUSE: ReadOnlyRules = ReadOnlyRules {
    allowed_first: &["SELECT", "WITH", "SHOW", "DESCRIBE", "DESC", "EXPLAIN", "EXISTS", "SET"],
    denied_first: DENIED_FIRST_KEYWORDS,
    denied_anywhere: &[
        "RENAME", "OPTIMIZE", "SYSTEM", "KILL", "EXCHANGE", "BACKUP", "RESTORE", "UNDROP", "MOVE", "WATCH", "OUTFILE",
    ],
    qualifier_names: true,
};

// Every way the server might lex the text. A statement counts as read-only only when it is under all of them.
fn interpretations(driver: &DriverType) -> Vec<LexRules> {
    let base = Dialect::for_driver(driver).lex;
    match driver.dialect() {
        // standard_conforming_strings may be off, making backslash an escape in '...'.
        Some(SqlDialect::Postgres) => vec![base, LexRules { backslash_escapes: true, ..base }],
        // NO_BACKSLASH_ESCAPES turns escapes off and ANSI_QUOTES makes "..." an identifier.
        Some(SqlDialect::MySql) => {
            let literal = LexRules { backslash_escapes: false, escape_strings: false, ..base };
            let ansi = LexRules { double_quote_strings: false, ..base };
            vec![base, literal, ansi, LexRules { double_quote_strings: false, ..literal }]
        }
        Some(SqlDialect::Sqlite | SqlDialect::TSql | SqlDialect::ClickHouse) | None => vec![base],
    }
}

pub fn assert_read_only(driver: &DriverType, sql: &str) -> Result<(), String> {
    if is_read_only(driver, sql) { Ok(()) } else { Err(READ_ONLY_ERROR.into()) }
}

pub fn is_read_only(driver: &DriverType, sql: &str) -> bool {
    let rules = Dialect::for_driver(driver).read_only;
    interpretations(driver).iter().all(|lex| {
        statement_chunks(sql, lex).into_iter().all(|chunk| {
            let masked = mask_exact(chunk, lex, true);
            read_only_words(&masked, lex, rules)
        })
    })
}

// Runs on writable connections too, since the table-data path must never escape its WHERE clause.
pub fn validate_table_filter(driver: &DriverType, filter: &str) -> Result<(), String> {
    let filter = filter.trim();
    if filter.is_empty() {
        return Ok(());
    }
    let rules = Dialect::for_driver(driver).read_only;
    for lex in interpretations(driver) {
        // Mask strings but not comments, because a bare `--` or `/*` would comment out the LIMIT.
        let masked = mask_exact(filter, &lex, false);
        if masked.contains(';') {
            return Err("filter must be a single boolean expression: ';' is not allowed".into());
        }
        let hash_comment = (0..masked.len()).any(|i| hash_comment_at(masked.as_bytes(), i, lex.hash_comments));
        if masked.contains("--") || masked.contains("/*") || masked.contains("*/") || hash_comment {
            return Err("filter must not contain comment markers".into());
        }
        if writes(&tokens(&masked, &lex), rules) {
            return Err("filter must not contain write keywords".into());
        }
    }
    Ok(())
}

// The statements the server would run, cut with the dialect's own lexer. T-SQL cuts at `;` and GO lines.
fn statement_chunks<'a>(sql: &'a str, lex: &LexRules) -> Vec<&'a str> {
    let mut chunks = Vec::new();
    let mut start = 0;
    for boundary in boundaries(sql, lex, Mode::Statements) {
        let (at, end) = match boundary {
            Boundary::Terminator { at, len, .. } => (at, at + len),
            Boundary::DelimiterLine { at, end } | Boundary::BatchSeparator { at, end, .. } => (at, end),
        };
        chunks.push(&sql[start..at]);
        start = end;
    }
    chunks.push(&sql[start..]);
    chunks
}

// Each string, quoted identifier and dollar quote becomes one space, so their contents can't look like keywords.
// Comments become a space too when `strip_comments` is set, since servers treat them as token separators. MySQL
// runs the body of an executable comment, so only its markers go.
fn mask_exact(sql: &str, lex: &LexRules, strip_comments: bool) -> String {
    let b = sql.as_bytes();
    let n = b.len();
    let mut out = String::with_capacity(n);
    let mut copied = 0;
    let mut in_exec_comment = false;
    let mut i = 0;
    while i < n {
        // A whole word, so a `$` inside an identifier can't open a dollar quote.
        if lex.dollar_in_words && lex.dollar_quotes && is_word_start(b[i], lex) {
            i = word_end(b, i, lex);
            continue;
        }
        let marker_end = if !strip_comments || !lex.exec_comments {
            None
        } else if in_exec_comment && b[i] == b'*' && b.get(i + 1) == Some(&b'/') {
            in_exec_comment = false;
            Some(i + 2)
        } else if b[i..].starts_with(b"/*!") || b[i..].starts_with(b"/*M!") {
            in_exec_comment = true;
            let body = i + if b[i + 2] == b'!' { 3 } else { 4 };
            Some(body + b[body..].iter().take_while(|c| c.is_ascii_digit()).count())
        } else {
            None
        };
        let comment_end = if !strip_comments || marker_end.is_some() {
            None
        } else if dash_comment_at(b, i, lex.dash_needs_space) {
            Some(line_comment_end(b, i + 2).unwrap_or(n))
        } else if hash_comment_at(b, i, lex.hash_comments) {
            Some(line_comment_end(b, i + 1).unwrap_or(n))
        } else if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
            Some(block_comment_end(b, i, lex.nested_block_comments).unwrap_or(n))
        } else {
            None
        };
        let end = marker_end.or(comment_end).or_else(|| {
            lexed_span_end(b, i, lex).map(|end| end.unwrap_or(n)).filter(|&end| end > i + 1 || b[i] != b'$')
        });
        match end {
            Some(end) => {
                out.push_str(&sql[copied..i]);
                out.push(' ');
                i = end;
                copied = end;
            }
            None => i += 1,
        }
    }
    out.push_str(&sql[copied..]);
    out
}

#[derive(Debug, PartialEq)]
enum Tok {
    Word(String),
    Open,
    Close,
    Dot,
    Other,
}

// Words, uppercased, and parentheses of masked text. The flag marks a token with whitespace right after it.
fn tokens(masked: &str, lex: &LexRules) -> Vec<(Tok, bool)> {
    let b = masked.as_bytes();
    let special = |c: u8| lex.at_hash_words && matches!(c, b'@' | b'#');
    let word_start = |c: u8| c.is_ascii_alphabetic() || c == b'_' || c >= 0x80 || special(c);
    let word_char =
        |c: u8| c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80 || special(c) || (lex.dollar_in_words && c == b'$');
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() || c == 0x0b {
            i += 1;
            continue;
        }
        let start = i;
        let tok = if word_start(c) {
            while i < b.len() && word_char(b[i]) {
                i += 1;
            }
            Tok::Word(masked[start..i].to_ascii_uppercase())
        } else if c.is_ascii_digit() {
            while i < b.len() && (b[i].is_ascii_alphanumeric() || matches!(b[i], b'.' | b'_')) {
                i += 1;
            }
            Tok::Other
        } else {
            i += 1;
            match c {
                b'(' => Tok::Open,
                b')' => Tok::Close,
                b'.' => Tok::Dot,
                _ => Tok::Other,
            }
        };
        let spaced = b.get(i).is_some_and(|c| c.is_ascii_whitespace() || *c == 0x0b);
        out.push((tok, spaced));
    }
    out
}

fn word(tok: Option<&(Tok, bool)>) -> Option<&str> {
    match tok {
        Some((Tok::Word(w), _)) => Some(w),
        _ => None,
    }
}

fn read_only_words(masked: &str, lex: &LexRules, rules: &ReadOnlyRules) -> bool {
    let toks = tokens(masked, lex);
    let Some(first) = toks.iter().find(|(t, _)| *t != Tok::Open) else { return true };
    let Tok::Word(first) = &first.0 else { return false };
    if rules.denied_first.contains(&first.as_str()) || !rules.allowed_first.contains(&first.as_str()) {
        return false;
    }
    let write_after_cte = toks
        .windows(2)
        .any(|w| w[0].0 == Tok::Close && word(Some(&w[1])).is_some_and(|x| WRITES_AFTER_CTE.contains(&x)));
    if first == "WITH" && write_after_cte {
        return false;
    }
    if first == "PRAGMA" && !is_read_only_pragma(masked) {
        return false;
    }
    !writes(&toks, rules)
}

fn writes(toks: &[(Tok, bool)], rules: &ReadOnlyRules) -> bool {
    let mut selected = false;
    for (i, (tok, spaced)) in toks.iter().enumerate() {
        let Tok::Word(w) = tok else { continue };
        let w = w.as_str();
        // FOR UPDATE and FOR NO KEY UPDATE lock rows. They don't write.
        let before = |back: usize| i.checked_sub(back).and_then(|j| word(toks.get(j)));
        let locking = w == "UPDATE"
            && (before(1) == Some("FOR")
                || (before(1) == Some("KEY") && before(2) == Some("NO") && before(3) == Some("FOR")));
        let qualifier = rules.qualifier_names && toks.get(i + 1).is_some_and(|(next, _)| *next == Tok::Dot);
        if !locking && (WRITE_WORDS.contains(&w) || (rules.denied_anywhere.contains(&w) && !qualifier)) {
            return true;
        }
        // SELECT ... INTO writes: a new table on Postgres and SQL Server, OUTFILE on MySQL.
        if w == "INTO" && selected {
            return true;
        }
        if w == "COPY" && *spaced {
            return true;
        }
        // Only REPLACE INTO. The REPLACE() function is a read.
        if w == "REPLACE" && word(toks.get(i + 1)) == Some("INTO") {
            return true;
        }
        selected |= w == "SELECT";
    }
    false
}

fn is_read_only_pragma(stmt: &str) -> bool {
    match PRAGMA_WITH_ARGUMENT.captures(stmt) {
        None => true,
        Some(caps) => READ_ONLY_PRAGMAS.contains(&caps[1].to_lowercase().as_str()),
    }
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
    fn comments_cannot_glue_or_hide_a_write() {
        for driver in [DriverType::Postgres, DriverType::MySql, DriverType::Sqlite] {
            assert!(!is_read_only(&driver, "EXPLAIN ANALYZE DELETE/**/FROM t"), "{driver:?}");
            assert!(!is_read_only(&driver, "WITH d AS (DELETE/**/FROM t RETURNING 1) SELECT 1"), "{driver:?}");
            assert!(is_read_only(&driver, "SELECT/**/1"), "{driver:?}");
        }
        assert!(!is_read_only(&DriverType::MySql, "SELECT 1--1; DELETE FROM t"));
        assert!(is_read_only(&DriverType::MySql, "SELECT 1 -- ; DELETE FROM t"));
        assert!(is_read_only(&DriverType::MySql, "SELECT 1 --\t; DELETE FROM t"));
        assert!(is_read_only(&DriverType::MySql, "SELECT 1 --"));
        assert!(is_read_only(&DriverType::Postgres, "SELECT 1--1; DELETE FROM t"));
        assert!(is_read_only(&DriverType::Sqlite, "SELECT 1--1; DELETE FROM t"));
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
            assert_eq!(validate_table_filter(&DriverType::Unset, filter).is_err(), want_err, "{filter}");
        }
    }

    #[test]
    fn helpers_keep_strings() {
        let lex = LexRules::COMMON;
        assert_eq!(statement_chunks(r#"SELECT ';' FROM t; SELECT "a;b""#, &lex).len(), 2);
        let out = mask_exact("SELECT '-- not a comment', 1 -- real comment\nFROM t", &lex, true);
        assert!(!out.contains("not a comment") && !out.contains("real comment") && out.contains("FROM t"), "{out}");
        assert!(assert_read_only(&DriverType::Unset, "SELECT 1").is_ok());
        assert!(assert_read_only(&DriverType::Unset, "DROP TABLE t").is_err());
    }

    #[test]
    fn every_reading_of_quotes_must_be_read_only() {
        let pg = DriverType::Postgres;
        let mysql = DriverType::MySql;
        // Postgres honours the backslash in E'...', so the DELETE is code.
        assert!(!is_read_only(&pg, "WITH x AS (SELECT E'\\'') DELETE FROM t WHERE 'a'='a'"));
        // `$` continues a Postgres identifier, so `a$x$` doesn't open a dollar quote.
        assert!(!is_read_only(&pg, "WITH d AS (SELECT 1 AS a$x$) DELETE FROM t RETURNING 1 AS b$x$"));
        // With backslash escapes on, MySQL ends the string later and runs INTO OUTFILE.
        assert!(!is_read_only(&mysql, "SELECT 'a\\'' , 1 INTO OUTFILE '/tmp/x' -- '"));
        // With NO_BACKSLASH_ESCAPES, E is a column and the DELETE runs.
        assert!(!is_read_only(&mysql, "SELECT E'\\'; DELETE FROM t; -- '"));
        // Under ANSI_QUOTES "..." is an identifier, so the CALL runs.
        assert!(!is_read_only(&mysql, r#"SELECT "a\"; CALL p(); -- ""#));
        assert!(validate_table_filter(&mysql, "x = 'a\\'' ; DELETE FROM t -- '").is_err());
        // $$DELETE$$ is one MySQL identifier, not a keyword.
        assert!(is_read_only(&mysql, "SELECT $$DELETE$$"));
        // MySQL runs the bodies of executable comments.
        assert!(!is_read_only(&mysql, "SELECT 1 /*!; DELETE FROM t */"));
        assert!(!is_read_only(&mysql, "SELECT 1 /*M!100000 ; DELETE FROM t */"));
        assert!(is_read_only(&mysql, "SELECT /*!40001 SQL_NO_CACHE */ * FROM t"));
    }
}
