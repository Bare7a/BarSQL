use std::collections::HashSet;

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

// The identifiers a statement names, lowercased. Strings and comments don't count.
pub fn names_in(sql: &str) -> HashSet<String> {
    let tokens = tokenize(sql, Some(&DriverType::Sqlite));
    let names = tokens.iter().filter(|t| matches!(t.kind, TokenKind::Ident | TokenKind::Quoted));
    names.map(|t| t.ident_text().to_lowercase()).collect()
}

// What the first top-level parentheses hold, like a generated column's expression.
pub fn paren_body(text: &str) -> Option<&str> {
    let tokens = tokenize(text, Some(&DriverType::Sqlite));
    let open = tokens.iter().position(|t| t.is_punct("("))?;
    let mut depth = 0usize;
    for token in &tokens[open..] {
        if token.is_punct("(") {
            depth += 1;
        } else if token.is_punct(")") {
            depth -= 1;
            if depth == 0 {
                return Some(text[tokens[open].end..token.start].trim());
            }
        }
    }
    None
}

// One column definition: its name, its type, and each constraint clause as written, keyed by the clause's kind (the
// keyword after CONSTRAINT <name> for a named one).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnItem<'a> {
    pub name: String,
    pub data_type: &'a str,
    pub clauses: Vec<(String, &'a str)>,
}

pub fn column_item(item: &str) -> Option<ColumnItem<'_>> {
    let tokens: Vec<_> =
        tokenize(item, Some(&DriverType::Sqlite)).into_iter().filter(|t| t.kind != TokenKind::Comment).collect();
    let name = tokens.first().filter(|t| matches!(t.kind, TokenKind::Ident | TokenKind::Quoted))?;
    if name.kind == TokenKind::Ident && CONSTRAINT_STARTS.contains(&name.lower.as_str()) {
        return None;
    }
    let mut starts: Vec<(usize, String)> = Vec::new();
    let (mut depth, mut naming) = (0usize, 0u8);
    for (ix, token) in tokens.iter().enumerate().skip(1) {
        if token.is_punct("(") || token.is_punct(")") {
            depth = if token.text == "(" { depth + 1 } else { depth.saturating_sub(1) };
            continue;
        }
        if naming > 0 {
            naming -= 1;
            if naming == 0
                && let Some(last) = starts.last_mut()
            {
                last.1 = token.lower.clone();
            }
            continue;
        }
        if depth > 0 || token.kind != TokenKind::Ident {
            continue;
        }
        // SET NULL, SET DEFAULT and NOT DEFERRABLE belong to a REFERENCES clause.
        let before = tokens[ix - 1].lower.as_str();
        let starts_clause = match token.lower.as_str() {
            "constraint" | "primary" | "unique" | "check" | "collate" | "references" | "generated" => true,
            "as" => before != "always",
            "not" => tokens.get(ix + 1).is_some_and(|next| next.lower == "null"),
            "null" => !matches!(before, "not" | "set" | "default"),
            "default" => before != "set",
            _ => false,
        };
        if starts_clause {
            starts.push((token.start, token.lower.clone()));
            if token.lower == "constraint" {
                naming = 2;
            }
        }
    }
    let type_end = starts.first().map_or(item.len(), |(start, _)| *start);
    let clauses = starts
        .iter()
        .enumerate()
        .map(|(ix, (start, kind))| {
            let end = starts.get(ix + 1).map_or(item.len(), |(next, _)| *next);
            (kind.clone(), item[*start..end].trim())
        })
        .collect();
    Some(ColumnItem { name: name.ident_text(), data_type: item[name.end..type_end].trim(), clauses })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_in_lists_identifiers_but_not_strings() {
        let names = names_in("SELECT \"Nick\" FROM e2e_people WHERE note = 'e2e_orgs'");
        assert!(names.contains("nick") && names.contains("e2e_people") && !names.contains("e2e_orgs"), "{names:?}");
    }

    #[test]
    fn paren_bodies_skip_the_keyword_before_them() {
        assert_eq!(paren_body("GENERATED ALWAYS AS (price * (1 + tax)) STORED"), Some("price * (1 + tax)"));
        assert_eq!(paren_body("AS (')' || name)"), Some("')' || name"));
        assert_eq!(paren_body("COLLATE NOCASE"), None);
    }

    #[test]
    fn column_items_split_into_type_and_clauses() {
        let item =
            column_item("owner_id INTEGER CONSTRAINT owner_fk REFERENCES people(id) ON DELETE SET NULL NOT DEFERRABLE")
                .unwrap();
        assert_eq!((item.name.as_str(), item.data_type), ("owner_id", "INTEGER"));
        assert_eq!(
            item.clauses,
            [("references".to_string(), "CONSTRAINT owner_fk REFERENCES people(id) ON DELETE SET NULL NOT DEFERRABLE")]
        );
        let item = column_item("\"tag\" VARCHAR(10) UNIQUE NOT NULL DEFAULT NULL COLLATE NOCASE").unwrap();
        assert_eq!(item.data_type, "VARCHAR(10)");
        let kinds: Vec<&str> = item.clauses.iter().map(|(kind, _)| kind.as_str()).collect();
        assert_eq!(kinds, ["unique", "not", "default", "collate"]);
        assert_eq!(item.clauses[2].1, "DEFAULT NULL");
        let generated = column_item("total REAL GENERATED ALWAYS AS (a * 2) STORED").unwrap();
        assert_eq!(generated.clauses, [("generated".to_string(), "GENERATED ALWAYS AS (a * 2) STORED")]);
        assert!(column_item("CHECK (a > 0)").is_none());
    }

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
