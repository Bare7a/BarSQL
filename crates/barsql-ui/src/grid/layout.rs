// Column widths are in `ch`, the advance width of '0'.

use std::borrow::Cow;

// Top and bottom padding plus line height, in rem. row_height adds the 1px border.
const ROW_HEIGHT_REM: f32 = 0.308 * 2. + 1.231;
pub const SAMPLE_ROWS: usize = 80;
const COL_MIN_CH: usize = 8;
pub const COL_MAX_CH: usize = 50;
const COL_PAD_CH: usize = 3;
// Room for the header's sort chevron.
const COL_HEADER_ICON_CH: usize = 4;

pub fn row_height(zoom_px: f32) -> f32 {
    (zoom_px * ROW_HEIGHT_REM).round() + 1.
}

fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

// A plain decimal's point and the digits after it, so a column's numbers can line their points up. Zero for an
// integer, or for a form like 1.5e-7 that doesn't end in its fraction.
pub fn fraction_len(text: &str) -> usize {
    match text.find('.') {
        Some(dot) if text[dot + 1..].bytes().all(|b| b.is_ascii_digit()) => text.len() - dot,
        _ => 0,
    }
}

// The most fraction digits the grid shows. Copies, exports and the cell viewer keep them all.
const SHOWN_FRACTION: usize = 6;

// A plain decimal with a longer fraction, rounded to SHOWN_FRACTION digits and without its trailing zeros. Rounded
// as text, so a NUMERIC too big for an f64 keeps every digit. Anything else shows as it is.
pub fn shown_number(text: &str) -> Cow<'_, str> {
    let (sign, unsigned) = text.strip_prefix('-').map_or(("", text), |rest| ("-", rest));
    let Some((int, fraction)) = unsigned.split_once('.') else { return Cow::Borrowed(text) };
    let digits = |part: &str| part.bytes().all(|b| b.is_ascii_digit());
    if fraction.len() <= SHOWN_FRACTION || !digits(int) || !digits(fraction) {
        return Cow::Borrowed(text);
    }
    let mut kept: Vec<u8> = int.bytes().chain(fraction.bytes().take(SHOWN_FRACTION)).collect();
    if fraction.as_bytes()[SHOWN_FRACTION] >= b'5' {
        let mut ix = kept.len();
        loop {
            if ix == 0 {
                kept.insert(0, b'1');
                break;
            }
            ix -= 1;
            if kept[ix] != b'9' {
                kept[ix] += 1;
                break;
            }
            kept[ix] = b'0';
        }
    }
    let (int, fraction) = kept.split_at(kept.len() - SHOWN_FRACTION);
    let int = match std::str::from_utf8(int).unwrap_or_default() {
        "" => "0",
        int => int,
    };
    let fraction = std::str::from_utf8(fraction).unwrap_or_default().trim_end_matches('0');
    let sign = if fraction.is_empty() && int.bytes().all(|b| b == b'0') { "" } else { sign };
    Cow::Owned(match fraction {
        "" => format!("{sign}{int}"),
        fraction => format!("{sign}{int}.{fraction}"),
    })
}

// Longest of the header plus chevron and the sampled values, padded and clamped to 8..=50 ch.
pub fn auto_width_ch<'a>(header: &str, sample: impl IntoIterator<Item = Option<&'a str>>) -> usize {
    let mut longest = utf16_len(header) + COL_HEADER_ICON_CH;
    for text in sample.into_iter().flatten() {
        longest = longest.max(utf16_len(text));
        if longest >= COL_MAX_CH {
            break;
        }
    }
    (longest + COL_PAD_CH).clamp(COL_MIN_CH, COL_MAX_CH)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widths_clamp_and_pad_the_header() {
        assert_eq!(auto_width_ch("id", [Some("1"), Some("2")]), 9);
        assert_eq!(auto_width_ch("name", [Some("alice"), Some("a-very-long-username-that-overflows")]), 38);
        assert_eq!(auto_width_ch("col", [Some("x".repeat(200).as_str())]), 50);
        assert_eq!(auto_width_ch("hello", []), 12);
        assert_eq!(auto_width_ch("id", [None, None]), 9);
    }

    #[test]
    fn a_fraction_runs_from_the_point_of_a_plain_decimal() {
        let cases = [("133.28571428571428", 15), ("130", 0), ("0.5", 2), ("-12.25", 3), ("130.", 1), ("1.5e-7", 0)];
        for (text, want) in cases {
            assert_eq!(fraction_len(text), want, "{text}");
        }
    }

    #[test]
    fn long_fractions_round_for_the_grid() {
        let cases = [
            ("133.28571428571428", "133.285714"),
            ("0.30000000000000004", "0.3"),
            ("2.5", "2.5"),
            ("12.5000", "12.5000"),
            ("-1.99999999", "-2"),
            ("99.9999999", "100"),
            ("-0.0000001", "0"),
            ("12345678901234567890.123456789", "12345678901234567890.123457"),
            ("1.5e-7", "1.5e-7"),
            ("130", "130"),
        ];
        for (text, want) in cases {
            assert_eq!(shown_number(text), want, "{text}");
        }
    }

    #[test]
    fn rows_follow_the_zoom() {
        assert_eq!(row_height(13.), 25.);
        assert_eq!(row_height(16.), 31.);
    }
}
