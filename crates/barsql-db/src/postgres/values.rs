use jiff::tz::TimeZone;

use crate::ChunkBuilder;
use crate::display::{push_bytes, push_float};
use crate::result::CellKind;
use crate::temporal::parse_timestamp;

// Only the OIDs in for_oid decode to typed values. Everything else stays server text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decoder {
    Text,
    Bool,
    Bytea,
    Integer,
    Float4,
    Float8,
    Date,
    Timestamp,
    Timestamptz,
}

pub(crate) const MAX_SAFE_INTEGER: i128 = (1 << 53) - 1;

impl Decoder {
    pub fn for_oid(oid: u32) -> Self {
        match oid {
            16 => Self::Bool,
            17 => Self::Bytea,
            20 | 21 | 23 | 26 | 28 | 29 => Self::Integer,
            700 => Self::Float4,
            701 => Self::Float8,
            1082 => Self::Date,
            1114 => Self::Timestamp,
            1184 => Self::Timestamptz,
            _ => Self::Text,
        }
    }

    pub fn push(self, text: Option<&str>, local: &TimeZone, out: &mut ChunkBuilder) {
        let Some(text) = text else {
            out.push_null();
            return;
        };
        let kind = self.kind(text);
        out.push_kind(kind, |s| self.write(text, local, s));
    }

    pub fn display(self, text: &str, local: &TimeZone, out: &mut String) {
        self.write(text, local, out);
    }

    fn kind(self, text: &str) -> CellKind {
        match self {
            Self::Bool => CellKind::Bool,
            Self::Float4 | Self::Float8 if text.parse::<f64>().is_ok() => CellKind::Number,
            // Beyond 2^53 the value stays a string so the UI cannot round it.
            Self::Integer => match text.parse::<i128>() {
                Ok(v) if v.abs() <= MAX_SAFE_INTEGER => CellKind::Number,
                _ => CellKind::Text,
            },
            _ => CellKind::Text,
        }
    }

    fn write(self, text: &str, local: &TimeZone, out: &mut String) {
        match self {
            Self::Text | Self::Integer => out.push_str(text),
            Self::Bool => out.push_str(if text == "t" { "true" } else { "false" }),
            Self::Bytea => match text.strip_prefix("\\x").and_then(|hex| hex::decode(hex).ok()) {
                Some(bytes) => push_bytes(&bytes, out),
                None => out.push_str(text),
            },
            Self::Float4 => match text.parse::<f32>() {
                Ok(value) => push_float(f64::from(value), out),
                Err(_) => out.push_str(text),
            },
            Self::Float8 => match text.parse::<f64>() {
                Ok(value) => push_float(value, out),
                Err(_) => out.push_str(text),
            },
            Self::Date | Self::Timestamp => match parse_timestamp(text) {
                Some(ts) => ts.push_rfc3339_nano(0, out),
                None => out.push_str(text),
            },
            Self::Timestamptz => match parse_timestamp(text).and_then(|ts| ts.in_zone(local)) {
                Some((ts, offset)) => ts.push_rfc3339_nano(offset, out),
                None => out.push_str(text),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::result::Cell;
    use jiff::tz::Offset;

    fn show(decoder: Decoder, text: &str, zone: &TimeZone) -> String {
        let mut out = String::new();
        decoder.display(text, zone, &mut out);
        out
    }

    #[test]
    fn display_rules() {
        let utc = TimeZone::UTC;
        assert_eq!(show(Decoder::Bool, "t", &utc), "true");
        assert_eq!(show(Decoder::Bytea, "\\x68656c6c6f", &utc), "hello");
        assert_eq!(show(Decoder::Bytea, "\\xdeadbeef", &utc), "\\xdeadbeef");
        assert_eq!(show(Decoder::Float4, "1.1", &utc), "1.100000023841858");
        assert_eq!(show(Decoder::Float8, "1e+21", &utc), "1e+21");
        assert_eq!(show(Decoder::Date, "2024-02-29", &utc), "2024-02-29T00:00:00Z");
        assert_eq!(show(Decoder::Date, "0044-03-15 BC", &utc), "-0043-03-15T00:00:00Z");
        assert_eq!(show(Decoder::Date, "infinity", &utc), "infinity");
        assert_eq!(show(Decoder::Timestamp, "2024-02-29 13:45:30.123456", &utc), "2024-02-29T13:45:30.123456Z");
        assert_eq!(show(Decoder::Timestamptz, "2024-02-29 13:45:30.1234+02", &utc), "2024-02-29T11:45:30.1234Z");
        let plus3 = TimeZone::fixed(Offset::from_seconds(3 * 3600).unwrap());
        assert_eq!(show(Decoder::Timestamptz, "2024-02-29 11:45:30+00", &plus3), "2024-02-29T14:45:30+03:00");
        assert_eq!(show(Decoder::Timestamptz, "2024-02-29 11:45:30-05:30", &utc), "2024-02-29T17:15:30Z");
    }

    #[test]
    fn kinds_follow_the_json_types() {
        let mut b = ChunkBuilder::new(5, 1);
        let utc = TimeZone::UTC;
        Decoder::Integer.push(Some("42"), &utc, &mut b);
        Decoder::Integer.push(Some("9007199254740993"), &utc, &mut b);
        Decoder::Bool.push(Some("f"), &utc, &mut b);
        Decoder::Float8.push(Some("NaN"), &utc, &mut b);
        Decoder::Text.push(None, &utc, &mut b);
        b.end_row();
        let chunk = b.finish();
        assert_eq!(chunk.cell(0, 0), Cell::Number("42"));
        assert_eq!(chunk.cell(0, 1), Cell::Text("9007199254740993"));
        assert_eq!(chunk.cell(0, 2), Cell::Bool(false));
        assert_eq!(chunk.cell(0, 3), Cell::Number("NaN"));
        assert_eq!(chunk.cell(0, 4), Cell::Null);
    }
}
