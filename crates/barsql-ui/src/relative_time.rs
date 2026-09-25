use barsql_sql::lang::text::is_space;
use jiff::{Timestamp, Zoned};

// CLDR phrases, with words like "yesterday" in place of "1 day ago".
// Tested against `fixtures/golden/ui/relative_time.json`.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeBucket {
    Today,
    Yesterday,
    Last7,
    Last30,
    Older,
}

impl TimeBucket {
    pub fn label_key(self) -> &'static str {
        match self {
            Self::Today => "sidebar.today",
            Self::Yesterday => "sidebar.yesterday",
            Self::Last7 => "sidebar.last7days",
            Self::Last30 => "sidebar.last30days",
            Self::Older => "sidebar.older",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unit {
    Second,
    Minute,
    Hour,
    Day,
    Month,
    Year,
}

// Halves round up, where Rust's round goes away from zero.
fn round_half_up(x: f64) -> i64 {
    (x + 0.5).floor() as i64
}

pub fn format_relative_time(iso: &str, lang: &str, now_ms: i64) -> String {
    let Ok(ts) = iso.parse::<Timestamp>() else { return String::new() };
    let diff_sec = round_half_up((ts.as_millisecond() - now_ms) as f64 / 1000.);
    if diff_sec.abs() < 45 {
        return phrase(lang, Unit::Second, diff_sec);
    }
    let diff_min = round_half_up(diff_sec as f64 / 60.);
    if diff_min.abs() < 60 {
        return phrase(lang, Unit::Minute, diff_min);
    }
    let diff_hour = round_half_up(diff_sec as f64 / 3600.);
    if diff_hour.abs() < 24 {
        return phrase(lang, Unit::Hour, diff_hour);
    }
    let diff_day = round_half_up(diff_sec as f64 / 86400.);
    if diff_day.abs() < 30 {
        return phrase(lang, Unit::Day, diff_day);
    }
    let diff_month = round_half_up(diff_day as f64 / 30.);
    if diff_month.abs() <= 12 {
        return phrase(lang, Unit::Month, diff_month);
    }
    phrase(lang, Unit::Year, round_half_up(diff_day as f64 / 365.))
}

fn special(lang: &str, unit: Unit, value: i64) -> Option<&'static str> {
    use Unit::*;
    Some(match (lang, unit, value) {
        ("de", Second, 0) => "jetzt",
        ("de", Minute, 0) => "in dieser Minute",
        ("de", Hour, 0) => "in dieser Stunde",
        ("de", Day, -2) => "vorgestern",
        ("de", Day, -1) => "gestern",
        ("de", Day, 0) => "heute",
        ("de", Day, 1) => "morgen",
        ("de", Day, 2) => "übermorgen",
        ("de", Month, -1) => "letzten Monat",
        ("de", Month, 0) => "diesen Monat",
        ("de", Month, 1) => "nächsten Monat",
        ("de", Year, -1) => "letztes Jahr",
        ("de", Year, 0) => "dieses Jahr",
        ("de", Year, 1) => "nächstes Jahr",
        ("bg", Second, 0) => "сега",
        ("bg", Minute, 0) => "в тази минута",
        ("bg", Hour, 0) => "в този час",
        ("bg", Day, -2) => "онзи ден",
        ("bg", Day, -1) => "вчера",
        ("bg", Day, 0) => "днес",
        ("bg", Day, 1) => "утре",
        ("bg", Day, 2) => "вдругиден",
        ("bg", Month, -1) => "предходен месец",
        ("bg", Month, 0) => "този месец",
        ("bg", Month, 1) => "следващ месец",
        ("bg", Year, -1) => "миналата година",
        ("bg", Year, 0) => "тази година",
        ("bg", Year, 1) => "следващата година",
        (_, Second, 0) => "now",
        (_, Minute, 0) => "this minute",
        (_, Hour, 0) => "this hour",
        (_, Day, -1) => "yesterday",
        (_, Day, 0) => "today",
        (_, Day, 1) => "tomorrow",
        (_, Month, -1) => "last month",
        (_, Month, 0) => "this month",
        (_, Month, 1) => "next month",
        (_, Year, -1) => "last year",
        (_, Year, 0) => "this year",
        (_, Year, 1) => "next year",
        _ => return None,
    })
}

// (singular, plural). German uses the dative plural.
fn nouns(lang: &str, unit: Unit) -> (&'static str, &'static str) {
    use Unit::*;
    match (lang, unit) {
        ("de", Second) => ("Sekunde", "Sekunden"),
        ("de", Minute) => ("Minute", "Minuten"),
        ("de", Hour) => ("Stunde", "Stunden"),
        ("de", Day) => ("Tag", "Tagen"),
        ("de", Month) => ("Monat", "Monaten"),
        ("de", Year) => ("Jahr", "Jahren"),
        ("bg", Second) => ("секунда", "секунди"),
        ("bg", Minute) => ("минута", "минути"),
        ("bg", Hour) => ("час", "часа"),
        ("bg", Day) => ("ден", "дни"),
        ("bg", Month) => ("месец", "месеца"),
        ("bg", Year) => ("година", "години"),
        (_, Second) => ("second", "seconds"),
        (_, Minute) => ("minute", "minutes"),
        (_, Hour) => ("hour", "hours"),
        (_, Day) => ("day", "days"),
        (_, Month) => ("month", "months"),
        (_, Year) => ("year", "years"),
    }
}

fn phrase(lang: &str, unit: Unit, value: i64) -> String {
    if let Some(word) = special(lang, unit, value) {
        return word.to_string();
    }
    let n = value.unsigned_abs();
    let (one, other) = nouns(lang, unit);
    let noun = if n == 1 { one } else { other };
    match (lang, value < 0) {
        ("de", true) => format!("vor {n} {noun}"),
        ("de", false) => format!("in {n} {noun}"),
        ("bg", true) => format!("преди {n} {noun}"),
        ("bg", false) => format!("след {n} {noun}"),
        (_, true) => format!("{n} {noun} ago"),
        (_, false) => format!("in {n} {noun}"),
    }
}

// Local calendar days, then 7 and 30 days back from the start of today.
pub fn time_bucket(iso: &str, now: &Zoned) -> TimeBucket {
    let Ok(ts) = iso.parse::<Timestamp>() else { return TimeBucket::Older };
    let Ok(today) = now.start_of_day() else { return TimeBucket::Older };
    let start = today.timestamp().as_millisecond();
    let ts = ts.as_millisecond();
    const DAY: i64 = 86_400_000;
    if ts >= start {
        TimeBucket::Today
    } else if ts >= start - DAY {
        TimeBucket::Yesterday
    } else if ts >= start - 7 * DAY {
        TimeBucket::Last7
    } else if ts >= start - 30 * DAY {
        TimeBucket::Last30
    } else {
        TimeBucket::Older
    }
}

// Collapses whitespace and cuts at `max` UTF-16 units, adding an ellipsis.
pub fn one_line_preview(sql: &str, max: usize) -> String {
    let mut out = String::with_capacity(sql.len().min(max * 4));
    let mut pending_space = false;
    for c in sql.chars() {
        if is_space(c) {
            pending_space = !out.is_empty();
            continue;
        }
        if pending_space {
            out.push(' ');
            pending_space = false;
        }
        out.push(c);
    }
    if out.encode_utf16().count() <= max {
        return out;
    }
    let mut units = 0;
    let mut cut = String::new();
    for c in out.chars() {
        units += c.len_utf16();
        if units > max {
            break;
        }
        cut.push(c);
    }
    cut.push('…');
    cut
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOLDEN: &str = include_str!("../../../fixtures/golden/ui/relative_time.json");

    #[test]
    fn relative_times_match_the_golden_file() {
        let fixture: serde_json::Value = serde_json::from_str(GOLDEN).unwrap();
        let now: Timestamp = fixture["now"].as_str().unwrap().parse().unwrap();
        let locales: Vec<&str> = fixture["locales"].as_array().unwrap().iter().map(|l| l.as_str().unwrap()).collect();
        let mut checked = 0;
        for row in fixture["rows"].as_array().unwrap() {
            let offset = row[0].as_i64().unwrap();
            let iso = Timestamp::from_millisecond(now.as_millisecond() + offset * 1000).unwrap().to_string();
            for (i, locale) in locales.iter().enumerate() {
                let want = row[i + 1].as_str().unwrap();
                assert_eq!(format_relative_time(&iso, locale, now.as_millisecond()), want, "{locale} {offset}s");
                checked += 1;
            }
        }
        assert!(checked > 1000, "{checked}");
    }

    #[test]
    fn unparsable_times_are_blank_and_older() {
        let now: Zoned = "2026-06-15T12:00:00+03:00[Europe/Sofia]".parse().unwrap();
        assert_eq!(format_relative_time("yesterday", "en", 0), "");
        assert_eq!(time_bucket("", &now), TimeBucket::Older);
    }

    #[test]
    fn buckets_follow_local_calendar_days() {
        let now: Zoned = "2026-06-15T01:00:00+03:00[Europe/Sofia]".parse().unwrap();
        let bucket = |iso: &str| time_bucket(iso, &now);
        assert_eq!(bucket("2026-06-15T00:00:00+03:00"), TimeBucket::Today);
        assert_eq!(bucket("2026-06-14T23:59:59+03:00"), TimeBucket::Yesterday);
        assert_eq!(bucket("2026-06-14T20:59:59Z"), TimeBucket::Yesterday);
        assert_eq!(bucket("2026-06-13T23:59:59+03:00"), TimeBucket::Last7);
        assert_eq!(bucket("2026-06-08T00:00:00+03:00"), TimeBucket::Last7);
        assert_eq!(bucket("2026-06-07T23:59:59+03:00"), TimeBucket::Last30);
        assert_eq!(bucket("2026-05-16T00:00:00+03:00"), TimeBucket::Last30);
        assert_eq!(bucket("2026-05-15T23:59:59+03:00"), TimeBucket::Older);
    }

    #[test]
    fn previews_collapse_whitespace_and_cut_with_an_ellipsis() {
        assert_eq!(one_line_preview("  SELECT *\n\tFROM  t  ", 120), "SELECT * FROM t");
        assert_eq!(one_line_preview("abcdef", 3), "abc…");
        assert_eq!(one_line_preview("abc", 3), "abc");
        assert_eq!(one_line_preview("a🌍b", 2), "a…");
        assert_eq!(one_line_preview("", 5), "");
    }
}
