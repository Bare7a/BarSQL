use std::cmp::Reverse;

use barsql_core::{ConnectionConfig, SavedQuery};
use gpui_kit::{App, Global, Window};
use jiff::Timestamp;
use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

use crate::dialogs::{self, Prompt};
use crate::i18n::t;
use crate::state::{self, set_setting_json, setting_json};

const PINNED_KEY: &str = "barsql-pinned-queries";
pub const SORT_KEY: &str = "barsql-saved-sort";

// Pins live only in the settings because saved queries have no pin field.
#[derive(Default)]
pub struct SavedQueries {
    list: Vec<SavedQuery>,
    pinned: Vec<String>,
}

impl Global for SavedQueries {}

pub fn init(cx: &mut App) {
    let list = state::bar(cx).list_saved_queries("");
    let pinned = setting_json(cx, PINNED_KEY);
    cx.set_global(SavedQueries { list, pinned });
}

pub fn refresh(cx: &mut App) {
    let list = state::bar(cx).list_saved_queries("");
    cx.global_mut::<SavedQueries>().list = list;
}

pub fn list(cx: &App) -> &[SavedQuery] {
    &cx.global::<SavedQueries>().list
}

pub fn get<'a>(cx: &'a App, id: &str) -> Option<&'a SavedQuery> {
    list(cx).iter().find(|q| q.id == id)
}

pub fn pinned(cx: &App) -> &[String] {
    &cx.global::<SavedQueries>().pinned
}

pub fn toggle_pin(id: &str, cx: &mut App) {
    let pinned = &mut cx.global_mut::<SavedQueries>().pinned;
    match pinned.iter().position(|p| p == id) {
        Some(ix) => {
            pinned.remove(ix);
        }
        None => pinned.push(id.to_string()),
    }
    let pinned = pinned.clone();
    set_setting_json(cx, PINNED_KEY, &pinned);
}

pub fn save_as_new(
    connection_id: String,
    sql: String,
    window: &mut Window,
    cx: &mut App,
    on_saved: impl Fn(SavedQuery, &mut Window, &mut App) + 'static,
) {
    let prompt = Prompt {
        title: t(cx, "dialog.saveQueryTitle"),
        description: Some(t(cx, "dialog.saveQueryDescription")),
        label: t(cx, "dialog.queryNameLabel"),
        placeholder: t(cx, "dialog.queryNamePlaceholder"),
        initial: String::new(),
        confirm: t(cx, "common.save"),
    };
    dialogs::prompt(prompt, window, cx, move |name, window, cx| {
        let query = SavedQuery { name, connection_id: connection_id.clone(), sql: sql.clone(), ..Default::default() };
        match state::bar(cx).save_saved_query(query) {
            Ok(saved) => {
                refresh(cx);
                on_saved(saved, window, cx);
            }
            Err(error) => failed(error.message, "errors.saveQueryFailed", window, cx),
        }
    });
}

pub fn failed(message: String, title_key: &str, window: &mut Window, cx: &mut App) {
    crate::dialogs::alert(t(cx, title_key), message.into(), window, cx);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SavedSort {
    #[default]
    Name,
    UpdatedAt,
    CreatedAt,
}

impl SavedSort {
    pub const ALL: [SavedSort; 3] = [SavedSort::Name, SavedSort::UpdatedAt, SavedSort::CreatedAt];

    pub fn key(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::UpdatedAt => "updatedAt",
            Self::CreatedAt => "createdAt",
        }
    }

    pub fn parse(key: &str) -> Self {
        Self::ALL.into_iter().find(|s| s.key() == key).unwrap_or_default()
    }
}

// Rough base-strength collation. Ignores case and accents and sorts punctuation, then digits, then letters.
fn collation_key(name: &str) -> Vec<(u8, char)> {
    name.nfd()
        .filter(|c| !is_combining_mark(*c))
        .flat_map(char::to_lowercase)
        .map(|c| {
            (
                if c.is_alphabetic() {
                    2
                } else if c.is_numeric() {
                    1
                } else {
                    0
                },
                c,
            )
        })
        .collect()
}

fn date_ms(iso: &str) -> i64 {
    iso.parse::<Timestamp>().map_or(0, |t| t.as_millisecond())
}

// Connection-less queries show in every scope. Pinned ones go first, keeping the sort order.
pub fn visible<'a>(
    list: &'a [SavedQuery],
    connection_id: Option<&str>,
    needle: &str,
    sort: SavedSort,
    connections: &[ConnectionConfig],
    pinned: &[String],
) -> Vec<&'a SavedQuery> {
    let needle = needle.trim().to_lowercase();
    let connection_name = |q: &SavedQuery| {
        connections.iter().find(|c| c.id == q.connection_id).map_or(String::new(), |c| c.name.to_lowercase())
    };
    let mut items: Vec<&SavedQuery> = list
        .iter()
        .filter(|q| connection_id.is_none_or(|id| q.connection_id.is_empty() || q.connection_id == id))
        .filter(|q| {
            needle.is_empty()
                || q.name.to_lowercase().contains(&needle)
                || q.sql.to_lowercase().contains(&needle)
                || (!q.connection_id.is_empty() && connection_name(q).contains(&needle))
        })
        .collect();
    match sort {
        SavedSort::Name => items.sort_by_cached_key(|q| collation_key(&q.name)),
        SavedSort::UpdatedAt => items.sort_by_key(|q| Reverse(date_ms(&q.updated_at))),
        SavedSort::CreatedAt => items.sort_by_key(|q| Reverse(date_ms(&q.created_at))),
    }
    let (mut first, rest): (Vec<_>, Vec<_>) = items.into_iter().partition(|q| pinned.contains(&q.id));
    first.extend(rest);
    first
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(id: &str, name: &str, connection_id: &str, created: &str, updated: &str) -> SavedQuery {
        SavedQuery {
            id: id.into(),
            name: name.into(),
            connection_id: connection_id.into(),
            sql: format!("SELECT '{name}'"),
            created_at: created.into(),
            updated_at: updated.into(),
        }
    }

    fn names<'a>(items: &[&'a SavedQuery]) -> Vec<&'a str> {
        items.iter().map(|q| q.name.as_str()).collect()
    }

    #[test]
    fn names_sort_like_locale_compare_at_base_strength() {
        let list: Vec<SavedQuery> =
            ["beta", "Alpha", "alpha", "Élan", "elan", "zeta", "_x", "10 rows", "2 rows", "Ärger", "apple"]
                .iter()
                .enumerate()
                .map(|(i, n)| query(&i.to_string(), n, "", "", ""))
                .collect();
        let sorted = visible(&list, None, "", SavedSort::Name, &[], &[]);
        assert_eq!(
            names(&sorted),
            ["_x", "10 rows", "2 rows", "Alpha", "alpha", "apple", "Ärger", "beta", "Élan", "elan", "zeta"]
        );
    }

    #[test]
    fn dates_sort_newest_first_with_unparsable_last() {
        let list = vec![
            query("1", "old", "", "2024-01-01T00:00:00Z", "2025-06-01T00:00:00Z"),
            query("2", "new", "", "2025-01-01T00:00:00Z", "2024-06-01T00:00:00Z"),
            query("3", "broken", "", "", "not a date"),
        ];
        assert_eq!(names(&visible(&list, None, "", SavedSort::CreatedAt, &[], &[])), ["new", "old", "broken"]);
        assert_eq!(names(&visible(&list, None, "", SavedSort::UpdatedAt, &[], &[])), ["old", "new", "broken"]);
    }

    #[test]
    fn scope_keeps_connectionless_queries_and_the_filter_reads_connection_names() {
        let list =
            vec![query("1", "mine", "c1", "", ""), query("2", "theirs", "c2", "", ""), query("3", "any", "", "", "")];
        let connections = vec![ConnectionConfig { id: "c2".into(), name: "Warehouse".into(), ..Default::default() }];
        assert_eq!(names(&visible(&list, Some("c1"), "", SavedSort::Name, &connections, &[])), ["any", "mine"]);
        assert_eq!(names(&visible(&list, None, "WARE", SavedSort::Name, &connections, &[])), ["theirs"]);
        assert_eq!(names(&visible(&list, None, "'min", SavedSort::Name, &connections, &[])), ["mine"]);
    }

    #[test]
    fn pinned_queries_float_up_in_sort_order() {
        let list = vec![query("1", "a", "", "", ""), query("2", "b", "", "", ""), query("3", "c", "", "", "")];
        let pinned = vec!["3".to_string(), "2".to_string()];
        assert_eq!(names(&visible(&list, None, "", SavedSort::Name, &[], &pinned)), ["b", "c", "a"]);
        assert_eq!(SavedSort::parse("updatedAt"), SavedSort::UpdatedAt);
        assert_eq!(SavedSort::parse("bogus"), SavedSort::Name);
    }
}
