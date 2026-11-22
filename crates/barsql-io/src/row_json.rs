use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use barsql_core::SqlDialect;
use barsql_db::{Cell, ResultSet};
use barsql_sql::{TypeFamily, type_family};
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

// A table view's unsaved edits by (row, column). None is a staged NULL.
pub type StagedCells = HashMap<(usize, usize), Option<String>>;

// A staged value wins over the loaded one.
pub fn shown_cell<'a>(
    set: &'a ResultSet,
    staged: Option<&'a StagedCells>,
    row: usize,
    column: usize,
    dialect: Option<SqlDialect>,
) -> Cell<'a> {
    let loaded = set.cell(row, column);
    let Some(value) = staged.and_then(|staged| staged.get(&(row, column))) else { return loaded };
    match value.as_deref() {
        Some(text) => typed(loaded, text, &set.columns[column].type_name, dialect),
        None => Cell::Null,
    }
}

// An edit takes its cell's kind where it spells one, or over a NULL its column type's, so a number stays a number
// and a boolean a boolean. Numbers are JSON's, as JSON copies them bare.
pub fn typed<'a>(loaded: Cell<'_>, text: &'a str, type_name: &str, dialect: Option<SqlDialect>) -> Cell<'a> {
    let family = match loaded {
        Cell::Null => type_family(dialect, type_name),
        Cell::Number(_) => TypeFamily::Number,
        Cell::Bool(_) => TypeFamily::Boolean,
        Cell::Text(_) => TypeFamily::Text,
    };
    let as_flag = |type_name: &str| flag(Cell::Text(text), type_name).map_or(Cell::Text(text), Cell::Bool);
    match family {
        TypeFamily::Number if serde_json::from_str::<serde_json::Number>(text).is_ok() => Cell::Number(text.trim()),
        TypeFamily::Boolean => as_flag("boolean"),
        _ => as_flag(type_name),
    }
}

// The engine's own booleans, plus 1/0 and true/false in a column declared boolean, as SQLite and staged edits hold them.
pub fn flag(cell: Cell, type_name: &str) -> Option<bool> {
    let boolean = || ["boolean", "bool"].iter().any(|name| type_name.trim().eq_ignore_ascii_case(name));
    let spelled = |text: &str, words: [&str; 3]| words.iter().any(|word| text.eq_ignore_ascii_case(word));
    match cell {
        Cell::Bool(on) => Some(on),
        Cell::Number(text) | Cell::Text(text) if boolean() => {
            if spelled(text, ["1", "true", "t"]) {
                Some(true)
            } else {
                spelled(text, ["0", "false", "f"]).then_some(false)
            }
        }
        _ => None,
    }
}

// Visible columns in display order. A repeated name keeps its first position but its last value.
fn row_object(
    set: &ResultSet,
    staged: Option<&StagedCells>,
    row: usize,
    columns: &[usize],
    hide_nulls: bool,
    dialect: Option<SqlDialect>,
) -> JsonValue {
    let mut object = JsonObject::default();
    for &column in columns {
        let cell = shown_cell(set, staged, row, column, dialect);
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
pub fn row_json(
    set: &ResultSet,
    staged: Option<&StagedCells>,
    row: usize,
    columns: &[usize],
    query: &str,
    hide_nulls: bool,
    dialect: Option<SqlDialect>,
) -> Option<String> {
    let value = row_object(set, staged, row, columns, hide_nulls, dialect);
    let matcher = Matcher::new(query);
    let shown = if matches!(matcher, Matcher::All) { Some(value) } else { filter(&value, &matcher) }?;
    let mut out = String::new();
    shown.write(&mut out, "");
    Some(out)
}

// `value` is None for NULL; JSON, arrays and maps come pretty-printed. Shared, as each frame draws it.
#[derive(Debug, Clone, PartialEq)]
pub struct RowField {
    pub column: usize,
    pub value: Option<Arc<str>>,
    pub json: bool,
}

// Filters as `row_json` does, but a nested match keeps its whole value.
pub fn row_fields(
    set: &ResultSet,
    staged: Option<&StagedCells>,
    row: usize,
    columns: &[usize],
    query: &str,
    hide_nulls: bool,
    dialect: Option<SqlDialect>,
) -> Vec<RowField> {
    let matcher = Matcher::new(query);
    let mut fields = Vec::new();
    for &column in columns {
        let cell = shown_cell(set, staged, row, column, dialect);
        if hide_nulls && matches!(cell, Cell::Null) {
            continue;
        }
        let meta = &set.columns[column];
        let value = parse_value(cell, &meta.type_name);
        let hit = matcher.matches(&meta.name)
            || match &value {
                JsonValue::Object(_) => filter(&value, &matcher).is_some(),
                _ => matcher.matches(&match_text(&value)),
            };
        if !hit {
            continue;
        }
        let json = matches!(value, JsonValue::Object(_) | JsonValue::Array(_));
        let shown = match json {
            true => {
                let mut out = String::new();
                value.write(&mut out, "");
                Some(out.into())
            }
            false => cell.display().map(Into::into),
        };
        fields.push(RowField { column, value: shown, json });
    }
    fields
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
            .map(|(name, type_name)| ColumnMeta {
                name: name.to_string(),
                type_name: type_name.to_string(),
                ..Default::default()
            })
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
        row_json(set, None, 0, columns, query, hide_nulls, None).map(|text| serde_json::from_str(&text).unwrap())
    }

    #[test]
    fn staged_edits_show_over_the_loaded_values() {
        let row = set(&["id", "name", "note"], &[Some("7"), Some("ann"), Some("hi")]);
        let typed = StagedCells::from([((0, 0), Some("8".to_string()))]);
        assert_eq!(
            row_json(&row, Some(&typed), 0, &[0], "", false, None).unwrap(),
            "{\n  \"id\": 8\n}",
            "a number stays one"
        );
        let staged = StagedCells::from([((0, 1), Some("bob".to_string())), ((0, 2), None)]);
        let fields = row_fields(&row, Some(&staged), 0, &[1, 2], "", false, None);
        let values: Vec<_> = fields.iter().map(|f| f.value.as_deref()).collect();
        assert_eq!(values, [Some("bob"), None]);
        let json = row_json(&row, Some(&staged), 0, &[0, 1], "", false, None).unwrap();
        assert_eq!(json, "{\n  \"id\": 7,\n  \"name\": \"bob\"\n}");
    }

    #[test]
    fn an_edit_over_a_null_takes_its_column_types_kind() {
        let row = typed_set(&[("qty", "int"), ("paid", "bit"), ("note", "nvarchar")], &[None, None, None]);
        let staged = StagedCells::from([
            ((0, 0), Some("8".to_string())),
            ((0, 1), Some("1".to_string())),
            ((0, 2), Some("8".to_string())),
        ]);
        let shown = |column| shown_cell(&row, Some(&staged), 0, column, Some(SqlDialect::TSql));
        assert_eq!((shown(0), shown(1), shown(2)), (Cell::Number("8"), Cell::Bool(true), Cell::Text("8")));
        assert_eq!(typed(Cell::Null, ".5", "int", None), Cell::Text(".5"), "JSON's numbers only");
    }

    #[test]
    fn fields_keep_display_text_and_pretty_json() {
        let columns = [("id", "INT"), ("name", "TEXT"), ("meta", "JSON"), ("tags", "_TEXT"), ("gone", "TEXT")];
        let row = typed_set(&columns, &[Some("7"), Some("ann"), Some("{\"a\":1}"), Some("{x,y}"), None]);
        let fields = row_fields(&row, None, 0, &[1, 0, 2, 3, 4], "", false, None);
        let shown: Vec<_> =
            fields.iter().map(|f| (row.columns[f.column].name.as_str(), f.value.as_deref(), f.json)).collect();
        assert_eq!(
            shown,
            [
                ("name", Some("ann"), false),
                ("id", Some("7"), false),
                ("meta", Some("{\n  \"a\": 1\n}"), true),
                ("tags", Some("[\n  \"x\",\n  \"y\"\n]"), true),
                ("gone", None, false),
            ]
        );
        assert_eq!(fields[2].column, 2);
        assert_eq!(row_fields(&row, None, 0, &[4, 1], "", true, None).len(), 1);
    }

    #[test]
    fn fields_filter_by_name_or_value_like_the_json() {
        let columns = [("id", "INT"), ("email", "TEXT"), ("meta", "JSON")];
        let row = typed_set(&columns, &[Some("7"), Some("ann@example.com"), Some("{\"plan\":\"pro\"}")]);
        let names = |query: &str| -> Vec<String> {
            let fields = row_fields(&row, None, 0, &[0, 1, 2], query, false, None);
            fields.into_iter().map(|f| row.columns[f.column].name.clone()).collect()
        };
        assert_eq!(names("EXAMPLE"), ["email"]);
        assert_eq!(names("pro"), ["meta"]);
        assert_eq!(names("id"), ["id"]);
        assert_eq!(names("/^e/"), ["email"]);
        assert!(names("nothing").is_empty());
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
        assert_eq!(
            row_json(&people, None, 0, &[2, 0], "", false, None).unwrap(),
            "{\n  \"note\": \"hi\",\n  \"id\": 1\n}"
        );
        let nulls = set(&["id", "name", "note"], &[Some("1"), None, Some("")]);
        assert_eq!(object(&nulls, &[1], ""), Some(serde_json::json!({ "name": null })));
        let payload = set(&["payload"], &[Some("{\"a\":1,\"b\":[2,3]}")]);
        assert_eq!(object(&payload, &[0], ""), Some(serde_json::json!({ "payload": { "a": 1, "b": [2, 3] } })));
        let broken = set(&["payload"], &[Some("{not valid json}")]);
        assert_eq!(object(&broken, &[0], ""), Some(serde_json::json!({ "payload": "{not valid json}" })));
        let numbered = set(&["b", "10", "2"], &[Some("x"), Some("y"), Some("z")]);
        assert_eq!(
            row_json(&numbered, None, 0, &[0, 1, 2], "", false, None).unwrap(),
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
