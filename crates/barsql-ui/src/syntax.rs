use barsql_core::DriverType;
use barsql_sql::lex::LexRules;
use gpui_kit::component::highlighter::{LanguageConfig, LanguageRegistry};

// GPUI Kit's JSON query lists `(string) @string` before its key pattern and the first capture wins, so keys
// and values get one colour. This query tells keys, string values and literals apart.
const JSON_HIGHLIGHTS: &str = r#"
(comment) @comment
(pair key: (string) @property)
(string) @string.special
(number) @number
[(true) (false) (null)] @boolean
["," ":" "{" "}" "[" "]"] @punctuation
"#;

// The SQL query puts these keywords under names that themes draw in another color, or not at all.
const SQL_KEYWORD_GROUPS: [&str; 4] = ["] @attribute", "] @conditional", "] @storageclass", "] @type.qualifier"];

// Number patterns that never match first, behind `(literal) @string`, though each holds a regex.
const SQL_UNUSED: [&str; 2] = ["((literal) @number\n", "((literal) @float\n"];

// A node keeps its first capture, so these come before the patterns they override. TRUE, FALSE and NULL draw as
// numbers, since many themes give booleans the keyword color, and a table alias like its table, as in `u.email`.
// A regex holds tens of KB per editor, hence #any-of? and an ASCII-only number check.
const SQL_FIRST: &str = r#"
((literal) @number (#match? @number "(?-u)^[-+]?[[:space:]]*[.]?[0-9]"))
((literal) @number (#any-of? @number "TRUE" "true" "True" "FALSE" "false" "False" "NULL" "null" "Null"))
[(keyword_true) (keyword_false) (keyword_null)] @number
(relation alias: (identifier) @type)
"#;

// The grammar reads "..." in an expression as a string. Everywhere but MySQL it's a name, drawn like a bare one.
const SQL_QUOTED_NAMES: &str = r#"((literal) @field (#match? @field "(?-u)^\""))"#;

fn sql_highlights(quoted_names: bool) -> String {
    let query = SQL_KEYWORD_GROUPS
        .iter()
        .fold(tree_sitter_sequel::HIGHLIGHTS_QUERY.to_string(), |query, group| query.replace(group, "] @keyword"));
    let query = SQL_UNUSED.iter().fold(query, |query, head| {
        let Some(start) = query.find(head) else { return query };
        let end = query[start..].find("))").map_or(start, |len| start + len + 2);
        format!("{}{}", &query[..start], &query[end..])
    });
    let names = if quoted_names { SQL_QUOTED_NAMES } else { "" };
    format!("{names}{SQL_FIRST}{query}")
}

// Call before any editor exists. Highlighters keep the query they were built with.
pub fn init() {
    let json = LanguageConfig::new("json", tree_sitter_json::LANGUAGE.into(), Vec::new(), JSON_HIGHLIGHTS, "", "");
    LanguageRegistry::singleton().register("json", &json);
    for (name, quoted_names) in [("sql", true), ("mysql", false)] {
        let query = sql_highlights(quoted_names);
        let sql = LanguageConfig::new(name, tree_sitter_sequel::LANGUAGE.into(), Vec::new(), &query, "", "");
        LanguageRegistry::singleton().register(name, &sql);
    }
}

pub fn sql_language(driver: &DriverType) -> &'static str {
    if LexRules::for_driver(Some(driver)).double_quote_strings { "mysql" } else { "sql" }
}

#[cfg(test)]
mod tests {
    use barsql_core::DriverType;
    use gpui_kit::HighlightStyle;
    use gpui_kit::base::input::HighlightStyleResolver;
    use gpui_kit::component::Rope;
    use gpui_kit::component::highlighter::{SyntaxColors, SyntaxHighlighter};
    use serde_json::{Map, Value, json};

    use super::{SQL_KEYWORD_GROUPS, SQL_UNUSED, init, sql_highlights, sql_language};

    // Every syntax name in its own color, looked up as GPUI Kit does, so a run's color tells what it drew as.
    struct Named {
        names: Vec<String>,
        colors: SyntaxColors,
    }

    impl Named {
        fn new() -> Self {
            let Ok(Value::Object(names)) = serde_json::to_value(SyntaxColors::default()) else { unreachable!() };
            let names: Vec<String> = names.keys().cloned().collect();
            let colors: Map<String, Value> = names
                .iter()
                .enumerate()
                .map(|(ix, name)| (name.clone(), json!({ "color": format!("#{:06x}", (ix + 1) * 0x010101) })))
                .collect();
            Self { colors: serde_json::from_value(Value::Object(colors)).unwrap(), names }
        }

        fn name(&self, style: &HighlightStyle) -> &str {
            let name = self.names.iter().find(|name| self.colors.style(name).as_ref() == Some(style));
            name.map_or("plain", String::as_str)
        }
    }

    impl HighlightStyleResolver for Named {
        fn style(&self, name: &str) -> Option<HighlightStyle> {
            self.colors.style(name)
        }
    }

    fn assert_drawn<const N: usize>(language: &str, text: &str, expected: [(&str, &str); N]) {
        init();
        let named = Named::new();
        let mut highlighter = SyntaxHighlighter::new(language);
        highlighter.update(None, &Rope::from(text), None);
        let runs = highlighter.styles(&(0..text.len()), &named);
        let mut words = Vec::new();
        let mut start = None;
        for (ix, ch) in text.char_indices().chain([(text.len(), ' ')]) {
            match (start, ch.is_alphanumeric() || ch == '_') {
                (None, true) => start = Some(ix),
                (Some(from), false) => {
                    let mut names: Vec<&str> = runs
                        .iter()
                        .filter(|(range, _)| range.start < ix && from < range.end)
                        .map(|(_, style)| named.name(style))
                        .collect();
                    names.dedup();
                    words.push((&text[from..ix], names.join("+")));
                    start = None;
                }
                _ => {}
            }
        }
        // Every place a word appears, so one drawn two ways shows both.
        let actual = expected.map(|(word, _)| {
            let mut names: Vec<&str> =
                words.iter().filter(|(drawn, _)| *drawn == word).map(|(_, name)| name.as_str()).collect();
            names.sort_unstable();
            names.dedup();
            (word, names.join(" and "))
        });
        assert_eq!(actual, expected.map(|(word, name)| (word, name.to_string())), "{text}");
    }

    #[test]
    fn sql_keywords_draw_as_keywords() {
        let text = "SELECT CASE WHEN a THEN 1 END FROM t ORDER BY id DESC NULLS LAST;\n\
                    CREATE TEMP TABLE u (id INT UNIQUE NOT NULL);";
        assert_drawn(
            "sql",
            text,
            [
                ("SELECT", "keyword"),
                ("CASE", "keyword"),
                ("WHEN", "keyword"),
                ("THEN", "keyword"),
                ("DESC", "keyword"),
                ("NULLS", "keyword"),
                ("LAST", "keyword"),
                ("TEMP", "keyword"),
                ("UNIQUE", "keyword"),
                ("NULL", "number"),
                ("INT", "type"),
            ],
        );
        let query = sql_highlights(true);
        for edited in SQL_KEYWORD_GROUPS.iter().chain(&SQL_UNUSED) {
            assert_eq!(tree_sitter_sequel::HIGHLIGHTS_QUERY.matches(edited).count(), 1, "{edited}");
            assert!(!query.contains(edited), "{edited}");
        }
    }

    #[test]
    fn sql_literals_draw_as_numbers_or_strings() {
        let text = "SELECT 42, -7, 2.5, 0x1F, 1e3, 'abc', TRUE, false FROM t WHERE b IS NULL";
        assert_drawn(
            "sql",
            text,
            [
                ("42", "number"),
                ("7", "number"),
                ("2", "number"),
                ("0x1F", "number"),
                ("1e3", "number"),
                ("abc", "string"),
                ("TRUE", "number"),
                ("false", "number"),
                ("NULL", "number"),
            ],
        );
    }

    #[test]
    fn quoted_names_draw_like_bare_ones_except_on_mysql() {
        let text = r#"SELECT * FROM "Users" WHERE "email" IS NOT NULL AND email <> 'TEST'"#;
        assert_drawn("sql", text, [("Users", "type"), ("email", "plain"), ("TEST", "string"), ("NULL", "number")]);
        let text = r#"SELECT * FROM users WHERE email = "John" OR `email` = 'Jane'"#;
        assert_drawn("mysql", text, [("users", "type"), ("email", "plain"), ("John", "string"), ("Jane", "string")]);
        assert_eq!(sql_language(&DriverType::MySql), "mysql");
        assert_eq!(sql_language(&DriverType::Postgres), "sql");
        assert_eq!(sql_language(&DriverType::SqlServer), "sql");
    }

    #[test]
    fn table_aliases_draw_like_their_tables() {
        let text = r#"SELECT count(*) AS cnt FROM "Users" u JOIN orders o ON o.user_id = u.id WHERE u.email <> ''"#;
        assert_drawn(
            "sql",
            text,
            [("Users", "type"), ("u", "type"), ("orders", "type"), ("o", "type"), ("cnt", "variable")],
        );
    }
}
