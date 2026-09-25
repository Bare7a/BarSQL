use std::sync::LazyLock;

use regex::Regex;

use super::statements::parse_statements;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxnControl {
    Begin,
    Commit,
    Rollback,
}

static BLOCK_COMMENT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)/\*.*?\*/").expect("regex"));
static LINE_COMMENT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"--[^\n]*").expect("regex"));
static TRAILING_SEMI: LazyLock<Regex> = LazyLock::new(|| Regex::new(r";\s*$").expect("regex"));
static SPACES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").expect("regex"));

// Some only when `sql` is a single transaction-control statement, ignoring comments, whitespace and one
// trailing semicolon. Anything else runs normally, including `ROLLBACK TO SAVEPOINT ...` since it stays
// inside the transaction.
pub fn detect_transaction_control(sql: &str) -> Option<TxnControl> {
    let statements = parse_statements(sql, None);
    let [statement] = statements.as_slice() else { return None };
    let code = BLOCK_COMMENT.replace_all(statement.text, " ");
    let code = LINE_COMMENT.replace_all(&code, " ");
    let code = TRAILING_SEMI.replace(&code, "");
    let code = SPACES.replace_all(code.trim(), " ").to_uppercase();
    match code.as_str() {
        "BEGIN" | "BEGIN WORK" | "BEGIN TRANSACTION" | "START TRANSACTION" => Some(TxnControl::Begin),
        "COMMIT" | "COMMIT WORK" | "COMMIT TRANSACTION" => Some(TxnControl::Commit),
        "ROLLBACK" | "ROLLBACK WORK" | "ROLLBACK TRANSACTION" => Some(TxnControl::Rollback),
        _ => None,
    }
}
