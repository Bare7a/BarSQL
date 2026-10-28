use barsql_core::DriverType;

use crate::lang::tokens::{Token, TokenKind, tokenize};

// A lone SELECT from a single table, without a join, grouping or set operation, which would let a key from one row
// stand beside values from another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectSource {
    pub schema: Option<String>,
    pub table: String,
    pub items: Vec<SelectItem>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectItem {
    All,
    Column { name: String, alias: Option<String> },
    Other,
}

// Anything else after the table, like JOIN, a comma or TABLESAMPLE, means the rows aren't simply the table's.
const AFTER_TABLE: [&str; 9] = ["where", "order", "limit", "offset", "fetch", "for", "window", "option", "lock"];
// SELECT or EXEC outside parentheses starts another statement, as a T-SQL batch without semicolons does.
const NOT_ONE_TABLE: [&str; 12] = [
    "union",
    "intersect",
    "except",
    "minus",
    "group",
    "having",
    "into",
    "join",
    "connect",
    "select",
    "exec",
    "execute",
];
// Bare words a select list reads as values, not columns. Quoted, they name columns again.
const PSEUDO_COLUMNS: [&str; 12] = [
    "null",
    "true",
    "false",
    "default",
    "user",
    "current_user",
    "session_user",
    "current_date",
    "current_time",
    "current_timestamp",
    "localtime",
    "localtimestamp",
];
// MySQL's SELECT modifiers, which change how a query runs but not which rows it returns.
const MODIFIERS: [&str; 7] = [
    "sql_no_cache",
    "sql_calc_found_rows",
    "high_priority",
    "straight_join",
    "sql_small_result",
    "sql_big_result",
    "sql_buffer_result",
];

pub fn select_source(driver: &DriverType, sql: &str) -> Option<SelectSource> {
    let tokens = tokenize(sql, Some(driver));
    let mut code: Vec<&Token> = tokens.iter().filter(|t| t.kind != TokenKind::Comment).collect();
    while code.last().is_some_and(|t| t.is_punct(";")) {
        code.pop();
    }
    if code.iter().any(|t| t.is_punct(";") || t.unterminated) {
        return None;
    }
    let mut at = 0;
    let ctes = skip_ctes(&code, &mut at)?;
    if !code.get(at)?.is_keyword("select") {
        return None;
    }
    at += 1;
    skip_select_modifiers(&code, &mut at)?;
    let from = top_level(&code, at).find(|&i| code[i].is_keyword("from"))?;
    let items = select_items(&code[at..from])?;
    at = from + 1;
    let (schema, table) = table_name(&code, &mut at)?;
    if ctes.iter().any(|cte| cte.eq_ignore_ascii_case(&table)) {
        return None;
    }
    skip_alias(&code, &mut at);
    // T-SQL's table hints, WITH (NOLOCK).
    if code.get(at).is_some_and(|t| t.is_keyword("with")) && code.get(at + 1).is_some_and(|t| t.is_punct("(")) {
        at = matching_paren(&code, at + 1)? + 1;
    }
    if code.get(at).is_some_and(|t| !AFTER_TABLE.iter().any(|word| t.is_keyword(word))) {
        return None;
    }
    for i in top_level(&code, at) {
        if NOT_ONE_TABLE.iter().any(|word| code[i].is_keyword(word)) {
            return None;
        }
        // FOR UPDATE and FOR SHARE lock rows, where FOR XML or FOR JSON makes one document of them.
        if code[i].is_keyword("for")
            && !["update", "share", "no", "key", "read"]
                .iter()
                .any(|w| code.get(i + 1).is_some_and(|t| t.is_keyword(w)))
        {
            return None;
        }
    }
    Some(SelectSource { schema, table, items })
}

fn skip_ctes(code: &[&Token], at: &mut usize) -> Option<Vec<String>> {
    let mut ctes = Vec::new();
    if !code.get(*at)?.is_keyword("with") {
        return Some(ctes);
    }
    *at += 1;
    if code.get(*at)?.is_keyword("recursive") {
        *at += 1;
    }
    loop {
        let name = code.get(*at).filter(|t| t.is_ident_like())?;
        ctes.push(name.ident_text());
        *at += 1;
        if code.get(*at)?.is_punct("(") {
            *at = matching_paren(code, *at)? + 1;
        }
        if !code.get(*at)?.is_keyword("as") {
            return None;
        }
        *at += 1;
        while code.get(*at).is_some_and(|t| t.is_keyword("not") || t.is_keyword("materialized")) {
            *at += 1;
        }
        if !code.get(*at)?.is_punct("(") {
            return None;
        }
        *at = matching_paren(code, *at)? + 1;
        if !code.get(*at)?.is_punct(",") {
            return Some(ctes);
        }
        *at += 1;
    }
}

fn skip_select_modifiers(code: &[&Token], at: &mut usize) -> Option<()> {
    loop {
        let token = code.get(*at)?;
        if token.is_keyword("distinct") && code.get(*at + 1).is_some_and(|t| t.is_keyword("on")) {
            *at = matching_paren(code, *at + 2)? + 1;
        } else if token.is_keyword("distinct") || token.is_keyword("all") {
            *at += 1;
        } else if token.is_keyword("top") {
            *at += 1;
            *at = match code.get(*at)? {
                t if t.is_punct("(") => matching_paren(code, *at)? + 1,
                _ => *at + 1,
            };
            if code.get(*at)?.is_keyword("percent") {
                *at += 1;
            }
            if code.get(*at)?.is_keyword("with") && code.get(*at + 1)?.is_keyword("ties") {
                *at += 2;
            }
        } else if MODIFIERS.iter().any(|word| token.is_keyword(word)) {
            *at += 1;
        } else {
            return Some(());
        }
    }
}

fn select_items(list: &[&Token]) -> Option<Vec<SelectItem>> {
    let mut items = Vec::new();
    let mut start = 0;
    let mut depth = 0;
    for (i, token) in list.iter().enumerate() {
        if token.is_punct("(") {
            depth += 1;
        } else if token.is_punct(")") {
            depth -= 1;
        } else if depth == 0 && token.is_keyword("into") {
            return None;
        } else if depth == 0 && token.is_punct(",") {
            items.push(select_item(&list[start..i]));
            start = i + 1;
        }
    }
    items.push(select_item(&list[start..]));
    Some(items)
}

fn select_item(item: &[&Token]) -> SelectItem {
    let star = |t: &Token| t.kind == TokenKind::Op && t.text == "*";
    match item {
        [only] if star(only) => return SelectItem::All,
        [qualifier, dot, last] if qualifier.is_ident_like() && dot.is_punct(".") && star(last) => {
            return SelectItem::All;
        }
        _ => {}
    }
    let mut end = 1;
    while item.get(end).is_some_and(|t| t.is_punct(".")) && item.get(end + 1).is_some_and(|t| t.is_ident_like()) {
        end += 2;
    }
    let Some(name) = item.first().filter(|t| t.is_ident_like() && !PSEUDO_COLUMNS.contains(&t.lower.as_str())) else {
        return SelectItem::Other;
    };
    let name = if end == 1 { name.ident_text() } else { item[end - 1].ident_text() };
    let alias = match &item[end..] {
        [] => None,
        [alias] if alias.is_ident_like() => Some(alias.ident_text()),
        [as_, alias] if as_.is_keyword("as") && alias.is_ident_like() => Some(alias.ident_text()),
        _ => return SelectItem::Other,
    };
    SelectItem::Column { name, alias }
}

// A database-qualified name, a function or a subquery gets no edits.
fn table_name(code: &[&Token], at: &mut usize) -> Option<(Option<String>, String)> {
    let first = code.get(*at).filter(|t| t.is_ident_like() && !t.is_keyword("only") && !t.is_keyword("lateral"))?;
    *at += 1;
    let mut parts = vec![first.ident_text()];
    while code.get(*at).is_some_and(|t| t.is_punct(".")) {
        parts.push(code.get(*at + 1).filter(|t| t.is_ident_like())?.ident_text());
        *at += 2;
    }
    if code.get(*at).is_some_and(|t| t.is_punct("(")) {
        return None;
    }
    match <[String; 2]>::try_from(parts) {
        Ok([schema, table]) => Some((Some(schema), table)),
        Err(parts) if parts.len() == 1 => Some((None, parts.into_iter().next()?)),
        Err(_) => None,
    }
}

fn skip_alias(code: &[&Token], at: &mut usize) {
    match code.get(*at) {
        Some(t) if t.is_keyword("as") => *at += 2,
        Some(t) if t.kind == TokenKind::Quoted => *at += 1,
        Some(t) if t.kind == TokenKind::Ident && !is_clause_word(&t.lower) => *at += 1,
        _ => {}
    }
}

fn is_clause_word(lower: &str) -> bool {
    AFTER_TABLE.contains(&lower)
        || NOT_ONE_TABLE.contains(&lower)
        || [
            "with",
            "inner",
            "left",
            "right",
            "full",
            "cross",
            "natural",
            "outer",
            "lateral",
            "tablesample",
            "use",
            "force",
            "ignore",
            "partition",
            "indexed",
            "not",
            "final",
            "sample",
            "on",
        ]
        .contains(&lower)
}

// Indices from `from` on that sit outside any parentheses opened after it.
fn top_level<'a>(code: &'a [&Token], from: usize) -> impl Iterator<Item = usize> + 'a {
    let mut depth = 0;
    (from..code.len()).filter(move |&i| {
        if code[i].is_punct("(") {
            depth += 1;
        } else if code[i].is_punct(")") {
            depth -= 1;
        }
        depth == 0 && !code[i].is_punct(")")
    })
}

fn matching_paren(code: &[&Token], open: usize) -> Option<usize> {
    if !code.get(open)?.is_punct("(") {
        return None;
    }
    let mut depth = 0;
    for (i, token) in code.iter().enumerate().skip(open) {
        if token.is_punct("(") {
            depth += 1;
        } else if token.is_punct(")") {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(driver: DriverType, sql: &str) -> Option<(Option<String>, String)> {
        select_source(&driver, sql).map(|s| (s.schema, s.table))
    }

    fn table(sql: &str) -> Option<String> {
        source(DriverType::Postgres, sql).map(|(_, table)| table)
    }

    #[test]
    fn one_table_queries_name_their_table() {
        assert_eq!(table("SELECT * FROM users"), Some("users".into()));
        assert_eq!(table("select id, name from users u where u.id > 3 order by 1 limit 10;"), Some("users".into()));
        assert_eq!(table("SELECT DISTINCT * FROM users FOR UPDATE SKIP LOCKED"), Some("users".into()));
        assert_eq!(table("SELECT DISTINCT ON (org_id) * FROM users ORDER BY org_id"), Some("users".into()));
        assert_eq!(table("-- users\nSELECT /* x */ * FROM \"Users\" AS u"), Some("Users".into()));
        assert_eq!(
            table("SELECT * FROM users WHERE id IN (SELECT user_id FROM a JOIN b ON true)"),
            Some("users".into())
        );
        assert_eq!(table("WITH r AS (SELECT 1) SELECT * FROM users, r"), None, "a comma joins");
        assert_eq!(table("WITH r AS (SELECT 1) SELECT * FROM users WHERE id IN (TABLE r)"), Some("users".into()));
        assert_eq!(source(DriverType::Postgres, "SELECT * FROM app.users"), Some((Some("app".into()), "users".into())));
        assert_eq!(
            source(
                DriverType::SqlServer,
                "SELECT TOP (10) PERCENT WITH TIES * FROM [dbo].[t] WITH (NOLOCK) ORDER BY id"
            ),
            Some((Some("dbo".into()), "t".into()))
        );
        assert_eq!(
            source(DriverType::MySql, "SELECT SQL_NO_CACHE * FROM `shop`.`orders` LOCK IN SHARE MODE"),
            Some((Some("shop".into()), "orders".into()))
        );
    }

    #[test]
    fn joins_groups_sets_and_other_shapes_have_no_one_table() {
        for sql in [
            "SELECT * FROM a JOIN b ON a.id = b.a_id",
            "SELECT * FROM a, b",
            "SELECT * FROM a x LEFT JOIN a y ON x.boss = y.id",
            "SELECT org_id, count(*) FROM users GROUP BY org_id",
            "SELECT * FROM a UNION SELECT * FROM b",
            "SELECT * FROM a; SELECT * FROM b",
            "SELECT * INTO copy FROM a",
            "SELECT * FROM (SELECT * FROM a) s",
            "SELECT * FROM generate_series(1, 3)",
            "SELECT * FROM ONLY users",
            "SELECT * FROM db.dbo.users",
            "SELECT * FROM t FOR XML AUTO",
            "SELECT * FROM t TABLESAMPLE SYSTEM (10)",
            "SELECT * FROM a WHERE id = 1 SELECT * FROM b",
            "SELECT * FROM a EXEC sp_who",
            "WITH t AS (SELECT * FROM users) SELECT * FROM t",
            "SELECT 1",
            "UPDATE users SET a = 1 RETURNING *",
            "SELECT * FROM 'unterminated",
        ] {
            assert_eq!(table(sql), None, "{sql}");
        }
    }

    #[test]
    fn select_items_name_their_columns() {
        let items = |sql: &str| select_source(&DriverType::Postgres, sql).map(|s| s.items).unwrap_or_default();
        let column = |name: &str, alias: Option<&str>| SelectItem::Column {
            name: name.into(),
            alias: alias.map(str::to_string),
        };
        assert_eq!(
            items("SELECT u.*, id, u.name AS n, \"Email\" mail, upper(name), name || 'x', 1 FROM users u"),
            [
                SelectItem::All,
                column("id", None),
                column("name", Some("n")),
                column("Email", Some("mail")),
                SelectItem::Other,
                SelectItem::Other,
                SelectItem::Other,
            ]
        );
        assert_eq!(items("SELECT public.users.id FROM users"), [column("id", None)]);
        assert_eq!(items("SELECT * FROM users"), [SelectItem::All]);
    }
}
