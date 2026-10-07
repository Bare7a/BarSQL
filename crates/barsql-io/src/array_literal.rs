// Array, map and tuple cells as JSON. Postgres sends arrays in its `{…}` text form, and ClickHouse sends its
// compound values quoted, as `[…]`, `{key:value}` and `(…)`.

use serde_json::Value as Json;

use crate::export::{JsonObject, JsonValue, is_space};

// Deeper input is left as text. Postgres stops at 6 dimensions anyway.
const MAX_DEPTH: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArrayKind {
    Array,
    Map,
    Tuple,
}

// How a column's elements read, from its type name.
#[derive(Debug, Clone, PartialEq)]
enum Shape {
    Postgres { element: Scalar, delimiter: u8 },
    ClickHouse(ChType),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Scalar {
    Number,
    Bool,
    Json,
    Text,
}

#[derive(Debug, Clone, PartialEq)]
enum ChType {
    Array(Box<ChType>),
    Map(Box<ChType>, Box<ChType>),
    // Named fields read as an object.
    Tuple(Vec<(Option<String>, ChType)>),
    Other,
}

// None for a column whose cells aren't arrays, maps or tuples.
pub fn array_kind(type_name: &str) -> Option<ArrayKind> {
    match shape(type_name)? {
        Shape::Postgres { .. } | Shape::ClickHouse(ChType::Array(_)) => Some(ArrayKind::Array),
        Shape::ClickHouse(ChType::Map(..)) => Some(ArrayKind::Map),
        Shape::ClickHouse(ChType::Tuple(_)) => Some(ArrayKind::Tuple),
        Shape::ClickHouse(ChType::Other) => None,
    }
}

// Pretty-printed. None when the column isn't an array type or the text doesn't parse as one.
pub fn array_json(text: &str, type_name: &str) -> Option<String> {
    let value = array_value(text, type_name)?;
    let mut out = String::new();
    value.write(&mut out, "");
    Some(out)
}

pub(crate) fn array_value(text: &str, type_name: &str) -> Option<JsonValue> {
    match shape(type_name)? {
        Shape::Postgres { element, delimiter } => {
            Postgres { b: text.as_bytes(), text, i: 0, element, delimiter }.parse()
        }
        Shape::ClickHouse(ChType::Other) => None,
        Shape::ClickHouse(ty) => ClickHouse { b: text.as_bytes(), text, i: 0 }.parse(&ty),
    }
}

fn shape(type_name: &str) -> Option<Shape> {
    let name = type_name.trim();
    // Postgres names an array type after its element with a leading underscore, and declares it with [].
    let element = name.strip_prefix('_').or_else(|| name.strip_suffix("[]")).filter(|e| !e.is_empty());
    if let Some(element) = element
        && element.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b' ' || b == b'[' || b == b']')
    {
        let element = element.trim_end_matches("[]").to_ascii_lowercase();
        let scalar = match element.as_str() {
            "int2" | "int4" | "int8" | "oid" | "float4" | "float8" | "numeric" | "smallint" | "integer" | "bigint"
            | "real" | "double precision" | "decimal" => Scalar::Number,
            "bool" | "boolean" => Scalar::Bool,
            "json" | "jsonb" => Scalar::Json,
            _ => Scalar::Text,
        };
        // Boxes hold commas, so their arrays separate elements with semicolons.
        let delimiter = if element == "box" { b';' } else { b',' };
        return Some(Shape::Postgres { element: scalar, delimiter });
    }
    match ch_type(name, 0)? {
        ChType::Other => None,
        ty => Some(Shape::ClickHouse(ty)),
    }
}

// `Array(Nullable(String))`, `Map(String, UInt8)` or `Tuple(a UInt8, b String)`. Nullable and LowCardinality
// change nothing about how a value is written.
fn ch_type(name: &str, depth: usize) -> Option<ChType> {
    if depth > MAX_DEPTH {
        return None;
    }
    let name = name.trim();
    let Some((head, args)) = name.split_once('(').filter(|_| name.ends_with(')')) else { return Some(ChType::Other) };
    let args = split_top(&args[..args.len() - 1]);
    Some(match head.trim() {
        "Nullable" | "LowCardinality" if args.len() == 1 => ch_type(args[0], depth + 1)?,
        "Array" if args.len() == 1 => ChType::Array(Box::new(ch_type(args[0], depth + 1)?)),
        "Map" if args.len() == 2 => {
            ChType::Map(Box::new(ch_type(args[0], depth + 1)?), Box::new(ch_type(args[1], depth + 1)?))
        }
        "Tuple" => {
            let mut fields = Vec::with_capacity(args.len());
            for arg in args {
                let (name, ty) = tuple_field(arg);
                fields.push((name, ch_type(ty, depth + 1)?));
            }
            ChType::Tuple(fields)
        }
        _ => ChType::Other,
    })
}

// A named field is `name Type`, its name maybe in backticks. A bare type has no space outside parentheses.
fn tuple_field(arg: &str) -> (Option<String>, &str) {
    let arg = arg.trim();
    if let Some(rest) = arg.strip_prefix('`')
        && let Some(end) = rest.find('`')
    {
        return (Some(rest[..end].to_string()), rest[end + 1..].trim());
    }
    match arg.split_once(' ') {
        Some((name, ty)) if !name.contains('(') && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') => {
            (Some(name.to_string()), ty.trim())
        }
        _ => (None, arg),
    }
}

// Splits at commas outside parentheses and quotes, as in `Decimal(10, 2), Enum8('a,b' = 1)`.
fn split_top(args: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let (mut depth, mut start, mut quoted) = (0usize, 0, false);
    let b = args.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\\' if quoted => i += 1,
            b'\'' => quoted = !quoted,
            b'(' if !quoted => depth += 1,
            b')' if !quoted => depth = depth.saturating_sub(1),
            b',' if !quoted && depth == 0 => {
                parts.push(&args[start..i]);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    parts.push(&args[start..]);
    parts.into_iter().filter(|part| !part.trim().is_empty()).collect()
}

// The text exactly as written when JSON would read it as a number, so wide integers and decimals keep their
// digits. NaN, Infinity and the like stay strings.
fn number(text: &str) -> JsonValue {
    if is_json_number(text) { JsonValue::NumberText(text.to_string()) } else { JsonValue::String(text.to_string()) }
}

fn is_json_number(text: &str) -> bool {
    let b = text.as_bytes();
    let mut i = usize::from(b.first() == Some(&b'-'));
    let digits = |i: &mut usize| {
        let start = *i;
        while *i < b.len() && b[*i].is_ascii_digit() {
            *i += 1;
        }
        *i - start
    };
    let int_start = i;
    let int_len = digits(&mut i);
    if int_len == 0 || (int_len > 1 && b[int_start] == b'0') {
        return false;
    }
    if b.get(i) == Some(&b'.') {
        i += 1;
        if digits(&mut i) == 0 {
            return false;
        }
    }
    if matches!(b.get(i), Some(b'e' | b'E')) {
        i += 1;
        if matches!(b.get(i), Some(b'+' | b'-')) {
            i += 1;
        }
        if digits(&mut i) == 0 {
            return false;
        }
    }
    i == b.len()
}

struct Postgres<'a> {
    b: &'a [u8],
    text: &'a str,
    i: usize,
    element: Scalar,
    delimiter: u8,
}

impl Postgres<'_> {
    fn parse(mut self) -> Option<JsonValue> {
        self.skip_space();
        // Bounds other than 1 come first, as in `[0:2]={1,2,3}`. JSON arrays start at 0 anyway.
        if self.b.get(self.i) == Some(&b'[') {
            self.i += self.text[self.i..].find('=')? + 1;
            self.skip_space();
        }
        let value = self.array(0)?;
        self.skip_space();
        (self.i == self.b.len()).then_some(value)
    }

    // ASCII only: a byte of a multi-byte character must never count as a space.
    fn skip_space(&mut self) {
        while self.i < self.b.len() && self.b[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }

    fn array(&mut self, depth: usize) -> Option<JsonValue> {
        if depth > MAX_DEPTH || self.b.get(self.i) != Some(&b'{') {
            return None;
        }
        self.i += 1;
        let mut items = Vec::new();
        self.skip_space();
        if self.b.get(self.i) == Some(&b'}') {
            self.i += 1;
            return Some(JsonValue::Array(items));
        }
        loop {
            self.skip_space();
            let item = match self.b.get(self.i)? {
                b'{' => self.array(depth + 1)?,
                b'"' => {
                    let text = self.quoted()?;
                    self.scalar(text)
                }
                _ => {
                    let start = self.i;
                    while self.i < self.b.len() && self.b[self.i] != self.delimiter && self.b[self.i] != b'}' {
                        self.i += 1;
                    }
                    let token = self.text[start..self.i].trim_matches(is_space);
                    if token.eq_ignore_ascii_case("NULL") { JsonValue::Null } else { self.scalar(token.to_string()) }
                }
            };
            items.push(item);
            self.skip_space();
            match self.b.get(self.i)? {
                b'}' => {
                    self.i += 1;
                    return Some(JsonValue::Array(items));
                }
                &c if c == self.delimiter => self.i += 1,
                _ => return None,
            }
        }
    }

    // Backslash escapes the next character.
    fn quoted(&mut self) -> Option<String> {
        self.i += 1;
        let mut out = String::new();
        let mut start = self.i;
        while let Some(&c) = self.b.get(self.i) {
            match c {
                b'"' => {
                    out.push_str(&self.text[start..self.i]);
                    self.i += 1;
                    return Some(out);
                }
                b'\\' => {
                    out.push_str(&self.text[start..self.i]);
                    let next = self.text[self.i + 1..].chars().next()?;
                    out.push(next);
                    self.i += 1 + next.len_utf8();
                    start = self.i;
                }
                _ => self.i += 1,
            }
        }
        None
    }

    fn scalar(&self, text: String) -> JsonValue {
        match self.element {
            Scalar::Number => number(&text),
            Scalar::Bool => match text.as_str() {
                "t" | "true" => JsonValue::Bool(true),
                "f" | "false" => JsonValue::Bool(false),
                _ => JsonValue::String(text),
            },
            Scalar::Json => match serde_json::from_str::<Json>(&text) {
                Ok(json) => JsonValue::from_json(json),
                Err(_) => JsonValue::String(text),
            },
            Scalar::Text => JsonValue::String(text),
        }
    }
}

struct ClickHouse<'a> {
    b: &'a [u8],
    text: &'a str,
    i: usize,
}

impl ClickHouse<'_> {
    fn parse(mut self, ty: &ChType) -> Option<JsonValue> {
        let value = self.value(ty, 0)?;
        self.skip_space();
        (self.i == self.b.len()).then_some(value)
    }

    // ASCII only: a byte of a multi-byte character must never count as a space.
    fn skip_space(&mut self) {
        while self.i < self.b.len() && self.b[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }

    fn value(&mut self, ty: &ChType, depth: usize) -> Option<JsonValue> {
        if depth > MAX_DEPTH {
            return None;
        }
        self.skip_space();
        match *self.b.get(self.i)? {
            b'[' => {
                let element = match ty {
                    ChType::Array(element) => element,
                    _ => &ChType::Other,
                };
                let items = self.list(b']', depth, |_| element)?;
                Some(JsonValue::Array(items))
            }
            b'(' => {
                let fields: &[(Option<String>, ChType)] = match ty {
                    ChType::Tuple(fields) => fields,
                    _ => &[],
                };
                let items = self.list(b')', depth, |ix| fields.get(ix).map_or(&ChType::Other, |(_, ty)| ty))?;
                let named = fields.len() == items.len() && fields.iter().all(|(name, _)| name.is_some());
                if !named {
                    return Some(JsonValue::Array(items));
                }
                let mut object = JsonObject::default();
                for ((name, _), item) in fields.iter().zip(items) {
                    object.insert(name.clone().unwrap_or_default(), item);
                }
                Some(JsonValue::Object(object))
            }
            b'{' => {
                let (key_ty, value_ty) = match ty {
                    ChType::Map(key, value) => (&**key, &**value),
                    _ => (&ChType::Other, &ChType::Other),
                };
                self.map(key_ty, value_ty, depth)
            }
            b'\'' => self.quoted().map(JsonValue::String),
            _ => {
                let start = self.i;
                while self.i < self.b.len() && !matches!(self.b[self.i], b',' | b']' | b')' | b'}' | b':') {
                    self.i += 1;
                }
                let token = self.text[start..self.i].trim_matches(is_space);
                Some(match token {
                    "" => return None,
                    "NULL" => JsonValue::Null,
                    "true" => JsonValue::Bool(true),
                    "false" => JsonValue::Bool(false),
                    _ => number(token),
                })
            }
        }
    }

    // Items up to `close`, each read as the type `item_type` gives for its position.
    fn list<'t>(&mut self, close: u8, depth: usize, item_type: impl Fn(usize) -> &'t ChType) -> Option<Vec<JsonValue>> {
        self.i += 1;
        let mut items = Vec::new();
        self.skip_space();
        if self.b.get(self.i) == Some(&close) {
            self.i += 1;
            return Some(items);
        }
        loop {
            items.push(self.value(item_type(items.len()), depth + 1)?);
            self.skip_space();
            match *self.b.get(self.i)? {
                b',' => self.i += 1,
                c if c == close => {
                    self.i += 1;
                    return Some(items);
                }
                _ => return None,
            }
        }
    }

    // Keys become strings: a String key as itself, anything else as its compact JSON.
    fn map(&mut self, key_ty: &ChType, value_ty: &ChType, depth: usize) -> Option<JsonValue> {
        self.i += 1;
        let mut object = JsonObject::default();
        self.skip_space();
        if self.b.get(self.i) == Some(&b'}') {
            self.i += 1;
            return Some(JsonValue::Object(object));
        }
        loop {
            let key = match self.value(key_ty, depth + 1)? {
                JsonValue::String(text) | JsonValue::NumberText(text) => text,
                other => {
                    let mut text = String::new();
                    other.write_compact(&mut text);
                    text
                }
            };
            self.skip_space();
            if self.b.get(self.i) != Some(&b':') {
                return None;
            }
            self.i += 1;
            object.insert(key, self.value(value_ty, depth + 1)?);
            self.skip_space();
            match *self.b.get(self.i)? {
                b',' => self.i += 1,
                b'}' => {
                    self.i += 1;
                    return Some(JsonValue::Object(object));
                }
                _ => return None,
            }
        }
    }

    // ClickHouse's escapes: \b \f \n \r \t \0, \xHH for a byte, and a backslash before anything else is dropped.
    fn quoted(&mut self) -> Option<String> {
        self.i += 1;
        let mut bytes = Vec::new();
        while let Some(&c) = self.b.get(self.i) {
            self.i += 1;
            match c {
                b'\'' => return Some(String::from_utf8_lossy(&bytes).into_owned()),
                b'\\' => {
                    let next = *self.b.get(self.i)?;
                    self.i += 1;
                    match next {
                        b'b' => bytes.push(0x08),
                        b'f' => bytes.push(0x0c),
                        b'n' => bytes.push(b'\n'),
                        b'r' => bytes.push(b'\r'),
                        b't' => bytes.push(b'\t'),
                        b'0' => bytes.push(0),
                        b'x' => {
                            let hex = self.text.get(self.i..self.i + 2)?;
                            bytes.push(u8::from_str_radix(hex, 16).ok()?);
                            self.i += 2;
                        }
                        other => bytes.push(other),
                    }
                }
                other => bytes.push(other),
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compact(text: &str, type_name: &str) -> Option<String> {
        let value = array_value(text, type_name)?;
        let mut out = String::new();
        value.write_compact(&mut out);
        Some(out)
    }

    #[test]
    fn postgres_arrays_read_by_element_type() {
        assert_eq!(compact("{1,2,NULL}", "_INT4").as_deref(), Some("[1,2,null]"));
        assert_eq!(compact("{9007199254740993,-0.5}", "_NUMERIC").as_deref(), Some("[9007199254740993,-0.5]"));
        assert_eq!(compact("{NaN,Infinity,1e+20}", "_FLOAT8").as_deref(), Some(r#"["NaN","Infinity",1e+20]"#));
        assert_eq!(compact("{t,f,NULL}", "_BOOL").as_deref(), Some("[true,false,null]"));
        assert_eq!(compact("{}", "_TEXT").as_deref(), Some("[]"));
        assert_eq!(compact("{{1,2},{3,4}}", "_INT8").as_deref(), Some("[[1,2],[3,4]]"));
    }

    #[test]
    fn postgres_quoting_and_nulls() {
        // A quoted NULL is the word, and backslashes escape inside quotes.
        let text = r#"{plain,"two words","NULL",NULL,"say \"hi\"","back\\slash",""}"#;
        assert_eq!(
            compact(text, "_TEXT").as_deref(),
            Some(r#"["plain","two words","NULL",null,"say \"hi\"","back\\slash",""]"#)
        );
        assert_eq!(compact(r#"{"{\"a\": 1}","[1, 2]"}"#, "_JSONB").as_deref(), Some(r#"[{"a":1},[1,2]]"#));
        assert_eq!(compact("[0:2]={7,8,9}", "_INT4").as_deref(), Some("[7,8,9]"));
        assert_eq!(compact("{(1,1),(0,0);(2,2),(1,1)}", "_BOX").as_deref(), Some(r#"["(1,1),(0,0)","(2,2),(1,1)"]"#));
        assert_eq!(compact("{ü,\"日本\"}", "text[]").as_deref(), Some(r#"["ü","日本"]"#));
        // à is C3 A0, and A0 alone would read as a no-break space.
        assert_eq!(compact("{à, à}", "_TEXT").as_deref(), Some(r#"["à","à"]"#));
        assert_eq!(compact("['à', 'à']", "Array(String)").as_deref(), Some(r#"["à","à"]"#));
    }

    #[test]
    fn broken_or_foreign_text_stays_text() {
        for (text, type_name) in [
            ("{1,2", "_INT4"),
            ("1,2}", "_INT4"),
            ("{1,2}x", "_INT4"),
            (r#"{"open}"#, "_TEXT"),
            ("[1,2", "Array(UInt8)"),
            ("['a'", "Array(String)"),
            ("{'k':}", "Map(String, UInt8)"),
            ("{1,2}", "INT4"),
            ("{1,2}", "TEXT"),
            ("[1,2]", "String"),
        ] {
            assert_eq!(compact(text, type_name), None, "{text} as {type_name}");
        }
        let deep = format!("{}1{}", "{".repeat(100), "}".repeat(100));
        assert_eq!(compact(&deep, "_INT4"), None);
    }

    #[test]
    fn clickhouse_values_read_from_their_quoting() {
        assert_eq!(compact("[1,2,3]", "Array(UInt8)").as_deref(), Some("[1,2,3]"));
        assert_eq!(compact(r"['a','it\'s',NULL]", "Array(Nullable(String))").as_deref(), Some(r#"["a","it's",null]"#));
        assert_eq!(compact(r"['tab\there','\x41\\']", "Array(String)").as_deref(), Some(r#"["tab\there","A\\"]"#));
        assert_eq!(
            compact("[18446744073709551615,nan,-inf]", "Array(Float64)").as_deref(),
            Some(r#"[18446744073709551615,"nan","-inf"]"#)
        );
        assert_eq!(compact("[[1],[]]", "Array(Array(UInt8))").as_deref(), Some("[[1],[]]"));
        assert_eq!(compact("[true,false]", "Array(Bool)").as_deref(), Some("[true,false]"));
        assert_eq!(
            compact("['2024-01-02','2024-01-03']", "Array(Date)").as_deref(),
            Some(r#"["2024-01-02","2024-01-03"]"#)
        );
    }

    #[test]
    fn clickhouse_maps_and_tuples() {
        assert_eq!(compact("{'a':1,'b':2}", "Map(String, UInt8)").as_deref(), Some(r#"{"a":1,"b":2}"#));
        assert_eq!(compact("{}", "Map(String, UInt8)").as_deref(), Some("{}"));
        assert_eq!(compact("{1:'x'}", "Map(UInt8, String)").as_deref(), Some(r#"{"1":"x"}"#));
        assert_eq!(
            compact("{'k':[1,2]}", "Map(LowCardinality(String), Array(UInt8))").as_deref(),
            Some(r#"{"k":[1,2]}"#)
        );
        assert_eq!(compact("(1,'a')", "Tuple(UInt8, String)").as_deref(), Some(r#"[1,"a"]"#));
        assert_eq!(compact("(1,'a')", "Tuple(id UInt8, name String)").as_deref(), Some(r#"{"id":1,"name":"a"}"#));
        assert_eq!(
            compact("[(1,'x'),(2,'y')]", "Array(Tuple(`the id` UInt8, name String))").as_deref(),
            Some(r#"[{"the id":1,"name":"x"},{"the id":2,"name":"y"}]"#)
        );
        assert_eq!(
            compact("(1.50,'b')", "Tuple(Decimal(10, 2), Enum8('a,b' = 1, 'b' = 2))").as_deref(),
            Some(r#"[1.50,"b"]"#)
        );
    }

    #[test]
    fn kinds_follow_the_type() {
        assert_eq!(array_kind("_INT4"), Some(ArrayKind::Array));
        assert_eq!(array_kind("integer[]"), Some(ArrayKind::Array));
        assert_eq!(array_kind("Array(String)"), Some(ArrayKind::Array));
        assert_eq!(array_kind("Map(String, UInt64)"), Some(ArrayKind::Map));
        assert_eq!(array_kind("Nullable(Tuple(UInt8))"), Some(ArrayKind::Tuple));
        for plain in ["INT4", "TEXT", "String", "Nullable(String)", "DateTime64(3, 'UTC')", "", "_", "[]", "1007"] {
            assert_eq!(array_kind(plain), None, "{plain}");
        }
    }

    #[test]
    fn json_numbers_are_recognised_exactly() {
        for yes in ["0", "-0", "1", "-12", "1.5", "1e5", "1E+5", "2.5e-3", "18446744073709551615"] {
            assert!(is_json_number(yes), "{yes}");
        }
        for no in ["", "-", "01", "+1", ".5", "1.", "1e", "NaN", "inf", "0x10", "1 2", "١"] {
            assert!(!is_json_number(no), "{no}");
        }
    }

    #[test]
    fn pretty_output_matches_the_json_viewer() {
        assert_eq!(array_json("{1,2}", "_INT4").as_deref(), Some("[\n  1,\n  2\n]"));
    }
}
