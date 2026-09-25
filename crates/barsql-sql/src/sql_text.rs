// Characters that uppercase to more than one char, like German sharp s, are left as is.
pub fn to_upper(s: &str) -> String {
    if s.is_ascii() {
        return s.to_ascii_uppercase();
    }
    s.chars()
        .map(|c| {
            let mut upper = c.to_uppercase();
            match (upper.next(), upper.next()) {
                (Some(u), None) => u,
                _ => c,
            }
        })
        .collect()
}

// Parentheses are skipped so `(SELECT 1) UNION (SELECT 2)` resolves to SELECT.
pub fn first_keyword(stmt: &str) -> String {
    let stmt = stmt.trim().trim_start_matches(|c: char| c == '(' || c.is_whitespace());
    stmt.split_whitespace().next().map(to_upper).unwrap_or_default()
}

// Also strips leading whitespace, so the first keyword is visible for prefix dispatch.
pub fn strip_leading_comments(sql: &str) -> &str {
    let b = sql.as_bytes();
    let n = b.len();
    let mut i = 0;
    while i < n {
        match b[i] {
            b' ' | b'\t' | b'\n' | b'\r' => i += 1,
            b'-' if b.get(i + 1) == Some(&b'-') => {
                i += 2;
                while i < n && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < n && !(b[i] == b'*' && b[i + 1] == b'/') {
                    i += 1;
                }
                i = if i + 1 < n { i + 2 } else { n };
            }
            _ => break,
        }
    }
    &sql[i..]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upper_keeps_multi_char_uppercases() {
        assert_eq!(to_upper("select ß ünï"), "SELECT ß ÜNÏ");
    }

    #[test]
    fn first_keyword_skips_parens() {
        assert_eq!(first_keyword("  (( select 1"), "SELECT");
        assert_eq!(first_keyword("(SELECT 1) UNION (SELECT 2)"), "SELECT");
        assert_eq!(first_keyword("((("), "");
    }

    #[test]
    fn strips_leading_comments() {
        let cases = [
            ("SELECT 1", "SELECT 1"),
            ("  SELECT 1", "SELECT 1"),
            ("-- comment\nSELECT 1", "SELECT 1"),
            ("-- line1\n-- line2\nSELECT 1", "SELECT 1"),
            ("/* block */SELECT 1", "SELECT 1"),
            ("/* block */ SELECT 1", "SELECT 1"),
            ("-- line\n/* block */\nSELECT 1", "SELECT 1"),
            (" \t\n -- c\n /* b */ \n SELECT 1", "SELECT 1"),
            ("INSERT INTO t VALUES (1)", "INSERT INTO t VALUES (1)"),
            ("", ""),
            ("-- only comment", ""),
            ("/* unterminated", ""),
        ];
        for (sql, want) in cases {
            assert_eq!(strip_leading_comments(sql), want, "{sql:?}");
        }
    }
}
