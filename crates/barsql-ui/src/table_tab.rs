use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use barsql_app::{EditorTab, RunEvent, TableViewRef};
use barsql_core::{ConnectionConfig, QueryError, Row, RowDelete, RowUpdate, TableDataRequest, Value};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{InputEvent, InputMode, InputState};
use gpui_kit::component::{ActiveTheme, Disableable, Icon, IconName, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde_json::Value as Json;

use crate::cell_viewer;
use crate::completion::{self, Completion};
use crate::dialogs;
use crate::form::{self, ToolButton};
use crate::grid::{self, Grid, GridEvent, RowRef, TableOverlay};
use crate::i18n::{I18n, t, t_with};
use crate::results::ShownError;
use crate::schema;
use crate::sql_language::FilterCompletion;
use crate::state;
use crate::table_edits::{Keys, Outcome, OwnedCell, Staging, changes_row, foreign_key_filter};
use crate::toast;
use crate::tokens::{ICON_SM, TEXT_SM};

// Next page loads on scroll.
const PAGE_SIZE: usize = 100;

pub enum TableTabEvent {
    // Filter, sort or hidden columns changed, so the session needs saving.
    Edited,
    FocusedRowChanged,
    // Foreign-key jump to the referenced table, filtered to the row.
    OpenTable { schema: String, table: String, filter: String },
}

pub struct TableTab {
    pub id: String,
    pub title: SharedString,
    pub connection: ConnectionConfig,
    pub schema: String,
    pub table: String,
    filter: Entity<InputState>,
    completion: Entity<Completion<InputMode>>,
    applied_filter: String,
    order_by: Option<String>,
    order_desc: bool,
    hidden: Vec<String>,
    grid: Entity<Grid>,
    primary_keys: Vec<String>,
    keys: Keys,
    staging: Staging,
    has_more: bool,
    page_rows: usize,
    replace: bool,
    loaded: bool,
    loading: Option<Task<()>>,
    applying: bool,
    error: Option<ShownError>,
    foreign: HashMap<String, (String, String)>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<TableTabEvent> for TableTab {}

impl TableTab {
    pub fn new(tab: EditorTab, connection: ConnectionConfig, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let view = tab.table_view.unwrap_or_default();
        let filter = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t(cx, "tableView.filterPlaceholder"))
                .default_value(view.filter.clone())
        });
        let completion = cx.new(|cx| Completion::new(filter.clone(), None, window, cx).sized(TEXT_SM, rems(1.385)));
        let grid = cx.new(|cx| Grid::new(Arc::from(Vec::new()), cx));
        let subscriptions = vec![
            cx.subscribe_in(&filter, window, |this, filter, event: &InputEvent, window, cx| match event {
                InputEvent::PressEnter { .. } => this.apply_filter(window, cx),
                // Emptying the filter shows every row again, like the schema search does.
                InputEvent::Change if filter.read(cx).value().is_empty() && !this.applied_filter.is_empty() => {
                    this.apply_filter(window, cx)
                }
                _ => {}
            }),
            cx.subscribe_in(&grid, window, Self::grid_event),
            cx.observe(&grid, |_, _, cx| cx.notify()),
            cx.observe_global_in::<I18n>(window, |this, window, cx| {
                let placeholder = t(cx, "tableView.filterPlaceholder");
                this.filter.update(cx, |filter, cx| filter.set_placeholder(placeholder, window, cx));
            }),
        ];
        let mut this = Self {
            id: tab.id,
            title: tab.title.into(),
            connection,
            schema: view.schema,
            table: view.table,
            filter,
            completion,
            applied_filter: view.filter,
            order_by: Some(view.order_by).filter(|column| !column.is_empty()),
            order_desc: view.order_dir.eq_ignore_ascii_case("desc"),
            hidden: view.hidden_columns,
            grid,
            primary_keys: Vec::new(),
            keys: Keys::new(&Default::default(), &[]),
            staging: Staging::default(),
            has_more: false,
            page_rows: 0,
            replace: true,
            loaded: false,
            loading: None,
            applying: false,
            error: None,
            foreign: HashMap::new(),
            _subscriptions: subscriptions,
        };
        this.load_foreign_keys(cx);
        this.fetch(0, true, window, cx);
        this
    }

    pub(crate) fn completion(&self) -> Entity<Completion<InputMode>> {
        self.completion.clone()
    }

    #[cfg(test)]
    pub fn is_loaded(&self) -> bool {
        self.loaded && self.loading.is_none()
    }

    #[cfg(any(test, feature = "snapshot"))]
    pub(crate) fn grid(&self) -> Entity<Grid> {
        self.grid.clone()
    }

    #[cfg(any(test, feature = "snapshot"))]
    pub(crate) fn idle(&self) -> bool {
        self.loaded && self.loading.is_none() && !self.applying
    }

    // (edits, deletes) as the footer pills count them.
    #[cfg(any(test, feature = "snapshot"))]
    pub(crate) fn pending(&self) -> (usize, usize) {
        (self.staging.edit_count(), self.staging.delete_count())
    }

    #[cfg(test)]
    pub(crate) fn applied_filter(&self) -> &str {
        &self.applied_filter
    }

    #[cfg(test)]
    pub(crate) fn filter_text(&self, cx: &App) -> String {
        self.filter.read(cx).value().to_string()
    }

    // README screenshots. Leaves the tab as Reset plus a cleared filter would.
    #[cfg(feature = "snapshot")]
    pub(crate) fn start_over(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.filter.update(cx, |filter, cx| filter.set_value("", window, cx));
        self.applied_filter.clear();
        self.reset(window, cx);
    }

    pub fn stored(&self, cx: &App) -> EditorTab {
        let hidden = if self.loaded { self.grid.read(cx).hidden_names() } else { self.hidden.clone() };
        EditorTab {
            id: self.id.clone(),
            connection_id: self.connection.id.clone(),
            title: self.title.to_string(),
            color: self.connection.color.clone(),
            table_view: Some(TableViewRef {
                schema: self.schema.clone(),
                table: self.table.clone(),
                filter: self.applied_filter.clone(),
                order_by: self.order_by.clone().unwrap_or_default(),
                order_dir: if self.order_by.is_some() { self.dir().into() } else { String::new() },
                hidden_columns: hidden,
            }),
            ..Default::default()
        }
    }

    fn dir(&self) -> &'static str {
        if self.order_desc { "DESC" } else { "ASC" }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        let focus = self.grid.read(cx).focus_handle(cx);
        window.focus(&focus, cx);
    }

    pub fn focused_row(&self, cx: &App) -> Option<RowRef> {
        self.grid.read(cx).focused_row()
    }

    pub fn status(&self, cx: &App) -> (SharedString, bool) {
        match &self.error {
            Some(error) => (error.message.clone(), true),
            None if self.loading.is_some() => (t(cx, "tableView.loading"), false),
            None => {
                let rows = self.grid.read(cx).set().rows().to_string();
                (t_with(cx, "tableView.rowCount", &[("count", &rows)]), false)
            }
        }
    }

    // Called when the schema tree renames the table or changes its columns or rows. Staged edits no longer
    // fit, so start over.
    pub fn table_changed(&mut self, renamed: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(table) = renamed {
            if self.title.as_ref() == self.table {
                self.title = table.clone().into();
            }
            self.table = table;
            cx.emit(TableTabEvent::Edited);
        }
        self.reset(window, cx);
    }

    // A foreign-key jump into an open tab refilters it.
    pub fn show_filter(&mut self, filter: String, window: &mut Window, cx: &mut Context<Self>) {
        self.filter.update(cx, |state, cx| state.set_value(filter.clone(), window, cx));
        self.applied_filter = filter;
        self.staging.clear();
        self.fetch(0, true, window, cx);
        cx.emit(TableTabEvent::Edited);
    }

    fn editable(&self) -> bool {
        !self.connection.read_only && !self.primary_keys.is_empty()
    }

    fn load_foreign_keys(&mut self, cx: &mut Context<Self>) {
        let load = schema::columns(&self.connection.id, &self.schema, &self.table, cx);
        cx.spawn(async move |this, cx| {
            let Ok(columns) = load.await else { return };
            this.update(cx, |this, cx| {
                this.foreign = columns
                    .iter()
                    .filter(|c| c.is_foreign && !c.foreign_table.is_empty())
                    .map(|c| (c.name.clone(), (c.foreign_table.clone(), c.foreign_column.clone())))
                    .collect();
                this.sync_overlay(cx);
            })
            .ok();
        })
        .detach();
    }

    fn request(&self, offset: usize) -> TableDataRequest {
        TableDataRequest {
            schema: self.schema.clone(),
            table: self.table.clone(),
            offset: offset as i64,
            limit: PAGE_SIZE as i64,
            order_by: self.order_by.clone().unwrap_or_default(),
            order_dir: if self.order_by.is_some() { self.dir().into() } else { String::new() },
            filter: self.applied_filter.trim().to_string(),
        }
    }

    // A replacing load empties the grid only once the new rows start to arrive.
    fn fetch(&mut self, offset: usize, replace: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.error = None;
        self.replace = replace;
        self.page_rows = 0;
        self.grid.update(cx, |grid, _| grid.set_loading(true));
        let bar = state::bar(cx);
        let (connection, tab, request) = (self.connection.id.clone(), self.id.clone(), self.request(offset));
        let start = state::spawn(cx, async move { bar.query_table_stream(&connection, &tab, request).await });
        self.loading = Some(cx.spawn_in(window, async move |this, cx| {
            match start.await {
                Some(Ok(handle)) => {
                    while let Ok(event) = handle.events.recv().await {
                        let done = matches!(event, RunEvent::Done { .. });
                        if this.update(cx, |this, cx| this.apply_event(event, cx)).is_err() || done {
                            break;
                        }
                    }
                }
                Some(Err(error)) => {
                    this.update(cx, |this, cx| this.error = Some(ShownError::new(error, cx))).ok();
                }
                None => {}
            }
            this.update(cx, |this, cx| this.finish_fetch(cx)).ok();
        }));
        cx.notify();
    }

    fn apply_event(&mut self, event: RunEvent, cx: &mut Context<Self>) {
        match event {
            RunEvent::Meta { columns, .. } => {
                let completion = FilterCompletion {
                    columns: columns.iter().map(|c| (c.name.clone(), c.type_name.clone())).collect(),
                    driver: self.connection.driver.clone(),
                };
                self.completion.update(cx, |this, _| this.set_provider(Rc::new(completion)));
                if self.replace {
                    self.grid.update(cx, |grid, cx| grid.reset(columns, cx));
                    if !self.loaded {
                        let hidden = std::mem::take(&mut self.hidden);
                        self.grid.update(cx, |grid, cx| grid.set_hidden_names(&hidden, cx));
                    }
                }
                self.loaded = true;
            }
            RunEvent::Rows { chunk, .. } => {
                self.page_rows += chunk.rows();
                self.grid.update(cx, |grid, cx| grid.push(chunk, cx));
            }
            RunEvent::Result(result) => match result.error {
                Some(error) => self.error = Some(ShownError::new(error, cx)),
                None => {
                    if let Some(summary) = result.summary {
                        self.primary_keys = summary.primary_keys;
                    }
                }
            },
            RunEvent::Done { error: Some(error), .. } => self.error = Some(ShownError::new(error, cx)),
            RunEvent::Done { .. } => {}
        }
        cx.notify();
    }

    fn finish_fetch(&mut self, cx: &mut Context<Self>) {
        self.loading = None;
        self.applying = false;
        self.has_more = self.page_rows >= PAGE_SIZE;
        self.keys = Keys::new(self.grid.read(cx).set(), &self.primary_keys);
        self.grid.update(cx, |grid, cx| {
            grid.set_table_name(Some(self.table.clone()));
            grid.set_loading(false);
            cx.notify();
        });
        self.sync_overlay(cx);
    }

    // Maps the PK-keyed staging onto result rows for the grid.
    fn sync_overlay(&mut self, cx: &mut Context<Self>) {
        let grid = self.grid.read(cx);
        let set = grid.set();
        let column_ix = |name: &str| set.columns.iter().position(|c| c.name == name);
        let mut staged = HashMap::new();
        let mut deleted = std::collections::HashSet::new();
        for row in 0..set.rows() {
            let Some(key) = self.keys.key(row) else { continue };
            if let Some(edits) = self.staging.pending.edits.get(key) {
                for (column, value) in edits {
                    if let Some(ix) = column_ix(column) {
                        staged.insert((row, ix), value.clone());
                    }
                }
            }
            if self.staging.pending.deletes.contains(key) {
                deleted.insert(row);
            }
        }
        let sorted = self.order_by.clone().or_else(|| self.primary_keys.first().cloned());
        let sort = sorted.and_then(|name| column_ix(&name)).or((!set.columns.is_empty()).then_some(0));
        let foreign =
            self.foreign.iter().filter_map(|(column, (table, _))| Some((column_ix(column)?, table.clone()))).collect();
        let overlay = TableOverlay {
            staged: Arc::new(staged),
            deleted,
            sort: sort.map(|column| (column, self.order_desc)),
            foreign,
            editable: self.editable(),
        };
        self.grid.update(cx, |grid, cx| grid.set_overlay(overlay, cx));
        cx.notify();
    }

    fn apply_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.applied_filter = self.filter.read(cx).value().to_string();
        self.staging.clear();
        self.fetch(0, true, window, cx);
        cx.emit(TableTabEvent::Edited);
    }

    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.loading.is_none() && !self.applying {
            self.fetch(0, true, window, cx);
        }
    }

    fn reset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.staging.clear();
        self.fetch(0, true, window, cx);
    }

    fn load_more(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.loading.is_some() || !self.has_more || self.staging.has_pending() {
            return;
        }
        let offset = self.grid.read(cx).set().rows();
        self.fetch(offset, false, window, cx);
    }

    // Same column flips direction. A new column starts ascending, and staged changes are dropped.
    pub(crate) fn sort_by(&mut self, column: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(name) = self.grid.read(cx).set().columns.get(column).map(|c| c.name.clone()) else { return };
        let current = self.order_by.clone().or_else(|| self.primary_keys.first().cloned());
        self.order_desc = current.as_deref() == Some(name.as_str()) && !self.order_desc;
        self.order_by = Some(name);
        self.staging.clear();
        self.fetch(0, true, window, cx);
        cx.emit(TableTabEvent::Edited);
    }

    fn staged(&mut self, outcome: Outcome, cx: &mut Context<Self>) {
        if outcome == Outcome::Ambiguous {
            toast::error(t(cx, "tableView.ambiguousRow"), cx);
        }
        self.sync_overlay(cx);
    }

    fn grid_event(&mut self, grid: &Entity<Grid>, event: &GridEvent, window: &mut Window, cx: &mut Context<Self>) {
        let editable = self.editable();
        match event {
            GridEvent::EditCell { row, column, value } if editable => {
                let outcome = self.staging.edit_cell(grid.read(cx).set(), &self.keys, *row, *column, value.clone());
                self.staged(outcome, cx);
            }
            GridEvent::PasteCells(cells) if editable => {
                let outcome = self.staging.paste(grid.read(cx).set(), &self.keys, cells);
                self.staged(outcome, cx);
            }
            GridEvent::ToggleDelete(row) if editable => {
                let outcome = self.staging.toggle_delete(&self.keys, *row);
                self.staged(outcome, cx);
            }
            GridEvent::Undo => {
                if self.staging.undo() {
                    self.sync_overlay(cx);
                }
            }
            GridEvent::Redo => {
                if self.staging.redo() {
                    self.sync_overlay(cx);
                }
            }
            GridEvent::Apply => self.apply(window, cx),
            GridEvent::Refresh => self.refresh(window, cx),
            GridEvent::LoadMore => self.load_more(window, cx),
            GridEvent::SortRequested(column) => self.sort_by(*column, window, cx),
            GridEvent::OpenForeignKey { row, column } => self.open_foreign_key(*row, *column, window, cx),
            GridEvent::ViewCell { row, column } => {
                let state = grid.read(cx);
                let name = state.set().columns[*column].name.clone();
                let value = state.shown(*row, *column).display().map(str::to_string);
                cell_viewer::open(name, value, window, cx);
            }
            GridEvent::EditInViewer { row, column } if editable => {
                let state = grid.read(cx);
                let name = state.set().columns[*column].name.clone();
                let value = state.shown(*row, *column).display().map(str::to_string);
                let (grid, row, column) = (grid.downgrade(), *row, *column);
                let on_save: cell_viewer::OnSave = Rc::new(move |value, _, cx| {
                    grid.update(cx, |_, cx| cx.emit(GridEvent::EditCell { row, column, value })).ok();
                });
                cell_viewer::open_editor(name, value, on_save, window, cx);
            }
            GridEvent::Export => crate::export_dialog::open(grid.read(cx).export_source(), window, cx),
            GridEvent::FocusedRowChanged => cx.emit(TableTabEvent::FocusedRowChanged),
            GridEvent::HiddenChanged => cx.emit(TableTabEvent::Edited),
            _ => {}
        }
    }

    // SQLite leaves the column empty for a reference to the primary key, so look it up on the target.
    fn open_foreign_key(&mut self, row: usize, column: usize, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.grid.read(cx);
        let Some(name) = state.set().columns.get(column).map(|c| c.name.clone()) else { return };
        let Some((table, target)) = self.foreign.get(&name).cloned() else { return };
        let value = state.shown(row, column);
        let driver = self.connection.driver.clone();
        let schema = self.schema.clone();
        if !target.is_empty() {
            let filter = foreign_key_filter(&target, value, &driver);
            cx.emit(TableTabEvent::OpenTable { schema, table, filter });
            return;
        }
        let value = OwnedCell::new(value);
        let load = schema::columns(&self.connection.id, &schema, &table, cx);
        cx.spawn_in(window, async move |this, cx| {
            let key = load.await.ok().and_then(|columns| columns.iter().find(|c| c.is_primary).map(|c| c.name.clone()));
            this.update(cx, |_, cx| match key {
                Some(key) => {
                    let filter = foreign_key_filter(&key, value.cell(), &driver);
                    cx.emit(TableTabEvent::OpenTable { schema, table, filter });
                }
                None => toast::error(t(cx, "tableView.foreignKeyUnresolved"), cx),
            })
            .ok();
        })
        .detach();
    }

    // Deletes first, then edits of rows not deleted. Reload either way since part of a failed batch may
    // have landed, and re-applying what's still staged is safe.
    fn apply(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.editable() || !self.staging.has_pending() || self.applying || self.loading.is_some() {
            return;
        }
        self.applying = true;
        let pending = self.staging.pending.clone();
        let (schema, table) = (self.schema.clone(), self.table.clone());
        let deletes: Vec<Row> = pending.deletes.iter().map(|key| key_row(key)).collect();
        let updates: Vec<RowUpdate> = pending
            .edits
            .iter()
            .filter(|(key, _)| !pending.deletes.contains(key))
            .map(|(key, changes)| RowUpdate {
                schema: schema.clone(),
                table: table.clone(),
                primary_key: key_row(key),
                changes: changes_row(changes),
            })
            .collect();
        let bar = state::bar(cx);
        let connection = self.connection.id.clone();
        let work = state::spawn(cx, async move {
            if !deletes.is_empty() {
                let delete = RowDelete { schema, table, primary_keys: deletes };
                if let (_, Some(error)) = bar.delete_rows(&connection, &delete).await {
                    return Err(error);
                }
            }
            for update in &updates {
                bar.update_row(&connection, update).await?;
            }
            Ok::<(), QueryError>(())
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = work.await;
            this.update_in(cx, |this, window, cx| {
                match result {
                    Some(Ok(())) => this.staging.clear(),
                    Some(Err(error)) => dialogs::alert(t(cx, "errors.generic"), error.message.into(), window, cx),
                    None => {}
                }
                let filter = this.applied_filter.clone();
                this.filter.update(cx, |state, cx| state.set_value(filter, window, cx));
                this.fetch(0, true, window, cx);
                this.applying = true;
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn delete_focused(&mut self, cx: &mut Context<Self>) {
        if let Some(row) = self.grid.read(cx).focused_row().map(|row| row.row) {
            let outcome = self.staging.toggle_delete(&self.keys, row);
            self.staged(outcome, cx);
        }
    }

    fn filter_bar(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        h_flex()
            .h(rems(3.077))
            .flex_none()
            .px(rems(0.769))
            .gap(rems(0.615))
            .border_b_1()
            .border_color(theme.border)
            .child(Icon::new(Lucide::Funnel).size(ICON_SM).text_color(theme.muted_foreground))
            .child(
                completion::handlers(div(), &self.completion)
                    .when(self.completion.read(cx).is_open(cx), |el| el.key_context(completion::OPEN_CONTEXT))
                    .flex_1()
                    .min_w_0()
                    .debug_selector(|| "table-filter".into())
                    .child(form::filter_input(&self.filter, window, cx).cleanable(true))
                    .child(self.completion.clone()),
            )
            .child(
                form::filter_button("table-apply-filter", Icon::new(IconName::Search))
                    .debug_selector(|| "table-apply-filter".into())
                    .tooltip(t(cx, "tableView.applyFilter"))
                    .disabled(self.loading.is_some())
                    .on_click(cx.listener(|this, _, window, cx| this.apply_filter(window, cx))),
            )
    }

    fn footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let busy = self.loading.is_some() || self.applying;
        let pending = self.staging.has_pending();
        let (edits, deletes) = (self.staging.edit_count(), self.staging.delete_count());
        let pill = |text: SharedString, color: Hsla| form::badge(text, color);
        let read_only = self.connection.read_only;
        h_flex()
            .flex_none()
            .justify_between()
            .px(rems(0.923))
            .py(rems(0.615))
            .gap(rems(0.923))
            .border_t_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .gap(rems(0.462))
                    .child(
                        Button::new("table-reset")
                            .debug_selector(|| "table-reset".into())
                            .tool_label(t(cx, "tableView.reset"))
                            .disabled(!pending || busy)
                            .on_click(cx.listener(|this, _, window, cx| this.reset(window, cx))),
                    )
                    .child(
                        Button::new("table-apply")
                            .debug_selector(|| "table-apply".into())
                            .primary()
                            .tool_label(t(cx, "tableView.apply"))
                            .disabled(!pending || read_only || busy)
                            .on_click(cx.listener(|this, _, window, cx| this.apply(window, cx))),
                    )
                    .when(edits > 0, |el| {
                        el.child(pill(
                            t_with(cx, "tableView.pendingUpdates", &[("count", &edits.to_string())]),
                            theme.warning,
                        ))
                    })
                    .when(deletes > 0, |el| {
                        el.child(pill(
                            t_with(cx, "tableView.pendingDeletes", &[("count", &deletes.to_string())]),
                            theme.danger,
                        ))
                    }),
            )
            .child(
                h_flex()
                    .gap(rems(0.462))
                    .child(
                        Button::new("table-refresh")
                            .debug_selector(|| "table-refresh".into())
                            .tool_icon(Icon::new(Lucide::RefreshCw), ICON_SM)
                            .tooltip(t(cx, "tableView.refresh"))
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, window, cx| this.refresh(window, cx))),
                    )
                    .when(!read_only, |el| {
                        el.child(
                            Button::new("table-add-row")
                                .debug_selector(|| "table-add-row".into())
                                .tool_icon(Icon::new(IconName::Plus), ICON_SM)
                                .tooltip(t(cx, "tableView.addRow"))
                                .disabled(busy || !self.loaded)
                                .on_click(cx.listener(|this, _, window, cx| this.add_row(window, cx))),
                        )
                        .child(
                            Button::new("table-delete-row")
                                .debug_selector(|| "table-delete-row".into())
                                .tool_icon(Icon::new(IconName::Minus), ICON_SM)
                                .tooltip(t(cx, "tableView.deleteRow"))
                                .disabled(busy || self.primary_keys.is_empty())
                                .on_click(cx.listener(|this, _, _, cx| this.delete_focused(cx))),
                        )
                    }),
            )
    }

    fn add_row(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        crate::insert_row::open(
            self.connection.clone(),
            self.schema.clone(),
            self.table.clone(),
            window,
            cx,
            move |window, cx| {
                this.update(cx, |this, cx| {
                    this.staging.clear();
                    this.fetch(0, true, window, cx);
                })
                .ok();
            },
        );
    }

    fn body(&self, cx: &mut Context<Self>) -> AnyElement {
        if let Some(error) = &self.error {
            return error.card(None, cx).into_any_element();
        }
        if !self.loaded {
            let key = if self.loading.is_some() { "tableView.loading" } else { "results.noResults" };
            return div().p_4().text_color(cx.theme().muted_foreground).child(t(cx, key)).into_any_element();
        }
        let rows = self.grid.read(cx).set().rows().to_string();
        let mut meta = t_with(cx, "tableView.rowCount", &[("count", &rows)]).to_string();
        if self.loading.is_some() || self.applying {
            meta = format!("{meta} · {}", t(cx, "tableView.loading"));
        }
        v_flex()
            .size_full()
            .child(grid::toolbar(&self.grid, meta.into(), cx))
            .child(div().flex_1().min_h_0().child(self.grid.clone()))
            .into_any_element()
    }
}

// Decodes a PkKey for update_row and delete_rows.
fn key_row(key: &str) -> Row {
    let map: serde_json::Map<String, Json> = serde_json::from_str(key).unwrap_or_default();
    map.into_iter()
        .map(|(column, value)| {
            let value = match value {
                Json::Null => Value::Null,
                Json::Bool(b) => Value::Bool(b),
                Json::Number(n) => {
                    n.as_i64().map(Value::Int).unwrap_or_else(|| Value::Float(n.as_f64().unwrap_or(f64::NAN)))
                }
                Json::String(s) => Value::Text(s),
                other => Value::Text(other.to_string()),
            };
            (column, value)
        })
        .collect()
}

impl Render for TableTab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Panel colour behind the filter, the grid toolbar and the footer.
        v_flex()
            .size_full()
            .bg(cx.theme().sidebar)
            .child(self.filter_bar(window, cx))
            .child(div().flex_1().min_h_0().child(self.body(cx)))
            .child(self.footer(cx))
    }
}

#[cfg(test)]
mod tests;
