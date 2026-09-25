use std::fmt::Write as _;

use mysql_async::consts::{ColumnFlags, ColumnType};
use mysql_async::{Column, Value as MyValue};

use crate::ChunkBuilder;
use crate::display::{push_bytes, push_float, push_float_f32};
use crate::postgres::MAX_SAFE_INTEGER;
use crate::temporal::parse_timestamp;

const BINARY_CHARSET: u16 = 63;
const ZERO_DATETIME: &str = "0000-00-00 00:00:00.000000";

// Dates and datetimes parse as UTC timestamps.
#[derive(Clone, Copy, Debug)]
pub enum Decoder {
    Integer,
    Float,
    Double,
    DateTime,
    Bytes,
}

impl Decoder {
    pub fn for_column(column: &Column) -> Self {
        use ColumnType::*;
        match column.column_type() {
            MYSQL_TYPE_TINY | MYSQL_TYPE_SHORT | MYSQL_TYPE_INT24 | MYSQL_TYPE_YEAR | MYSQL_TYPE_LONG
            | MYSQL_TYPE_LONGLONG => Self::Integer,
            MYSQL_TYPE_FLOAT => Self::Float,
            MYSQL_TYPE_DOUBLE => Self::Double,
            MYSQL_TYPE_TIMESTAMP | MYSQL_TYPE_DATETIME | MYSQL_TYPE_DATE | MYSQL_TYPE_NEWDATE => Self::DateTime,
            _ => Self::Bytes,
        }
    }

    pub fn push(self, bytes: &[u8], out: &mut ChunkBuilder) {
        let text = std::str::from_utf8(bytes).unwrap_or_default();
        match self {
            Self::Integer => match text.parse::<i128>() {
                Ok(v) if v.abs() <= MAX_SAFE_INTEGER => out.push_number(|s| s.push_str(text)),
                _ => out.push_text(|s| s.push_str(text)),
            },
            Self::Float => match text.parse::<f32>() {
                Ok(value) => out.push_number(|s| push_float_f32(value, s)),
                Err(_) => out.push_text(|s| push_bytes(bytes, s)),
            },
            Self::Double => match text.parse::<f64>() {
                Ok(value) => out.push_number(|s| push_float(value, s)),
                Err(_) => out.push_text(|s| push_bytes(bytes, s)),
            },
            Self::DateTime => out.push_text(|s| {
                if !text.is_empty() && ZERO_DATETIME.starts_with(text) {
                    s.push_str("0001-01-01T00:00:00Z");
                } else {
                    match parse_timestamp(text) {
                        Some(ts) => ts.push_rfc3339_nano(0, s),
                        None => push_bytes(bytes, s),
                    }
                }
            }),
            Self::Bytes => out.push_text(|s| push_bytes(bytes, s)),
        }
    }

    pub fn display(self, bytes: &[u8]) -> String {
        let mut b = ChunkBuilder::new(1, 1);
        self.push(bytes, &mut b);
        b.end_row();
        b.finish().display(0, 0).unwrap_or_default().to_string()
    }
}

// Prepared statements return binary-protocol values.
pub(crate) fn push_binary(value: &MyValue, decoder: &Decoder, out: &mut ChunkBuilder) {
    match value {
        MyValue::NULL => out.push_null(),
        MyValue::Bytes(bytes) => decoder.push(bytes, out),
        MyValue::Int(i) if i128::from(*i).abs() <= MAX_SAFE_INTEGER => out.push_number(|s| {
            let _ = write!(s, "{i}");
        }),
        MyValue::Int(i) => out.push_text(|s| {
            let _ = write!(s, "{i}");
        }),
        MyValue::UInt(u) if i128::from(*u) <= MAX_SAFE_INTEGER => out.push_number(|s| {
            let _ = write!(s, "{u}");
        }),
        MyValue::UInt(u) => out.push_text(|s| {
            let _ = write!(s, "{u}");
        }),
        MyValue::Float(f) => out.push_number(|s| push_float_f32(*f, s)),
        MyValue::Double(d) => out.push_number(|s| push_float(*d, s)),
        MyValue::Date(year, month, day, hour, minute, second, micros) => out.push_text(|s| {
            if (*year, *month, *day) == (0, 0, 0) {
                s.push_str("0001-01-01T00:00:00Z");
                return;
            }
            let _ = write!(s, "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}");
            if *micros > 0 {
                let fraction = format!("{micros:06}");
                s.push('.');
                s.push_str(fraction.trim_end_matches('0'));
            }
            s.push('Z');
        }),
        MyValue::Time(negative, days, hours, minutes, seconds, micros) => out.push_text(|s| {
            let hours = u32::from(*hours) + days * 24;
            let sign = if *negative { "-" } else { "" };
            let _ = write!(s, "{sign}{hours:02}:{minutes:02}:{seconds:02}");
            if *micros > 0 {
                let _ = write!(s, ".{micros:06}");
            }
        }),
    }
}

pub fn type_name(column: &Column) -> String {
    use ColumnType::*;
    let flags = column.flags();
    let unsigned = |signed: &str| {
        if flags.contains(ColumnFlags::UNSIGNED_FLAG) { format!("UNSIGNED {signed}") } else { signed.to_string() }
    };
    let binary = column.character_set() == BINARY_CHARSET;
    let text_or_blob = |text: &str, blob: &str| if binary { blob } else { text }.to_string();
    match column.column_type() {
        MYSQL_TYPE_BIT => "BIT".into(),
        MYSQL_TYPE_BLOB => text_or_blob("TEXT", "BLOB"),
        MYSQL_TYPE_DATE | MYSQL_TYPE_NEWDATE => "DATE".into(),
        MYSQL_TYPE_DATETIME => "DATETIME".into(),
        MYSQL_TYPE_DECIMAL | MYSQL_TYPE_NEWDECIMAL => "DECIMAL".into(),
        MYSQL_TYPE_DOUBLE => "DOUBLE".into(),
        MYSQL_TYPE_ENUM => "ENUM".into(),
        MYSQL_TYPE_FLOAT => "FLOAT".into(),
        MYSQL_TYPE_GEOMETRY => "GEOMETRY".into(),
        MYSQL_TYPE_INT24 => unsigned("MEDIUMINT"),
        MYSQL_TYPE_JSON => "JSON".into(),
        MYSQL_TYPE_LONG => unsigned("INT"),
        MYSQL_TYPE_LONG_BLOB => text_or_blob("LONGTEXT", "LONGBLOB"),
        MYSQL_TYPE_LONGLONG => unsigned("BIGINT"),
        MYSQL_TYPE_MEDIUM_BLOB => text_or_blob("MEDIUMTEXT", "MEDIUMBLOB"),
        MYSQL_TYPE_NULL => "NULL".into(),
        MYSQL_TYPE_SET => "SET".into(),
        MYSQL_TYPE_SHORT => unsigned("SMALLINT"),
        MYSQL_TYPE_STRING if flags.contains(ColumnFlags::ENUM_FLAG) => "ENUM".into(),
        MYSQL_TYPE_STRING if flags.contains(ColumnFlags::SET_FLAG) => "SET".into(),
        MYSQL_TYPE_STRING => text_or_blob("CHAR", "BINARY"),
        MYSQL_TYPE_TIME => "TIME".into(),
        MYSQL_TYPE_TIMESTAMP => "TIMESTAMP".into(),
        MYSQL_TYPE_TINY => unsigned("TINYINT"),
        MYSQL_TYPE_TINY_BLOB => text_or_blob("TINYTEXT", "TINYBLOB"),
        MYSQL_TYPE_VARCHAR | MYSQL_TYPE_VAR_STRING => text_or_blob("VARCHAR", "VARBINARY"),
        MYSQL_TYPE_YEAR => "YEAR".into(),
        MYSQL_TYPE_VECTOR => "VECTOR".into(),
        _ => String::new(),
    }
}
