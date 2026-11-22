use gpui_kit::{Pixels, px};

// What a bar too wide for its window gives up, whole and in order: Min and Max, the engine label, Avg, the chips'
// words, the update's words, the long caret, then Sum. Each step keeps the ones before it.
pub const STEPS: usize = 7;

// Each part's natural width, with its gap.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Widths {
    // Always drawn: padding, the toggles, the dot and the name, the gap between the halves.
    pub base: Pixels,
    pub engine: Pixels,
    pub chips: Pixels,
    // Each chip as its icon, the extensions keeping their count and the transaction its dot.
    pub chip_icons: Pixels,
    // Count, Sum, Avg, Min, Max. Zero while the bar shows the caret.
    pub stats: [Pixels; 5],
    pub caret: Pixels,
    pub caret_short: Pixels,
    pub live: Pixels,
    pub update: Pixels,
    pub update_icon: Pixels,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shown {
    pub engine: bool,
    pub chip_words: bool,
    pub update_words: bool,
    pub short_caret: bool,
    pub stats: [bool; 5],
}

pub fn shown(step: usize) -> Shown {
    Shown {
        engine: step < 2,
        chip_words: step < 4,
        update_words: step < 5,
        short_caret: step >= 6,
        stats: [true, step < 7, step < 3, step < 1, step < 1],
    }
}

pub fn width_at(widths: &Widths, step: usize) -> Pixels {
    let shown = shown(step);
    let pick = |on: bool, yes: Pixels, no: Pixels| if on { yes } else { no };
    let stats = widths.stats.iter().zip(shown.stats).filter(|(_, on)| *on).fold(px(0.), |sum, (width, _)| sum + *width);
    widths.base
        + pick(shown.engine, widths.engine, px(0.))
        + pick(shown.chip_words, widths.chips, widths.chip_icons)
        + stats
        + pick(shown.short_caret, widths.caret_short, widths.caret)
        + widths.live
        + pick(shown.update_words, widths.update, widths.update_icon)
}

// The fewest steps that fit. Past the last, the name gives up characters.
pub fn step_for(widths: &Widths, available: Pixels) -> usize {
    (0..STEPS).find(|&step| width_at(widths, step) <= available).unwrap_or(STEPS)
}

#[cfg(test)]
mod tests {
    use gpui_kit::px;

    use super::{STEPS, Shown, Widths, shown, step_for, width_at};

    fn widths() -> Widths {
        Widths {
            base: px(200.),
            engine: px(90.),
            chips: px(260.),
            chip_icons: px(80.),
            stats: [px(60.), px(80.), px(70.), px(50.), px(60.)],
            caret: px(80.),
            caret_short: px(30.),
            live: px(120.),
            update: px(110.),
            update_icon: px(20.),
        }
    }

    #[test]
    fn each_step_is_the_fewest_that_fits() {
        let w = widths();
        assert_eq!(step_for(&w, px(5000.)), 0);
        for step in 1..=STEPS {
            assert!(width_at(&w, step) < width_at(&w, step - 1), "step {step} frees room");
            assert_eq!(step_for(&w, width_at(&w, step)), step);
        }
        assert_eq!(step_for(&w, px(0.)), STEPS, "the last step even when nothing fits");
    }

    #[test]
    fn items_give_way_in_the_agreed_order() {
        assert_eq!(
            shown(0),
            Shown { engine: true, chip_words: true, update_words: true, short_caret: false, stats: [true; 5] }
        );
        assert_eq!(shown(1).stats, [true, true, true, false, false]);
        assert!(!shown(2).engine && shown(2).chip_words);
        assert_eq!(shown(3).stats, [true, true, false, false, false]);
        assert!(!shown(4).chip_words && shown(4).update_words);
        assert!(!shown(5).update_words && !shown(5).short_caret);
        assert!(shown(6).short_caret);
        assert_eq!(shown(7).stats, [true, false, false, false, false]);
    }
}
