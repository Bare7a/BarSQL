// A name an engine's error says it doesn't know, for errors that don't say where. The editor marks the name in the
// statement, and the schema may hold what was meant.
use std::sync::LazyLock;

use barsql_core::DriverType;
use regex::Regex;

use super::tokens::{TokenKind, tokenize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnknownKind {
    Column,
    Table,
}

// Each engine's wording, first match wins: SQLite, PostgreSQL, MySQL and MariaDB, SQL Server, then ClickHouse.
static WORDINGS: LazyLock<Vec<(Regex, UnknownKind)>> = LazyLock::new(|| {
    use UnknownKind::{Column, Table};
    [
        (r"no such column: (\S+)", Column),
        (r"no such table: (\S+)", Table),
        (r#"column "?([^"\s]+)"? does not exist"#, Column),
        (r#"column "([^"]+)" of relation"#, Column),
        (r#"relation "?([^"\s]+)"? does not exist"#, Table),
        (r"Unknown column '([^']+)'", Column),
        (r"Table '([^']+)' doesn't exist", Table),
        (r"Invalid column name '([^']+)'", Column),
        (r"Invalid object name '([^']+)'", Table),
        (r"Unknown expression identifier `([^`]+)`", Column),
        (r"Missing columns: '([^']+)'", Column),
        (r"Unknown identifier: (\S+)", Column),
        (r"Table (\S+) (?:does not|doesn't) exist", Table),
    ]
    .into_iter()
    .map(|(wording, kind)| (Regex::new(wording).unwrap(), kind))
    .collect()
});

// The unknown column or table an error names, without its qualifier: `u.usernme` is usernme.
pub fn unknown_name(message: &str) -> Option<(UnknownKind, String)> {
    let (name, kind) = WORDINGS.iter().find_map(|(wording, kind)| Some((wording.captures(message)?.get(1)?, *kind)))?;
    let unquote = |part: &str| part.trim_matches(|c| matches!(c, '"' | '`' | '\'' | '[' | ']')).to_string();
    let name = unquote(name.as_str().rsplit('.').next().unwrap_or_default());
    (!name.is_empty()).then_some((kind, name))
}

// Where `name` first appears in the statement as a name of its own, quoted or not, as a 1-based character position
// like the ones engines report. Strings and comments don't count.
pub fn find_name(statement: &str, name: &str, driver: Option<&DriverType>) -> Option<u32> {
    let token = tokenize(statement, driver).into_iter().find(|token| match token.kind {
        TokenKind::Ident => token.text.eq_ignore_ascii_case(name),
        TokenKind::Quoted => {
            token.text.get(1..token.text.len() - 1).is_some_and(|inner| inner.eq_ignore_ascii_case(name))
        }
        _ => false,
    })?;
    let start = token.start + usize::from(token.kind == TokenKind::Quoted);
    Some(statement[..start].chars().count() as u32 + 1)
}

// The candidate a typo or two away from `name`, ignoring case: one edit in up to five letters, one more each three
// after. The first of equally close ones.
pub fn closest<'a>(name: &str, candidates: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let name = name.to_lowercase();
    let limit = (name.chars().count() / 3).max(1);
    let mut best: Option<(usize, &str)> = None;
    for candidate in candidates {
        let distance = edit_distance(&name, &candidate.to_lowercase());
        if distance <= limit && best.is_none_or(|(closest, _)| distance < closest) {
            best = Some((distance, candidate));
        }
    }
    best.map(|(_, candidate)| candidate)
}

// Optimal string alignment: an insertion, a deletion, a substitution or a swap of neighbours each costs one.
fn edit_distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut d = vec![vec![0; b.len() + 1]; a.len() + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in d[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            d[i][j] = (d[i - 1][j] + 1).min(d[i][j - 1] + 1).min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                d[i][j] = d[i][j].min(d[i - 2][j - 2] + 1);
            }
        }
    }
    d[a.len()][b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_engines_wording_gives_the_unknown_name() {
        let column = |name: &str| Some((UnknownKind::Column, name.to_string()));
        let table = |name: &str| Some((UnknownKind::Table, name.to_string()));
        let cases = [
            ("SQL logic error: no such column: usernme (1)", column("usernme")),
            ("SQL logic error: no such column: u.usernme (1)", column("usernme")),
            ("SQL logic error: no such table: main.userz (1)", table("userz")),
            ("column \"usernme\" does not exist", column("usernme")),
            ("column u.usernme does not exist", column("usernme")),
            ("relation \"userz\" does not exist", table("userz")),
            ("Unknown column 'u.usernme' in 'field list'", column("usernme")),
            ("Table 'forum.userz' doesn't exist", table("userz")),
            ("Invalid column name 'usernme'.", column("usernme")),
            ("Invalid object name 'dbo.userz'.", table("userz")),
            ("Code: 47. DB::Exception: Unknown expression identifier `usernme` in scope SELECT", column("usernme")),
            ("Code: 47. DB::Exception: Missing columns: 'usernme' while processing query", column("usernme")),
            ("Code: 60. DB::Exception: Table default.userz does not exist.", table("userz")),
            ("syntax error at or near \"FORM\"", None),
            ("permission denied for table users", None),
        ];
        for (message, want) in cases {
            assert_eq!(unknown_name(message), want, "{message}");
        }
    }

    // 1-based characters, as engines report positions. Strings and comments don't count.
    #[test]
    fn a_name_is_found_as_a_whole_word_outside_strings() {
        let sqlite = Some(&DriverType::Sqlite);
        assert_eq!(find_name("SELECT id, usernme FROM users", "usernme", sqlite), Some(12));
        assert_eq!(find_name("SELECT 'usernme', usernme FROM t", "USERNME", sqlite), Some(19));
        assert_eq!(find_name("-- usernme\nSELECT usernme", "usernme", sqlite), Some(19));
        assert_eq!(find_name("SELECT \"usernme\" FROM t", "usernme", sqlite), Some(9));
        assert_eq!(find_name("SELECT usernmes FROM t", "usernme", sqlite), None);
        assert_eq!(find_name("SELECT 'é', usernme FROM t", "usernme", sqlite), Some(13));
    }

    #[test]
    fn the_closest_name_is_a_typo_away() {
        let columns = ["id", "username", "email", "display_name", "created_at"];
        assert_eq!(closest("usernme", columns), Some("username"));
        assert_eq!(closest("emial", columns), Some("email"));
        assert_eq!(closest("Username", columns), Some("username"));
        assert_eq!(closest("price", columns), None, "nothing close");
        assert_eq!(closest("idd", ["id", "ids"]), Some("id"), "a tie goes to the first");
    }
}
