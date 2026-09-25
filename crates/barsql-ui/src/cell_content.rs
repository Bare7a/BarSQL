use barsql_io::{is_space, restringify_json};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Json,
    Xml,
    Html,
    Text,
    Null,
    Empty,
}

// Null and Empty are only ever detected, never offered in the type menu.
pub const SELECTABLE: [Kind; 4] = [Kind::Text, Kind::Json, Kind::Xml, Kind::Html];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Beautify,
    Minify,
}

impl Kind {
    pub fn structured(self) -> bool {
        matches!(self, Self::Json | Self::Xml | Self::Html)
    }

    // Kind to tick in the type menu.
    pub fn selectable(self) -> Self {
        if self.structured() { self } else { Self::Text }
    }

    pub fn label_key(self) -> &'static str {
        match self {
            Self::Null => "cellViewer.kindNull",
            Self::Empty => "cellViewer.kindEmpty",
            Self::Json => "cellViewer.kindJson",
            Self::Xml => "cellViewer.kindXml",
            Self::Html => "cellViewer.kindHtml",
            Self::Text => "cellViewer.kindText",
        }
    }

    pub fn language(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Html => "html",
            // GPUI Kit has no XML grammar, but its HTML one colours XML tags, attributes and values too.
            Self::Xml => "html",
            _ => "text",
        }
    }
}

fn trim_whitespace(text: &str) -> &str {
    text.trim_matches(is_space)
}

fn starts_with_ignore_case(text: &str, prefix: &str) -> bool {
    text.len() >= prefix.len() && text.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
}

const HTML_TAGS: [&str; 14] =
    ["html", "head", "body", "div", "span", "p", "a", "script", "style", "meta", "link", "table", "form", "input"];

// Case-insensitive `</?(html|head|body|...|input)\b` anywhere in the text.
fn has_html_tag(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.iter().enumerate().filter(|(_, b)| **b == b'<').any(|(ix, _)| {
        let mut rest = &text[ix + 1..];
        if let Some(stripped) = rest.strip_prefix('/') {
            rest = stripped;
        }
        HTML_TAGS.iter().any(|tag| {
            starts_with_ignore_case(rest, tag)
                && !rest.as_bytes().get(tag.len()).is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
        })
    })
}

pub fn detect(raw: &str) -> Kind {
    let trimmed = trim_whitespace(raw);
    if trimmed.is_empty() {
        return Kind::Empty;
    }
    if (trimmed.starts_with('{') || trimmed.starts_with('['))
        && serde_json::from_str::<serde_json::Value>(trimmed).is_ok()
    {
        return Kind::Json;
    }
    let doctype = starts_with_ignore_case(trimmed, "<!doctype") && {
        let rest = &trimmed[9..];
        let after = rest.trim_start_matches(is_space);
        after.len() < rest.len() && starts_with_ignore_case(after, "html")
    };
    let html_open = starts_with_ignore_case(trimmed, "<html")
        && trimmed[5..].chars().next().is_some_and(|c| c == '>' || is_space(c));
    if doctype || html_open {
        return Kind::Html;
    }
    if trimmed.starts_with("<?xml") {
        return Kind::Xml;
    }
    if trimmed.starts_with('<') && trimmed.contains('>') {
        return if has_html_tag(trimmed) { Kind::Html } else { Kind::Xml };
    }
    Kind::Text
}

// Reformatting reads numbers as f64, so a 16+ digit integer like a bigint id would lose digits.
fn unsafe_int(text: &str) -> bool {
    text.as_bytes().split(|b| !b.is_ascii_digit()).any(|run| run.len() >= 16)
}

pub fn beautify_json(raw: &str) -> Option<String> {
    let trimmed = trim_whitespace(raw);
    if unsafe_int(trimmed) {
        return Some(trimmed.to_string());
    }
    restringify_json(trimmed, true)
}

pub fn minify_json(raw: &str) -> Option<String> {
    let trimmed = trim_whitespace(raw);
    if unsafe_int(trimmed) {
        return Some(trimmed.to_string());
    }
    restringify_json(trimmed, false)
}

// Beautify turns `>\s*<` into `>\n<`. Minify turns `>\s+<` into `><`.
fn join_tags(markup: &str, separator: &str, require_space: bool) -> String {
    let mut out = String::with_capacity(markup.len());
    let mut rest = markup;
    while let Some(ix) = rest.find('>') {
        out.push_str(&rest[..=ix]);
        rest = &rest[ix + 1..];
        let after = rest.trim_start_matches(is_space);
        if after.starts_with('<') && (!require_space || after.len() < rest.len()) {
            out.push_str(separator);
            out.push('<');
            rest = &after[1..];
        }
    }
    out.push_str(rest);
    out
}

// Same as `^<[^!?/][^>]*[^/]>$`, a line holding a single opening tag.
fn opens_block(line: &str) -> bool {
    let chars: Vec<char> = line.chars().collect();
    let n = chars.len();
    n >= 4
        && chars[0] == '<'
        && chars[n - 1] == '>'
        && !matches!(chars[1], '!' | '?' | '/')
        && chars[n - 2] != '/'
        && !chars[2..n - 2].contains(&'>')
}

pub fn format_markup(markup: &str) -> String {
    let broken = join_tags(markup, "\n", false);
    let mut depth = 0usize;
    broken
        .split('\n')
        .map(trim_whitespace)
        .filter(|line| !line.is_empty())
        .map(|line| {
            if line.starts_with("</") {
                depth = depth.saturating_sub(1);
            }
            let indented = format!("{}{line}", "  ".repeat(depth));
            if opens_block(line) {
                depth += 1;
            }
            indented
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// Only whitespace between tags goes. Inside a text node it's content.
pub fn minify_markup(markup: &str) -> String {
    join_tags(trim_whitespace(markup), "", true)
}

pub fn initial(raw: &str, null: bool) -> (String, Kind) {
    if null {
        return ("NULL".into(), Kind::Null);
    }
    match detect(raw) {
        Kind::Json => match beautify_json(raw) {
            Some(text) => (text, Kind::Json),
            None => (raw.to_string(), Kind::Text),
        },
        kind @ (Kind::Xml | Kind::Html) => (format_markup(trim_whitespace(raw)), kind),
        kind => (raw.to_string(), kind),
    }
}

// Unstructured kinds are re-detected from the content. None when it doesn't parse as a structured kind.
pub fn apply(content: &str, kind: Kind, mode: Mode) -> Option<(String, Kind)> {
    let kind = if kind.structured() { kind } else { detect(content) };
    let text = match (kind, mode) {
        (Kind::Json, Mode::Beautify) => beautify_json(content)?,
        (Kind::Json, Mode::Minify) => minify_json(content)?,
        (Kind::Xml | Kind::Html, Mode::Beautify) => format_markup(content),
        (Kind::Xml | Kind::Html, Mode::Minify) => minify_markup(content),
        _ => return None,
    };
    Some((text, kind))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minify_markup_keeps_text_node_whitespace() {
        assert_eq!(minify_markup("<a>  <b>x</b>\n  </a>"), "<a><b>x</b></a>");
        assert_eq!(minify_markup("<p>line1\n  line2</p>"), "<p>line1\n  line2</p>");
        assert_eq!(minify_markup("<a>\n  <b>x</b>\n</a>"), "<a><b>x</b></a>");
    }

    #[test]
    fn kinds_are_detected() {
        let cases = [
            ("", Kind::Empty),
            ("   \n\t", Kind::Empty),
            ("{\"a\":1}", Kind::Json),
            ("[1,2,3]", Kind::Json),
            ("{ not valid json", Kind::Text),
            ("<?xml version=\"1.0\"?><root/>", Kind::Xml),
            ("<!DOCTYPE html><html></html>", Kind::Html),
            ("<html><body/></html>", Kind::Html),
            ("<note><to>X</to></note>", Kind::Xml),
            ("<a><b>x</b></a>", Kind::Html),
            ("plain words", Kind::Text),
            ("<anchor>x</anchor>", Kind::Xml),
            ("<!doctype  HTML>", Kind::Html),
        ];
        for (input, kind) in cases {
            assert_eq!(detect(input), kind, "{input:?}");
        }
    }

    #[test]
    fn kinds_map_to_languages_labels_and_menu_entries() {
        assert!(Kind::Json.structured() && Kind::Xml.structured() && Kind::Html.structured());
        assert!(!Kind::Text.structured() && !Kind::Null.structured() && !Kind::Empty.structured());
        assert_eq!([Kind::Json, Kind::Html, Kind::Null].map(Kind::language), ["json", "html", "text"]);
        assert_eq!([Kind::Json, Kind::Empty, Kind::Null].map(Kind::selectable), [Kind::Json, Kind::Text, Kind::Text]);
        assert_eq!(Kind::Empty.label_key(), "cellViewer.kindEmpty");
        assert_eq!(SELECTABLE, [Kind::Text, Kind::Json, Kind::Xml, Kind::Html]);
    }

    #[test]
    fn json_beautifies_and_minifies() {
        assert_eq!(beautify_json("{\"a\":1,\"b\":2}").unwrap(), "{\n  \"a\": 1,\n  \"b\": 2\n}");
        assert_eq!(beautify_json(&minify_json("{\"a\": 1}").unwrap()).unwrap(), "{\n  \"a\": 1\n}");
        assert_eq!(beautify_json("not json"), None);
        assert_eq!(beautify_json(" {\"id\": 12345678901234567890} ").unwrap(), "{\"id\": 12345678901234567890}");
    }

    #[test]
    fn markup_breaks_and_indents_tags() {
        assert_eq!(format_markup("<root><child>x</child></root>"), "<root>\n  <child>x</child>\n</root>");
        let out = format_markup("<root><br/><br/></root>");
        assert!(out.starts_with("<root>") && out.contains("  <br/>"));
    }

    #[test]
    fn initial_content_formats_by_kind() {
        assert_eq!(initial("", true), ("NULL".into(), Kind::Null));
        assert_eq!(initial("{\"a\":1}", false), ("{\n  \"a\": 1\n}".into(), Kind::Json));
        assert_eq!(initial("[1,]", false).1, Kind::Text);
        let (text, kind) = initial("<note><to>X</to></note>", false);
        assert_eq!(kind, Kind::Xml);
        assert!(text.lines().count() > 1);
        assert_eq!(initial("hello world", false), ("hello world".into(), Kind::Text));
    }

    #[test]
    fn formats_apply_by_kind_or_detection() {
        assert!(apply("{\"a\":1}", Kind::Json, Mode::Beautify).unwrap().0.contains('\n'));
        assert_eq!(apply("{\n  \"a\": 1\n}", Kind::Json, Mode::Minify).unwrap().0, "{\"a\":1}");
        assert_eq!(apply("{\"a\":1}", Kind::Text, Mode::Beautify).unwrap().1, Kind::Json);
        assert_eq!(apply("just text", Kind::Text, Mode::Beautify), None);
    }
}
