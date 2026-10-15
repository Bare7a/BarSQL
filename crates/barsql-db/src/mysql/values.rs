use std::fmt::Write as _;

use jiff::tz::TimeZone;
use mysql_async::consts::{ColumnFlags, ColumnType};
use mysql_async::{Column, Value as MyValue};

use crate::ChunkBuilder;
use crate::display::{push_bytes, push_float, push_float_f32};
use crate::postgres::MAX_SAFE_INTEGER;
use crate::temporal::parse_timestamp;

const BINARY_CHARSET: u16 = 63;

// Dates and datetimes have no zone and parse as UTC. TIMESTAMPs show the offset of the session's time zone.
#[derive(Clone, Copy, Debug)]
pub enum Decoder {
    Integer,
    Float,
    Double,
    Date,
    // DATETIME and TIMESTAMP carry the column's fractional digits.
    DateTime(u8),
    Timestamp(u8),
    Bits(u32),
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
            MYSQL_TYPE_DATE | MYSQL_TYPE_NEWDATE => Self::Date,
            MYSQL_TYPE_DATETIME => Self::DateTime(column.decimals().min(6)),
            MYSQL_TYPE_TIMESTAMP => Self::Timestamp(column.decimals().min(6)),
            MYSQL_TYPE_BIT => Self::Bits(column.column_length()),
            _ => Self::Bytes,
        }
    }

    pub fn push(self, bytes: &[u8], zone: &TimeZone, out: &mut ChunkBuilder) {
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
            Self::Date | Self::DateTime(_) => out.push_text(|s| push_date_time(text, bytes, None, s)),
            Self::Timestamp(_) => out.push_text(|s| push_date_time(text, bytes, Some(zone), s)),
            Self::Bits(width) => out.push_text(|s| push_bits(bytes, width, s)),
            Self::Bytes => out.push_text(|s| push_bytes(bytes, s)),
        }
    }
}

// A lax sql_mode stores zero dates like 0000-00-00 and partial ones like 2024-00-00, which show as sent.
fn push_date_time(text: &str, bytes: &[u8], zone: Option<&TimeZone>, out: &mut String) {
    match parse_timestamp(text) {
        Some(ts) if ts.is_calendar_date() => ts.push_rfc3339_nano(zone.map_or(0, |zone| ts.offset_in(zone)), out),
        _ => push_bytes(bytes, out),
    }
}

// The server sends BIT(n) as whole bytes, most significant first. It shows as n digits, as Postgres shows bit strings.
fn push_bits(bytes: &[u8], width: u32, out: &mut String) {
    let total = bytes.len() * 8;
    let width = if width == 0 { total } else { width as usize };
    // `place` counts from the least significant bit.
    for place in (0..width).rev() {
        let set = place < total && (bytes[bytes.len() - 1 - place / 8] >> (place % 8)) & 1 == 1;
        out.push(if set { '1' } else { '0' });
    }
}

// Prepared statements return binary-protocol values.
pub(crate) fn push_binary(value: &MyValue, decoder: &Decoder, zone: &TimeZone, out: &mut ChunkBuilder) {
    match value {
        MyValue::NULL => out.push_null(),
        MyValue::Bytes(bytes) => decoder.push(bytes, zone, out),
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
        // Written as the text protocol sends it, so a value shows the same either way.
        MyValue::Date(year, month, day, hour, minute, second, micros) => {
            let mut text = format!("{year:04}-{month:02}-{day:02}");
            if let Decoder::DateTime(decimals) | Decoder::Timestamp(decimals) = decoder {
                let _ = write!(text, " {hour:02}:{minute:02}:{second:02}");
                let digits = if *decimals == 0 && *micros > 0 { 6 } else { usize::from(*decimals) };
                if digits > 0 {
                    let fraction = micros / 10u32.pow(6 - digits as u32);
                    let _ = write!(text, ".{fraction:0digits$}");
                }
            }
            decoder.push(text.as_bytes(), zone, out)
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::tz::Offset;

    fn show(decoder: Decoder, bytes: &[u8], zone: &TimeZone) -> String {
        let mut b = ChunkBuilder::new(1, 1);
        decoder.push(bytes, zone, &mut b);
        b.end_row();
        b.finish().display(0, 0).unwrap_or_default().to_string()
    }

    fn fixed(seconds: i32) -> TimeZone {
        TimeZone::fixed(Offset::from_seconds(seconds).unwrap())
    }

    #[test]
    fn bits_show_as_their_digits() {
        let utc = TimeZone::UTC;
        assert_eq!(show(Decoder::Bits(4), &[0b1010], &utc), "1010");
        assert_eq!(show(Decoder::Bits(4), &[0], &utc), "0000");
        assert_eq!(show(Decoder::Bits(1), &[1], &utc), "1");
        assert_eq!(show(Decoder::Bits(10), &[0b10, 0b0000_0101], &utc), "1000000101");
        assert_eq!(show(Decoder::Bits(64), &[0x80, 0, 0, 0, 0, 0, 0, 1], &utc), format!("1{}1", "0".repeat(62)));
        assert_eq!(show(Decoder::Bits(12), &[0xff], &utc), "000011111111");
        assert_eq!(show(Decoder::Bits(0), &[0b1010], &utc), "00001010");
    }

    #[test]
    fn timestamps_show_the_offset_of_the_sessions_zone() {
        let ts = |text: &str, zone: &TimeZone| show(Decoder::Timestamp(3), text.as_bytes(), zone);
        let sofia = TimeZone::get("Europe/Sofia").unwrap();
        assert_eq!(ts("2024-02-29 13:45:30.123", &TimeZone::UTC), "2024-02-29T13:45:30.123Z");
        assert_eq!(ts("2024-02-29 18:45:30.123", &fixed(5 * 3600)), "2024-02-29T18:45:30.123+05:00");
        assert_eq!(ts("2024-02-29 10:15:30", &fixed(-(3 * 3600 + 1800))), "2024-02-29T10:15:30-03:30");
        assert_eq!(ts("2024-02-29 15:45:30", &sofia), "2024-02-29T15:45:30+02:00");
        assert_eq!(ts("2024-07-01 15:00:00", &sofia), "2024-07-01T15:00:00+03:00");
        // 03:30 comes twice when the clocks go back at 04:00, and shows the earlier one.
        assert_eq!(ts("2024-10-27 03:30:00", &sofia), "2024-10-27T03:30:00+03:00");
        assert_eq!(ts("0000-00-00 00:00:00", &sofia), "0000-00-00 00:00:00");
        assert_eq!(show(Decoder::DateTime(0), b"2024-02-29 13:45:30", &sofia), "2024-02-29T13:45:30Z");
        assert_eq!(show(Decoder::Date, b"2024-02-29", &sofia), "2024-02-29T00:00:00Z");
    }

    #[test]
    fn binary_timestamps_show_the_offset_too() {
        let value = MyValue::Date(2024, 2, 29, 18, 45, 30, 123_000);
        let mut b = ChunkBuilder::new(2, 1);
        push_binary(&value, &Decoder::Timestamp(3), &fixed(5 * 3600), &mut b);
        push_binary(&value, &Decoder::DateTime(3), &fixed(5 * 3600), &mut b);
        b.end_row();
        let chunk = b.finish();
        assert_eq!(chunk.display(0, 0), Some("2024-02-29T18:45:30.123+05:00"));
        assert_eq!(chunk.display(0, 1), Some("2024-02-29T18:45:30.123Z"));
    }

    fn display_binary(value: MyValue, decoder: Decoder) -> String {
        let mut b = ChunkBuilder::new(1, 1);
        push_binary(&value, &decoder, &TimeZone::UTC, &mut b);
        b.end_row();
        b.finish().display(0, 0).unwrap_or_default().to_string()
    }

    #[test]
    fn dates_without_a_day_show_as_the_server_sends_them() {
        let utc = TimeZone::UTC;
        assert_eq!(show(Decoder::Date, b"2024-02-29", &utc), "2024-02-29T00:00:00Z");
        assert_eq!(show(Decoder::DateTime(6), b"2024-02-29 13:45:30.123000", &utc), "2024-02-29T13:45:30.123Z");
        assert_eq!(show(Decoder::Date, b"0000-00-00", &utc), "0000-00-00");
        assert_eq!(show(Decoder::DateTime(0), b"0000-00-00 00:00:00", &utc), "0000-00-00 00:00:00");
        assert_eq!(show(Decoder::DateTime(6), b"0000-00-00 00:00:00.000000", &utc), "0000-00-00 00:00:00.000000");
        assert_eq!(show(Decoder::Date, b"2024-00-00", &utc), "2024-00-00");
        assert_eq!(show(Decoder::DateTime(0), b"2024-02-00 10:00:00", &utc), "2024-02-00 10:00:00");
        // ALLOW_INVALID_DATES keeps any day up to 31.
        assert_eq!(show(Decoder::Date, b"2023-02-29", &utc), "2023-02-29");

        let zero = || MyValue::Date(0, 0, 0, 0, 0, 0, 0);
        assert_eq!(display_binary(zero(), Decoder::Date), "0000-00-00");
        assert_eq!(display_binary(zero(), Decoder::DateTime(0)), "0000-00-00 00:00:00");
        assert_eq!(display_binary(zero(), Decoder::DateTime(3)), "0000-00-00 00:00:00.000");
        assert_eq!(display_binary(zero(), Decoder::Timestamp(0)), "0000-00-00 00:00:00");
        assert_eq!(display_binary(MyValue::Date(2024, 0, 0, 0, 0, 0, 0), Decoder::Date), "2024-00-00");
        assert_eq!(
            display_binary(MyValue::Date(2024, 0, 0, 1, 2, 3, 500_000), Decoder::DateTime(6)),
            "2024-00-00 01:02:03.500000"
        );
        assert_eq!(display_binary(MyValue::Date(2024, 2, 29, 0, 0, 0, 0), Decoder::Date), "2024-02-29T00:00:00Z");
        assert_eq!(
            display_binary(MyValue::Date(2024, 2, 29, 13, 45, 30, 123_000), Decoder::DateTime(3)),
            "2024-02-29T13:45:30.123Z"
        );
        assert_eq!(
            display_binary(MyValue::Date(2024, 2, 29, 13, 45, 30, 123_456), Decoder::DateTime(0)),
            "2024-02-29T13:45:30.123456Z",
            "a fraction the column doesn't declare still shows"
        );
    }
}
