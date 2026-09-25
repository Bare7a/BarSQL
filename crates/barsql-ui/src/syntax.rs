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

// Call before any editor exists. Highlighters keep the query they were built with.
pub fn init() {
    let json = LanguageConfig::new("json", tree_sitter_json::LANGUAGE.into(), Vec::new(), JSON_HIGHLIGHTS, "", "");
    LanguageRegistry::singleton().register("json", &json);
}
