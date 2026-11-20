use std::sync::Arc;
use std::time::Duration;

use barsql_core::DriverType;
use barsql_db::Cell;
use barsql_io::{RowField, match_ranges, row_fields, row_json};
use barsql_sql::display_type;
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{
    Editor, EditorState, InputEvent, InputState, RangeDecoration, RangeDecorationCollection, RangeDecorationStyle,
};
use gpui_kit::component::scroll::ScrollbarAxis;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, IconName, Selectable, Sizable, StyledExt, TITLE_BAR_HEIGHT, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::actions::{ToggleJsonPanel, shortcut_label};
use crate::form;
use crate::grid::RowRef;
use crate::i18n::{I18n, count, t, t_with};
use crate::schema::{self, Reference, References, Schemas};
use crate::scrollbars::{HoverScrollbar as _, ScrollbarsOnHover as _};
use crate::state::{set_setting, set_setting_bool, setting, setting_bool};
use crate::table_edits::foreign_key_filter;
use crate::tokens::{ICON_SM, ICON_XS, RADIUS, TEXT_SM, TEXT_XS, TINT_BORDER};
use crate::{theme, toast};

const FILTER_DEBOUNCE: Duration = Duration::from_millis(150);
// Rows wait for the focus to rest for SETTLE after one took SLOW_SHOW to lay out, so held arrow keys keep up.
const SLOW_SHOW: Duration = Duration::from_millis(8);
const SETTLE: Duration = Duration::from_millis(100);
const HIDE_NULLS_KEY: &str = "barsql-json-hide-nulls";
const VIEW_KEY: &str = "barsql-json-view";
// Long values show their first lines; the cell viewer has the rest.
const VALUE_LINES: usize = 6;
const JSON_LINES: usize = 12;

// A table view's table, whose rows other tables' foreign keys point at.
#[derive(Clone, PartialEq)]
pub struct RowSource {
    pub connection_id: String,
    pub driver: DriverType,
    pub schema: String,
    pub table: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum View {
    Fields,
    Json,
}

pub enum JsonPanelEvent {
    OpenForeignKey { row: usize, column: usize },
    // `filter` picks the rows that point at this one.
    OpenTable { schema: String, table: String, filter: String },
}

// Filters by key or value. Wrapping the filter in slashes makes it a regex.
pub struct JsonPanel {
    row: Option<RowRef>,
    next: Option<Option<RowRef>>,
    settling: Option<Task<()>>,
    filter: Entity<InputState>,
    editor: Entity<EditorState>,
    // Filter matches in `shown`.
    marks: RangeDecorationCollection,
    shown: String,
    // False while `shown` is the no-match note.
    matched: bool,
    // `shown` hasn't reached the editor, which only the JSON view fills, as filling it highlights the text.
    editor_stale: bool,
    hide_nulls: bool,
    view: View,
    fields: Vec<RowField>,
    fields_scroll: ScrollHandle,
    source: Option<RowSource>,
    references: Option<References>,
    debounce: Option<Task<()>>,
    // Tests lower it.
    slow_show: Duration,
    _subscriptions: Vec<Subscription>,
}

impl JsonPanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder(t(cx, "jsonViewer.filterPlaceholder")));
        let editor =
            cx.new(|cx| EditorState::new(window, cx).language("json").line_number(false).soft_wrap(true).folding(true));
        let marks = editor.update(cx, |state, cx| state.create_range_decorations_collection(Vec::new(), cx));
        let subscriptions = vec![
            cx.subscribe_in(&filter, window, |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Change) {
                    this.schedule_refresh(window, cx);
                }
            }),
            cx.observe_global_in::<I18n>(window, |this, window, cx| {
                this.filter
                    .update(cx, |state, cx| state.set_placeholder(t(cx, "jsonViewer.filterPlaceholder"), window, cx));
                this.refresh(window, cx);
            }),
            cx.observe_global::<Schemas>(|this, cx| this.update_references(cx)),
        ];
        Self {
            row: None,
            next: None,
            settling: None,
            filter,
            editor,
            marks,
            shown: String::new(),
            matched: false,
            editor_stale: false,
            hide_nulls: setting_bool(cx, HIDE_NULLS_KEY, false),
            view: match setting(cx, VIEW_KEY).as_deref() {
                Some("fields") => View::Fields,
                _ => View::Json,
            },
            fields: Vec::new(),
            fields_scroll: ScrollHandle::new(),
            source: None,
            references: None,
            debounce: None,
            slow_show: SLOW_SHOW,
            _subscriptions: subscriptions,
        }
    }

    pub fn show(
        &mut self,
        row: Option<RowRef>,
        source: Option<RowSource>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Only moving within one result waits for the focus to rest; another tab or result shows at once.
        let same = source == self.source
            && matches!((&row, &self.row), (Some(new), Some(old)) if Arc::ptr_eq(&new.set.columns, &old.set.columns));
        if self.settling.is_some() && same {
            self.next = Some(row);
            self.settle(window, cx);
            return;
        }
        self.settling = None;
        self.next = None;
        if source != self.source
            && let Some(source) = &source
        {
            schema::retry_failed_references(&source.connection_id, &source.schema, &source.table, cx);
        }
        self.source = source;
        self.set_row(row, window, cx);
    }

    fn set_row(&mut self, row: Option<RowRef>, window: &mut Window, cx: &mut Context<Self>) {
        self.row = row;
        self.refresh(window, cx);
        self.update_references(cx);
    }

    fn settle(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settling = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(SETTLE).await;
            this.update_in(cx, |this, window, cx| {
                this.settling = None;
                if let Some(row) = this.next.take() {
                    this.set_row(row, window, cx);
                }
            })
            .ok();
        }));
    }

    fn schedule_refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.debounce = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(FILTER_DEBOUNCE).await;
            this.update_in(cx, |this, window, cx| this.refresh(window, cx)).ok();
        }));
    }

    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = &self.row else {
            cx.notify();
            return;
        };
        let started = cx.background_executor().now();
        let query = self.filter.read(cx).value();
        let (set, staged, dialect) = (&row.set, row.staged.as_deref(), row.dialect);
        let json = row_json(set, staged, row.row, &row.columns, &query, self.hide_nulls, dialect);
        self.update_fields(cx);
        self.matched = json.is_some();
        let text = json.unwrap_or_else(|| t(cx, "jsonViewer.noMatch").to_string());
        if text != self.shown {
            self.shown = text;
            self.editor_stale = true;
        }
        if self.view == View::Json {
            if self.editor_stale {
                self.editor_stale = false;
                let text = self.shown.clone();
                self.editor.update(cx, |state, cx| state.set_value(text, window, cx));
            }
            // Set after the text, since replacing it would shift them.
            let color = cx.theme().primary.opacity(TINT_BORDER);
            let found = if self.matched { match_ranges(&self.shown, &query) } else { Vec::new() };
            let marks = found
                .into_iter()
                .map(|range| RangeDecoration::new(range).with_style(RangeDecorationStyle::Fill).with_color(color))
                .collect();
            self.marks.set(marks, cx);
        }
        if self.settling.is_none() && cx.background_executor().now() - started >= self.slow_show {
            self.settle(window, cx);
        }
        cx.notify();
    }

    fn set_view(&mut self, view: View, window: &mut Window, cx: &mut Context<Self>) {
        self.view = view;
        set_setting(cx, VIEW_KEY, if view == View::Fields { "fields" } else { "json" });
        self.refresh(window, cx);
        self.update_references(cx);
    }

    // Only the field list reads them.
    fn update_fields(&mut self, cx: &App) {
        self.fields = match (&self.row, self.view) {
            (Some(row), View::Fields) => {
                let query = self.filter.read(cx).value();
                row_fields(&row.set, row.staged.as_deref(), row.row, &row.columns, &query, self.hide_nulls, row.dialect)
            }
            _ => Vec::new(),
        };
    }

    // Only the field list shows them, so only it asks: whenever the cache holds nothing for the table, as after a
    // schema change.
    fn update_references(&mut self, cx: &mut Context<Self>) {
        let entry = self.source.as_ref().and_then(|source| Some((source, schema::get(cx, &source.connection_id)?)));
        let references = entry.and_then(|(source, entry)| entry.references(&source.schema, &source.table).cloned());
        // Asking connects, so it waits for the schema: after a disconnect, or while a reload runs.
        let ask = self.view == View::Fields
            && entry.is_some_and(|(source, entry)| {
                entry.schemas().is_some() && !entry.references_requested(&source.schema, &source.table)
            });
        let same = match (&references, &self.references) {
            (Some(new), Some(old)) => Arc::ptr_eq(new, old),
            (new, old) => new.is_none() && old.is_none(),
        };
        if !same {
            self.references = references;
            cx.notify();
        }
        if ask && let Some(source) = &self.source {
            schema::load_references(&source.connection_id, &source.schema, &source.table, cx);
        }
    }

    fn toggle_nulls(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.hide_nulls = !self.hide_nulls;
        set_setting_bool(cx, HIDE_NULLS_KEY, self.hide_nulls);
        self.refresh(window, cx);
    }

    // Copies what's shown, filter applied.
    fn copy(&self, cx: &mut App) {
        cx.write_to_clipboard(ClipboardItem::new_string(self.shown.clone()));
        toast::success(t(cx, "toast.copiedClipboard"), cx);
    }

    #[cfg(feature = "snapshot")]
    pub(crate) fn set_filter(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.filter.update(cx, |filter, cx| filter.set_value(text.to_string(), window, cx));
        self.refresh(window, cx);
    }

    #[cfg(test)]
    pub fn text(&self) -> Option<&str> {
        self.row.as_ref().map(|_| self.shown.as_str())
    }

    fn field(&self, ix: usize, field: &RowField, row: &RowRef, cx: &Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let foreign = row.foreign.get(&field.column).filter(|_| field.value.is_some()).cloned();
        let key = match (row.primary.contains(&field.column), row.foreign.contains_key(&field.column)) {
            (true, _) => Some((Lucide::KeyRound, theme.warning)),
            (false, true) => Some((Lucide::Link, theme.primary)),
            _ => None,
        };
        let meta = &row.set.columns[field.column];
        let type_name = display_type(row.dialect, &meta.type_name);
        let lines = if field.json { JSON_LINES } else { VALUE_LINES };
        let value = match &field.value {
            Some(text) => div().child(SharedString::from(text.clone())).text_color(match foreign {
                Some(_) => theme.primary,
                None => theme.foreground,
            }),
            None => div().italic().text_color(theme.muted_foreground).child(t(cx, "common.null")),
        };
        let value = value
            .id(("json-field-value", ix))
            .debug_selector(move || format!("json-field-value-{ix}"))
            .font_family(theme.mono_font_family.clone())
            .text_size(px(theme::editor_font_size(cx)))
            .line_clamp(lines)
            .text_ellipsis()
            .cursor_pointer();
        let value = match foreign {
            Some(table) => {
                let tip = t_with(cx, "tableView.viewForeignKey", &[("table", &table)]);
                let (row, column) = (row.row, field.column);
                value
                    .hover(|style| style.underline())
                    .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                    .on_click(cx.listener(move |_, _, _, cx| cx.emit(JsonPanelEvent::OpenForeignKey { row, column })))
            }
            None => {
                let (text, tip) = (field.value.clone(), t(cx, "jsonViewer.copyValue"));
                value.tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx)).on_click(
                    move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(text.as_deref().unwrap_or_default().into()));
                        toast::success(t(cx, "toast.copiedClipboard"), cx);
                    },
                )
            }
        };
        v_flex()
            .id(("json-field", ix))
            .gap(rems(0.231))
            .px(rems(0.769))
            .py(rems(0.538))
            .border_b_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .gap(rems(0.462))
                    .text_size(TEXT_SM)
                    .children(key.map(|(icon, color)| Icon::new(icon).size(ICON_XS).text_color(color)))
                    .child(div().flex_1().min_w_0().truncate().font_medium().child(meta.name.clone()))
                    .child(div().flex_none().text_size(TEXT_XS).text_color(theme.muted_foreground).child(type_name)),
            )
            .child(value.test_support())
    }
}

impl EventEmitter<JsonPanelEvent> for JsonPanel {}

impl JsonPanel {
    // No rows point at a NULL, and a result without a referenced column can't say which do.
    fn reference(&self, ix: usize, reference: &Reference, row: &RowRef, cx: &Context<Self>) -> Option<AnyElement> {
        let driver = &self.source.as_ref()?.driver;
        let mut filters = Vec::new();
        for (column, referenced) in &reference.columns {
            let at = row.set.columns.iter().position(|meta| meta.name == *referenced)?;
            let value = row.set.cell(row.row, at);
            if matches!(value, Cell::Null) {
                return None;
            }
            filters.push(foreign_key_filter(column, value, driver));
        }
        let filter = filters.join(" AND ");
        let columns = reference.columns.iter().map(|(column, _)| column.as_str()).collect::<Vec<_>>().join(", ");
        let theme = cx.theme();
        let (schema, table) = (reference.schema.clone(), reference.table.clone());
        let element = h_flex()
            .id(("json-reference", ix))
            .debug_selector(move || format!("json-reference-{ix}"))
            .mx(rems(0.385))
            .px(rems(0.385))
            .py(rems(0.385))
            .gap(rems(0.462))
            .rounded(RADIUS)
            .text_size(TEXT_SM)
            .cursor_pointer()
            .hover(|style| style.bg(theme.sidebar_accent))
            .child(Icon::new(Lucide::Table).size(ICON_SM).text_color(theme.muted_foreground))
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(rems(0.308))
                    .child(div().flex_none().child(reference.table.clone()))
                    .child(div().min_w_0().truncate().text_color(theme.muted_foreground).child(columns)),
            )
            .child(Icon::new(IconName::ChevronRight).size(ICON_XS).text_color(theme.muted_foreground))
            .on_click(cx.listener(move |_, _, _, cx| {
                cx.emit(JsonPanelEvent::OpenTable {
                    schema: schema.clone(),
                    table: table.clone(),
                    filter: filter.clone(),
                })
            }))
            .test_support();
        Some(element.into_any_element())
    }

    fn switch(
        &self,
        id: &'static str,
        label: SharedString,
        view: View,
        cx: &Context<Self>,
    ) -> impl IntoElement + use<> {
        let (active, bright) = (self.view == view, cx.theme().foreground);
        form::caption(label, cx)
            .id(id)
            .debug_selector(move || id.into())
            .when(active, |el| el.text_color(bright))
            .when(!active, |el| el.cursor_pointer().hover(move |style| style.text_color(bright)))
            .on_click(cx.listener(move |this, _, window, cx| this.set_view(view, window, cx)))
            .test_support()
    }
}

impl Render for JsonPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let shortcut = shortcut_label(&ToggleJsonPanel, window).unwrap_or_default();
        let close = t_with(cx, "tooltip.closeJsonViewer", &[("shortcut", &shortcut)]);
        let number = self.row.as_ref().map(|row| t_with(cx, "jsonViewer.row", &[("number", &count(cx, row.number))]));
        // The header lines up with the tab bar, the filter row with the toolbar, and the buttons with the window's.
        let column = |button: Button| h_flex().flex_none().w(TITLE_BAR_HEIGHT).justify_center().child(button);
        let nulls_tip = t(cx, if self.hide_nulls { "jsonViewer.showNulls" } else { "jsonViewer.hideNulls" });
        let header = h_flex()
            .debug_selector(|| "json-panel-header".into())
            .h(rems(2.769))
            .flex_none()
            .pl(rems(0.769))
            .justify_between()
            .border_b_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .min_w_0()
                    .gap(rems(0.615))
                    .child(self.switch("json-view-fields", t(cx, "jsonViewer.fields"), View::Fields, cx))
                    .child(self.switch("json-view-json", t(cx, "jsonViewer.json"), View::Json, cx))
                    .children(number.map(|number| {
                        form::badge(number.to_uppercase(), theme.foreground).font_family(theme.mono_font_family.clone())
                    })),
            )
            .child(
                h_flex()
                    .flex_none()
                    .when(self.row.is_some(), |el| {
                        el.child(column(
                            Button::new("json-hide-nulls")
                                .ghost()
                                .small()
                                .icon(Icon::new(Lucide::EyeOff))
                                .selected(self.hide_nulls)
                                .tooltip(nulls_tip)
                                .on_click(cx.listener(|this, _, window, cx| this.toggle_nulls(window, cx))),
                        ))
                        .child(column(
                            Button::new("json-panel-copy")
                                .ghost()
                                .small()
                                .icon(Icon::new(Lucide::Copy))
                                .tooltip(t(cx, "jsonViewer.copy"))
                                .disabled(!self.matched)
                                .on_click(cx.listener(|this, _, _, cx| this.copy(cx))),
                        ))
                    })
                    .child(column(
                        Button::new("json-panel-close")
                            .ghost()
                            .small()
                            .icon(Icon::new(IconName::Close))
                            .tooltip(close)
                            .on_click(|_, window, cx| window.dispatch_action(Box::new(ToggleJsonPanel), cx)),
                    )),
            );
        let body = match &self.row {
            None => div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .p_4()
                .text_color(theme.muted_foreground)
                .child(t(cx, "jsonViewer.emptySelectRow"))
                .into_any_element(),
            Some(row) => v_flex()
                .flex_1()
                .min_h_0()
                .child(
                    h_flex()
                        .h(rems(3.077))
                        .flex_none()
                        .gap(rems(0.615))
                        .px(rems(0.769))
                        .border_b_1()
                        .border_color(theme.border)
                        .debug_selector(|| "json-filter".into())
                        .child(Icon::new(IconName::Search).size(ICON_SM).text_color(theme.muted_foreground))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(form::filter_input(&self.filter, window, cx).cleanable(true)),
                        ),
                )
                .child(match self.view {
                    View::Json => div()
                        .flex_1()
                        .min_h_0()
                        .child(
                            Editor::new(&self.editor)
                                .readonly(true)
                                .context_menu(crate::context_menu::editor(&self.editor))
                                .size_full()
                                .bordered(false)
                                .text_size(px(theme::editor_font_size(cx)))
                                .scrollbars_on_hover(),
                        )
                        .into_any_element(),
                    View::Fields => {
                        let filtered = !self.filter.read(cx).value().trim().is_empty();
                        // Without a filter, only Hide NULL columns empties a list of shown columns.
                        let note = match (filtered, self.hide_nulls && !row.columns.is_empty()) {
                            (true, _) => Some("jsonViewer.noFieldMatch"),
                            (false, true) => Some("jsonViewer.allNull"),
                            (false, false) => None,
                        };
                        let empty = note
                            .filter(|_| self.fields.is_empty())
                            .map(|key| div().p(rems(0.769)).text_color(theme.muted_foreground).child(t(cx, key)));
                        let references: Vec<AnyElement> = self
                            .references
                            .iter()
                            .flat_map(|references| references.iter())
                            .enumerate()
                            .filter_map(|(ix, reference)| self.reference(ix, reference, row, cx))
                            .collect();
                        let list = v_flex()
                            .id("json-fields")
                            .debug_selector(|| "json-fields".into())
                            .size_full()
                            .overflow_y_scroll()
                            .track_scroll(&self.fields_scroll)
                            .children(empty)
                            .children(self.fields.iter().enumerate().map(|(ix, field)| self.field(ix, field, row, cx)))
                            .when(!references.is_empty(), |el| {
                                el.child(
                                    div()
                                        .px(rems(0.769))
                                        .pt(rems(0.923))
                                        .pb(rems(0.308))
                                        .child(form::caption(t(cx, "jsonViewer.referencedBy"), cx)),
                                )
                                .children(references)
                                .child(div().h(rems(0.462)))
                            });
                        div()
                            .relative()
                            .flex_1()
                            .min_h_0()
                            .child(list)
                            .hover_scrollbar(&self.fields_scroll, ScrollbarAxis::Vertical)
                            .into_any_element()
                    }
                })
                .into_any_element(),
        };
        v_flex()
            .size_full()
            .debug_selector(|| "json-panel".into())
            .bg(theme.sidebar)
            .border_l_1()
            .border_color(theme.border)
            .child(header)
            .child(body)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use barsql_db::{ChunkBuilder, ColumnMeta, ResultSet};
    use gpui_kit::component::TITLE_BAR_HEIGHT;
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{AppContext as _, Entity, TestAppContext, VisualTestContext, px};

    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;

    use super::{JsonPanel, JsonPanelEvent, RowSource, SETTLE, View};
    use crate::grid::RowRef;
    use crate::schema;
    use crate::test_support::{Env, settle};

    fn row() -> RowRef {
        row_with(Some("{\"nick\":\"ann\"}"))
    }

    // (id 7, name), with a NULL name for None.
    fn row_with(name: Option<&str>) -> RowRef {
        let columns: Arc<[ColumnMeta]> = ["id", "name"]
            .map(|name| ColumnMeta { name: name.into(), type_name: "TEXT".into(), ..Default::default() })
            .to_vec()
            .into();
        let mut builder = ChunkBuilder::new(2, 1);
        builder.push_number(|s| s.push('7'));
        match name {
            Some(name) => builder.push_text(|s| s.push_str(name)),
            None => builder.push_null(),
        }
        builder.end_row();
        let mut set = ResultSet::new(columns);
        set.push(Arc::new(builder.finish()));
        RowRef {
            set,
            row: 0,
            columns: vec![0, 1],
            number: 1,
            dialect: None,
            primary: Default::default(),
            foreign: Default::default(),
            staged: None,
        }
    }

    fn ids() -> ResultSet {
        let columns: Arc<[ColumnMeta]> =
            [ColumnMeta { name: "id".into(), type_name: "INT".into(), ..Default::default() }].into();
        let mut builder = ChunkBuilder::new(1, 3);
        for id in 1..=3 {
            builder.push_number(|s| s.push_str(&id.to_string()));
            builder.end_row();
        }
        let mut set = ResultSet::new(columns);
        set.push(Arc::new(builder.finish()));
        set
    }

    fn numbered(set: &ResultSet, number: usize) -> RowRef {
        RowRef {
            set: set.clone(),
            row: number - 1,
            columns: vec![0],
            number,
            dialect: None,
            primary: Default::default(),
            foreign: Default::default(),
            staged: None,
        }
    }

    fn shown_text(panel: &Entity<JsonPanel>, cx: &mut VisualTestContext) -> Option<String> {
        panel.read_with(cx, |panel, _| panel.text().map(str::to_string))
    }

    #[gpui_kit::test]
    fn rows_after_a_slow_one_wait_for_the_focus_to_rest(cx: &mut TestAppContext) {
        let _env = Env::new(cx);
        let (panel, cx) = cx.add_window_view(JsonPanel::new);
        // A show takes no time on the test clock, so only zero counts it as slow.
        panel.update(cx, |panel, _| panel.slow_show = Duration::ZERO);
        let set = ids();
        let show = |number, cx: &mut VisualTestContext| {
            panel.update_in(cx, |panel, window, cx| panel.show(Some(numbered(&set, number)), None, window, cx))
        };
        let shown = |cx: &mut VisualTestContext| {
            panel.read_with(cx, |panel, _| (panel.row.as_ref().map(|row| row.number), panel.text().map(str::to_string)))
        };
        show(1, cx);
        assert_eq!(shown(cx), (Some(1), Some("{\n  \"id\": 1\n}".into())), "the first row shows at once");
        show(2, cx);
        cx.executor().advance_clock(SETTLE / 2);
        show(3, cx);
        cx.executor().advance_clock(SETTLE / 2);
        cx.run_until_parked();
        assert_eq!(shown(cx), (Some(1), Some("{\n  \"id\": 1\n}".into())), "the focus is still moving");
        cx.executor().advance_clock(SETTLE);
        cx.run_until_parked();
        assert_eq!(shown(cx), (Some(3), Some("{\n  \"id\": 3\n}".into())));
        show(2, cx);
        panel.update_in(cx, |panel, window, cx| panel.show(Some(row()), None, window, cx));
        assert_eq!(shown(cx).0, Some(1), "another result shows at once");
    }

    #[gpui_kit::test]
    fn staged_edits_show_as_the_grid_shows_them(cx: &mut TestAppContext) {
        let _env = Env::new(cx);
        let (panel, cx) = cx.add_window_view(JsonPanel::new);
        let mut edited = row();
        edited.staged = Some(Arc::new(HashMap::from([((0, 1), Some("bob".to_string()))])));
        panel.update_in(cx, |panel, window, cx| panel.set_view(View::Fields, window, cx));
        panel.update_in(cx, |panel, window, cx| panel.show(Some(edited), None, window, cx));
        assert_eq!(shown_text(&panel, cx).as_deref(), Some("{\n  \"id\": 7,\n  \"name\": \"bob\"\n}"));
        let values = panel.read_with(cx, |panel, _| {
            panel.fields.iter().map(|f| f.value.as_deref().map(str::to_string)).collect::<Vec<_>>()
        });
        assert_eq!(values, [Some("7".to_string()), Some("bob".to_string())]);
    }

    fn set_filter(panel: &Entity<JsonPanel>, text: &'static str, cx: &mut VisualTestContext) {
        panel.update_in(cx, |panel, window, cx| {
            panel.filter.update(cx, |state, cx| state.set_value(text, window, cx));
            panel.refresh(window, cx);
        });
    }

    #[gpui_kit::test]
    fn copy_puts_the_json_shown_on_the_clipboard(cx: &mut TestAppContext) {
        let _env = Env::new(cx);
        let (panel, cx) = cx.add_window_view(JsonPanel::new);
        panel.update_in(cx, |panel, window, cx| panel.show(Some(row()), None, window, cx));
        set_filter(&panel, "nick", cx);
        cx.update(|window, cx| window.click("json-panel-copy", cx));
        let copied = cx.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text()));
        assert_eq!(copied.as_deref(), Some("{\n  \"name\": {\n    \"nick\": \"ann\"\n  }\n}"));
    }

    #[gpui_kit::test]
    fn the_field_list_opens_foreign_keys_and_copies_other_values(cx: &mut TestAppContext) {
        let _env = Env::new(cx);
        let (panel, cx) = cx.add_window_view(JsonPanel::new);
        let opened = Rc::new(RefCell::new(Vec::new()));
        let seen = opened.clone();
        cx.update(|window, cx| {
            window
                .subscribe(&panel, cx, move |_, event: &JsonPanelEvent, _, _| {
                    if let JsonPanelEvent::OpenForeignKey { row, column } = event {
                        seen.borrow_mut().push((*row, *column));
                    }
                })
                .detach();
        });
        let mut keyed = row();
        keyed.foreign.insert(0, "people".into());
        panel.update_in(cx, |panel, window, cx| panel.show(Some(keyed), None, window, cx));
        cx.update(|window, cx| window.click("json-view-fields", cx));
        assert_eq!(panel.read_with(cx, |panel, _| panel.view), View::Fields);
        cx.update(|window, cx| window.click(("json-field-value", 0usize), cx));
        assert_eq!(*opened.borrow(), [(0, 0)]);
        cx.update(|window, cx| window.click(("json-field-value", 1usize), cx));
        let copied = cx.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text()));
        assert_eq!(copied.as_deref(), Some("{\n  \"nick\": \"ann\"\n}"));
        let next = cx.update(|window, cx| cx.new(|cx| JsonPanel::new(window, cx)));
        assert_eq!(next.read_with(cx, |panel, _| panel.view), View::Fields, "remembered");
    }

    #[gpui_kit::test]
    fn references_load_once_per_table_and_open_filtered(cx: &mut TestAppContext) {
        let env = Env::new(cx);
        for ddl in [
            "CREATE TABLE people (id INTEGER PRIMARY KEY)",
            "CREATE TABLE pets (id INTEGER PRIMARY KEY, owner_id INTEGER REFERENCES people (id))",
        ] {
            env.runtime.block_on(env.bar.execute_query(&env.connection.id, ddl)).unwrap();
        }
        let (panel, cx) = cx.add_window_view(JsonPanel::new);
        let opened = Rc::new(RefCell::new(Vec::new()));
        let seen = opened.clone();
        cx.update(|window, cx| {
            window
                .subscribe(&panel, cx, move |_, event: &JsonPanelEvent, _, _| {
                    if let JsonPanelEvent::OpenTable { table, filter, .. } = event {
                        seen.borrow_mut().push((table.clone(), filter.clone()));
                    }
                })
                .detach();
        });
        let people = RowSource {
            connection_id: env.connection.id.clone(),
            driver: env.connection.driver.clone(),
            schema: "main".into(),
            table: "people".into(),
        };
        let show = |source: Option<RowSource>, cx: &mut VisualTestContext| {
            panel.update_in(cx, |panel, window, cx| panel.show(Some(row()), source, window, cx))
        };
        let loaded = |cx: &mut VisualTestContext| panel.read_with(cx, |panel, _| panel.references.clone());
        let id = env.connection.id.clone();
        let asking = move |cx: &mut VisualTestContext| {
            cx.update(|_, cx| schema::get(cx, &id).is_some_and(|entry| entry.references_loading("main", "people")))
        };
        // A table view loads its connection's schema; the panel waits for it, as asking would connect.
        let schemas = env.connection.id.clone();
        cx.update(|_, cx| schema::ensure_loaded(&schemas, barsql_core::DriverType::Sqlite, cx));
        settle(cx, |cx| cx.update(|_, cx| schema::get(cx, &schemas).is_some_and(|entry| entry.schemas().is_some())));
        panel.update_in(cx, |panel, window, cx| panel.set_view(View::Fields, window, cx));
        show(Some(people.clone()), cx);
        assert!(asking(cx));
        cx.update(|_, cx| schema::reload_tables(&env.connection.id, "main", cx));
        assert!(asking(cx), "a reload that dropped the load asks again");
        settle(cx, |cx| loaded(cx).is_some());
        let first = loaded(cx).unwrap();
        show(None, cx);
        show(Some(people.clone()), cx);
        assert!(!asking(cx), "a table seen before asks no more");
        assert_eq!(first.len(), 1);
        cx.update(|window, cx| window.click(("json-reference", 0usize), cx));
        let opened = opened.borrow().clone();
        assert_eq!(opened.len(), 1);
        assert_eq!(opened[0].0, "pets");
        assert!(opened[0].1.contains("owner_id") && opened[0].1.ends_with("= 7"), "{}", opened[0].1);
        cx.update(|_, cx| schema::reload_tables(&env.connection.id, "main", cx));
        settle(cx, |cx| loaded(cx).is_some_and(|keys| !Arc::ptr_eq(&keys, &first)));
        cx.update(|_, cx| schema::forget(&env.connection.id, cx));
        assert!(!asking(cx), "a disconnect asks nothing, as asking would connect");
    }

    #[gpui_kit::test]
    fn the_null_switch_hides_null_columns_and_is_remembered(cx: &mut TestAppContext) {
        let _env = Env::new(cx);
        let (panel, cx) = cx.add_window_view(JsonPanel::new);
        panel.update_in(cx, |panel, window, cx| panel.show(Some(row_with(None)), None, window, cx));
        assert_eq!(shown_text(&panel, cx).as_deref(), Some("{\n  \"id\": 7,\n  \"name\": null\n}"));
        cx.update(|window, cx| window.click("json-hide-nulls", cx));
        assert_eq!(shown_text(&panel, cx).as_deref(), Some("{\n  \"id\": 7\n}"));
        let next = cx.update(|window, cx| cx.new(|cx| JsonPanel::new(window, cx)));
        next.update_in(cx, |panel, window, cx| panel.show(Some(row_with(None)), None, window, cx));
        assert_eq!(shown_text(&next, cx).as_deref(), Some("{\n  \"id\": 7\n}"));
    }

    #[gpui_kit::test]
    fn the_filter_marks_what_it_matched(cx: &mut TestAppContext) {
        let _env = Env::new(cx);
        let (panel, cx) = cx.add_window_view(JsonPanel::new);
        panel.update_in(cx, |panel, window, cx| panel.show(Some(row()), None, window, cx));
        set_filter(&panel, "NICK", cx);
        let marked = panel.read_with(cx, |panel, cx| {
            panel.marks.get_ranges(cx).into_iter().map(|range| panel.shown[range].to_string()).collect::<Vec<_>>()
        });
        assert_eq!(marked, ["nick"]);
        set_filter(&panel, "", cx);
        assert!(panel.read_with(cx, |panel, cx| panel.marks.get_ranges(cx)).is_empty(), "no filter, no marks");
    }

    #[gpui_kit::test]
    fn the_panel_shows_the_row_and_filters_it(cx: &mut TestAppContext) {
        let _env = Env::new(cx);
        let (panel, cx) = cx.add_window_view(JsonPanel::new);
        let text = |cx: &mut VisualTestContext| panel.read_with(cx, |panel, _| panel.text().map(str::to_string));
        assert_eq!(text(cx), None);
        panel.update_in(cx, |panel, window, cx| panel.show(Some(row()), None, window, cx));
        assert_eq!(text(cx).as_deref(), Some("{\n  \"id\": 7,\n  \"name\": {\n    \"nick\": \"ann\"\n  }\n}"));
        panel.update_in(cx, |panel, window, cx| {
            panel.filter.update(cx, |state, cx| state.set_value("NICK", window, cx));
            panel.refresh(window, cx);
        });
        assert_eq!(text(cx).as_deref(), Some("{\n  \"name\": {\n    \"nick\": \"ann\"\n  }\n}"));
        panel.update_in(cx, |panel, window, cx| {
            panel.filter.update(cx, |state, cx| state.set_value("zzz", window, cx));
            panel.refresh(window, cx);
        });
        assert_eq!(text(cx).as_deref(), Some("// No key or value matches the filter"));
        panel.update_in(cx, |panel, window, cx| panel.show(None, None, window, cx));
        assert_eq!(text(cx), None);
    }

    // The window's buttons are TITLE_BAR_HEIGHT-wide columns at the right edge. Smallest, default and largest zoom.
    #[gpui_kit::test]
    fn the_header_buttons_sit_under_the_windows_buttons(cx: &mut TestAppContext) {
        let _env = Env::new(cx);
        let (panel, cx) = cx.add_window_view(JsonPanel::new);
        panel.update_in(cx, |panel, window, cx| panel.show(Some(row()), None, window, cx));
        for zoom in [10., 13., 22.] {
            cx.update(|window, cx| {
                window.set_rem_size(px(zoom));
                window.render_frame(cx);
                let close_y = window.find("json-panel-close").bounds().center().y;
                for (column, id) in ["json-panel-close", "json-panel-copy", "json-hide-nulls"].into_iter().enumerate() {
                    let center = window.find(id).bounds().center();
                    let under = window.viewport_size().width - TITLE_BAR_HEIGHT * (column as f32 + 0.5);
                    assert!((center.x - under).abs() < px(0.5), "zoom {zoom}, {id}: {:?} vs {under:?}", center.x);
                    assert!((center.y - close_y).abs() < px(0.5), "zoom {zoom}, {id} is in the header");
                }
            });
        }
    }

    #[gpui_kit::test]
    fn the_filter_clears_with_its_x(cx: &mut TestAppContext) {
        let _env = Env::new(cx);
        let (panel, cx) = cx.add_window_view(JsonPanel::new);
        panel.update_in(cx, |panel, window, cx| {
            panel.show(Some(row()), None, window, cx);
            panel.filter.update(cx, |state, cx| state.set_value("nick", window, cx));
        });
        // GPUI Kit's clear button.
        cx.update(|window, cx| window.click("clean", cx));
        cx.executor().advance_clock(super::FILTER_DEBOUNCE);
        cx.run_until_parked();
        let (filter, text) = panel
            .read_with(cx, |panel, cx| (panel.filter.read(cx).value().to_string(), panel.text().map(str::to_string)));
        assert_eq!(filter, "");
        assert_eq!(text.as_deref(), Some("{\n  \"id\": 7,\n  \"name\": {\n    \"nick\": \"ann\"\n  }\n}"));
    }

    // The editor element sets read-only on its state as it draws, so the element has to ask for it.
    #[gpui_kit::test]
    fn the_json_cannot_be_typed_into(cx: &mut TestAppContext) {
        let _env = Env::new(cx);
        let (panel, cx) = cx.add_window_view(JsonPanel::new);
        panel.update_in(cx, |panel, window, cx| panel.show(Some(row()), None, window, cx));
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let editor = panel.read_with(cx, |panel, _| panel.editor.clone());
        editor.update_in(cx, |state, window, cx| state.focus(window, cx));
        let value = |cx: &mut VisualTestContext| editor.read_with(cx, |state, _| state.value().to_string());
        let before = value(cx);
        cx.simulate_input("x");
        assert_eq!(value(cx), before);
    }
}
