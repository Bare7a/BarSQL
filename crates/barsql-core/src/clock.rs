use jiff::Zoned;

// RFC 3339 in local time with whole seconds. A zero offset prints as "Z".
pub fn now_rfc3339() -> String {
    rfc3339(&Zoned::now())
}

pub fn rfc3339(time: &Zoned) -> String {
    let offset = time.offset().seconds();
    let zone = if offset == 0 {
        "Z".to_string()
    } else {
        let sign = if offset < 0 { '-' } else { '+' };
        let abs = offset.unsigned_abs();
        format!("{sign}{:02}:{:02}", abs / 3600, abs % 3600 / 60)
    };
    format!("{}{zone}", time.strftime("%Y-%m-%dT%H:%M:%S"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;
    use jiff::tz::{Offset, TimeZone};

    #[test]
    fn formats_rfc3339_with_whole_seconds() {
        let utc = date(2024, 2, 29).at(13, 45, 30, 123).to_zoned(TimeZone::UTC).unwrap();
        assert_eq!(rfc3339(&utc), "2024-02-29T13:45:30Z");
        let sofia =
            date(2024, 2, 29).at(13, 45, 30, 0).to_zoned(TimeZone::fixed(Offset::from_hours(2).unwrap())).unwrap();
        assert_eq!(rfc3339(&sofia), "2024-02-29T13:45:30+02:00");
        let nfld =
            date(2024, 2, 29).at(1, 2, 3, 0).to_zoned(TimeZone::fixed(Offset::from_seconds(-12600).unwrap())).unwrap();
        assert_eq!(rfc3339(&nfld), "2024-02-29T01:02:03-03:30");
    }
}
