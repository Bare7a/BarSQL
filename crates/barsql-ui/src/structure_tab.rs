mod checks;
mod columns;
mod foreign_keys;
mod indexes;
mod preview;
mod sheet;
#[cfg(test)]
mod tests;

use std::rc::Rc;

use barsql_app::{EditorTab, StructureRef};
use barsql_core::{Check, ConnectionConfig, ForeignKey, StructColumn, StructIndex, TableStructure};
use barsql_sql::structure::{
    ChangePlan, ColumnRow, Draft, Problem, ProblemKind, Row, Section, changes, plan_changes, plan_create,
};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{EditorState, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme, Disableable, Icon, IconName, Sizable, StyledExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use self::preview::PreviewDrag;
use self::sheet::Sheet;
use crate::cell_viewer;
use crate::form::{self, ToolButton as _};
use crate::grid::{Grid, GridEvent};
use crate::i18n::{I18n, t, t_count, t_with};
use crate::schema;
use crate::schema_actions;
use crate::schema_tree::TableChange;
use crate::state;
use crate::toast;
use crate::tokens::{ICON_SM, TEXT_SM};

const CONTEXT: &str = "StructureTab";

actions!(structure_tab, [ApplyStructure]);

#[cfg(target_os = "macos")]
const MOD: &str = "cmd";
#[cfg(not(target_os = "macos"))]
const MOD: &str = "ctrl";

pub fn init(cx: &mut App) {
    cx.bind_keys([KeyBinding::new(&format!("{MOD}-s"), ApplyStructure, Some(CONTEXT))]);
}

pub enum StructureTabEvent {
    Edited,
    Applied { created: Option<String> },
}

enum Load {
    Loading,
    Ready,
    Failed(String),
}

const SECTIONS: [(Section, &str); 4] = [
    (Section::Columns, "structure.columns"),
    (Section::Indexes, "structure.indexes"),
    (Section::ForeignKeys, "structure.foreignKeys"),
    (Section::Checks, "structure.checks"),
];

fn slot(section: Section) -> usize {
    match section {
        Section::Columns | Section::Table => 0,
        Section::Indexes => 1,
        Section::ForeignKeys => 2,
        Section::Checks => 3,
    }
}

// Undo and redo, as whole drafts.
#[derive(Default)]
struct History {
    past: Vec<Draft>,
    future: Vec<Draft>,
}

// A table's columns, keys, indexes and checks in a tab of its own. An empty `table` is a new table, not yet created.
pub struct StructureTab {
    pub id: String,
    pub connection: ConnectionConfig,
    pub schema: String,
    pub table: String,
    focus: FocusHandle,
    load: Load,
    original: Option<TableStructure>,
    pub(crate) draft: Draft,
    plan: Result<ChangePlan, Vec<Problem>>,
    history: History,
    // One per section, in SECTIONS' order.
    sheets: [Sheet; 4],
    preview: Entity<EditorState>,
    script: String,
    preview_open: bool,
    section: Section,
    problems_scroll: ScrollHandle,
    // The table was dropped, or changed elsewhere, while this tab held changes.
    gone: bool,
    stale: bool,
    // Only a new table has one.
    name_field: Option<Entity<InputState>>,
    _load: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<StructureTabEvent> for StructureTab {}

impl StructureTab {
    pub fn new(tab: EditorTab, connection: ConnectionConfig, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let target = tab.structure.unwrap_or_default();
        let draft = Draft::new_table(&connection.driver, &target.schema);
        let load = if target.table.is_empty() { Load::Ready } else { Load::Loading };
        let preview = cx.new(|cx| EditorState::new(window, cx).language("sql").line_number(false));
        let name_field = target
            .table
            .is_empty()
            .then(|| cx.new(|cx| InputState::new(window, cx).placeholder(t(cx, "structure.tableNamePlaceholder"))));
        let mut subscriptions: Vec<Subscription> = name_field
            .iter()
            .map(|field| {
                cx.subscribe_in(field, window, |tab: &mut Self, state, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::Change) {
                        tab.draft.name = state.read(cx).value().trim().to_string();
                        tab.changed(window, cx);
                    }
                })
            })
            .collect();
        let sheets = [(); 4].map(|_| Sheet::new(cx));
        for (sheet, (section, _)) in sheets.iter().zip(SECTIONS) {
            subscriptions.push(cx.subscribe_in(&sheet.grid, window, move |tab: &mut Self, grid, event, window, cx| {
                tab.grid_event(section, grid, event, window, cx)
            }));
            subscriptions.push(cx.observe(&sheet.grid, |_, _, cx| cx.notify()));
        }
        // The pickers list what the schema holds, as it loads, in the language it's shown in.
        subscriptions.push(cx.observe_global::<schema::Schemas>(|tab, cx| tab.sync(cx)));
        subscriptions.push(cx.observe_global::<I18n>(|tab, cx| tab.sync(cx)));
        let mut this = Self {
            id: tab.id,
            connection,
            schema: target.schema,
            table: target.table,
            focus: cx.focus_handle(),
            load,
            original: None,
            draft,
            plan: Ok(ChangePlan::default()),
            history: History::default(),
            sheets,
            preview,
            script: String::new(),
            preview_open: true,
            section: Section::Columns,
            problems_scroll: ScrollHandle::new(),
            gone: false,
            stale: false,
            name_field,
            _load: Task::ready(()),
            _subscriptions: subscriptions,
        };
        if this.table.is_empty() {
            this.replan(window, cx);
        }
        this
    }

    pub fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.table.is_empty() {
            return;
        }
        self.load = Load::Loading;
        self.stale = false;
        let bar = state::bar(cx);
        let (id, schema, table) = (self.connection.id.clone(), self.schema.clone(), self.table.clone());
        let loaded = state::spawn(cx, async move { bar.table_structure(&id, &schema, &table).await });
        self._load = cx.spawn_in(window, async move |this, cx| {
            let result = loaded.await;
            let _ = this.update_in(cx, |this, window, cx| {
                match result {
                    Some(Ok(structure)) => {
                        this.draft = Draft::from_structure(&structure);
                        this.original = Some(structure);
                        this.load = Load::Ready;
                        this.history = History::default();
                        this.load_targets(cx);
                        this.replan(window, cx);
                    }
                    Some(Err(error)) => this.load = Load::Failed(error.message),
                    None => {}
                }
                cx.notify();
            });
        });
    }

    pub fn title(&self, cx: &App) -> SharedString {
        if self.table.is_empty() { t(cx, "structure.newTableTitle") } else { self.table.clone().into() }
    }

    fn naming(&self) -> Option<&Entity<InputState>> {
        self.name_field.as_ref().filter(|_| self.table.is_empty())
    }

    pub fn persisted(&self) -> bool {
        !self.table.is_empty()
    }

    pub fn stored(&self, cx: &App) -> EditorTab {
        let structure = StructureRef { schema: self.schema.clone(), table: self.table.clone() };
        EditorTab {
            id: self.id.clone(),
            connection_id: self.connection.id.clone(),
            title: self.title(cx).to_string(),
            color: self.connection.color.clone(),
            structure: self.persisted().then_some(structure),
            ..Default::default()
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        match self.naming() {
            Some(field) => field.update(cx, |state, cx| state.focus(window, cx)),
            None => {
                let grid = self.sheet(self.section).grid.read(cx).focus_handle(cx);
                window.focus(&grid, cx);
            }
        }
    }

    pub fn set_connection(&mut self, connection: ConnectionConfig, cx: &mut Context<Self>) {
        self.connection = connection;
        self.sync(cx);
        cx.notify();
    }

    pub fn has_changes(&self) -> bool {
        match &self.original {
            Some(original) => !changes(original, &self.draft).is_empty(),
            None => self.table.is_empty() && self.draft != Draft::new_table(&self.connection.driver, &self.schema),
        }
    }

    #[cfg(test)]
    pub fn script(&self) -> &str {
        &self.script
    }

    #[cfg(test)]
    pub fn grid(&self) -> Entity<Grid> {
        self.sheet(self.section).grid.clone()
    }

    // Read-only connections get a viewer, and nothing is editable until the structure has loaded.
    pub(crate) fn editable(&self) -> bool {
        !self.connection.read_only
            && self.connection.driver.capabilities().structure.editable
            && matches!(self.load, Load::Ready)
    }

    fn section_editable(&self, section: Section) -> bool {
        let editing = &self.connection.driver.capabilities().structure;
        self.editable()
            && match section {
                Section::Columns | Section::Table => true,
                Section::Indexes => editing.indexes,
                Section::ForeignKeys => editing.foreign_keys,
                Section::Checks => editing.checks,
            }
    }

    pub fn can_apply(&self) -> bool {
        !self.gone && self.editable() && self.plan.as_ref().is_ok_and(|plan| !plan.is_empty())
    }

    pub fn apply(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(plan) = self.plan.as_ref().ok().filter(|_| self.can_apply()).cloned() else { return };
        let this = cx.entity().downgrade();
        schema_actions::open_structure_apply(self.connection.clone(), plan, window, cx, move |cx| {
            let _ = this.update(cx, |tab, cx| {
                let created = tab.table.is_empty().then(|| tab.draft.name.clone());
                let name = created.clone().unwrap_or_else(|| tab.table.clone());
                toast::success(t_with(cx, "structure.applied", &[("name", &name)]), cx);
                cx.emit(StructureTabEvent::Applied { created });
            });
        });
    }

    pub fn after_apply(&mut self, created: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(name) = created {
            self.table = name;
        }
        // Until the reload lands, the old structure would make the applied draft look pending.
        self.original = None;
        self.load(window, cx);
        cx.notify();
    }

    // A dropped table's tab is closed by the workspace when it holds no changes.
    pub fn table_changed(&mut self, change: &TableChange, window: &mut Window, cx: &mut Context<Self>) {
        match change {
            TableChange::Renamed(name) => {
                self.table = name.clone();
                self.draft.name = name.clone();
                if let Some(original) = &mut self.original {
                    original.name = name.clone();
                }
                self.replan(window, cx);
                cx.emit(StructureTabEvent::Edited);
            }
            TableChange::Dropped => self.gone = true,
            TableChange::Altered if matches!(self.load, Load::Loading) => {}
            TableChange::Altered if self.has_changes() => self.stale = true,
            TableChange::Altered => self.load(window, cx),
        }
        cx.notify();
    }

    fn sheet(&self, section: Section) -> &Sheet {
        &self.sheets[slot(section)]
    }

    // The grids show the draft over the loaded structure.
    fn sync(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.load, Load::Ready) {
            return;
        }
        let shapes = [
            columns::shape(self, cx),
            indexes::shape(self, cx),
            foreign_keys::shape(self, cx),
            checks::shape(self, cx),
        ];
        let editable = SECTIONS.map(|(section, _)| self.section_editable(section));
        for ((sheet, shape), editable) in self.sheets.iter_mut().zip(shapes).zip(editable) {
            sheet.show(shape, editable, cx);
        }
    }

    fn replan(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let driver = &self.connection.driver;
        self.plan = match &self.original {
            Some(original) => plan_changes(driver, original, &self.draft),
            None => plan_create(driver, &self.draft),
        };
        let script = self.plan.as_ref().map(|plan| plan.script(driver)).unwrap_or_default();
        // Setting the editor's text re-shapes it, so only a changed script goes in.
        if script != self.script {
            self.preview.update(cx, |state, cx| state.set_value(script.clone(), window, cx));
            self.script = script;
        }
        self.sync(cx);
        cx.notify();
    }

    fn changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.replan(window, cx);
        cx.emit(StructureTabEvent::Edited);
    }

    fn record(&mut self, before: Draft, window: &mut Window, cx: &mut Context<Self>) {
        if self.draft != before {
            self.history.past.push(before);
            self.history.future.clear();
            self.changed(window, cx);
        }
    }

    // A new table's name stays as its field has it.
    fn travel(&mut self, back: bool, window: &mut Window, cx: &mut Context<Self>) {
        let History { past, future } = &mut self.history;
        let (from, to) = if back { (past, future) } else { (future, past) };
        let Some(mut draft) = from.pop() else { return };
        draft.name = self.draft.name.clone();
        to.push(std::mem::replace(&mut self.draft, draft));
        self.changed(window, cx);
    }

    fn grid_event(
        &mut self,
        section: Section,
        grid: &Entity<Grid>,
        event: &GridEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let editable = self.section_editable(section);
        match event {
            GridEvent::EditCell { row, column, value } if editable => {
                self.edit_cells(section, &[(*row, *column, value.clone())], window, cx)
            }
            GridEvent::PasteCells(cells) if editable => self.edit_cells(section, cells, window, cx),
            GridEvent::ToggleDelete(row) if editable => self.drop_rows(section, &[*row], window, cx),
            GridEvent::DeleteRows(rows) if editable => self.drop_rows(section, rows, window, cx),
            GridEvent::Undo if editable => self.travel(true, window, cx),
            GridEvent::Redo if editable => self.travel(false, window, cx),
            GridEvent::Apply => self.apply(window, cx),
            GridEvent::Refresh if !self.has_changes() => self.load(window, cx),
            GridEvent::ViewCell { row, column } => {
                let (name, value) = shown(grid, *row, *column, cx);
                cell_viewer::open(name, String::new(), value, window, cx);
            }
            GridEvent::EditInViewer { row, column } if editable => {
                let (name, value) = shown(grid, *row, *column, cx);
                let (grid, row, column) = (grid.downgrade(), *row, *column);
                let on_save: cell_viewer::OnSave = Rc::new(move |value, _, cx| {
                    grid.update(cx, |_, cx| cx.emit(GridEvent::EditCell { row, column, value })).ok();
                });
                cell_viewer::open_editor(name, String::new(), value, on_save, window, cx);
            }
            GridEvent::Export => crate::export_dialog::open(grid.read(cx).export_source(), window, cx),
            _ => {}
        }
    }

    fn edit_cells(
        &mut self,
        section: Section,
        cells: &[(usize, usize, Option<String>)],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let before = self.draft.clone();
        for (row, column, value) in cells.iter().cloned() {
            match section {
                Section::Columns | Section::Table => columns::edit(self, row, column, value, cx),
                Section::Indexes => indexes::edit(self, row, column, value),
                Section::ForeignKeys => foreign_keys::edit(self, row, column, value, window, cx),
                Section::Checks => checks::edit(self, row, column, value),
            }
        }
        self.record(before, window, cx);
    }

    // A generated column isn't dropped here.
    fn drop_rows(&mut self, section: Section, rows: &[usize], window: &mut Window, cx: &mut Context<Self>) {
        let before = self.draft.clone();
        let draft = &mut self.draft;
        match section {
            Section::Columns | Section::Table => {
                let columns = &draft.columns;
                let rows: Vec<usize> = rows
                    .iter()
                    .copied()
                    .filter(|&row| columns.get(row).is_some_and(|line| line.column.generated.is_empty()))
                    .collect();
                toggle_dropped(&mut draft.columns, &rows);
            }
            Section::Indexes => toggle_dropped(&mut draft.indexes, rows),
            Section::ForeignKeys => toggle_dropped(&mut draft.foreign_keys, rows),
            Section::Checks => toggle_dropped(&mut draft.checks, rows),
        }
        self.record(before, window, cx);
    }

    // The shown section's focused row, if any, and whether it's dropped.
    fn focused_row(&self, cx: &App) -> Option<(usize, bool)> {
        let grid = self.sheet(self.section).grid.read(cx);
        let (row, _) = grid.focused_cell().filter(|&(row, _)| row < grid.set().rows())?;
        Some((row, grid.is_deleted(row)))
    }

    fn drop_focused(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((row, _)) = self.focused_row(cx) {
            self.drop_rows(self.section, &[row], window, cx);
        }
    }

    // The new row opens for typing at its name, or at what names it: a key's or index's columns, a check's condition.
    fn add_row(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let section = self.section;
        if !self.section_editable(section) {
            return;
        }
        let before = self.draft.clone();
        let draft = &mut self.draft;
        let (row, column) = match section {
            Section::Columns | Section::Table => {
                let column = StructColumn { nullable: true, ..Default::default() };
                draft.columns.push(ColumnRow { origin: None, dropped: false, column, primary: false });
                (draft.columns.len() - 1, 0)
            }
            Section::Indexes => {
                draft.indexes.push(Row::added(StructIndex::default()));
                (draft.indexes.len() - 1, 1)
            }
            Section::ForeignKeys => {
                draft.foreign_keys.push(Row::added(ForeignKey::default()));
                (draft.foreign_keys.len() - 1, 1)
            }
            Section::Checks => {
                draft.checks.push(Row::added(Check::default()));
                (draft.checks.len() - 1, 1)
            }
        };
        self.record(before, window, cx);
        let grid = self.sheet(section).grid.clone();
        grid.update(cx, |grid, cx| grid.edit_cell(row, column, window, cx));
    }

    pub fn revert(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let before = self.draft.clone();
        self.draft = match &self.original {
            Some(original) => Draft::from_structure(original),
            None => Draft::new_table(&self.connection.driver, &self.schema),
        };
        if let Some(field) = self.naming() {
            field.update(cx, |state, cx| state.set_value("", window, cx));
        }
        self.record(before, window, cx);
    }

    fn problem_lines(&self, cx: &App) -> Vec<(Section, usize, SharedString)> {
        let problems = self.plan.as_ref().err().map(Vec::as_slice).unwrap_or_default();
        let line = |problem: &Problem| {
            let text = problem_text(&problem.kind, cx);
            let Some((_, key)) = SECTIONS.iter().find(|(section, _)| *section == problem.section) else { return text };
            let row = (problem.row + 1).to_string();
            let at = t_with(cx, "structure.problemRow", &[("row", &row), ("problem", &text)]);
            format!("{} \u{b7} {at}", t(cx, key)).into()
        };
        problems.iter().map(|problem| (problem.section, problem.row, line(problem))).collect()
    }

    // A problem's row, focused in its section, or the new table's name.
    fn show_problem(&mut self, section: Section, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        if section == Section::Table {
            if let Some(field) = self.naming() {
                field.update(cx, |state, cx| state.focus(window, cx));
            }
            return;
        }
        self.section = section;
        let grid = self.sheet(section).grid.clone();
        grid.update(cx, |grid, cx| grid.focus_cell(row, 0, window, cx));
        cx.notify();
    }

    // The sections with rows added, changed or dropped, and how many such rows in all.
    fn pending(&self) -> (Vec<Section>, usize) {
        let Some(original) = &self.original else {
            return (Vec::new(), 0);
        };
        let found = changes(original, &self.draft);
        let columns = self
            .draft
            .columns
            .iter()
            .filter(|row| match row.origin.and_then(|origin| original.columns.get(origin)) {
                Some(old) => row.dropped || *old != row.column,
                None => true,
            })
            .count()
            + usize::from(found.primary_key.is_some());
        // A changed row is dropped and added again, so the dropped ones and the new rows count each row once.
        let new = |rows: &mut dyn Iterator<Item = (bool, bool)>| {
            rows.filter(|&(loaded, dropped)| !loaded && !dropped).count()
        };
        let draft = &self.draft;
        let counts = [
            (Section::Columns, columns),
            (
                Section::Indexes,
                found.dropped_indexes.len() + new(&mut draft.indexes.iter().map(|r| (r.origin.is_some(), r.dropped))),
            ),
            (
                Section::ForeignKeys,
                found.dropped_foreign_keys.len()
                    + new(&mut draft.foreign_keys.iter().map(|r| (r.origin.is_some(), r.dropped))),
            ),
            (
                Section::Checks,
                found.dropped_checks.len() + new(&mut draft.checks.iter().map(|r| (r.origin.is_some(), r.dropped))),
            ),
        ];
        let marked = counts.iter().filter(|(_, n)| *n > 0).map(|(section, _)| *section).collect();
        (marked, counts.iter().map(|(_, n)| n).sum())
    }

    fn count(&self, section: Section) -> usize {
        let draft = &self.draft;
        match section {
            Section::Columns | Section::Table => draft.columns.iter().filter(|row| !row.dropped).count(),
            Section::Indexes => draft.indexes.iter().filter(|row| !row.dropped).count(),
            Section::ForeignKeys => draft.foreign_keys.iter().filter(|row| !row.dropped).count(),
            Section::Checks => draft.checks.iter().filter(|row| !row.dropped).count(),
        }
    }

    // Engines without checks or foreign keys don't get those sections.
    fn shows(&self, section: Section) -> bool {
        let editing = &self.connection.driver.capabilities().structure;
        match section {
            Section::ForeignKeys => editing.foreign_keys,
            Section::Checks => editing.checks,
            Section::Columns | Section::Table | Section::Indexes => true,
        }
    }

    fn header(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let (marked, _) = self.pending();
        let switch = SECTIONS.iter().enumerate().fold(form::toggle_group(), |group, (ix, (section, key))| {
            let section = *section;
            if !self.shows(section) {
                return group;
            }
            let label = format!("{} {}", t(cx, key), self.count(section));
            let dot = marked.contains(&section).then(|| {
                div()
                    .size(rems(0.462))
                    .ml(rems(0.308))
                    .rounded_full()
                    .bg(theme.warning)
                    .debug_selector(move || format!("structure:section-dot:{ix}"))
            });
            group.child(
                form::toggle(("structure-section", ix), label, self.section == section)
                    .debug_selector(move || format!("structure-section:{ix}"))
                    .children(dot)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.section = section;
                        let grid = this.sheet(section).grid.read(cx).focus_handle(cx);
                        window.focus(&grid, cx);
                        cx.notify();
                    })),
            )
        });
        h_flex()
            .debug_selector(|| "structure-header".into())
            .flex_none()
            .justify_between()
            .gap(rems(0.923))
            .px(rems(1.231))
            .h(rems(3.077))
            .border_b_1()
            .border_color(theme.border)
            .child(match self.naming() {
                Some(field) => h_flex()
                    .gap(rems(0.462))
                    .text_size(TEXT_SM)
                    .child(div().text_color(theme.muted_foreground).child(t(cx, "structure.tableName")))
                    .child(format!("{}.", self.schema))
                    .child(
                        div()
                            .w(rems(16.))
                            .debug_selector(|| "structure:table-name".into())
                            .child(form::input(field, window, cx).disabled(!self.editable())),
                    )
                    .into_any_element(),
                None => div()
                    .text_size(TEXT_SM)
                    .font_semibold()
                    .debug_selector(|| "structure-title".into())
                    .child(format!("{}.{}", self.schema, self.table))
                    .into_any_element(),
            })
            .child(div().w(rems(30.769)).child(switch))
    }

    fn banner(&self, cx: &mut Context<Self>) -> Option<Div> {
        let theme = cx.theme().clone();
        if self.gone {
            let text = t(cx, "structure.gone");
            return Some(
                form::error(text, cx).mx(rems(1.231)).mt(rems(0.615)).debug_selector(|| "structure-gone".into()),
            );
        }
        self.stale.then(|| {
            form::alert(theme.warning)
                .mx(rems(1.231))
                .mt(rems(0.615))
                .debug_selector(|| "structure-stale".into())
                .child(
                    h_flex()
                        .justify_between()
                        .gap(rems(0.923))
                        .child(div().flex_1().min_w_0().child(t(cx, "structure.stale")))
                        .child(
                            Button::new("structure-reload")
                                .small()
                                .debug_selector(|| "structure-reload".into())
                                .label(t(cx, "structure.reload"))
                                .on_click(cx.listener(|this, _, window, cx| this.load(window, cx))),
                        ),
                )
        })
    }

    fn body(&self, cx: &mut Context<Self>) -> AnyElement {
        match &self.load {
            Load::Loading => form::empty_state(None, t(cx, "tableView.loading"), cx).m_auto().into_any_element(),
            Load::Failed(error) => v_flex()
                .m_auto()
                .gap(rems(0.615))
                .items_center()
                .child(form::error(format!("{}\n{error}", t(cx, "structure.loadFailed")), cx))
                .child(
                    Button::new("structure-retry")
                        .small()
                        .debug_selector(|| "structure-retry".into())
                        .label(t(cx, "structure.retry"))
                        .on_click(cx.listener(|this, _, window, cx| this.load(window, cx))),
                )
                .into_any_element(),
            Load::Ready => {
                let section = self.section;
                let empty = match section {
                    Section::Indexes if self.draft.indexes.is_empty() => Some("structure.noIndexes"),
                    Section::ForeignKeys if self.draft.foreign_keys.is_empty() => Some("structure.noForeignKeys"),
                    Section::Checks if self.draft.checks.is_empty() => Some("structure.noChecks"),
                    _ => None,
                };
                div()
                    .relative()
                    .size_full()
                    .debug_selector(|| "structure-body".into())
                    .child(self.sheet(section).grid.clone())
                    .children(empty.map(|key| {
                        div().absolute().inset_0().flex().items_center().justify_center().child(form::empty_state(
                            None,
                            t(cx, key),
                            cx,
                        ))
                    }))
                    .into_any_element()
            }
        }
    }

    fn toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let (_, pending) = self.pending();
        let editable = self.editable();
        let rows = self.section_editable(self.section);
        let focused = self.focused_row(cx);
        let (icon, tip) = match focused {
            Some((_, true)) => (Lucide::Undo2, "structure.restore"),
            _ => (Lucide::Trash, "structure.drop"),
        };
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
                    .when(editable, |bar| {
                        bar.child(
                            Button::new("structure-revert")
                                .debug_selector(|| "structure-revert".into())
                                .tool_label(t(cx, "structure.revert"))
                                .disabled(!self.has_changes())
                                .on_click(cx.listener(|this, _, window, cx| this.revert(window, cx))),
                        )
                        .child(
                            Button::new("structure-apply")
                                .debug_selector(|| "structure-apply".into())
                                .primary()
                                .tool_label(t(cx, "structure.apply"))
                                .disabled(!self.can_apply())
                                .on_click(cx.listener(|this, _, window, cx| this.apply(window, cx))),
                        )
                    })
                    .when(pending > 0, |bar| {
                        let label = t_count(cx, "structure.pendingChanges", pending as i64, &[]);
                        bar.child(form::badge(label, theme.warning))
                    }),
            )
            .when(editable, |bar| {
                bar.child(
                    h_flex()
                        .gap(rems(0.462))
                        .child(
                            Button::new("structure-drop")
                                .debug_selector(|| "structure-drop".into())
                                .tool_icon(Icon::new(icon), ICON_SM)
                                .tooltip(t(cx, tip))
                                .disabled(!rows || focused.is_none())
                                .on_click(cx.listener(|this, _, window, cx| this.drop_focused(window, cx))),
                        )
                        .child(
                            Button::new("structure-add")
                                .debug_selector(|| "structure-add".into())
                                .tool_icon(Icon::new(IconName::Plus), ICON_SM)
                                .tooltip(t(cx, "structure.add"))
                                .disabled(!rows)
                                .on_click(cx.listener(|this, _, window, cx| this.add_row(window, cx))),
                        ),
                )
            })
    }
}

fn shown(grid: &Entity<Grid>, row: usize, column: usize, cx: &App) -> (String, Option<String>) {
    let grid = grid.read(cx);
    (grid.set().columns[column].name.clone(), grid.shown(row, column).display().map(str::to_string))
}

trait Listed {
    // Its place in the loaded structure, and whether it's dropped.
    fn state(&mut self) -> (Option<usize>, &mut bool);
}

impl Listed for ColumnRow {
    fn state(&mut self) -> (Option<usize>, &mut bool) {
        (self.origin, &mut self.dropped)
    }
}

impl<T> Listed for Row<T> {
    fn state(&mut self) -> (Option<usize>, &mut bool) {
        (self.origin, &mut self.dropped)
    }
}

// Marks the rows dropped, or brings them back when all of them are. An added row just goes.
fn toggle_dropped<T: Listed>(list: &mut Vec<T>, rows: &[usize]) {
    let mut rows: Vec<usize> = rows.iter().copied().filter(|&row| row < list.len()).collect();
    rows.sort_unstable();
    rows.dedup();
    let restore = !rows.is_empty() && rows.iter().all(|&row| *list[row].state().1);
    for &row in rows.iter().rev() {
        let (origin, dropped) = list[row].state();
        if origin.is_some() {
            *dropped = !restore;
        } else {
            list.remove(row);
        }
    }
}

fn section_id(section: Section) -> &'static str {
    match section {
        Section::Table => "table",
        Section::Columns => "columns",
        Section::Indexes => "indexes",
        Section::ForeignKeys => "foreign-keys",
        Section::Checks => "checks",
    }
}

fn problem_text(kind: &ProblemKind, cx: &App) -> SharedString {
    let key = match kind {
        ProblemKind::EmptyTableName => "emptyTableName",
        ProblemKind::EmptyName => "emptyName",
        ProblemKind::DuplicateName => "duplicateName",
        ProblemKind::EmptyType => "emptyType",
        ProblemKind::NoColumns => "noColumns",
        ProblemKind::NoIndexColumns => "noIndexColumns",
        ProblemKind::UsesDroppedColumn(column) => {
            return t_with(cx, "structure.problem.usesDroppedColumn", &[("column", column)]);
        }
        ProblemKind::ForeignKeyColumnCount => "foreignKeyColumnCount",
        ProblemKind::NoReferencedTable => "noReferencedTable",
        ProblemKind::EmptyExpression => "emptyExpression",
        ProblemKind::Unsupported => "unsupported",
    };
    t(cx, &format!("structure.problem.{key}"))
}

impl Render for StructureTab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        v_flex()
            .size_full()
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &ApplyStructure, window, cx| this.apply(window, cx)))
            .bg(theme.background)
            .debug_selector(|| "structure-tab".into())
            .child(self.header(window, cx))
            .children(self.banner(cx))
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .on_drag_move(cx.listener(|_, event: &DragMoveEvent<PreviewDrag>, window, cx| {
                        preview::resize(event, window, cx);
                        cx.notify();
                    }))
                    .child(div().flex_1().min_h_0().child(self.body(cx)))
                    .when(self.editable(), |tab| tab.child(preview::panel(self, cx))),
            )
            .child(self.toolbar(cx))
    }
}
