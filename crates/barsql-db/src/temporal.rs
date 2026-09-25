use std::fmt::Write as _;

use jiff::civil::DateTime;
use jiff::tz::{Offset, TimeZone};

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Timestamp {
    pub(crate) year: i32,
    pub(crate) month: i8,
    pub(crate) day: i8,
    pub(crate) hour: i8,
    pub(crate) minute: i8,
    pub(crate) second: i8,
    pub(crate) nanos: i32,
    pub(crate) offset: Option<i32>,
}

impl Timestamp {
    pub(crate) fn in_zone(self, zone: &TimeZone) -> Option<(Timestamp, i32)> {
        let civil =
            DateTime::new(self.year as i16, self.month, self.day, self.hour, self.minute, self.second, self.nanos)
                .ok()?;
        let instant = Offset::from_seconds(self.offset.unwrap_or(0)).ok()?.to_timestamp(civil).ok()?;
        let zoned = instant.to_zoned(zone.clone());
        let local = zoned.datetime();
        let converted = Timestamp {
            year: i32::from(local.year()),
            month: local.month(),
            day: local.day(),
            hour: local.hour(),
            minute: local.minute(),
            second: local.second(),
            nanos: local.subsec_nanosecond(),
            offset: None,
        };
        Some((converted, zoned.offset().seconds()))
    }

    pub(crate) fn push_rfc3339_nano(self, offset: i32, out: &mut String) {
        if self.year < 0 {
            let _ = write!(out, "-{:04}", -self.year);
        } else {
            let _ = write!(out, "{:04}", self.year);
        }
        let _ =
            write!(out, "-{:02}-{:02}T{:02}:{:02}:{:02}", self.month, self.day, self.hour, self.minute, self.second);
        if self.nanos > 0 {
            let fraction = format!("{:09}", self.nanos);
            out.push('.');
            out.push_str(fraction.trim_end_matches('0'));
        }
        if offset == 0 {
            out.push('Z');
        } else {
            let sign = if offset < 0 { '-' } else { '+' };
            let abs = offset.unsigned_abs();
            let _ = write!(out, "{sign}{:02}:{:02}", abs / 3600, abs % 3600 / 60);
        }
    }
}

pub(crate) fn parse_timestamp(text: &str) -> Option<Timestamp> {
    let (body, bc) = match text.strip_suffix(" BC") {
        Some(body) => (body, true),
        None => (text, false),
    };
    let (date, time) = body.split_once(' ').unwrap_or((body, ""));
    let mut parts = date.splitn(3, '-');
    let year: i32 = parts.next()?.parse().ok()?;
    let month: i8 = parts.next()?.parse().ok()?;
    let day: i8 = parts.next()?.parse().ok()?;
    let mut ts = Timestamp {
        year: if bc { 1 - year } else { year },
        month,
        day,
        hour: 0,
        minute: 0,
        second: 0,
        nanos: 0,
        offset: None,
    };
    if time.is_empty() {
        return Some(ts);
    }
    let (clock, offset) = match time.find(['+', '-']) {
        Some(ix) => (&time[..ix], Some(&time[ix..])),
        None => (time, None),
    };
    let mut hms = clock.splitn(3, ':');
    ts.hour = hms.next()?.parse().ok()?;
    ts.minute = hms.next()?.parse().ok()?;
    let seconds = hms.next()?;
    let (whole, fraction) = seconds.split_once('.').unwrap_or((seconds, ""));
    ts.second = whole.parse().ok()?;
    if !fraction.is_empty() {
        let digits: String = fraction.chars().chain(std::iter::repeat('0')).take(9).collect();
        ts.nanos = digits.parse().ok()?;
    }
    ts.offset = match offset {
        Some(raw) => Some(parse_offset(raw)?),
        None => None,
    };
    Some(ts)
}

fn parse_offset(raw: &str) -> Option<i32> {
    let (sign, rest) = raw.split_at(1);
    let mut parts = rest.split(':');
    let hours: i32 = parts.next()?.parse().ok()?;
    let minutes: i32 = parts.next().map_or(Some(0), |m| m.parse().ok())?;
    let seconds: i32 = parts.next().map_or(Some(0), |s| s.parse().ok())?;
    let total = hours * 3600 + minutes * 60 + seconds;
    Some(if sign == "-" { -total } else { total })
}
