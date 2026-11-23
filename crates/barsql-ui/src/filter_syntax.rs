use std::collections::HashSet;
use std::ops::Range;
use std::rc::Rc;

use barsql_core::DriverType;
use barsql_sql::lang::suggestions::filter_keywords;
use barsql_sql::lang::{TokenKind, tokenize};
use gpui_kit::base::input::{
    EditorState, FoldRange, HighlightStyleResolver, InputEdit, InputHighlighter, InputHighlighterFactory,
};
use gpui_kit::component::Rope;
use gpui_kit::{Context, HighlightStyle, SharedString, Window};

// Words a WHERE expression can hold besides the dialect's operators, as in a CASE or a subquery.
const EXPRESSION_WORDS: &[&str] = &[
    "case", "when", "then", "else", "end", "escape", "select", "from", "where", "as", "any", "all", "some", "cast",
    "interval", "collate", "group", "by", "order", "having", "limit", "offset", "join", "on", "union", "asc", "desc",
];

// The theme's syntax names for a filter's tokens. A filter is a bare expression, which the SQL grammar can't
// parse, so the lexer colors it. NULL takes the string color, as the grammar gives the editor's literals.
pub fn spans(text: &str, driver: &DriverType) -> Vec<(Range<usize>, &'static str)> {
    let words: HashSet<String> = filter_keywords(Some(driver))
        .into_iter()
        .flat_map(|keyword| keyword.split(' '))
        .map(str::to_lowercase)
        .chain(EXPRESSION_WORDS.iter().map(|word| word.to_string()))
        .collect();
    let tokens = tokenize(text, Some(driver));
    let calls = |ix: usize| tokens.get(ix + 1).is_some_and(|next| next.text == "(");
    let named = tokens.iter().enumerate().filter_map(|(ix, token)| {
        let name = match token.kind {
            TokenKind::Ident if token.lower == "null" => "string",
            TokenKind::Ident if token.lower == "true" || token.lower == "false" => "boolean",
            TokenKind::Ident if words.contains(&token.lower) => "keyword",
            TokenKind::Ident if calls(ix) => "function",
            TokenKind::String => "string",
            TokenKind::Number => "number",
            TokenKind::Comment => "comment",
            TokenKind::Op => "operator",
            TokenKind::Punct if matches!(token.text, "(" | ")") => "punctuation.bracket",
            TokenKind::Punct => "punctuation.delimiter",
            TokenKind::Ident | TokenKind::Quoted => return None,
        };
        Some((token.start..token.end, name))
    });
    named.collect()
}

struct FilterHighlighter {
    driver: DriverType,
    spans: Vec<(Range<usize>, &'static str)>,
}

impl InputHighlighter for FilterHighlighter {
    fn language(&self) -> SharedString {
        "sql".into()
    }

    fn update(&mut self, _: Option<InputEdit>, text: &Rope, _: bool, _: &mut Window, _: &mut Context<EditorState>) {
        self.spans = spans(&text.to_string(), &self.driver);
    }

    fn styles(
        &self,
        range: &Range<usize>,
        resolver: &dyn HighlightStyleResolver,
    ) -> Vec<(Range<usize>, HighlightStyle)> {
        let mut runs = Vec::new();
        let mut at = range.start;
        for (span, name) in &self.spans {
            let (start, end) = (span.start.max(range.start), span.end.min(range.end));
            if start >= end {
                continue;
            }
            if at < start {
                runs.push((at..start, HighlightStyle::default()));
            }
            runs.push((start..end, resolver.style(name).unwrap_or_default()));
            at = end;
        }
        if at < range.end {
            runs.push((at..range.end, HighlightStyle::default()));
        }
        runs
    }

    fn fold_ranges(&self, _: &Rope) -> Vec<FoldRange> {
        Vec::new()
    }
}

pub fn factory(driver: DriverType) -> InputHighlighterFactory {
    Rc::new(move |_| Some(Box::new(FilterHighlighter { driver: driver.clone(), spans: Vec::new() })))
}

#[cfg(test)]
mod tests {
    use barsql_core::DriverType;
    use gpui_kit::HighlightStyle;
    use gpui_kit::base::input::{HighlightStyleResolver, InputHighlighter as _};

    use super::{FilterHighlighter, spans};

    fn named(text: &str, driver: DriverType) -> Vec<(&str, &'static str)> {
        spans(text, &driver).into_iter().map(|(range, name)| (&text[range], name)).collect()
    }

    #[test]
    fn filter_words_get_the_editors_colors() {
        let text = "name ILIKE 'a%' AND id > 5 OR lower(x) IS NULL AND done = TRUE";
        let expected = [
            ("ILIKE", "keyword"),
            ("'a%'", "string"),
            ("AND", "keyword"),
            (">", "operator"),
            ("5", "number"),
            ("OR", "keyword"),
            ("lower", "function"),
            ("(", "punctuation.bracket"),
            (")", "punctuation.bracket"),
            ("IS", "keyword"),
            ("NULL", "string"),
            ("AND", "keyword"),
            ("=", "operator"),
            ("TRUE", "boolean"),
        ];
        assert_eq!(named(text, DriverType::Postgres), expected);
    }

    #[test]
    fn ilike_is_plain_text_on_mysql() {
        assert_eq!(named("name ILIKE 'a%'", DriverType::MySql), [("'a%'", "string")]);
    }

    struct Painted;

    impl HighlightStyleResolver for Painted {
        fn style(&self, _: &str) -> Option<HighlightStyle> {
            Some(HighlightStyle { color: Some(gpui_kit::red()), ..Default::default() })
        }
    }

    #[test]
    fn runs_cover_the_whole_range() {
        let highlighter = FilterHighlighter { driver: DriverType::Sqlite, spans: spans("a = 1", &DriverType::Sqlite) };
        let runs = highlighter.styles(&(0..5), &Painted);
        let painted: Vec<_> = runs.iter().map(|(range, style)| (range.clone(), style.color.is_some())).collect();
        assert_eq!(painted, [(0..2, false), (2..3, true), (3..4, false), (4..5, true)]);
    }
}
