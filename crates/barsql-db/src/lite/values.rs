use std::fmt::Write as _;

use super::LiteRef;
use crate::ChunkBuilder;
use crate::display::{push_bytes, push_float};
use crate::postgres::MAX_SAFE_INTEGER;
use crate::temporal::Timestamp;

// Only TEXT in these columns parses as a time. Integers stay numbers.
pub fn is_time_type(decl_upper: &str) -> bool {
    matches!(decl_upper, "DATE" | "DATETIME" | "TIMESTAMP")
}

pub fn push(value: LiteRef<'_>, time_column: bool, out: &mut ChunkBuilder) {
    match value {
        LiteRef::Null => out.push_null(),
        LiteRef::Integer(i) if i128::from(i).abs() <= MAX_SAFE_INTEGER => out.push_number(|s| {
            let _ = write!(s, "{i}");
        }),
        LiteRef::Integer(i) => out.push_text(|s| {
            let _ = write!(s, "{i}");
        }),
        LiteRef::Real(f) => out.push_number(|s| push_float(f, s)),
        LiteRef::Text(bytes) => {
            let text = String::from_utf8_lossy(bytes);
            match time_column.then(|| parse_time(&text)).flatten() {
                Some(rfc3339) => out.push_text(|s| s.push_str(&rfc3339)),
                None => out.push_text(|s| s.push_str(&text)),
            }
        }
        LiteRef::Blob(bytes) => out.push_text(|s| push_bytes(bytes, s)),
    }
}

const LAYOUTS: [(char, bool, bool); 7] = [
    // (date/time separator, has seconds and optional fraction, has offset)
    (' ', true, true),
    ('T', true, true),
    (' ', true, false),
    ('T', true, false),
    (' ', false, false),
    ('T', false, false),
    ('\0', false, false),
];

// Tries the zoned string form, then SQLite's time formats 1-7. Output is RFC3339Nano.
pub fn parse_time(s: &str) -> Option<String> {
    if let Some(time) = parse_zoned_string(s) {
        return Some(time);
    }
    let trimmed = s.strip_suffix('Z').unwrap_or(s);
    LAYOUTS.iter().find_map(|&(sep, seconds, offset)| parse_layout(trimmed, sep, seconds, offset)).map(
        |(ts, offset)| {
            let mut out = String::new();
            ts.push_rfc3339_nano(offset, &mut out);
            out
        },
    )
}

struct Cursor<'a> {
    s: &'a [u8],
    i: usize,
}

impl Cursor<'_> {
    fn digits(&mut self, n: usize) -> Option<i32> {
        let end = self.i + n;
        let chunk = self.s.get(self.i..end)?;
        if !chunk.iter().all(u8::is_ascii_digit) {
            return None;
        }
        self.i = end;
        Some(chunk.iter().fold(0, |acc, d| acc * 10 + i32::from(d - b'0')))
    }

    // Only the hour may be a single digit.
    fn one_or_two(&mut self) -> Option<i32> {
        let first = *self.s.get(self.i)?;
        if !first.is_ascii_digit() {
            return None;
        }
        match self.s.get(self.i + 1) {
            Some(d) if d.is_ascii_digit() => self.digits(2),
            _ => self.digits(1),
        }
    }

    fn expect(&mut self, c: u8) -> Option<()> {
        (self.s.get(self.i) == Some(&c)).then(|| self.i += 1)
    }

    fn done(&self) -> bool {
        self.i == self.s.len()
    }
}

fn parse_layout(s: &str, sep: char, seconds: bool, offset: bool) -> Option<(Timestamp, i32)> {
    let mut c = Cursor { s: s.as_bytes(), i: 0 };
    let year = c.digits(4)?;
    c.expect(b'-')?;
    let month = c.digits(2)?;
    c.expect(b'-')?;
    let day = c.digits(2)?;
    let mut ts =
        Timestamp { year, month: month as i8, day: day as i8, hour: 0, minute: 0, second: 0, nanos: 0, offset: None };
    if sep != '\0' {
        c.expect(sep as u8)?;
        ts.hour = c.one_or_two()? as i8;
        c.expect(b':')?;
        ts.minute = c.digits(2)? as i8;
        if seconds {
            c.expect(b':')?;
            ts.second = c.digits(2)? as i8;
            ts.nanos = fraction(&mut c);
        }
    }
    let mut zone = 0;
    if offset {
        let sign = match c.s.get(c.i)? {
            b'+' => 1,
            b'-' => -1,
            _ => return None,
        };
        c.i += 1;
        let hh = c.digits(2)?;
        c.expect(b':')?;
        let mm = c.digits(2)?;
        if hh > 24 || mm > 60 {
            return None;
        }
        zone = sign * (hh * 3600 + mm * 60);
    }
    if !c.done() || !valid(&ts) {
        return None;
    }
    Some((ts, zone))
}

// '.' or ',' then any number of digits, truncated to nanoseconds. 0 when absent.
fn fraction(c: &mut Cursor<'_>) -> i32 {
    let (Some(sep), Some(first)) = (c.s.get(c.i), c.s.get(c.i + 1)) else { return 0 };
    if !matches!(sep, b'.' | b',') || !first.is_ascii_digit() {
        return 0;
    }
    c.i += 1;
    let start = c.i;
    while c.s.get(c.i).is_some_and(u8::is_ascii_digit) {
        c.i += 1;
    }
    let digits: String =
        c.s[start..c.i].iter().take(9).map(|d| *d as char).chain(std::iter::repeat('0')).take(9).collect();
    digits.parse().unwrap_or(0)
}

fn valid(ts: &Timestamp) -> bool {
    let leap = ts.year % 4 == 0 && (ts.year % 100 != 0 || ts.year % 400 == 0);
    let days = match ts.month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    (1..=days).contains(&ts.day)
        && (0..24).contains(&ts.hour)
        && (0..60).contains(&ts.minute)
        && (0..60).contains(&ts.second)
}

// Parses "2024-02-29 13:45:30.123456789 +0200 EET", ignoring anything from "m=" on.
fn parse_zoned_string(s: &str) -> Option<String> {
    let s = match s.find("m=") {
        Some(x) if x > 0 => &s[..x],
        _ => s,
    };
    let s = s.trim();
    let mut parts = s.split(' ');
    let (date, clock, zone, abbr) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some()
        || abbr.is_empty()
        || !abbr.chars().all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-')
    {
        return None;
    }
    let (mut ts, _) = parse_layout(&format!("{date} {clock}"), ' ', true, false)?;
    let bytes = zone.as_bytes();
    if bytes.len() != 5 || !matches!(bytes[0], b'+' | b'-') || !bytes[1..].iter().all(u8::is_ascii_digit) {
        return None;
    }
    let hh: i32 = zone[1..3].parse().ok()?;
    let mm: i32 = zone[3..5].parse().ok()?;
    let offset = if bytes[0] == b'-' { -(hh * 3600 + mm * 60) } else { hh * 3600 + mm * 60 };
    ts.offset = None;
    let mut out = String::new();
    ts.push_rfc3339_nano(offset, &mut out);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_time_text() {
        let cases = [
            ("2024-02-29", Some("2024-02-29T00:00:00Z")),
            ("2024-02-29 13:45:30.123", Some("2024-02-29T13:45:30.123Z")),
            ("2024-02-29 13:45", Some("2024-02-29T13:45:00Z")),
            ("2024-02-29T13:45:30Z", Some("2024-02-29T13:45:30Z")),
            ("2024-02-29 13:45:30+02:00", Some("2024-02-29T13:45:30+02:00")),
            ("2024-02-29 13:45:30-00:00", Some("2024-02-29T13:45:30Z")),
            ("2024-02-29 9:45:30", Some("2024-02-29T09:45:30Z")),
            ("2024-02-29 13:45:30,5", Some("2024-02-29T13:45:30.5Z")),
            ("2024-02-29 13:45:30.1234567891", Some("2024-02-29T13:45:30.123456789Z")),
            ("2024-02-29 13:45:30 +0200 EET m=+0.001", Some("2024-02-29T13:45:30+02:00")),
            ("2023-02-29", None),
            ("2024-13-01", None),
            ("2024-02-29 24:00:00", None),
            ("not a date", None),
            ("13:45:30", None),
            ("2024-2-29", None),
        ];
        for (input, want) in cases {
            assert_eq!(parse_time(input).as_deref(), want, "{input}");
        }
    }
}
