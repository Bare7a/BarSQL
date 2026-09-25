// Column widths are in `ch`, the advance width of '0'.

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
    fn rows_follow_the_zoom() {
        assert_eq!(row_height(13.), 25.);
        assert_eq!(row_height(16.), 31.);
    }
}
