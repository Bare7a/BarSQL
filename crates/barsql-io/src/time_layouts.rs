struct Cursor<'a> {
    s: &'a [u8],
    i: usize,
}

impl Cursor<'_> {
    fn rest(&self) -> &[u8] {
        &self.s[self.i..]
    }

    fn done(&self) -> bool {
        self.i == self.s.len()
    }

    fn lit(&mut self, c: u8) -> Option<()> {
        (self.s.get(self.i) == Some(&c)).then(|| self.i += 1)
    }

    fn digit(&self, at: usize) -> bool {
        self.s.get(self.i + at).is_some_and(u8::is_ascii_digit)
    }

    // Four characters, which may include a leading sign.
    fn year(&mut self) -> Option<i32> {
        let chunk = std::str::from_utf8(self.s.get(self.i..self.i + 4)?).ok()?;
        let digits = chunk.strip_prefix(['+', '-']).unwrap_or(chunk);
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        self.i += 4;
        chunk.parse().ok()
    }

    // Two digits when fixed, else one or two.
    fn num(&mut self, fixed: bool) -> Option<i32> {
        if !self.digit(0) {
            return None;
        }
        let first = i32::from(self.s[self.i] - b'0');
        if !self.digit(1) {
            if fixed {
                return None;
            }
            self.i += 1;
            return Some(first);
        }
        let n = first * 10 + i32::from(self.s[self.i + 1] - b'0');
        self.i += 2;
        Some(n)
    }

    // '.' or ',' then any number of digits.
    fn fraction(&mut self) {
        if matches!(self.s.get(self.i), Some(b'.' | b',')) && self.digit(1) {
            self.i += 1;
            while self.digit(0) {
                self.i += 1;
            }
        }
    }

    // Z, or a sign with hh:mm.
    fn zone(&mut self) -> Option<()> {
        if self.lit(b'Z').is_some() {
            return Some(());
        }
        let rest = self.rest();
        if rest.len() < 6 || !matches!(rest[0], b'+' | b'-') || rest[3] != b':' {
            return None;
        }
        let two = |a: u8, b: u8| {
            (a.is_ascii_digit() && b.is_ascii_digit()).then(|| i32::from(a - b'0') * 10 + i32::from(b - b'0'))
        };
        let (hh, mm) = (two(rest[1], rest[2])?, two(rest[4], rest[5])?);
        if hh > 24 || mm > 60 {
            return None;
        }
        self.i += 6;
        Some(())
    }
}

fn days_in(month: i32, year: i32) -> i32 {
    match month {
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

fn valid_date(year: i32, month: i32, day: i32) -> bool {
    (1..=12).contains(&month) && day >= 1 && day <= days_in(month, year)
}

#[derive(Clone, Copy)]
enum DateOrder {
    Ymd(u8),
    Dmy,
    Mdy,
}

fn parse_date(c: &mut Cursor<'_>, order: DateOrder) -> Option<(i32, i32, i32)> {
    match order {
        DateOrder::Ymd(sep) => {
            let year = c.year()?;
            c.lit(sep)?;
            let month = c.num(true)?;
            c.lit(sep)?;
            let day = c.num(true)?;
            Some((year, month, day))
        }
        DateOrder::Dmy => {
            let day = c.num(true)?;
            c.lit(b'/')?;
            let month = c.num(true)?;
            c.lit(b'/')?;
            Some((c.year()?, month, day))
        }
        DateOrder::Mdy => {
            let month = c.num(true)?;
            c.lit(b'/')?;
            let day = c.num(true)?;
            c.lit(b'/')?;
            Some((c.year()?, month, day))
        }
    }
}

// YYYY-MM-DD, YYYY/MM/DD, DD/MM/YYYY, MM/DD/YYYY.
pub fn matches_date(v: &str) -> bool {
    [DateOrder::Ymd(b'-'), DateOrder::Ymd(b'/'), DateOrder::Dmy, DateOrder::Mdy].into_iter().any(|order| {
        let mut c = Cursor { s: v.as_bytes(), i: 0 };
        parse_date(&mut c, order).is_some_and(|(y, m, d)| c.done() && valid_date(y, m, d))
    })
}

// YYYY-MM-DD, ' ' or 'T', then h:mm:ss with an optional fraction. Only the 'T' form may end in Z or +/-hh:mm.
pub fn matches_timestamp(v: &str) -> bool {
    [(b' ', false), (b'T', false), (b'T', true)].into_iter().any(|(sep, zone)| {
        let mut c = Cursor { s: v.as_bytes(), i: 0 };
        let parsed = (|| {
            let (y, m, d) = parse_date(&mut c, DateOrder::Ymd(b'-'))?;
            c.lit(sep)?;
            let hour = c.num(false)?;
            c.lit(b':')?;
            let minute = c.num(true)?;
            c.lit(b':')?;
            let second = c.num(true)?;
            c.fraction();
            if zone {
                c.zone()?;
            }
            (valid_date(y, m, d) && hour < 24 && minute < 60 && second < 60).then_some(())
        })();
        parsed.is_some() && c.done()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_date_layouts() {
        for ok in ["2026-01-02", "2024/02/29", "29/02/2024", "02/29/2024", "13/01/2024"] {
            assert!(matches_date(ok), "{ok}");
        }
        for bad in ["2026-1-02", "2023-02-29", "2026-13-01", "02/30/2024", "2026-01-02 10:00", "nope"] {
            assert!(!matches_date(bad), "{bad}");
        }
    }

    #[test]
    fn matches_timestamp_layouts() {
        for ok in [
            "2026-01-02 15:04:05",
            "2026-01-02T15:04:05",
            "2026-01-02T15:04:05Z",
            "2026-01-02T15:04:05+02:00",
            "2026-01-02 15:04:05.123456789",
            "2026-01-02 9:04:05",
            "2024-02-29T13:45:30,5Z",
        ] {
            assert!(matches_timestamp(ok), "{ok}");
        }
        for bad in [
            "2026-01-02",
            "2026-01-02 15:04",
            "2026-01-02 24:00:00",
            "2026-01-02 15:04:05Z",
            "2026-01-02T15:04:05+2:00",
        ] {
            assert!(!matches_timestamp(bad), "{bad}");
        }
    }
}
