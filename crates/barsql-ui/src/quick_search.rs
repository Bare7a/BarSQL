use std::ops::Range;
use std::rc::Rc;

use barsql_core::{ConnectionConfig, SavedQuery, TableInfo};
use barsql_io::is_space;
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::input::{Enter, InputEvent, InputState, MoveDown, MoveUp};
use gpui_kit::component::scroll::ScrollbarAxis;
use gpui_kit::component::{ActiveTheme, Icon, WindowExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::connections_panel::parse_color;
use crate::fuzzy::rank_candidate;
use crate::i18n::t;
use crate::scrollbars::HoverScrollbar as _;
use crate::tokens::{ICON_SM, RADIUS_LG, TEXT_BASE, TEXT_XS};
use crate::{form, saved_queries, schema};

const MAX_ITEMS: usize = 10;

#[derive(Clone, Debug, PartialEq)]
pub struct TabEntry {
    pub id: String,
    pub title: String,
    pub connection_id: String,
    pub icon: Lucide,
    // Schema and table, for table view tabs.
    pub table: Option<(String, String)>,
    pub saved_query_id: String,
}

pub struct SchemaTables {
    pub connection_id: String,
    pub schema: String,
    pub tables: Vec<TableInfo>,
}

#[derive(Default)]
pub struct Candidates {
    pub tabs: Vec<TabEntry>,
    pub connections: Vec<ConnectionConfig>,
    pub tables: Vec<SchemaTables>,
    pub saved: Vec<SavedQuery>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    Tab(String),
    Table { connection_id: String, schema: String, table: String },
    Saved(Box<SavedQuery>),
    Connection(Box<ConnectionConfig>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Tab,
    Table,
    Saved,
    Connection,
}

impl Kind {
    fn bias(self) -> f64 {
        match self {
            Self::Tab => 100.,
            Self::Table => 70.,
            Self::Saved => 40.,
            Self::Connection => 10.,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Item {
    pub kind: Kind,
    pub label: String,
    pub detail: Option<String>,
    // Connection colour. None draws the icon muted.
    pub color: Option<String>,
    pub icon: Lucide,
    pub score: f64,
    pub ranges: Vec<Range<usize>>,
    pub target: Target,
}

impl Item {
    fn rank(&self) -> f64 {
        self.score + self.kind.bias()
    }
}

// Linked tabs count, and so do unlinked tabs of the same name opened before linking.
fn is_saved_open(tabs: &[TabEntry], saved: &SavedQuery) -> bool {
    tabs.iter().any(|tab| tab.saved_query_id == saved.id)
        || tabs.iter().any(|tab| {
            tab.saved_query_id.is_empty()
                && tab.title == saved.name
                && (saved.connection_id.is_empty() || tab.connection_id == saved.connection_id)
        })
}

fn relation_icon(kind: &str) -> Lucide {
    if schema::is_view_kind(kind) { Lucide::View } else { Lucide::Table2 }
}

// An empty query lists only tabs and connections. Tables and saved queries need a query, and ones already open
// as tabs are skipped.
pub fn rank(query: &str, candidates: &Candidates) -> Vec<Item> {
    let q = query.trim_matches(is_space).to_lowercase();
    let connection = |id: &str| candidates.connections.iter().find(|c| c.id == id);
    let name = |id: &str| connection(id).map(|c| c.name.clone());
    let color = |id: &str| connection(id).map(|c| c.color.clone());
    let mut items = Vec::new();

    for tab in &candidates.tabs {
        let detail = name(&tab.connection_id);
        let Some(m) = rank_candidate(&q, &tab.title, &[detail.as_deref().unwrap_or_default()]) else { continue };
        items.push(Item {
            kind: Kind::Tab,
            label: tab.title.clone(),
            detail,
            color: color(&tab.connection_id),
            icon: tab.icon,
            score: m.score,
            ranges: m.ranges,
            target: Target::Tab(tab.id.clone()),
        });
    }

    for conn in &candidates.connections {
        let Some(m) = rank_candidate(&q, &conn.name, &[&conn.host, &conn.database]) else { continue };
        let detail = if conn.database.is_empty() {
            conn.driver.to_string()
        } else {
            format!("{} · {}", conn.driver, conn.database)
        };
        items.push(Item {
            kind: Kind::Connection,
            label: conn.name.clone(),
            detail: Some(detail),
            color: Some(conn.color.clone()),
            icon: Lucide::Database,
            score: m.score,
            ranges: m.ranges,
            target: Target::Connection(Box::new(conn.clone())),
        });
    }

    if !q.is_empty() {
        for group in &candidates.tables {
            let conn_name = name(&group.connection_id).unwrap_or_default();
            for table in &group.tables {
                let open = candidates.tabs.iter().any(|tab| {
                    tab.connection_id == group.connection_id
                        && tab
                            .table
                            .as_ref()
                            .is_some_and(|(schema, name)| *schema == group.schema && *name == table.name)
                });
                if open {
                    continue;
                }
                let Some(m) = rank_candidate(&q, &table.name, &[&group.schema, &conn_name]) else { continue };
                items.push(Item {
                    kind: Kind::Table,
                    label: table.name.clone(),
                    detail: Some(if conn_name.is_empty() { group.schema.clone() } else { conn_name.clone() }),
                    color: color(&group.connection_id),
                    icon: relation_icon(&table.kind),
                    score: m.score,
                    ranges: m.ranges,
                    target: Target::Table {
                        connection_id: group.connection_id.clone(),
                        schema: group.schema.clone(),
                        table: table.name.clone(),
                    },
                });
            }
        }

        for saved in &candidates.saved {
            if is_saved_open(&candidates.tabs, saved) {
                continue;
            }
            let detail = (!saved.connection_id.is_empty()).then(|| name(&saved.connection_id)).flatten();
            let Some(m) = rank_candidate(&q, &saved.name, &[detail.as_deref().unwrap_or_default()]) else { continue };
            items.push(Item {
                kind: Kind::Saved,
                label: saved.name.clone(),
                detail,
                color: (!saved.connection_id.is_empty()).then(|| color(&saved.connection_id)).flatten(),
                icon: Lucide::Bookmark,
                score: m.score,
                ranges: m.ranges,
                target: Target::Saved(Box::new(saved.clone())),
            });
        }
    }

    items.sort_by(|a, b| b.rank().total_cmp(&a.rank()));
    items.truncate(MAX_ITEMS);
    items
}

fn loaded_tables(connections: &[ConnectionConfig], cx: &App) -> Vec<SchemaTables> {
    connections
        .iter()
        .filter_map(|conn| schema::get(cx, &conn.id).map(|entry| (conn, entry)))
        .flat_map(|(conn, entry)| {
            entry.loaded_tables().into_iter().map(|(schema, tables)| SchemaTables {
                connection_id: conn.id.clone(),
                schema: schema.to_string(),
                tables: tables.to_vec(),
            })
        })
        .collect()
}

// Converts character ranges to byte ranges.
fn byte_ranges(text: &str, ranges: &[Range<usize>]) -> Vec<Range<usize>> {
    let offsets: Vec<usize> = text.char_indices().map(|(at, _)| at).chain([text.len()]).collect();
    ranges.iter().filter_map(|range| Some(*offsets.get(range.start)?..*offsets.get(range.end)?)).collect()
}

type OnOpen = Rc<dyn Fn(Target, &mut Window, &mut App)>;

// Tables show up only once their schema has loaded.
pub struct QuickSearchDialog {
    input: Entity<InputState>,
    candidates: Candidates,
    query: String,
    items: Vec<Item>,
    active: usize,
    scroll: ScrollHandle,
    on_open: OnOpen,
    _subscriptions: Vec<Subscription>,
}

impl QuickSearchDialog {
    fn new(candidates: Candidates, on_open: OnOpen, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(t(cx, "quickSearch.placeholder")));
        let subscriptions = vec![
            cx.subscribe(&input, |this, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    let value = input.read(cx).value().to_string();
                    this.set_query(value, cx);
                }
            }),
            cx.observe_global::<schema::Schemas>(|this, cx| {
                this.candidates.tables = loaded_tables(&this.candidates.connections, cx);
                this.rerank(cx);
            }),
        ];
        let mut this = Self {
            input,
            candidates,
            query: String::new(),
            items: Vec::new(),
            active: 0,
            scroll: ScrollHandle::new(),
            on_open,
            _subscriptions: subscriptions,
        };
        this.rerank(cx);
        this
    }

    fn set_query(&mut self, query: String, cx: &mut Context<Self>) {
        self.query = query;
        self.rerank(cx);
    }

    fn rerank(&mut self, cx: &mut Context<Self>) {
        self.items = rank(&self.query, &self.candidates);
        cx.notify();
    }

    // Results can shrink while typing, so clamp the active row on read.
    fn active_ix(&self) -> usize {
        self.active.min(self.items.len().saturating_sub(1))
    }

    fn step(&mut self, down: bool, cx: &mut Context<Self>) {
        if self.items.is_empty() {
            return;
        }
        let current = self.active_ix();
        self.active = if down { (current + 1).min(self.items.len() - 1) } else { current.saturating_sub(1) };
        self.scroll.scroll_to_item(self.active);
        cx.notify();
    }

    #[cfg(feature = "snapshot")]
    pub(crate) fn highlight(&mut self, label: &str, cx: &mut Context<Self>) {
        if let Some(ix) = self.items.iter().position(|item| item.label == label) {
            self.active = ix;
            self.scroll.scroll_to_item(ix);
            cx.notify();
        }
    }

    // Closes first, so the dialog hands focus back before the opened tab takes it.
    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = self.items.get(self.active_ix()) else { return };
        let (target, on_open) = (item.target.clone(), self.on_open.clone());
        window.close_dialog(cx);
        on_open(target, window, cx);
    }

    fn row(&self, ix: usize, item: &Item, active: bool, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let color = item.color.as_deref().and_then(parse_color).unwrap_or(theme.muted_foreground);
        let highlight =
            HighlightStyle { color: Some(theme.foreground.blend(theme.primary.opacity(0.7))), ..Default::default() };
        let label = StyledText::new(item.label.clone())
            .with_highlights(byte_ranges(&item.label, &item.ranges).into_iter().map(|range| (range, highlight)));
        let detail = item.detail.clone().filter(|detail| !detail.is_empty());
        h_flex()
            .id(("quick-search-item", ix))
            .gap(rems(0.462))
            .px(rems(0.923))
            .py(rems(0.769))
            .cursor_pointer()
            .when(active, |el| el.bg(theme.accent))
            .hover(|style| style.bg(theme.accent))
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                if *hovered && this.active != ix {
                    this.active = ix;
                    cx.notify();
                }
            }))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.active = ix;
                this.confirm(window, cx);
            }))
            .child(Icon::new(item.icon).size(ICON_SM).text_color(color))
            .child(div().flex_1().min_w_0().truncate().text_size(TEXT_BASE).child(label))
            .when_some(detail, |el, detail| {
                el.child(div().flex_none().text_size(TEXT_XS).text_color(theme.muted_foreground).child(detail))
            })
    }
}

impl Render for QuickSearchDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let rem = window.rem_size();
        let max_height = (rem * 32.308).min(window.viewport_size().height - rem * 16.923).max(px(0.));
        let active = self.active_ix();
        let list = if self.items.is_empty() {
            div()
                .px(rems(0.923))
                .py(rems(1.077))
                .text_color(theme.muted_foreground)
                .child(t(cx, "quickSearch.noResults"))
                .into_any_element()
        } else {
            let rows: Vec<_> =
                self.items.iter().enumerate().map(|(ix, item)| self.row(ix, item, ix == active, cx)).collect();
            let list = v_flex()
                .id("quick-search-list")
                .max_h(max_height)
                .overflow_y_scroll()
                .track_scroll(&self.scroll)
                .children(rows);
            div().relative().child(list).hover_scrollbar(&self.scroll, ScrollbarAxis::Vertical).into_any_element()
        };
        v_flex()
            .key_context("QuickSearch")
            .capture_action(cx.listener(|this, _: &MoveUp, _, cx| {
                cx.stop_propagation();
                this.step(false, cx);
            }))
            .capture_action(cx.listener(|this, _: &MoveDown, _, cx| {
                cx.stop_propagation();
                this.step(true, cx);
            }))
            .on_action(cx.listener(|this, _: &Enter, window, cx| this.confirm(window, cx)))
            .child(
                div()
                    .p(rems(0.769))
                    .border_b_1()
                    .border_color(theme.border)
                    .child(Styled::h(form::input(&self.input, window, cx), rems(2.462))),
            )
            .child(list)
    }
}

pub fn open(
    tabs: Vec<TabEntry>,
    connections: Vec<ConnectionConfig>,
    window: &mut Window,
    cx: &mut App,
    on_open: impl Fn(Target, &mut Window, &mut App) + 'static,
) -> Entity<QuickSearchDialog> {
    let candidates = Candidates {
        tables: loaded_tables(&connections, cx),
        saved: saved_queries::list(cx).to_vec(),
        tabs,
        connections,
    };
    let view = cx.new(|cx| QuickSearchDialog::new(candidates, Rc::new(on_open), window, cx));
    let dialog = view.clone();
    window.open_dialog(cx, move |modal, window, cx| {
        let rem = window.rem_size();
        modal
            .w(rem * 55.385)
            .margin_top(rem * 5.5)
            .p_0()
            .bg(cx.theme().sidebar)
            .rounded(RADIUS_LG)
            .close_button(false)
            .on_ok(|_, _, _| false)
            .child(dialog.clone())
    });
    let input = view.read(cx).input.clone();
    window.defer(cx, move |window, cx| input.update(cx, |state, cx| state.focus(window, cx)));
    view
}

// Used by the screenshots.
pub fn type_query(view: &Entity<QuickSearchDialog>, query: &str, window: &mut Window, cx: &mut App) {
    let input = view.read(cx).input.clone();
    input.update(cx, |state, cx| state.set_value(query.to_string(), window, cx));
    view.update(cx, |this, cx| this.set_query(query.to_string(), cx));
}

#[cfg(test)]
mod tests;
