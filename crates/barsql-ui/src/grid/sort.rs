use std::cmp::Ordering;

use barsql_db::{Cell, ResultSet};
use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::{decompose_canonical, is_combining_mark};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDir {
    Asc,
    Desc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SortState {
    pub column: Option<usize>,
    pub desc: bool,
}

impl SortState {
    // Same column flips direction. A new column starts ascending.
    pub fn toggle(&mut self, column: usize) {
        if self.column == Some(column) {
            self.desc = !self.desc;
        } else {
            *self = Self { column: Some(column), desc: false };
        }
    }

    pub fn dir(&self) -> SortDir {
        if self.desc { SortDir::Desc } else { SortDir::Asc }
    }
}

// ICU root order for ASCII punctuation and symbols, whitespace first and `$` last.
const ASCII_ORDER: &[u8] = b"\t _-,;:!?.'\"()[]{}@*/\\&#%`^+<=>|~$";

const fn ascii_marks() -> [u32; 128] {
    let mut table = [0u32; 128];
    let mut c = 0;
    while c < 128 {
        table[c] = 0x100 + c as u32;
        c += 1;
    }
    let mut ix = 0;
    while ix < ASCII_ORDER.len() {
        table[ASCII_ORDER[ix] as usize] = ix as u32;
        ix += 1;
    }
    table[b'\n' as usize] = 1;
    table[b'\r' as usize] = 1;
    table[0x0b] = 1;
    table[0x0c] = 1;
    table
}

static ASCII_MARKS: [u32; 128] = ascii_marks();

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Weight<'a> {
    // Whitespace, punctuation and symbols, before digits and letters.
    Mark(u32),
    // Digit run compared by value, via its length without leading zeros and then its digits.
    Number(usize, &'a str),
    Letter(char),
}

fn mark_weight(c: char) -> u32 {
    match c {
        c if c.is_ascii() => ASCII_MARKS[c as usize],
        c if c.is_whitespace() => 1,
        c => 0x100 + c as u32,
    }
}

// Lazy primary weights. Case and accents are dropped, digit runs become numbers and sharp s becomes ss.
struct Primary<'a> {
    text: &'a str,
    pos: usize,
    pending: [Weight<'a>; 16],
    pending_len: usize,
    pending_ix: usize,
}

impl<'a> Primary<'a> {
    fn new(text: &'a str) -> Self {
        Self { text, pos: 0, pending: [Weight::Mark(0); 16], pending_len: 0, pending_ix: 0 }
    }

    fn queue(&mut self, weight: Weight<'a>) {
        if self.pending_len < self.pending.len() {
            self.pending[self.pending_len] = weight;
            self.pending_len += 1;
        }
    }
}

impl<'a> Iterator for Primary<'a> {
    type Item = Weight<'a>;

    fn next(&mut self) -> Option<Weight<'a>> {
        loop {
            if self.pending_ix < self.pending_len {
                self.pending_ix += 1;
                return Some(self.pending[self.pending_ix - 1]);
            }
            let rest = &self.text[self.pos..];
            let c = rest.chars().next()?;
            if c.is_ascii_digit() {
                let run = rest.bytes().position(|b| !b.is_ascii_digit()).unwrap_or(rest.len());
                let digits = rest[..run].trim_start_matches('0');
                self.pos += run;
                return Some(Weight::Number(digits.len(), digits));
            }
            self.pos += c.len_utf8();
            if c.is_ascii() {
                return Some(if c.is_ascii_alphanumeric() {
                    Weight::Letter(c.to_ascii_lowercase())
                } else {
                    Weight::Mark(ASCII_MARKS[c as usize])
                });
            }
            self.pending_len = 0;
            self.pending_ix = 0;
            let mut parts = [None; 8];
            let mut count = 0;
            decompose_canonical(c, |part| {
                if count < parts.len() {
                    parts[count] = Some(part);
                    count += 1;
                }
            });
            for part in parts.into_iter().flatten().filter(|part| !is_combining_mark(*part)) {
                if part == 'ß' {
                    self.queue(Weight::Letter('s'));
                    self.queue(Weight::Letter('s'));
                } else if part.is_alphanumeric() {
                    for lower in part.to_lowercase() {
                        self.queue(Weight::Letter(lower));
                    }
                } else {
                    self.queue(Weight::Mark(mark_weight(part)));
                }
            }
        }
    }
}

// Approximates ICU root collation with numeric ordering, breaking ties on accents, then lowercase-first
// case, then code points. sort_order gets the same order from byte keys.
#[cfg(test)]
fn natural_cmp(a: &str, b: &str) -> Ordering {
    Primary::new(a).cmp(Primary::new(b)).then_with(|| tie_break(a, b))
}

fn tie_break(a: &str, b: &str) -> Ordering {
    if a == b {
        return Ordering::Equal;
    }
    a.nfd()
        .flat_map(char::to_lowercase)
        .cmp(b.nfd().flat_map(char::to_lowercase))
        .then_with(|| a.chars().map(char::is_uppercase).cmp(b.chars().map(char::is_uppercase)))
        .then_with(|| a.cmp(b))
}

// Byte keys that sort like the weights. Marks stay below 0x30, numbers are 0x30 then their length, and
// letters are their UTF-8.
fn push_primary_key(text: &str, key: &mut Vec<u8>) {
    for weight in Primary::new(text) {
        match weight {
            Weight::Mark(w) if w < 0x28 => key.push(0x01 + w as u8),
            Weight::Mark(w) => {
                key.push(0x29);
                key.extend_from_slice(&w.to_be_bytes()[1..]);
            }
            Weight::Number(len, digits) => {
                key.push(0x30);
                match u8::try_from(len) {
                    Ok(len) if len < u8::MAX => key.push(len),
                    _ => {
                        key.push(u8::MAX);
                        key.extend_from_slice(&(len as u64).to_be_bytes());
                    }
                }
                key.extend_from_slice(digits.as_bytes());
            }
            Weight::Letter(c) => key.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes()),
        }
    }
}

fn number(text: &str) -> f64 {
    text.parse::<f64>().unwrap_or(f64::NAN)
}

// NaN has no order, so it goes last like a null.
fn cmp_numbers(a: f64, b: f64) -> Ordering {
    match (a.is_nan(), b.is_nan()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        _ => a.partial_cmp(&b).unwrap_or(Ordering::Equal),
    }
}

// Nulls and NaN stay last in both directions. An all-number column sorts by value, anything else as text.
pub fn sort_order(set: &ResultSet, column: usize, dir: SortDir) -> Vec<usize> {
    let rows = set.rows();
    let mut order: Vec<usize> = (0..rows).collect();
    let flip = |o: Ordering| if dir == SortDir::Desc { o.reverse() } else { o };
    let numeric = (0..rows).all(|r| matches!(set.cell(r, column), Cell::Null | Cell::Number(_)));
    if numeric {
        let keys: Vec<Option<f64>> = (0..rows)
            .map(|r| match set.cell(r, column) {
                Cell::Number(text) => Some(number(text)),
                _ => None,
            })
            .collect();
        order.sort_by(|&ia, &ib| match (keys[ia], keys[ib]) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            (Some(a), Some(b)) if a.is_nan() || b.is_nan() => cmp_numbers(a, b),
            (Some(a), Some(b)) => flip(cmp_numbers(a, b)),
        });
    } else {
        // All keys go into one buffer. The row index breaks ties so equal values keep their order.
        let mut arena = Vec::new();
        let keys: Vec<Option<(usize, usize, &str)>> = (0..rows)
            .map(|r| {
                set.cell(r, column).display().map(|text| {
                    let start = arena.len();
                    push_primary_key(text, &mut arena);
                    (start, arena.len(), text)
                })
            })
            .collect();
        order.sort_unstable_by(|&ia, &ib| match (keys[ia], keys[ib]) {
            (None, None) => ia.cmp(&ib),
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            (Some((sa, ea, a)), Some((sb, eb, b))) => {
                flip(arena[sa..ea].cmp(&arena[sb..eb]).then_with(|| tie_break(a, b))).then(ia.cmp(&ib))
            }
        });
    }
    order
}

// Display position <-> result row index.
#[derive(Debug, Clone, Default)]
pub struct RowOrder {
    rows: usize,
    sorted: Option<(Vec<usize>, Vec<usize>)>,
}

impl RowOrder {
    pub fn identity(rows: usize) -> Self {
        Self { rows, sorted: None }
    }

    pub fn sorted(order: Vec<usize>) -> Self {
        let mut inverse = vec![0; order.len()];
        for (display, &global) in order.iter().enumerate() {
            inverse[global] = display;
        }
        Self { rows: order.len(), sorted: Some((order, inverse)) }
    }

    pub fn len(&self) -> usize {
        self.rows
    }

    pub fn global_at(&self, display: usize) -> Option<usize> {
        match &self.sorted {
            Some((order, _)) => order.get(display).copied(),
            None => (display < self.rows).then_some(display),
        }
    }

    pub fn display_of(&self, global: usize) -> Option<usize> {
        match &self.sorted {
            Some((_, inverse)) => inverse.get(global).copied(),
            None => (global < self.rows).then_some(global),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use barsql_db::{ChunkBuilder, ColumnMeta};

    use super::*;

    #[test]
    fn text_sorts_like_icu_with_numeric_order() {
        let want = [
            "",
            "_x",
            "-5",
            "$5",
            "€5",
            "01",
            "1",
            "1.5",
            "1.10",
            "2 rows",
            "5",
            "9",
            "10",
            "10 rows",
            "a",
            "A",
            "á",
            "apfel",
            "Apfel",
            "äpfel",
            "b",
            "B",
            "eclair",
            "Eclair",
            "éclair",
            "item1",
            "Item1",
            "item2",
            "item10",
            "ost",
            "Österreich",
            "ss",
            "ß",
            "user id",
            "user_id",
            "userid",
            "x ray",
            "x-ray",
            "xray",
            "z",
            "zeta",
            "Zeta",
            "Ω",
        ];
        let mut words = want.to_vec();
        words.reverse();
        words.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(words, want);
        let key = |text: &str| {
            let mut key = Vec::new();
            push_primary_key(text, &mut key);
            key
        };
        words.reverse();
        words.sort_by(|a, b| key(a).cmp(&key(b)).then_with(|| tie_break(a, b)));
        assert_eq!(words, want, "byte keys order like the weights");
        assert!(key("x\u{1}") > key("x_"), "control characters are marks after ASCII punctuation");
        assert!(key("a 9") < key("a 10") && key(&"9".repeat(300)) > key(&"9".repeat(299)));
    }

    fn set(values: &[Option<(&str, bool)>]) -> ResultSet {
        let columns: Arc<[ColumnMeta]> = vec![ColumnMeta { name: "v".into(), type_name: "X".into() }].into();
        let mut builder = ChunkBuilder::new(1, values.len());
        for value in values {
            match value {
                None => builder.push_null(),
                Some((text, true)) => builder.push_number(|s| s.push_str(text)),
                Some((text, false)) => builder.push_text(|s| s.push_str(text)),
            }
            builder.end_row();
        }
        let mut set = ResultSet::new(columns);
        set.push(Arc::new(builder.finish()));
        set
    }

    fn sorted(set: &ResultSet, dir: SortDir) -> Vec<Option<String>> {
        sort_order(set, 0, dir).into_iter().map(|r| set.cell(r, 0).display().map(str::to_string)).collect()
    }

    #[test]
    fn numbers_compare_by_value_with_nan_and_nulls_last() {
        let numbers = set(&[
            Some(("5", true)),
            Some(("-3", true)),
            None,
            Some(("NaN", true)),
            Some(("-10", true)),
            Some(("2.5", true)),
        ]);
        let asc = sorted(&numbers, SortDir::Asc);
        assert_eq!(
            asc,
            ["-10", "-3", "2.5", "5", "NaN"].map(|s| Some(s.to_string())).into_iter().chain([None]).collect::<Vec<_>>()
        );
        let desc = sorted(&numbers, SortDir::Desc);
        assert_eq!(desc[..4], ["5", "2.5", "-3", "-10"].map(|s| Some(s.to_string())));
        assert_eq!(desc[5], None, "nulls stay last when descending");
    }

    #[test]
    fn mixed_columns_sort_as_text() {
        let mixed =
            set(&[Some(("item10", false)), Some(("2", true)), Some(("item2", false)), None, Some(("10", true))]);
        let asc = sorted(&mixed, SortDir::Asc);
        assert_eq!(asc, [Some("2"), Some("10"), Some("item2"), Some("item10"), None].map(|s| s.map(str::to_string)));
    }

    #[test]
    fn toggling_flips_the_same_column_and_resets_on_another() {
        let mut sort = SortState::default();
        sort.toggle(2);
        assert_eq!((sort.column, sort.dir()), (Some(2), SortDir::Asc));
        sort.toggle(2);
        assert_eq!(sort.dir(), SortDir::Desc);
        sort.toggle(0);
        assert_eq!((sort.column, sort.dir()), (Some(0), SortDir::Asc));
    }

    #[test]
    fn row_orders_map_both_ways() {
        let order = RowOrder::sorted(vec![2, 0, 1]);
        assert_eq!((order.global_at(0), order.display_of(0)), (Some(2), Some(1)));
        assert_eq!(order.global_at(3), None);
        let identity = RowOrder::identity(3);
        assert_eq!((identity.global_at(1), identity.display_of(5)), (Some(1), None));
    }
}
