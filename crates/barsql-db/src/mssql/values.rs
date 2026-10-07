// SQL Server values as cells. Dates and times arrive as day and tick counts and are written in RFC 3339, like the
// other engines' timestamps.

use std::fmt::Write as _;

use jiff::ToSpan;
use jiff::civil::Date as CivilDate;
use tiberius::numeric::Numeric;
use tiberius::time::{DateTime2, Time};
use tiberius::{ColumnData, ColumnType};

use crate::ChunkBuilder;
use crate::display::{push_bytes, push_float, push_float_f32};
use crate::postgres::MAX_SAFE_INTEGER;
use crate::temporal::Timestamp;

const NANOS_PER_SECOND: i64 = 1_000_000_000;

pub(crate) fn push(data: &ColumnData<'_>, column: ColumnType, out: &mut ChunkBuilder) {
    match data {
        ColumnData::U8(Some(v)) => out.push_number(|s| push_display(v, s)),
        ColumnData::I16(Some(v)) => out.push_number(|s| push_display(v, s)),
        ColumnData::I32(Some(v)) => out.push_number(|s| push_display(v, s)),
        ColumnData::I64(Some(v)) if i128::from(*v).abs() <= MAX_SAFE_INTEGER => out.push_number(|s| push_display(v, s)),
        // Past 2^53 a number would lose digits in the grid.
        ColumnData::I64(Some(v)) => out.push_text(|s| push_display(v, s)),
        ColumnData::F32(Some(v)) => out.push_number(|s| push_float_f32(*v, s)),
        // Money comes as a float scaled from its integer, so 15 digits are exact. SQL Server shows four decimals.
        // Text, like every engine's decimals.
        ColumnData::F64(Some(v)) if matches!(column, ColumnType::Money | ColumnType::Money4) => out.push_text(|s| {
            let _ = write!(s, "{v:.4}");
        }),
        ColumnData::F64(Some(v)) => out.push_number(|s| push_float(*v, s)),
        ColumnData::Bit(Some(v)) => out.push_bool(*v),
        ColumnData::String(Some(text)) => out.push_text(|s| s.push_str(text)),
        // SQL Server writes them in capitals.
        ColumnData::Guid(Some(id)) => out.push_text(|s| s.push_str(&id.hyphenated().to_string().to_uppercase())),
        ColumnData::Binary(Some(bytes)) => out.push_text(|s| push_bytes(bytes, s)),
        ColumnData::Numeric(Some(n)) => out.push_text(|s| push_numeric(*n, s)),
        ColumnData::Xml(Some(xml)) => out.push_text(|s| s.push_str(xml.as_ref().as_ref())),
        ColumnData::DateTime(Some(dt)) => {
            // Ticks of 1/300 s, rounded to the millisecond as SQL Server shows them.
            let millis = (i64::from(dt.seconds_fragments()) * 1000 + 150) / 300;
            push_timestamp(from_1900(dt.days()), millis * 1_000_000, out);
        }
        // Minutes since midnight, whatever tiberius calls the field.
        ColumnData::SmallDateTime(Some(dt)) => push_timestamp(
            from_1900(i32::from(dt.days())),
            i64::from(dt.seconds_fragments()) * 60 * NANOS_PER_SECOND,
            out,
        ),
        ColumnData::Date(Some(date)) => push_timestamp(from_year_one(date.days()), 0, out),
        ColumnData::Time(Some(time)) => out.push_text(|s| push_time(nanos(*time), s)),
        ColumnData::DateTime2(Some(dt)) => push_timestamp(from_year_one(dt.date().days()), nanos(dt.time()), out),
        // The date and time are UTC, shown at their own offset.
        ColumnData::DateTimeOffset(Some(dto)) => {
            let offset = i64::from(dto.offset()) * 60;
            match local(dto.datetime2(), offset) {
                Some(ts) => out.push_text(|s| ts.push_rfc3339_nano(offset as i32, s)),
                None => out.push_null(),
            }
        }
        ColumnData::U8(None)
        | ColumnData::I16(None)
        | ColumnData::I32(None)
        | ColumnData::I64(None)
        | ColumnData::F32(None)
        | ColumnData::F64(None)
        | ColumnData::Bit(None)
        | ColumnData::String(None)
        | ColumnData::Guid(None)
        | ColumnData::Binary(None)
        | ColumnData::Numeric(None)
        | ColumnData::Xml(None)
        | ColumnData::DateTime(None)
        | ColumnData::SmallDateTime(None)
        | ColumnData::Date(None)
        | ColumnData::Time(None)
        | ColumnData::DateTime2(None)
        | ColumnData::DateTimeOffset(None) => out.push_null(),
    }
}

fn push_display(value: impl std::fmt::Display, out: &mut String) {
    let _ = write!(out, "{value}");
}

// Exact, with every digit of the scale, like `12.50` for a decimal(10, 2).
pub(crate) fn push_numeric(n: Numeric, out: &mut String) {
    let scale = usize::from(n.scale());
    if n.value() < 0 {
        out.push('-');
    }
    let digits = n.value().unsigned_abs().to_string();
    if scale == 0 {
        out.push_str(&digits);
        return;
    }
    let padded = format!("{digits:0>width$}", width = scale + 1);
    let (whole, fraction) = padded.split_at(padded.len() - scale);
    let _ = write!(out, "{whole}.{fraction}");
}

// datetime and smalldatetime count days from 1900-01-01, date and datetime2 from 0001-01-01.
fn from_1900(days: i32) -> Option<CivilDate> {
    CivilDate::new(1900, 1, 1).ok()?.checked_add(i64::from(days).days()).ok()
}

fn from_year_one(days: u32) -> Option<CivilDate> {
    CivilDate::new(1, 1, 1).ok()?.checked_add(i64::from(days).days()).ok()
}

// Since midnight, from the time's 10^-scale second increments.
fn nanos(time: Time) -> i64 {
    let scale = u32::from(time.scale().min(9));
    time.increments() as i64 * 10_i64.pow(9 - scale)
}

fn timestamp(date: CivilDate, nanos: i64) -> Timestamp {
    let seconds = nanos / NANOS_PER_SECOND;
    Timestamp {
        year: i32::from(date.year()),
        month: date.month(),
        day: date.day(),
        hour: (seconds / 3600) as i8,
        minute: (seconds / 60 % 60) as i8,
        second: (seconds % 60) as i8,
        nanos: (nanos % NANOS_PER_SECOND) as i32,
        offset: None,
    }
}

fn push_timestamp(date: Option<CivilDate>, nanos: i64, out: &mut ChunkBuilder) {
    match date {
        Some(date) => out.push_text(|s| timestamp(date, nanos).push_rfc3339_nano(0, s)),
        None => out.push_null(),
    }
}

// The wall clock at `offset` seconds from UTC.
fn local(utc: DateTime2, offset: i64) -> Option<Timestamp> {
    let date = from_year_one(utc.date().days())?;
    let shifted = nanos(utc.time()) + offset * NANOS_PER_SECOND;
    let day = NANOS_PER_SECOND * 86_400;
    let date = date.checked_add(shifted.div_euclid(day).days()).ok()?;
    Some(timestamp(date, shifted.rem_euclid(day)))
}

// `13:45:30.25`, trailing zeros of the fraction dropped.
fn push_time(nanos: i64, out: &mut String) {
    let seconds = nanos / NANOS_PER_SECOND;
    let _ = write!(out, "{:02}:{:02}:{:02}", seconds / 3600, seconds / 60 % 60, seconds % 60);
    let fraction = nanos % NANOS_PER_SECOND;
    if fraction > 0 {
        let digits = format!("{fraction:09}");
        out.push('.');
        out.push_str(digits.trim_end_matches('0'));
    }
}

// The declared type, for the column header. `Datetimen` only says which on its first value.
pub(crate) fn type_name(column: ColumnType, first: Option<&ColumnData<'_>>) -> &'static str {
    match column {
        ColumnType::Null => "null",
        ColumnType::Bit | ColumnType::Bitn => "bit",
        ColumnType::Int1 => "tinyint",
        ColumnType::Int2 => "smallint",
        ColumnType::Int4 => "int",
        ColumnType::Int8 => "bigint",
        ColumnType::Intn => match first {
            Some(ColumnData::U8(_)) => "tinyint",
            Some(ColumnData::I16(_)) => "smallint",
            Some(ColumnData::I64(_)) => "bigint",
            _ => "int",
        },
        ColumnType::Datetime4 => "smalldatetime",
        ColumnType::Datetime => "datetime",
        ColumnType::Datetimen => match first {
            Some(ColumnData::SmallDateTime(_)) => "smalldatetime",
            _ => "datetime",
        },
        ColumnType::Float4 => "real",
        ColumnType::Float8 => "float",
        ColumnType::Floatn => match first {
            Some(ColumnData::F32(_)) => "real",
            _ => "float",
        },
        ColumnType::Money => "money",
        ColumnType::Money4 => "smallmoney",
        ColumnType::Guid => "uniqueidentifier",
        ColumnType::Decimaln => "decimal",
        ColumnType::Numericn => "numeric",
        ColumnType::Daten => "date",
        ColumnType::Timen => "time",
        ColumnType::Datetime2 => "datetime2",
        ColumnType::DatetimeOffsetn => "datetimeoffset",
        ColumnType::BigVarBin => "varbinary",
        ColumnType::BigVarChar => "varchar",
        ColumnType::BigBinary => "binary",
        ColumnType::BigChar => "char",
        ColumnType::NVarchar => "nvarchar",
        ColumnType::NChar => "nchar",
        ColumnType::Xml => "xml",
        ColumnType::Udt => "udt",
        ColumnType::Text => "text",
        ColumnType::Image => "image",
        ColumnType::NText => "ntext",
        ColumnType::SSVariant => "sql_variant",
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use tiberius::time::{Date, DateTime, DateTimeOffset, SmallDateTime};
    use tiberius::xml::XmlData;
    use tiberius::{ColumnData, ColumnType, Uuid};

    use super::*;

    fn show(data: ColumnData<'_>, column: ColumnType) -> Option<String> {
        let mut builder = ChunkBuilder::new(1, 1);
        push(&data, column, &mut builder);
        builder.end_row();
        builder.finish().display(0, 0).map(str::to_string)
    }

    fn shown(data: ColumnData<'_>) -> String {
        show(data, ColumnType::Null).expect("a value")
    }

    #[test]
    fn numbers_keep_their_digits() {
        assert_eq!(shown(ColumnData::U8(Some(255))), "255");
        assert_eq!(shown(ColumnData::I64(Some(9_007_199_254_740_993))), "9007199254740993");
        assert_eq!(shown(ColumnData::F32(Some(1.1))), "1.1");
        assert_eq!(shown(ColumnData::F64(Some(0.1 + 0.2))), "0.30000000000000004");
        assert_eq!(show(ColumnData::F64(Some(12.5)), ColumnType::Money).as_deref(), Some("12.5000"));
        assert_eq!(shown(ColumnData::Numeric(Some(Numeric::new_with_scale(1250, 2)))), "12.50");
        assert_eq!(shown(ColumnData::Numeric(Some(Numeric::new_with_scale(-5, 3)))), "-0.005");
        assert_eq!(shown(ColumnData::Numeric(Some(Numeric::new_with_scale(42, 0)))), "42");
        assert_eq!(show(ColumnData::I32(None), ColumnType::Int4), None);
    }

    #[test]
    fn text_bytes_and_ids() {
        assert_eq!(shown(ColumnData::Bit(Some(true))), "true");
        assert_eq!(shown(ColumnData::String(Some(Cow::Borrowed("héllo")))), "héllo");
        assert_eq!(shown(ColumnData::Binary(Some(Cow::Borrowed(&[0xde, 0xad, 0xbe, 0xef])))), "\\xdeadbeef");
        let id = Uuid::parse_str("6f9619ff-8b86-d011-b42d-00c04fc964ff").unwrap();
        assert_eq!(shown(ColumnData::Guid(Some(id))), "6F9619FF-8B86-D011-B42D-00C04FC964FF");
        assert_eq!(shown(ColumnData::Xml(Some(Cow::Owned(XmlData::new("<a/>"))))), "<a/>");
    }

    #[test]
    fn dates_and_times_read_as_rfc_3339() {
        // 2024-02-29 is day 45349 from 1900-01-01; 13:45:30.123 is 14_859_037 ticks of 1/300 s.
        assert_eq!(shown(ColumnData::DateTime(Some(DateTime::new(45_349, 14_859_037)))), "2024-02-29T13:45:30.123Z");
        assert_eq!(shown(ColumnData::DateTime(Some(DateTime::new(-53_690, 0)))), "1753-01-01T00:00:00Z");
        assert_eq!(shown(ColumnData::SmallDateTime(Some(SmallDateTime::new(45_349, 825)))), "2024-02-29T13:45:00Z");
        // 0001-01-01 + 738_944 days.
        let date = Date::new(738_944);
        assert_eq!(shown(ColumnData::Date(Some(date))), "2024-02-29T00:00:00Z");
        let time = Time::new(495_301_234_567, 7);
        assert_eq!(shown(ColumnData::Time(Some(time))), "13:45:30.1234567");
        assert_eq!(shown(ColumnData::Time(Some(Time::new(0, 0)))), "00:00:00");
        assert_eq!(shown(ColumnData::DateTime2(Some(DateTime2::new(date, time)))), "2024-02-29T13:45:30.1234567Z");
        // Stored as 23:30 UTC, shown at +02:00 on the next day.
        let utc = DateTime2::new(date, Time::new(84_600, 0));
        assert_eq!(shown(ColumnData::DateTimeOffset(Some(DateTimeOffset::new(utc, 120)))), "2024-03-01T01:30:00+02:00");
        assert_eq!(
            shown(ColumnData::DateTimeOffset(Some(DateTimeOffset::new(utc, -330)))),
            "2024-02-29T18:00:00-05:30"
        );
    }

    #[test]
    fn type_names_follow_the_declared_width() {
        assert_eq!(type_name(ColumnType::Intn, Some(&ColumnData::I16(Some(1)))), "smallint");
        assert_eq!(type_name(ColumnType::Intn, None), "int");
        assert_eq!(type_name(ColumnType::Datetimen, Some(&ColumnData::SmallDateTime(None))), "smalldatetime");
        assert_eq!(type_name(ColumnType::NVarchar, None), "nvarchar");
    }
}
