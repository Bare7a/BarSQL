// How a ClickHouse type's text shows in the grid. Numbers that fit a double are numbers, wider integers and
// decimals stay exact as text, and dates read like every other engine's.

use std::fmt::Write as _;

use crate::ChunkBuilder;
use crate::display::{push_bytes, push_float};
use crate::postgres::MAX_SAFE_INTEGER;
use crate::temporal::parse_timestamp;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Bool,
    Integer,
    Float,
    Date,
    DateTime,
    // Decimals, strings, UUIDs, arrays, maps, tuples and the rest, as the server wrote them.
    Text,
}

// Without Nullable(...) and LowCardinality(...), which only change how values are stored.
pub(crate) fn base_type(type_name: &str) -> &str {
    let mut t = type_name.trim();
    loop {
        let unwrapped = ["Nullable(", "LowCardinality("]
            .iter()
            .find_map(|w| t.strip_prefix(w).and_then(|rest| rest.strip_suffix(')')));
        match unwrapped {
            Some(inner) => t = inner.trim(),
            None => return t,
        }
    }
}

pub(crate) fn kind(type_name: &str) -> Kind {
    let base = base_type(type_name);
    let name = base.split('(').next().unwrap_or(base);
    match name {
        "Bool" => Kind::Bool,
        "UInt8" | "UInt16" | "UInt32" | "UInt64" | "UInt128" | "UInt256" | "Int8" | "Int16" | "Int32" | "Int64"
        | "Int128" | "Int256" => Kind::Integer,
        "Float32" | "Float64" | "BFloat16" => Kind::Float,
        "Date" | "Date32" => Kind::Date,
        "DateTime" | "DateTime64" => Kind::DateTime,
        _ => Kind::Text,
    }
}

// `value` is the decoded field, None for NULL.
pub(crate) fn push(kind: Kind, value: Option<&[u8]>, out: &mut ChunkBuilder) {
    let Some(bytes) = value else { return out.push_null() };
    // A String can hold any bytes. Ones that aren't UTF-8 show as hex, like other engines' binary.
    if kind == Kind::Text {
        return out.push_text(|s| push_bytes(bytes, s));
    }
    let value = &*String::from_utf8_lossy(bytes);
    match kind {
        Kind::Bool => match value {
            "true" => out.push_bool(true),
            "false" => out.push_bool(false),
            other => out.push_text(|s| s.push_str(other)),
        },
        Kind::Integer => match value.parse::<i128>() {
            Ok(n) if n.abs() <= MAX_SAFE_INTEGER => out.push_number(|s| {
                let _ = write!(s, "{n}");
            }),
            _ => out.push_text(|s| s.push_str(value)),
        },
        Kind::Float => match float(value) {
            Some(f) => out.push_number(|s| push_float(f, s)),
            None => out.push_text(|s| s.push_str(value)),
        },
        Kind::Date => match timestamp(&format!("{value}T00:00:00Z")) {
            Some(text) => out.push_text(|s| s.push_str(&text)),
            None => out.push_text(|s| s.push_str(value)),
        },
        Kind::DateTime => match timestamp(value) {
            Some(text) => out.push_text(|s| s.push_str(&text)),
            None => out.push_text(|s| s.push_str(value)),
        },
        Kind::Text => unreachable!("written as bytes above"),
    }
}

// ClickHouse writes the specials as nan, inf and -inf.
fn float(value: &str) -> Option<f64> {
    match value {
        "nan" | "-nan" => Some(f64::NAN),
        "inf" | "+inf" => Some(f64::INFINITY),
        "-inf" => Some(f64::NEG_INFINITY),
        other => other.parse().ok(),
    }
}

// date_time_output_format=iso gives 2024-01-02T03:04:05.123Z in UTC. Without it, a DateTime is plain local text.
// A Date has no time, and reads as midnight UTC like other engines' dates.
fn timestamp(value: &str) -> Option<String> {
    let utc = value.strip_suffix('Z')?;
    let ts = parse_timestamp(&utc.replacen('T', " ", 1))?;
    let mut out = String::new();
    ts.push_rfc3339_nano(0, &mut out);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Cell;

    fn shown(type_name: &str, value: Option<&str>) -> (String, Option<String>) {
        let mut b = ChunkBuilder::new(1, 1);
        push(kind(type_name), value.map(str::as_bytes), &mut b);
        b.end_row();
        let chunk = b.finish();
        let cell = match chunk.cell(0, 0) {
            Cell::Null => "null",
            Cell::Bool(_) => "bool",
            Cell::Number(_) => "number",
            Cell::Text(_) => "text",
        };
        (cell.to_string(), chunk.display(0, 0).map(str::to_string))
    }

    #[test]
    fn wrappers_come_off() {
        assert_eq!(base_type("Nullable(LowCardinality(String))"), "String");
        assert_eq!(base_type("LowCardinality(Nullable(String))"), "String");
        assert_eq!(kind("Nullable(Decimal(18, 3))"), Kind::Text);
        assert_eq!(kind("DateTime64(3, 'UTC')"), Kind::DateTime);
        assert_eq!(kind("Array(UInt8)"), Kind::Text);
    }

    #[test]
    fn values_show_like_other_engines() {
        let cases = [
            ("UInt8", Some("1"), ("number", Some("1"))),
            ("UInt64", Some("18446744073709551615"), ("text", Some("18446744073709551615"))),
            ("Int64", Some("-9007199254740991"), ("number", Some("-9007199254740991"))),
            ("Float64", Some("0.30000000000000004"), ("number", Some("0.30000000000000004"))),
            ("Float64", Some("1e21"), ("number", Some("1e+21"))),
            ("Float64", Some("nan"), ("number", Some("NaN"))),
            ("Float32", Some("-inf"), ("number", Some("-Infinity"))),
            ("Bool", Some("true"), ("bool", Some("true"))),
            ("Decimal(18, 3)", Some("1.500"), ("text", Some("1.500"))),
            ("Date", Some("2024-02-29"), ("text", Some("2024-02-29T00:00:00Z"))),
            ("DateTime", Some("2024-01-02T03:04:05Z"), ("text", Some("2024-01-02T03:04:05Z"))),
            ("DateTime64(6)", Some("2024-01-02T03:04:05.123000Z"), ("text", Some("2024-01-02T03:04:05.123Z"))),
            ("DateTime", Some("2024-01-02 03:04:05"), ("text", Some("2024-01-02 03:04:05"))),
            ("Nullable(String)", None, ("null", None)),
        ];
        for (type_name, value, want) in cases {
            let want = (want.0.to_string(), want.1.map(str::to_string));
            assert_eq!(shown(type_name, value), want, "{type_name} {value:?}");
        }
    }
}
