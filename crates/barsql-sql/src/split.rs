use std::ops::Range;

use barsql_core::DriverType;

use crate::lex::LexRules;
use crate::lex::boundary::{Boundary, Mode, boundaries};
use crate::lex::prim::{block_comment_end, dash_comment_at, hash_comment_at, line_comment_end};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Statement<'a> {
    pub text: &'a str,
    pub range: Range<usize>,
    // `GO 3` runs its batch three times.
    pub repeat: u32,
}

// What goes to the server, one request per statement. Drops terminators and comment-only chunks. Ranges point
// at the trimmed statement in `sql`. T-SQL splits only at GO lines, since variables live for the whole batch.
pub fn split_statements<'a>(driver: &DriverType, sql: &'a str) -> Vec<Statement<'a>> {
    let rules = LexRules::for_driver(Some(driver));
    let mut out = Vec::new();
    let mut push = |start: usize, end: usize, repeat: u32| {
        let chunk = &sql[start..end];
        if has_code(chunk.as_bytes(), &rules) {
            let lead = chunk.len() - chunk.trim_start().len();
            let text = chunk.trim();
            out.push(Statement { text, range: start + lead..start + lead + text.len(), repeat });
        }
    };
    let mut start = 0;
    for boundary in boundaries(sql, &rules, Mode::ExecutionUnits) {
        match boundary {
            Boundary::Terminator { at, len, .. } => {
                push(start, at, 1);
                start = at + len;
            }
            Boundary::DelimiterLine { at, end } => {
                push(start, at, 1);
                start = end;
            }
            Boundary::BatchSeparator { at, end, repeat } => {
                push(start, at, repeat);
                start = end;
            }
        }
    }
    push(start, sql.len(), 1);
    out
}

// A batch followed by `GO n` appears n times.
pub fn split_statement_texts(driver: &DriverType, sql: &str) -> Vec<String> {
    split_statements(driver, sql)
        .into_iter()
        .flat_map(|s| std::iter::repeat_n(s.text.to_string(), s.repeat as usize))
        .collect()
}

pub(crate) fn has_code(s: &[u8], rules: &LexRules) -> bool {
    let n = s.len();
    let mut i = 0;
    while i < n {
        if matches!(s[i], b' ' | b'\t' | b'\n' | b'\r') {
            i += 1;
        } else if dash_comment_at(s, i, rules.dash_needs_space) {
            i = line_comment_end(s, i + 2).unwrap_or(n);
        } else if hash_comment_at(s, i, rules.hash_comments) {
            i = line_comment_end(s, i + 1).unwrap_or(n);
        } else if s[i] == b'/' && s.get(i + 1) == Some(&b'*') {
            i = block_comment_end(s, i, rules.nested_block_comments).unwrap_or(n);
        } else {
            return true;
        }
    }
    false
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
