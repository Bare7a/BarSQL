use std::ops::Range;

use barsql_db::{Cell, ResultSet};
use regex::{Regex, RegexBuilder};
use serde_json::Value as Json;

use crate::array_literal::array_value;
use crate::export::{JsonObject, JsonValue, is_space};

// An array, map or tuple shows as its JSON. Other text starting with { or [ that parses as JSON shows as that
// value.
fn parse_value(cell: Cell<'_>, type_name: &str) -> JsonValue {
    if let Cell::Text(text) = cell {
        if let Some(value) = array_value(text, type_name) {
            return value;
        }
        let trimmed = text.trim_matches(is_space);
        if (trimmed.starts_with('{') || trimmed.starts_with('['))
            && let Ok(json) = serde_json::from_str::<Json>(trimmed)
        {
            return JsonValue::from_json(json);
        }
    }
    JsonValue::from_cell(cell)
}

// Visible columns in display order. A repeated name keeps its first position but its last value.
fn row_object(set: &ResultSet, row: usize, columns: &[usize], hide_nulls: bool) -> JsonValue {
    let mut object = JsonObject::default();
    for &column in columns {
        let cell = set.cell(row, column);
        if hide_nulls && matches!(cell, Cell::Null) {
            continue;
        }
        let meta = &set.columns[column];
        object.insert(meta.name.clone(), parse_value(cell, &meta.type_name));
    }
    JsonValue::Object(object)
}

// A `/pattern/` query is a case-insensitive regex. If it doesn't compile it matches as plain text.
enum Matcher {
    All,
    Regex(Regex),
    Text(String),
}

impl Matcher {
    fn new(query: &str) -> Self {
        let query = query.trim_matches(is_space);
        if query.is_empty() {
            return Self::All;
        }
        if let Some(end) = query.rfind('/').filter(|&end| end > 0 && query.starts_with('/'))
            && let Ok(regex) = RegexBuilder::new(&query[1..end]).case_insensitive(true).build()
        {
            return Self::Regex(regex);
        }
        Self::Text(query.to_lowercase())
    }

    fn matches(&self, text: &str) -> bool {
        match self {
            Self::All => true,
            Self::Regex(regex) => regex.is_match(text),
            Self::Text(query) => text.to_lowercase().contains(query),
        }
    }
}

// Arrays join their items with commas, nulls inside them are empty, and objects read as "[object Object]".
fn match_text(value: &JsonValue) -> String {
    match value {
        JsonValue::Null => "null".into(),
        JsonValue::Bool(b) => b.to_string(),
        JsonValue::Number(f) => ryu_js::Buffer::new().format(*f).to_string(),
        JsonValue::NumberText(text) | JsonValue::String(text) => text.clone(),
        JsonValue::Array(items) => items
            .iter()
            .map(|item| if matches!(item, JsonValue::Null) { String::new() } else { match_text(item) })
            .collect::<Vec<_>>()
            .join(","),
        JsonValue::Object(_) => "[object Object]".into(),
    }
}

// Keeps entries whose key or value matches. Nested objects are filtered the same way.
fn filter(value: &JsonValue, matcher: &Matcher) -> Option<JsonValue> {
    let JsonValue::Object(object) = value else {
        let mut text = String::new();
        value.write_compact(&mut text);
        return matcher.matches(&text).then(|| value.clone());
    };
    let mut out = JsonObject::default();
    for (key, value) in object.ordered() {
        let key_hit = matcher.matches(key);
        if matches!(value, JsonValue::Object(_)) {
            match filter(value, matcher) {
                Some(nested) => out.insert(key.clone(), nested),
                None if key_hit => out.insert(key.clone(), value.clone()),
                None => {}
            }
        } else if key_hit || matcher.matches(&match_text(value)) {
            out.insert(key.clone(), value.clone());
        }
    }
    (!out.is_empty()).then_some(JsonValue::Object(out))
}

// Pretty-printed. None when nothing matches `query`.
pub fn row_json(set: &ResultSet, row: usize, columns: &[usize], query: &str, hide_nulls: bool) -> Option<String> {
    let value = row_object(set, row, columns, hide_nulls);
    let matcher = Matcher::new(query);
    let shown = if matches!(matcher, Matcher::All) { Some(value) } else { filter(&value, &matcher) }?;
    let mut out = String::new();
    shown.write(&mut out, "");
    Some(out)
}

// Caps what a broad filter like /./ marks.
const MAX_MARKS: usize = 10_000;

// Byte ranges of each key and value in pretty-printed JSON. Strings exclude their quotes and stay escaped.
fn tokens(text: &str) -> Vec<Range<usize>> {
    let bytes = text.as_bytes();
    let bare = |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'+' | b'.');
    let mut out = Vec::new();
    let mut ix = 0;
    while ix < bytes.len() {
        if bytes[ix] == b'"' {
            let start = ix + 1;
            let mut end = start;
            while end < bytes.len() && bytes[end] != b'"' {
                end += if bytes[end] == b'\\' { 2 } else { 1 };
            }
            let end = end.min(bytes.len());
            out.push(start..end);
            ix = end + 1;
        } else if bare(bytes[ix]) {
            let start = ix;
            while ix < bytes.len() && bare(bytes[ix]) {
                ix += 1;
            }
            out.push(start..ix);
        } else {
            ix += 1;
        }
    }
    out
}

// Where `query` matches in `row_json`'s output, each key and value matched on its own as the filter does.
pub fn match_ranges(text: &str, query: &str) -> Vec<Range<usize>> {
    let regex = match Matcher::new(query) {
        Matcher::All => return Vec::new(),
        Matcher::Regex(regex) => regex,
        Matcher::Text(query) => match RegexBuilder::new(&regex::escape(&query)).case_insensitive(true).build() {
            Ok(regex) => regex,
            Err(_) => return Vec::new(),
        },
    };
    let mut marks = Vec::new();
    for token in tokens(text) {
        let Some(slice) = text.get(token.clone()) else { continue };
        let found = regex.find_iter(slice).filter(|found| !found.is_empty());
        marks.extend(found.map(|found| token.start + found.start()..token.start + found.end()));
        if marks.len() >= MAX_MARKS {
            marks.truncate(MAX_MARKS);
            break;
        }
    }
    marks
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use barsql_db::{ChunkBuilder, ColumnMeta};

    use super::*;

    fn set(columns: &[&str], cells: &[Option<&str>]) -> ResultSet {
        let typed: Vec<(&str, &str)> = columns.iter().map(|name| (*name, "TEXT")).collect();
        typed_set(&typed, cells)
    }

    fn typed_set(columns: &[(&str, &str)], cells: &[Option<&str>]) -> ResultSet {
        let meta: Arc<[ColumnMeta]> = columns
            .iter()
            .map(|(name, type_name)| ColumnMeta { name: name.to_string(), type_name: type_name.to_string() })
            .collect();
        let mut builder = ChunkBuilder::new(columns.len(), 1);
        for cell in cells {
            match cell {
                Some(text) if text.parse::<f64>().is_ok() => builder.push_number(|s| s.push_str(text)),
                Some(text) => builder.push_text(|s| s.push_str(text)),
                None => builder.push_null(),
            }
        }
        builder.end_row();
        let mut set = ResultSet::new(meta);
        set.push(Arc::new(builder.finish()));
        set
    }

    fn object(set: &ResultSet, columns: &[usize], query: &str) -> Option<Json> {
        shown(set, columns, query, false)
    }

    fn shown(set: &ResultSet, columns: &[usize], query: &str, hide_nulls: bool) -> Option<Json> {
        row_json(set, 0, columns, query, hide_nulls).map(|text| serde_json::from_str(&text).unwrap())
    }

    #[test]
    fn arrays_maps_and_tuples_show_as_json_by_their_type() {
        let columns = [("tags", "_TEXT"), ("ids", "_INT8"), ("attrs", "Map(String, UInt8)"), ("note", "TEXT")];
        let row = typed_set(&columns, &[Some("{a,\"b c\"}"), Some("{1,NULL}"), Some("{'k':1}"), Some("{x,y}")]);
        assert_eq!(
            object(&row, &[0, 1, 2, 3], ""),
            Some(serde_json::json!({ "tags": ["a", "b c"], "ids": [1, null], "attrs": { "k": 1 }, "note": "{x,y}" }))
        );
    }

    #[test]
    fn rows_become_objects_of_their_visible_columns() {
        let people = set(&["id", "name", "note"], &[Some("1"), Some("alice"), Some("hi")]);
        assert_eq!(row_json(&people, 0, &[2, 0], "", false).unwrap(), "{\n  \"note\": \"hi\",\n  \"id\": 1\n}");
        let nulls = set(&["id", "name", "note"], &[Some("1"), None, Some("")]);
        assert_eq!(object(&nulls, &[1], ""), Some(serde_json::json!({ "name": null })));
        let payload = set(&["payload"], &[Some("{\"a\":1,\"b\":[2,3]}")]);
        assert_eq!(object(&payload, &[0], ""), Some(serde_json::json!({ "payload": { "a": 1, "b": [2, 3] } })));
        let broken = set(&["payload"], &[Some("{not valid json}")]);
        assert_eq!(object(&broken, &[0], ""), Some(serde_json::json!({ "payload": "{not valid json}" })));
        let numbered = set(&["b", "10", "2"], &[Some("x"), Some("y"), Some("z")]);
        assert_eq!(
            row_json(&numbered, 0, &[0, 1, 2], "", false).unwrap(),
            "{\n  \"2\": \"z\",\n  \"10\": \"y\",\n  \"b\": \"x\"\n}"
        );
    }

    #[test]
    fn filters_keep_matching_keys_values_and_nested_entries() {
        let row = set(&["name", "other"], &[Some("x"), Some("y")]);
        assert_eq!(object(&row, &[0, 1], "   "), Some(serde_json::json!({ "name": "x", "other": "y" })));
        assert_eq!(object(&row, &[0, 1], "name"), Some(serde_json::json!({ "name": "x" })));
        let person = set(&["name", "age"], &[Some("alice"), Some("30")]);
        assert_eq!(object(&person, &[0, 1], "lic"), Some(serde_json::json!({ "name": "alice" })));
        let nested = set(&["outer", "sibling"], &[Some("{\"inner\":\"hit\",\"other\":\"no\"}"), Some("no")]);
        assert_eq!(object(&nested, &[0, 1], "hit"), Some(serde_json::json!({ "outer": { "inner": "hit" } })));
        let one = set(&["a"], &[Some("1")]);
        assert_eq!(object(&one, &[0], "zzz"), None);
        let shout = set(&["name"], &[Some("ALICE")]);
        assert_eq!(object(&shout, &[0], "/alice/"), Some(serde_json::json!({ "name": "ALICE" })));
        let odd = set(&["/[/", "other"], &[Some("x"), Some("y")]);
        assert_eq!(object(&odd, &[0, 1], "/[/"), Some(serde_json::json!({ "/[/": "x" })));
    }

    #[test]
    fn values_match_as_joined_text() {
        let row = set(&["tags", "flag"], &[Some("[1,null,[2,3]]"), Some("{\"deep\":true}")]);
        assert_eq!(object(&row, &[0], "1,,2,3"), Some(serde_json::json!({ "tags": [1, null, [2, 3]] })));
        assert_eq!(object(&row, &[1], "true"), Some(serde_json::json!({ "flag": { "deep": true } })));
    }

    // Only NULL columns go, before filtering. A null inside a JSON value stays.
    #[test]
    fn hidden_nulls_leave_their_columns_out() {
        let row = set(&["id", "name", "note", "doc"], &[Some("1"), None, Some("hi"), Some("{\"gone\":null}")]);
        let all = [0, 1, 2, 3];
        assert_eq!(
            shown(&row, &all, "", true),
            Some(serde_json::json!({ "id": 1, "note": "hi", "doc": { "gone": null } }))
        );
        assert_eq!(shown(&row, &all, "name", true), None, "the only match was a NULL column");
        assert_eq!(shown(&row, &all, "name", false), Some(serde_json::json!({ "name": null })));
    }

    #[test]
    fn matches_are_marked_inside_each_key_and_value() {
        let text = "{\n  \"name\": \"Alice\",\n  \"nick\": \"ali\",\n  \"id\": 7,\n  \"call\": \"f(x)\",\n  \"quote\": \"say \\\"hi\\\"\"\n}";
        let found = |query: &str| match_ranges(text, query).into_iter().map(|range| &text[range]).collect::<Vec<_>>();
        assert_eq!(found("ALI"), ["Ali", "ali"]);
        assert_eq!(found("/^n/"), ["n", "n"]);
        assert_eq!(found("7"), ["7"]);
        assert_eq!(found("(x"), ["(x"]);
        assert_eq!(found("hi"), ["hi"], "an escaped quote doesn't end its string");
        assert!(found("").is_empty());
        assert!(found("/z*/").is_empty(), "an empty match marks nothing");
    }
}
