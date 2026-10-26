use barsql_core::DriverType;

use crate::lang::tokens::{TokenKind, tokenize};

// A SQLite CREATE TABLE cut into its column and constraint items, so a rebuild can copy the ones it doesn't change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateTable<'a> {
    pub items: Vec<&'a str>,
    // After the closing parenthesis, like " STRICT, WITHOUT ROWID".
    pub options: &'a str,
}

const CONSTRAINT_STARTS: [&str; 5] = ["constraint", "primary", "unique", "check", "foreign"];

pub fn split_create_table(sql: &str) -> Option<CreateTable<'_>> {
    let tokens = tokenize(sql, Some(&DriverType::Sqlite));
    let mut code = tokens.iter().filter(|t| t.kind != TokenKind::Comment);
    let open = code.by_ref().find(|t| t.is_punct("(") || (t.kind == TokenKind::Ident && t.lower == "as"))?;
    if !open.is_punct("(") {
        return None;
    }
    let (mut depth, mut start, mut items) = (1usize, open.end, Vec::new());
    for token in code.filter(|t| t.kind == TokenKind::Punct) {
        match token.text {
            "(" => depth += 1,
            "," if depth == 1 => {
                items.push(sql[start..token.start].trim());
                start = token.end;
            }
            ")" if depth == 1 => {
                items.push(sql[start..token.start].trim());
                items.retain(|item| !item.is_empty());
                return Some(CreateTable { items, options: &sql[token.end..] });
            }
            ")" => depth -= 1,
            _ => {}
        }
    }
    None
}

pub fn item_column(item: &str) -> Option<String> {
    let tokens = tokenize(item, Some(&DriverType::Sqlite));
    let first = tokens.iter().find(|t| t.kind != TokenKind::Comment)?;
    match first.kind {
        TokenKind::Ident if CONSTRAINT_STARTS.contains(&first.lower.as_str()) => None,
        TokenKind::Ident | TokenKind::Quoted => Some(first.ident_text()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TABLE: &str = r#"CREATE TABLE "t" (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  "na,me" TEXT DEFAULT 'a,b' COLLATE NOCASE /* shown, in lists */,
  [br] TEXT, `tk` TEXT,
  total REAL GENERATED ALWAYS AS (price * qty) STORED,
  CHECK (kind IN ('a', 'b')),
  CONSTRAINT fk FOREIGN KEY (org) REFERENCES orgs (id)
) STRICT, WITHOUT ROWID"#;

    #[test]
    fn splits_items_at_top_level_commas() {
        let table = split_create_table(TABLE).expect("parses");
        assert_eq!(table.items.len(), 7);
        assert_eq!(table.items[1], "\"na,me\" TEXT DEFAULT 'a,b' COLLATE NOCASE /* shown, in lists */");
        let columns: Vec<Option<String>> = table.items.iter().map(|item| item_column(item)).collect();
        let want = [Some("id"), Some("na,me"), Some("br"), Some("tk"), Some("total"), None, None];
        assert_eq!(columns, want.map(|c| c.map(String::from)));
        assert_eq!(table.options, " STRICT, WITHOUT ROWID");
        assert!(split_create_table("CREATE TABLE t AS SELECT 1").is_none());
    }
}
