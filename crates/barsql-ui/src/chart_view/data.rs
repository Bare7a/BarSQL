// What a chart reads from a result: what each column holds, the picks it starts with, and the values it plots.
use std::collections::{HashMap, HashSet};

use barsql_db::{Cell, ResultSet};

// Rows a chart reads.
pub const MAX_ROWS: usize = 10_000;
// Labels a bar chart shows, and points a line plots.
pub const MAX_BARS: usize = 200;
pub const MAX_POINTS: usize = 2_000;
// Value columns at once, one colour each.
pub const SLOTS: usize = 5;
// Rows looked at to tell what a column holds.
pub const PROFILE_ROWS: usize = 500;
// A good label repeats a few short values, like a region or a status.
const FEW_LABELS: usize = 60;
const SHORT_TEXT: f32 = 30.;
const LONG_TEXT: f32 = 60.;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Bars,
    Line,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Aggregate {
    Sum,
    Count,
    Average,
}

// A value keeps its slot, and with it its colour, while others come and go.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Settings {
    pub kind: Kind,
    pub label: usize,
    pub values: [Option<usize>; SLOTS],
    pub aggregate: Aggregate,
}

impl Settings {
    // (slot, column) of each picked value.
    pub fn picked(&self) -> impl Iterator<Item = (usize, usize)> + '_ {
        self.values.iter().enumerate().filter_map(|(slot, column)| column.map(|column| (slot, column)))
    }

    pub fn slot_of(&self, column: usize) -> Option<usize> {
        self.values.iter().position(|value| *value == Some(column))
    }

    // Takes the column out, or puts it in the first free slot. False when every slot is taken.
    pub fn toggle_value(&mut self, column: usize) -> bool {
        if let Some(slot) = self.slot_of(column) {
            self.values[slot] = None;
            return true;
        }
        match self.values.iter().position(Option::is_none) {
            Some(slot) => {
                self.values[slot] = Some(column);
                true
            }
            None => false,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Profile {
    // Every filled cell is a number, or numeric text in a numeric type, like Postgres NUMERIC.
    pub numeric: bool,
    pub id_like: bool,
    pub temporal: bool,
    // Among the profiled rows.
    pub filled: usize,
    pub distinct: usize,
    pub avg_len: f32,
}

pub fn profile(set: &ResultSet) -> Vec<Profile> {
    let rows = set.rows().min(PROFILE_ROWS);
    set.columns
        .iter()
        .enumerate()
        .map(|(column, meta)| {
            let numeric_type = numeric_type(&meta.type_name);
            let (mut filled, mut numbers, mut dates, mut chars) = (0, 0, 0, 0);
            let mut seen = HashSet::new();
            for row in 0..rows {
                let cell = set.cell(row, column);
                let Some(text) = cell.display() else { continue };
                filled += 1;
                chars += text.chars().count();
                seen.insert(text);
                numbers += usize::from(number(cell, numeric_type).is_some());
                dates += usize::from(looks_like_date(text));
            }
            let numeric = filled > 0 && numbers == filled;
            Profile {
                numeric,
                id_like: id_like(&meta.name, &meta.type_name),
                temporal: !numeric && (temporal_type(&meta.type_name) || (filled > 0 && dates == filled)),
                filled,
                distinct: seen.len(),
                avg_len: if filled == 0 { 0. } else { chars as f32 / filled as f32 },
            }
        })
        .collect()
}

// The measure is the last number that isn't a key, since a query's aggregates usually come last. The labels come
// from the best other column: short repeating text, then dates, then any short text, then numbers. Dates draw a
// line. Without a measure the chart counts rows.
pub fn defaults(profiles: &[Profile]) -> Settings {
    let mut value = (0..profiles.len()).rev().find(|&c| profiles[c].numeric && !profiles[c].id_like);
    let label = (0..profiles.len()).filter(|&c| Some(c) != value).min_by_key(|&c| (label_rank(&profiles[c]), c));
    let label = match label {
        Some(label) => label,
        None => value.take().unwrap_or(0),
    };
    let mut values = [None; SLOTS];
    values[0] = value;
    Settings {
        kind: if profiles.get(label).is_some_and(|p| p.temporal) { Kind::Line } else { Kind::Bars },
        label,
        values,
        aggregate: if value.is_some() { Aggregate::Sum } else { Aggregate::Count },
    }
}

fn label_rank(p: &Profile) -> u8 {
    let text = !p.numeric && !p.temporal && !p.id_like && p.filled > 0;
    match () {
        _ if text && p.distinct <= FEW_LABELS && p.avg_len <= SHORT_TEXT => 0,
        _ if p.temporal && !p.id_like => 1,
        _ if text && p.avg_len <= LONG_TEXT => 2,
        _ if p.numeric && !p.id_like => 3,
        _ => 4,
    }
}

// Why some labels are missing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cut {
    // The biggest by the first value, in the result's order.
    Top,
    // The first ones in order.
    First,
}

#[derive(Debug, Default, PartialEq)]
pub struct ChartData {
    // None is NULL.
    pub labels: Vec<Option<String>>,
    pub series: Vec<Series>,
    // Rows read, and whether the result had more.
    pub rows: usize,
    pub rows_cut: bool,
    // Labels before any were cut.
    pub groups: usize,
    pub cut: Option<Cut>,
}

impl ChartData {
    // The values' range, from zero for bars so they stand on it.
    pub fn range(&self, kind: Kind) -> Option<(f64, f64)> {
        let values = self.series.iter().flat_map(|s| s.values.iter().flatten().copied());
        let (lo, hi) = values.fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), v| (lo.min(v), hi.max(v)));
        if lo > hi {
            return None;
        }
        Some(match kind {
            Kind::Bars => (lo.min(0.), hi.max(0.)),
            Kind::Line => (lo, hi),
        })
    }
}

#[derive(Debug, PartialEq)]
pub struct Series {
    pub slot: usize,
    // None counts rows.
    pub column: Option<usize>,
    pub values: Vec<Option<f64>>,
}

// Rows that share a label make one bar or point, by the full label text. Sum and Average skip NULLs, and Count
// counts the values that aren't NULL, or the rows when no value is picked.
pub fn compute(set: &ResultSet, profiles: &[Profile], settings: &Settings) -> ChartData {
    let rows = set.rows().min(MAX_ROWS);
    let mut picked: Vec<(usize, Option<usize>)> = settings.picked().map(|(slot, c)| (slot, Some(c))).collect();
    if picked.is_empty() && settings.aggregate == Aggregate::Count {
        picked.push((0, None));
    }
    let numeric = |column: usize| profiles.get(column).is_some_and(|p| p.numeric);
    let mut groups: HashMap<Option<&str>, usize> = HashMap::new();
    let mut labels: Vec<Option<&str>> = Vec::new();
    // (sum, count) per series and label.
    let mut totals: Vec<Vec<(f64, usize)>> = vec![Vec::new(); picked.len()];
    for row in 0..rows {
        let label = set.display(row, settings.label);
        let group = *groups.entry(label).or_insert_with(|| {
            labels.push(label);
            totals.iter_mut().for_each(|series| series.push((0., 0)));
            labels.len() - 1
        });
        for (series, (_, column)) in totals.iter_mut().zip(&picked) {
            let total = &mut series[group];
            match column {
                None => total.1 += 1,
                Some(column) => {
                    if let Some(value) = number(set.cell(row, *column), numeric(*column)) {
                        total.0 += value;
                        total.1 += 1;
                    }
                }
            }
        }
    }
    let value = |(sum, count): (f64, usize)| match settings.aggregate {
        Aggregate::Count => Some(count as f64),
        _ if count == 0 => None,
        Aggregate::Sum => Some(sum),
        Aggregate::Average => Some(sum / count as f64),
    };
    let totals: Vec<Vec<Option<f64>>> = totals
        .into_iter()
        .zip(&picked)
        .map(|(series, (_, column))| match column {
            None => series.into_iter().map(|(_, count)| Some(count as f64)).collect(),
            Some(_) => series.into_iter().map(value).collect(),
        })
        .collect();

    let temporal = profiles.get(settings.label).is_some_and(|p| p.temporal);
    let mut order: Vec<usize> = (0..labels.len()).collect();
    // ISO dates and times sort as text. NULL goes last.
    if temporal {
        order.sort_by(|&a, &b| (labels[a].is_none(), labels[a]).cmp(&(labels[b].is_none(), labels[b])));
    }
    let limit = match settings.kind {
        Kind::Bars => MAX_BARS,
        Kind::Line => MAX_POINTS,
    };
    let cut = (order.len() > limit).then(|| match settings.kind {
        Kind::Bars if !temporal && !totals.is_empty() => {
            let first = &totals[0];
            let mut biggest = order.clone();
            biggest.sort_by(|&a, &b| {
                let (a, b) = (first[a].unwrap_or(f64::NEG_INFINITY), first[b].unwrap_or(f64::NEG_INFINITY));
                b.total_cmp(&a)
            });
            let kept: HashSet<usize> = biggest[..limit].iter().copied().collect();
            order.retain(|group| kept.contains(group));
            Cut::Top
        }
        _ => {
            order.truncate(limit);
            Cut::First
        }
    });
    ChartData {
        labels: order.iter().map(|&g| labels[g].map(str::to_string)).collect(),
        series: picked
            .iter()
            .zip(&totals)
            .map(|(&(slot, column), values)| Series {
                slot,
                column,
                values: order.iter().map(|&g| values[g]).collect(),
            })
            .collect(),
        rows,
        rows_cut: set.rows() > MAX_ROWS,
        groups: labels.len(),
        cut,
    }
}

// About `count` round ticks (1, 2, 2.5 or 5 times a power of ten apart) that cover lo..hi.
pub fn nice_ticks(lo: f64, hi: f64, count: usize) -> Vec<f64> {
    let (mut lo, mut hi) = (lo.min(hi), lo.max(hi));
    if !lo.is_finite() || !hi.is_finite() {
        return vec![0., 1.];
    }
    if lo == hi {
        match lo {
            0. => hi = 1.,
            v if v > 0. => (lo, hi) = (v * 0.9, v * 1.1),
            v => (lo, hi) = (v * 1.1, v * 0.9),
        }
    }
    let step = nice_step((hi - lo) / count.saturating_sub(1).max(1) as f64);
    let start = (lo / step).floor() * step;
    let steps = ((hi - start) / step - 1e-9).ceil().max(1.) as usize;
    // Snap the zero tick, which adding up steps leaves a hair off.
    (0..=steps).map(|i| start + i as f64 * step).map(|v| if v.abs() < step * 1e-9 { 0. } else { v }).collect()
}

fn nice_step(raw: f64) -> f64 {
    let power = 10f64.powf(raw.log10().floor());
    let nice = match raw / power {
        f if f <= 1. => 1.,
        f if f <= 2. => 2.,
        f if f <= 2.5 => 2.5,
        f if f <= 5. => 5.,
        _ => 10.,
    };
    nice * power
}

// How ticks `step` apart are written: divided by a unit (k, M or B once the axis reaches 10,000) with the decimals
// the step needs. Returns (unit, suffix, decimals).
pub fn tick_format(max_abs: f64, step: f64) -> (f64, &'static str, usize) {
    let (unit, suffix) = match max_abs {
        m if m >= 1e9 => (1e9, "B"),
        m if m >= 1e6 => (1e6, "M"),
        m if m >= 1e4 => (1e3, "k"),
        _ => (1., ""),
    };
    let scaled = step / unit;
    let decimals = (0..6)
        .find(|&d| {
            let shifted = scaled * 10f64.powi(d as i32);
            (shifted - shifted.round()).abs() < 1e-6 * shifted.abs().max(1.)
        })
        .unwrap_or(6);
    (unit, suffix, decimals)
}

pub fn number(cell: Cell, numeric_text: bool) -> Option<f64> {
    let text = match cell {
        Cell::Number(text) => text,
        Cell::Text(text) if numeric_text => text,
        _ => return None,
    };
    text.trim().parse::<f64>().ok().filter(|value| value.is_finite())
}

fn numeric_type(type_name: &str) -> bool {
    let name = type_name.to_ascii_lowercase();
    if ["decimal", "numeric", "number", "money", "real", "double", "float"].iter().any(|n| name.contains(n)) {
        return true;
    }
    name.split(|c: char| !c.is_ascii_alphanumeric()).any(|word| {
        let bare = word.strip_prefix('u').unwrap_or(word);
        let sized =
            bare.strip_prefix("int").is_some_and(|bits| !bits.is_empty() && bits.bytes().all(|b| b.is_ascii_digit()));
        sized
            || matches!(
                word,
                "int"
                    | "integer"
                    | "tinyint"
                    | "smallint"
                    | "mediumint"
                    | "bigint"
                    | "serial"
                    | "smallserial"
                    | "bigserial"
            )
    })
}

fn temporal_type(type_name: &str) -> bool {
    let name = type_name.to_ascii_lowercase();
    name.contains("date") || name.contains("time")
}

// Keys say little as a measure: id, uuid, customer_id, customerId.
fn id_like(name: &str, type_name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let kind = type_name.to_ascii_lowercase();
    matches!(lower.as_str(), "id" | "uuid" | "guid" | "oid" | "rowid")
        || ["_id", "_uuid", "_guid"].iter().any(|end| lower.ends_with(end))
        || (name.len() > 2 && name.ends_with("Id"))
        || kind.contains("uuid")
        || matches!(kind.as_str(), "uniqueidentifier" | "oid")
}

// 2026-01-31, maybe followed by a time.
fn looks_like_date(text: &str) -> bool {
    let b = text.as_bytes();
    b.len() >= 10
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'-'
        && b[5..7].iter().all(u8::is_ascii_digit)
        && b[7] == b'-'
        && b[8..10].iter().all(u8::is_ascii_digit)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use barsql_db::{ChunkBuilder, ColumnMeta, ResultSet};

    use super::{Aggregate, Cut, Kind, MAX_BARS, Settings, compute, defaults, nice_ticks, profile, tick_format};

    enum V {
        T(String),
        N(String),
        Null,
    }

    fn t(text: &str) -> V {
        V::T(text.into())
    }

    fn n(value: impl ToString) -> V {
        V::N(value.to_string())
    }

    fn set(columns: &[(&str, &str)], rows: Vec<Vec<V>>) -> ResultSet {
        let meta: Arc<[ColumnMeta]> = columns
            .iter()
            .map(|(name, type_name)| ColumnMeta { name: name.to_string(), type_name: type_name.to_string() })
            .collect::<Vec<_>>()
            .into();
        let mut set = ResultSet::new(meta);
        let mut chunk = ChunkBuilder::new(columns.len(), rows.len());
        for row in rows {
            for cell in row {
                match cell {
                    V::T(text) => chunk.push_text(|s| s.push_str(&text)),
                    V::N(text) => chunk.push_number(|s| s.push_str(&text)),
                    V::Null => chunk.push_null(),
                }
            }
            chunk.end_row();
        }
        set.push(Arc::new(chunk.finish()));
        set
    }

    fn values(settings: &Settings) -> Vec<usize> {
        settings.picked().map(|(_, column)| column).collect()
    }

    #[test]
    fn defaults_label_by_repeating_text_and_measure_the_last_number_that_isnt_a_key() {
        let columns = [
            ("id", "INTEGER"),
            ("region", "TEXT"),
            ("units", "INTEGER"),
            ("revenue", "REAL"),
            ("customer_id", "INTEGER"),
        ];
        let set = set(
            &columns,
            vec![
                vec![n(1), t("North"), n(3), n(10.5), n(7)],
                vec![n(2), t("South"), n(4), n(12), n(8)],
                vec![n(3), t("North"), n(1), n(3), n(9)],
            ],
        );
        let settings = defaults(&profile(&set));
        assert_eq!((settings.label, values(&settings)), (1, vec![3]));
        assert_eq!((settings.kind, settings.aggregate), (Kind::Bars, Aggregate::Sum));
    }

    #[test]
    fn a_key_labels_its_measure_when_nothing_else_can() {
        let set = set(&[("customer_id", "INTEGER"), ("orders", "BIGINT")], vec![vec![n(1), n(4)], vec![n(2), n(9)]]);
        let settings = defaults(&profile(&set));
        assert_eq!((settings.label, values(&settings)), (0, vec![1]));
    }

    #[test]
    fn dates_draw_a_line_and_sort_in_time() {
        let rows = vec![vec![t("2026-03-02"), n(5)], vec![t("2026-01-15"), n(2)], vec![t("2026-02-01"), n(4)]];
        let set = set(&[("day", ""), ("total", "")], rows);
        let profiles = profile(&set);
        assert!(profiles[0].temporal, "ISO dates in an untyped column");
        let settings = defaults(&profiles);
        assert_eq!((settings.kind, settings.label), (Kind::Line, 0));
        let data = compute(&set, &profiles, &settings);
        let labels: Vec<_> = data.labels.iter().map(|l| l.as_deref().unwrap()).collect();
        assert_eq!(labels, ["2026-01-15", "2026-02-01", "2026-03-02"]);
        assert_eq!(data.series[0].values, [Some(2.), Some(4.), Some(5.)]);
    }

    #[test]
    fn numeric_text_counts_as_numbers_only_in_a_numeric_type() {
        let rows = vec![vec![t("a"), t("1.50"), t("1000")], vec![t("b"), t("2.25"), t("2000")]];
        let set = set(&[("name", "text"), ("price", "numeric"), ("zip", "text")], rows);
        let profiles = profile(&set);
        assert!(profiles[1].numeric && !profiles[2].numeric);
        assert_eq!(values(&defaults(&profiles)), [1]);
    }

    #[test]
    fn without_numbers_the_chart_counts_rows() {
        let set = set(&[("city", "TEXT")], vec![vec![t("Sofia")], vec![t("Berlin")], vec![t("Sofia")]]);
        let profiles = profile(&set);
        let settings = defaults(&profiles);
        assert_eq!((settings.aggregate, values(&settings)), (Aggregate::Count, vec![]));
        let data = compute(&set, &profiles, &settings);
        assert_eq!(data.series.len(), 1);
        assert_eq!((data.series[0].column, &data.series[0].values), (None, &vec![Some(2.), Some(1.)]));
    }

    #[test]
    fn keys_look_like_keys() {
        let columns = [
            ("id", "int"),
            ("customerId", "int"),
            ("order_uuid", "text"),
            ("ref", "uuid"),
            ("paid", "int"),
            ("grid", "int"),
        ];
        let set = set(&columns, vec![vec![n(1), n(1), t("x"), t("y"), n(1), n(1)]]);
        let keys: Vec<bool> = profile(&set).iter().map(|p| p.id_like).collect();
        assert_eq!(keys, [true, true, true, true, false, false]);
    }

    #[test]
    fn sum_count_and_average_skip_nulls() {
        let rows = vec![
            vec![t("Sofia"), n(3)],
            vec![t("Berlin"), n(2)],
            vec![t("Sofia"), n(5)],
            vec![t("Sofia"), V::Null],
            vec![V::Null, n(1)],
        ];
        let set = set(&[("city", "TEXT"), ("sales", "INTEGER")], rows);
        let profiles = profile(&set);
        let mut settings = defaults(&profiles);
        let mut run = |aggregate| {
            settings.aggregate = aggregate;
            compute(&set, &profiles, &settings)
        };
        assert_eq!(run(Aggregate::Sum).series[0].values, [Some(8.), Some(2.), Some(1.)]);
        assert_eq!(run(Aggregate::Count).series[0].values, [Some(2.), Some(1.), Some(1.)]);
        let data = run(Aggregate::Average);
        assert_eq!(data.series[0].values, [Some(4.), Some(2.), Some(1.)]);
        assert_eq!(data.labels, [Some("Sofia".into()), Some("Berlin".into()), None]);
    }

    #[test]
    fn many_bars_keep_the_biggest_in_the_results_order() {
        let rows = (0..250).map(|i| vec![t(&format!("l{i}")), n(i)]).collect();
        let set = set(&[("label", "TEXT"), ("n", "INTEGER")], rows);
        let profiles = profile(&set);
        let data = compute(&set, &profiles, &defaults(&profiles));
        assert_eq!((data.labels.len(), data.groups, data.cut), (MAX_BARS, 250, Some(Cut::Top)));
        assert_eq!(data.labels.first().unwrap().as_deref(), Some("l50"), "the 50 smallest went");
        assert_eq!(data.labels.last().unwrap().as_deref(), Some("l249"));
    }

    #[test]
    fn a_value_keeps_its_slot_while_others_come_and_go() {
        let values = [Some(1), Some(2), None, None, None];
        let mut settings = Settings { kind: Kind::Bars, label: 0, values, aggregate: Aggregate::Sum };
        settings.toggle_value(1);
        settings.toggle_value(3);
        assert_eq!(settings.values, [Some(3), Some(2), None, None, None]);
        for column in 4..7 {
            assert!(settings.toggle_value(column));
        }
        assert!(!settings.toggle_value(9), "every slot is taken");
    }

    #[test]
    fn ticks_are_round_and_cover_the_range() {
        assert_eq!(nice_ticks(0., 97., 5), [0., 25., 50., 75., 100.]);
        assert_eq!(nice_ticks(-12., 40., 5), [-20., 0., 20., 40.]);
        assert_eq!(nice_ticks(0., 0., 5), [0., 0.25, 0.5, 0.75, 1.]);
        let ticks = nice_ticks(0.1, 0.7, 4);
        let expected = [0., 0.2, 0.4, 0.6, 0.8];
        assert!(ticks.len() == expected.len() && ticks.iter().zip(expected).all(|(a, b)| (a - b).abs() < 1e-9));
        assert_eq!(tick_format(250_000., 50_000.), (1e3, "k", 0));
        assert_eq!(tick_format(3.5, 0.25), (1., "", 2));
        assert_eq!(tick_format(2_500_000., 250_000.), (1e6, "M", 2));
    }
}
