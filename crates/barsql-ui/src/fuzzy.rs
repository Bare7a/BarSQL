use std::ops::Range;

use barsql_io::is_space;

// Positions and lengths count characters, not UTF-16 units.
const SCORE_EXACT: f64 = 10_000.;
const SCORE_PREFIX: f64 = 7_000.;
const SCORE_BOUNDARY: f64 = 5_000.;
const SCORE_SUBSTRING: f64 = 3_000.;
const SUBSEQUENCE_CAP: f64 = 1_500.;

const MATCH: f64 = 16.;
const BONUS_BOUNDARY: f64 = 30.;
const BONUS_CONSECUTIVE: f64 = 18.;
const PENALTY_GAP: f64 = 1.;
const PENALTY_LEADING: f64 = 3.;

const SECONDARY_CAP: f64 = SCORE_SUBSTRING - 100.;

#[derive(Clone, Debug, PartialEq)]
pub struct FuzzyResult {
    pub score: f64,
    // Character ranges into the matched text.
    pub ranges: Vec<Range<usize>>,
}

impl FuzzyResult {
    fn empty() -> Self {
        Self { score: 0., ranges: Vec::new() }
    }

    fn span(score: f64, range: Range<usize>) -> Self {
        Self { score, ranges: std::iter::once(range).collect() }
    }
}

fn lower(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

fn is_separator(c: char) -> bool {
    is_space(c) || matches!(c, '_' | '-' | '.' | '/' | ':' | '\\')
}

fn is_boundary(text: &[char], i: usize) -> bool {
    if i == 0 {
        return true;
    }
    let prev = text[i - 1];
    is_separator(prev) || (lower(prev) == prev && lower(text[i]) != text[i])
}

fn to_ranges(indices: &[usize]) -> Vec<Range<usize>> {
    let mut ranges: Vec<Range<usize>> = Vec::new();
    for &i in indices {
        match ranges.last_mut() {
            Some(last) if last.end == i => last.end = i + 1,
            _ => ranges.push(i..i + 1),
        }
    }
    ranges
}

pub fn fuzzy_match(query: &str, text: &str) -> Option<FuzzyResult> {
    if query.is_empty() {
        return Some(FuzzyResult::empty());
    }
    if text.is_empty() {
        return None;
    }
    let q: Vec<char> = query.chars().map(lower).collect();
    let chars: Vec<char> = text.chars().collect();
    let lowered: Vec<char> = chars.iter().copied().map(lower).collect();
    let len_adj = -(chars.len().min(80) as f64) * 0.05;

    if lowered == q {
        return Some(FuzzyResult::span(SCORE_EXACT + len_adj, 0..chars.len()));
    }
    if let Some(at) = lowered.windows(q.len()).position(|window| window == q.as_slice()) {
        if at == 0 {
            return Some(FuzzyResult::span(SCORE_PREFIX + len_adj, 0..q.len()));
        }
        let base = if is_boundary(&chars, at) { SCORE_BOUNDARY } else { SCORE_SUBSTRING };
        return Some(FuzzyResult::span(base + len_adj - at as f64 * 0.1, at..at + q.len()));
    }

    let mut matched: Vec<usize> = Vec::new();
    let mut raw = 0.;
    let mut prev: Option<usize> = None;
    for (i, &c) in lowered.iter().enumerate() {
        if matched.len() == q.len() {
            break;
        }
        if c != q[matched.len()] {
            continue;
        }
        let mut s = MATCH;
        if is_boundary(&chars, i) {
            s += BONUS_BOUNDARY;
        }
        match prev {
            Some(p) if i == p + 1 => s += BONUS_CONSECUTIVE,
            Some(p) => s -= PENALTY_GAP * (i - p - 1).min(10) as f64,
            None => s -= PENALTY_LEADING * i.min(5) as f64,
        }
        raw += s;
        matched.push(i);
        prev = Some(i);
    }
    if matched.len() < q.len() {
        return None;
    }
    Some(FuzzyResult { score: raw.min(SUBSEQUENCE_CAP) + len_adj, ranges: to_ranges(&matched) })
}

// Only the primary label is highlighted. Secondary fields just rank, always below a direct substring hit.
pub fn rank_candidate(query: &str, primary: &str, secondary: &[&str]) -> Option<FuzzyResult> {
    if query.is_empty() {
        return Some(FuzzyResult::empty());
    }
    let primary_match = fuzzy_match(query, primary);
    let mut score = primary_match.as_ref().map_or(f64::NEG_INFINITY, |m| m.score);
    for field in secondary.iter().filter(|field| !field.is_empty()) {
        if let Some(m) = fuzzy_match(query, field) {
            score = score.max(m.score.min(SECONDARY_CAP));
        }
    }
    score.is_finite().then(|| FuzzyResult { score, ranges: primary_match.map(|m| m.ranges).unwrap_or_default() })
}

#[cfg(test)]
mod tests {
    use super::{FuzzyResult, fuzzy_match, rank_candidate};

    fn score(query: &str, text: &str) -> f64 {
        fuzzy_match(query, text).unwrap_or_else(|| panic!("expected a match for {query:?} in {text:?}")).score
    }

    fn ranges(query: &str, text: &str) -> Vec<(usize, usize)> {
        fuzzy_match(query, text).unwrap().ranges.into_iter().map(|r| (r.start, r.end)).collect()
    }

    #[test]
    fn ranks_exact_over_prefix_over_substring_over_subsequence() {
        let (exact, prefix) = (score("users", "users"), score("user", "users"));
        let (substring, subsequence) = (score("ser", "users"), score("urs", "users"));
        assert!(exact > prefix && prefix > substring && substring > subsequence);
    }

    #[test]
    fn misses_when_the_query_is_not_a_subsequence() {
        assert_eq!(fuzzy_match("xyz", "users"), None);
        assert_eq!(fuzzy_match("sru", "users"), None);
    }

    #[test]
    fn an_empty_query_matches_with_score_zero_and_no_ranges() {
        assert_eq!(fuzzy_match("", "users"), Some(FuzzyResult { score: 0., ranges: vec![] }));
    }

    #[test]
    fn is_case_insensitive() {
        assert_eq!(score("USERS", "users"), score("users", "users"));
        assert_eq!(score("usr", "USERS"), score("usr", "users"));
    }

    #[test]
    fn ranges_point_into_the_original_text() {
        assert_eq!(ranges("users", "users"), [(0, 5)]);
        assert_eq!(ranges("user", "users"), [(0, 4)]);
        assert_eq!(ranges("ser", "users"), [(1, 4)]);
        assert_eq!(ranges("urs", "users"), [(0, 1), (3, 5)]);
    }

    #[test]
    fn rewards_matches_on_a_word_boundary() {
        assert!(score("acc", "user_accounts") > score("cco", "user_accounts"));
    }

    #[test]
    fn treats_camel_case_humps_as_boundaries() {
        assert!(score("nam", "userName") > score("ame", "userName"));
    }

    #[test]
    fn prefers_the_shorter_of_two_prefix_matches() {
        assert!(score("user", "users") > score("user", "users_archive_table"));
    }

    #[test]
    fn scores_match_the_expected_values() {
        let cases = [
            ("users", "users", 9999.75),
            ("ser", "users", 2999.65),
            ("urs", "users", 93.75),
            ("acc", "user_accounts", 4998.85),
            ("ab", "a b", 90.85),
            ("téb", "Tables_ÉBC", 119.5),
            ("ord", "orders", 6999.7),
            ("pos", "public.posts", 4998.7),
            ("ab", "xAbc", 4999.7),
        ];
        for (query, text, expected) in cases {
            assert_eq!(score(query, text), expected, "{query:?} in {text:?}");
        }
        assert_eq!(ranges("téb", "Tables_ÉBC"), [(0, 1), (7, 9)]);
        assert_eq!(ranges("ab", "a b"), [(0, 1), (2, 3)]);
        assert_eq!(rank_candidate("sales", "orders", &["sales_db"]).unwrap().score, 2900.);
    }

    #[test]
    fn rank_candidate_highlights_the_primary_label() {
        let rank = rank_candidate("foo", "foobar", &["unrelated"]).unwrap();
        assert_eq!(rank.ranges.iter().map(|r| (r.start, r.end)).collect::<Vec<_>>(), [(0, 3)]);
    }

    #[test]
    fn a_secondary_field_ranks_below_a_direct_substring_match() {
        let via_secondary = rank_candidate("sales", "orders", &["sales_db"]).unwrap();
        assert!(via_secondary.ranges.is_empty());
        let via_primary = rank_candidate("sales", "monthly_sales", &["nope"]).unwrap();
        assert!(via_primary.score > via_secondary.score);
    }

    #[test]
    fn rank_candidate_misses_when_nothing_matches() {
        assert_eq!(rank_candidate("zzz", "abc", &["def"]), None);
    }

    #[test]
    fn an_empty_query_ranks_everything() {
        assert_eq!(rank_candidate("", "anything", &["x"]), Some(FuzzyResult { score: 0., ranges: vec![] }));
    }
}
