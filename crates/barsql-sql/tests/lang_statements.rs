use barsql_core::DriverType;
use barsql_sql::lang::{
    EditorStatement, current_statement_range, current_statement_start, parse_statements, statement_at_offset,
    statement_at_run_line,
};

const PG: Option<&DriverType> = Some(&DriverType::Postgres);
const MY: Option<&DriverType> = Some(&DriverType::MySql);
const LITE: Option<&DriverType> = Some(&DriverType::Sqlite);

fn texts<'a>(sql: &'a str, driver: Option<&DriverType>) -> Vec<&'a str> {
    parse_statements(sql, driver).into_iter().map(|s| s.text).collect()
}

fn count(sql: &str, driver: Option<&DriverType>) -> usize {
    parse_statements(sql, driver).len()
}

#[test]
fn one_statement_per_semicolon_separated_chunk() {
    assert_eq!(texts("SELECT 1; SELECT 2; SELECT 3", None), ["SELECT 1;", "SELECT 2;", "SELECT 3"]);
}

#[test]
fn records_the_run_line_of_each_statement() {
    let lines: Vec<usize> =
        parse_statements("SELECT 1;\n\n  SELECT 2;\n   SELECT 3", None).iter().map(|s| s.run_line).collect();
    assert_eq!(lines, [1, 3, 4]);
}

#[test]
fn quotes_and_comments_hide_semicolons() {
    assert_eq!(texts("SELECT ';not;a;split;';", None), ["SELECT ';not;a;split;';"]);
    assert_eq!(count("SELECT 'O''Reilly;'; SELECT 2", None), 2);
    assert_eq!(count("SELECT 1 FROM \"weird;name\"; SELECT 2", None), 2);
    assert_eq!(count("SELECT 1 FROM `a;b`; SELECT 2", None), 2);
    assert_eq!(count("SELECT 1 -- ; ignore\n; SELECT 2", None), 2);
    assert_eq!(count("SELECT 1 /* ; ignore ; */; SELECT 2", None), 2);
}

#[test]
fn respects_dollar_quotes() {
    let out = parse_statements("DO $$ BEGIN PERFORM 1; END $$;\nSELECT 1", None);
    assert_eq!(out.len(), 2);
    assert!(out[0].text.starts_with("DO $$"));
    assert_eq!(count("DO $tag$ ; ; ; $tag$;\nSELECT 1", None), 2);
    // An all-digit tag is a placeholder, not a delimiter.
    assert_eq!(count("SELECT $1$; SELECT 2", None), 2);
}

#[test]
fn keeps_lone_semicolons_but_drops_blank_chunks() {
    assert_eq!(count("   \n   ", None), 0);
    assert_eq!(texts("SELECT 1;;\nSELECT 2", None), ["SELECT 1;", ";", "SELECT 2"]);
}

#[test]
fn keeps_a_trailing_statement_without_semicolon() {
    assert_eq!(
        parse_statements("SELECT 1", None),
        [EditorStatement { text: "SELECT 1", run_line: 1, start: 0, end: 8, terminated: false }]
    );
}

#[test]
fn tracks_run_lines_across_many_statements() {
    let sql = (0..50).map(|i| format!("SELECT {i};")).collect::<Vec<_>>().join("\n");
    let lines: Vec<usize> = parse_statements(&sql, None).iter().map(|s| s.run_line).collect();
    assert_eq!(lines, (1..=50).collect::<Vec<_>>());
}

#[test]
fn honours_each_dialect() {
    assert_eq!(count(r"SELECT 'it\'s a; test'; SELECT 2", MY), 2);
    assert_eq!(count("SELECT 1 # ; not a split\n; SELECT 2", MY), 2);
    assert_eq!(count("SELECT x # y FROM t; SELECT ';'", PG), 2);
    assert_eq!(count("SELECT a$tag$ FROM t; SELECT b$tag$ FROM u", MY), 2);
    let nested = "/* outer /* inner */ ; still comment */ SELECT 1";
    assert_eq!(texts(nested, PG).len(), 1);
    assert!(texts(nested, PG)[0].contains("SELECT 1"));
    assert_eq!(count(r"SELECT E'a\'b; not a split'; SELECT 2", PG), 2);
    assert_eq!(count(r"SELECT E'a\'b; not a split'; SELECT 2", None), 2);
    assert_eq!(count(r"SELECT 'path\'; SELECT 2", LITE), 2);
}

const PROC: &str = "DELIMITER //\nCREATE PROCEDURE p()\nBEGIN\n  SELECT 1;\n  SELECT 2;\nEND//\nDELIMITER ;\nSELECT 3;";

#[test]
fn mysql_delimiter_keeps_bodies_whole() {
    assert_eq!(texts(PROC, MY), ["CREATE PROCEDURE p()\nBEGIN\n  SELECT 1;\n  SELECT 2;\nEND", "SELECT 3;"]);
    assert_eq!(parse_statements(PROC, MY).iter().map(|s| s.run_line).collect::<Vec<_>>(), [2, 8]);
    let dollars = "DELIMITER $$\nCREATE TRIGGER t BEFORE INSERT ON x FOR EACH ROW BEGIN SET @a = 1; END$$\nDELIMITER ;";
    let out = texts(dollars, MY);
    assert_eq!(out.len(), 1);
    assert!(out[0].ends_with("END"));
    assert_eq!(count("SELECT delimiter FROM t; SELECT 2", MY), 2);
    assert!(count(PROC, PG) > 2, "DELIMITER is a mysql client command only");
}

#[test]
fn sqlite_trigger_bodies_stay_whole() {
    let sql = "CREATE TRIGGER t AFTER INSERT ON x BEGIN\n  SELECT 1;\n  SELECT 2;\nEND;\nSELECT 3";
    assert_eq!(
        texts(sql, LITE),
        ["CREATE TRIGGER t AFTER INSERT ON x BEGIN\n  SELECT 1;\n  SELECT 2;\nEND;", "SELECT 3"]
    );
    assert_eq!(parse_statements(sql, LITE).iter().map(|s| s.run_line).collect::<Vec<_>>(), [1, 5]);
    assert_eq!(count(sql, PG), 4);
}

#[test]
fn non_ascii_text_splits_on_character_boundaries() {
    assert_eq!(texts("SELECT 'é'; SELECT 'ü' AS ж; SELECT 🙂", None), ["SELECT 'é';", "SELECT 'ü' AS ж;", "SELECT 🙂"]);
    assert_eq!(texts("DELIMITER ж\nSELECT 1ж SELECT 2", MY), ["SELECT 1", "SELECT 2"]);
}

#[test]
fn finds_the_statement_at_a_run_line() {
    let statements = parse_statements("SELECT 1;\n\nSELECT 2", None);
    assert_eq!(statement_at_run_line(&statements, 3).map(|s| s.text), Some("SELECT 2"));
    assert!(statement_at_run_line(&parse_statements("SELECT 1;", None), 9).is_none());
}

#[test]
fn current_statement_start_follows_the_cursor() {
    let statements = parse_statements("SELECT 1;\nSELECT 2", None);
    assert_eq!(current_statement_start(&statements, 5), 0);
    assert_eq!(current_statement_start(&statements, 12), 9);
    assert_eq!(current_statement_start(&parse_statements("SELECT 1;\n", None), 10), 9);
    // At the end of an unterminated statement the cursor is still inside it.
    let open = "SELECT * FROM ";
    assert_eq!(current_statement_start(&parse_statements(open, None), open.len()), 0);
    let closed = "SELECT 1;";
    assert_eq!(current_statement_start(&parse_statements(closed, None), closed.len()), closed.len());
}

#[test]
fn current_statement_range_scopes_to_the_statement() {
    let sql = "SELECT 1;\nSELECT 2";
    let statements = parse_statements(sql, None);
    assert_eq!(current_statement_range(&statements, 5, sql.len()), 0..9);
    assert_eq!(current_statement_range(&statements, 12, sql.len()), 9..18);
    let sql = "SELECT * FROM users;\nSELECT * FROM ";
    let range = current_statement_range(&parse_statements(sql, None), sql.len(), sql.len());
    let scoped = &sql[range];
    assert!(!scoped.contains("users") && scoped.contains("SELECT * FROM"), "{scoped:?}");
}

#[test]
fn finds_the_statement_at_an_offset() {
    let statements = parse_statements("SELECT 1; SELECT 2", None);
    assert_eq!(statement_at_offset(&statements, 3).map(|s| s.text), Some("SELECT 1;"));
    assert_eq!(statement_at_offset(&statements, 14).map(|s| s.text), Some("SELECT 2"));
}
