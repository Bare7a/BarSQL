use std::collections::HashMap;
use std::sync::Arc;

use barsql_sql::lang::SqlLabels;
use gpui_kit::{App, Global, SharedString};
use serde_json::Value;

pub const LANGUAGES: [(&str, &str); 3] = [("en", "English"), ("de", "Deutsch"), ("bg", "Български")];

const EN: &str = include_str!("../locales/base/en.json");
const DE: &str = include_str!("../locales/base/de.json");
const BG: &str = include_str!("../locales/base/bg.json");
// Overrides and additions merged over the base locales.
const EN_OWN: &str = include_str!("../locales/en.json");
const DE_OWN: &str = include_str!("../locales/de.json");
const BG_OWN: &str = include_str!("../locales/bg.json");

// Lookups fall back to English, then to the key itself.
pub struct I18n {
    lang: &'static str,
    locale: Value,
    strings: HashMap<String, String>,
    fallback: HashMap<String, String>,
    sql: Arc<SqlLabels>,
}

impl Global for I18n {}

fn source(lang: &str) -> Option<(&'static str, &'static str, &'static str)> {
    match lang {
        "en" => Some(("en", EN, EN_OWN)),
        "de" => Some(("de", DE, DE_OWN)),
        "bg" => Some(("bg", BG, BG_OWN)),
        _ => None,
    }
}

fn merge(base: &mut Value, over: Value) {
    match (base, over) {
        (Value::Object(base), Value::Object(over)) => {
            for (key, value) in over {
                match base.get_mut(&key) {
                    Some(existing) => merge(existing, value),
                    None => {
                        base.insert(key, value);
                    }
                }
            }
        }
        (base, over) => *base = over,
    }
}

fn load(json: &str, own: &str) -> Value {
    let mut locale = parse(json);
    merge(&mut locale, parse(own));
    locale
}

fn flatten(value: &Value, prefix: &str, out: &mut HashMap<String, String>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let path = if prefix.is_empty() { key.clone() } else { format!("{prefix}.{key}") };
                flatten(child, &path, out);
            }
        }
        Value::String(text) => {
            out.insert(prefix.to_string(), text.clone());
        }
        _ => {}
    }
}

fn parse(json: &str) -> Value {
    serde_json::from_str(json).expect("locale json")
}

impl I18n {
    // Unknown languages fall back to English.
    pub fn new(lang: &str) -> Self {
        let (lang, json, own) = source(lang).unwrap_or(("en", EN, EN_OWN));
        let locale = load(json, own);
        let mut strings = HashMap::new();
        flatten(&locale, "", &mut strings);
        let mut fallback = HashMap::new();
        flatten(&load(EN, EN_OWN), "", &mut fallback);
        let sql = Arc::new(SqlLabels::from_locale(&locale));
        Self { lang, locale, strings, fallback, sql }
    }

    pub fn lang(&self) -> &'static str {
        self.lang
    }

    pub fn locale(&self) -> &Value {
        &self.locale
    }

    // Language service strings, from `editor.sql.*`.
    pub fn sql_labels(&self) -> Arc<SqlLabels> {
        self.sql.clone()
    }

    fn raw(&self, key: &str) -> Option<&str> {
        self.strings.get(key).or_else(|| self.fallback.get(key)).map(String::as_str)
    }

    pub fn t(&self, key: &str) -> SharedString {
        self.raw(key).map_or_else(|| SharedString::from(key.to_string()), |s| SharedString::from(s.to_string()))
    }

    pub fn t_with(&self, key: &str, vars: &[(&str, &str)]) -> SharedString {
        let mut text = self.raw(key).unwrap_or(key).to_string();
        for (name, value) in vars {
            text = text.replace(&format!("{{{{{name}}}}}"), value);
        }
        text.into()
    }

    // Only `key_one` and `key_other`, enough for en, de and bg. The bare key stands in for `_one`.
    pub fn t_count(&self, key: &str, count: i64, vars: &[(&str, &str)]) -> SharedString {
        let form = if count == 1 { "one" } else { "other" };
        let plural = format!("{key}_{form}");
        let key = if self.raw(&plural).is_some() { plural.as_str() } else { key };
        let count = count.to_string();
        let mut all = vec![("count", count.as_str())];
        all.extend_from_slice(vars);
        self.t_with(key, &all)
    }
}

// Bulgarian only groups numbers with five or more digits.
pub fn format_count(n: usize, lang: &str) -> String {
    format_number(n as f64, 0, lang)
}

// Mimics ICU. Rounds the shortest decimal form half away from zero and drops trailing zeros.
pub fn format_number(value: f64, max_fraction: usize, lang: &str) -> String {
    if !value.is_finite() {
        return value.to_string();
    }
    let (group, decimal, min_grouping) = match lang {
        "de" => ('.', ',', 4),
        "bg" => ('\u{a0}', ',', 5),
        _ => (',', '.', 4),
    };
    let shortest = format!("{}", value.abs());
    let (int, frac) = shortest.split_once('.').unwrap_or((&shortest, ""));
    let mut digits: Vec<u8> = int.bytes().chain(frac.bytes().take(max_fraction)).map(|b| b - b'0').collect();
    let mut int_len = int.len();
    if frac.as_bytes().get(max_fraction).is_some_and(|b| *b >= b'5') {
        let mut ix = digits.len();
        loop {
            if ix == 0 {
                digits.insert(0, 1);
                int_len += 1;
                break;
            }
            ix -= 1;
            if digits[ix] == 9 {
                digits[ix] = 0;
            } else {
                digits[ix] += 1;
                break;
            }
        }
    }
    let int_digits: String = digits[..int_len].iter().map(|d| char::from(b'0' + d)).collect();
    let frac_digits: String = digits[int_len..].iter().map(|d| char::from(b'0' + d)).collect();
    let frac_digits = frac_digits.trim_end_matches('0');
    let mut out = String::new();
    if value.is_sign_negative() {
        out.push('-');
    }
    for (ix, c) in int_digits.chars().enumerate() {
        if int_digits.len() >= min_grouping && ix > 0 && (int_digits.len() - ix).is_multiple_of(3) {
            out.push(group);
        }
        out.push(c);
    }
    if !frac_digits.is_empty() {
        out.push(decimal);
        out.push_str(frac_digits);
    }
    out
}

pub fn number(cx: &App, value: f64, max_fraction: usize) -> String {
    format_number(value, max_fraction, cx.global::<I18n>().lang())
}

pub fn count(cx: &App, n: usize) -> String {
    format_count(n, cx.global::<I18n>().lang())
}

pub fn t(cx: &App, key: &str) -> SharedString {
    cx.global::<I18n>().t(key)
}

pub fn t_with(cx: &App, key: &str, vars: &[(&str, &str)]) -> SharedString {
    cx.global::<I18n>().t_with(key, vars)
}

pub fn t_count(cx: &App, key: &str, count: i64, vars: &[(&str, &str)]) -> SharedString {
    cx.global::<I18n>().t_count(key, count, vars)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_group_by_language() {
        let cases = [
            ("en", ["0", "999", "1,234", "12,345", "1,234,567"]),
            ("de", ["0", "999", "1.234", "12.345", "1.234.567"]),
            ("bg", ["0", "999", "1234", "12\u{a0}345", "1\u{a0}234\u{a0}567"]),
        ];
        for (lang, want) in cases {
            assert_eq!([0, 999, 1234, 12345, 1234567].map(|n| format_count(n, lang)), want, "{lang}");
        }
    }

    #[test]
    fn numbers_round_the_shortest_decimal_like_icu() {
        assert_eq!(format_number(0.295, 2, "en"), "0.3");
        assert_eq!(format_number(12345.6, 0, "en"), "12,346");
        assert_eq!(format_number(0.0216, 3, "en"), "0.022");
        assert_eq!(format_number(4.27, 1, "en"), "4.3");
        assert_eq!(format_number(9.96, 1, "en"), "10");
        assert_eq!(format_number(999.95, 1, "de"), "1.000");
        assert_eq!(format_number(2.25, 1, "de"), "2,3");
        assert_eq!(format_number(12345.678, 2, "bg"), "12\u{a0}345,68");
        assert_eq!(format_number(-0.001, 2, "en"), "-0");
        assert_eq!(format_number(-1234.5, 0, "de"), "-1.235");
    }

    #[test]
    fn looks_up_with_fallbacks_and_interpolation() {
        let de = I18n::new("de");
        assert_eq!(de.t("menu.file").as_ref(), "Datei");
        assert_eq!(de.t("no.such.key").as_ref(), "no.such.key");
        let en = I18n::new("xx");
        assert_eq!(en.lang(), "en");
        assert_eq!(en.t_with("app.queryTab", &[("num", "3")]).as_ref(), "Query 3");
    }

    #[test]
    fn plurals_pick_the_other_form() {
        let en = I18n::new("en");
        let one = en.t_count("results.deleteTitle", 1, &[]);
        let many = en.t_count("results.deleteTitle", 3, &[]);
        assert_ne!(one, many);
        assert!(many.contains('3'), "{many}");
    }

    #[test]
    fn every_language_has_the_english_keys() {
        let en = I18n::new("en");
        for (lang, _) in LANGUAGES {
            let other = I18n::new(lang);
            let missing: Vec<&String> = en.strings.keys().filter(|k| !other.strings.contains_key(*k)).collect();
            assert!(missing.is_empty(), "{lang} misses {missing:?}");
        }
    }

    // Their keys are built from the ids, so no search for the literal key would find a missing one.
    #[test]
    fn every_system_view_has_a_title() {
        use barsql_sql::system_views::{ViewScope, views};
        let en = I18n::new("en");
        for driver in barsql_core::DriverType::KNOWN {
            for view in views(&driver, ViewScope::Server).chain(views(&driver, ViewScope::Table)) {
                assert!(en.raw(&view.title_key()).is_some(), "{driver}: {}", view.id);
            }
        }
    }
}
