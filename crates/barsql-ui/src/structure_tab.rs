mod checks;
mod columns;
mod foreign_keys;
mod indexes;
mod preview;
#[cfg(test)]
mod tests;

use barsql_app::{EditorTab, StructureRef};
use barsql_core::{Check, ConnectionConfig, ForeignKey, Identity, StructColumn, StructIndex, TableStructure};
use barsql_sql::structure::{
    ChangePlan, ColumnRow, Draft, Problem, ProblemKind, Section, changes, plan_changes, plan_create,
};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{EditorState, InputEvent, InputState};
use gpui_kit::component::scroll::ScrollbarAxis;
use gpui_kit::component::{ActiveTheme, Disableable, Icon, IconName, Sizable, StyledExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use self::checks::CheckEditors;
use self::columns::{Editors, Field};
use self::foreign_keys::KeyEditors;
use self::indexes::IndexEditors;
use crate::form::{self, ToolButton as _};
use crate::i18n::{t, t_count, t_with};
use crate::schema_actions;
use crate::schema_tree::TableChange;
use crate::scrollbars::HoverScrollbar as _;
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
    // One per draft column, in the same order.
    editors: Vec<Editors>,
    index_editors: Vec<IndexEditors>,
    check_editors: Vec<CheckEditors>,
    key_editors: Vec<KeyEditors>,
    next_key: u64,
    preview: Entity<EditorState>,
    script: String,
    preview_open: bool,
    section: Section,
    scroll: ScrollHandle,
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
        let subscriptions = name_field
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
            editors: Vec::new(),
            index_editors: Vec::new(),
            check_editors: Vec::new(),
            key_editors: Vec::new(),
            next_key: 0,
            preview,
            script: String::new(),
            preview_open: true,
            section: Section::Columns,
            scroll: ScrollHandle::new(),
            gone: false,
            stale: false,
            name_field,
            _load: Task::ready(()),
            _subscriptions: subscriptions,
        };
        if this.table.is_empty() {
            this.reset_editors(window, cx);
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
                        this.reset_editors(window, cx);
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
            None => window.focus(&self.focus, cx),
        }
    }

    pub fn set_connection(&mut self, connection: ConnectionConfig, cx: &mut Context<Self>) {
        self.connection = connection;
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

    // Read-only connections get a viewer, and nothing is editable until the structure has loaded.
    pub(crate) fn editable(&self) -> bool {
        !self.connection.read_only
            && self.connection.driver.capabilities().structure.editable
            && matches!(self.load, Load::Ready)
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

    fn key(&mut self) -> u64 {
        self.next_key += 1;
        self.next_key
    }

    fn reset_editors(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let columns: Vec<StructColumn> = self.draft.columns.iter().map(|row| row.column.clone()).collect();
        self.editors = columns
            .iter()
            .map(|column| {
                let key = self.key();
                columns::editors(key, column, window, cx)
            })
            .collect();
        let indexes: Vec<StructIndex> = self.draft.indexes.iter().map(|row| row.value.clone()).collect();
        self.index_editors = indexes
            .iter()
            .map(|index| {
                let key = self.key();
                indexes::editors(key, index, window, cx)
            })
            .collect();
        let checks: Vec<Check> = self.draft.checks.iter().map(|row| row.value.clone()).collect();
        self.check_editors = checks
            .iter()
            .map(|check| {
                let key = self.key();
                checks::editors(key, check, window, cx)
            })
            .collect();
        let keys: Vec<ForeignKey> = self.draft.foreign_keys.iter().map(|row| row.value.clone()).collect();
        self.key_editors = keys
            .iter()
            .map(|fk| {
                let key = self.key();
                foreign_keys::editors(key, fk, window, cx)
            })
            .collect();
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
        cx.notify();
    }

    fn changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.replan(window, cx);
        cx.emit(StructureTabEvent::Edited);
    }

    fn row_of(&self, key: u64) -> Option<usize> {
        self.editors.iter().position(|editors| editors.key == key)
    }

    fn edit(&mut self, key: u64, field: Field, value: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.row_of(key).filter(|_| self.editable()) else { return };
        let column = &mut self.draft.columns[ix].column;
        match field {
            Field::Name => {
                let name = value.trim().to_string();
                if column.name != name {
                    self.draft.rename_column(ix, name);
                }
            }
            Field::Type => column.data_type = value,
            Field::Default => column.default = value,
            Field::Comment => column.comment = value,
        }
        self.changed(window, cx);
    }

    fn pick_type(&mut self, key: u64, data_type: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.row_of(key).filter(|_| self.editable()) else { return };
        self.editors[ix].data_type.update(cx, |state, cx| state.set_value(data_type.clone(), window, cx));
        self.draft.columns[ix].column.data_type = data_type;
        self.changed(window, cx);
    }

    fn toggle_nullable(&mut self, key: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.row_of(key).filter(|_| self.editable()) else { return };
        let column = &mut self.draft.columns[ix].column;
        column.nullable = !column.nullable;
        self.changed(window, cx);
    }

    // A key column can't hold NULL on most engines, so ticking the key unticks nullable.
    fn toggle_primary(&mut self, key: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.row_of(key).filter(|_| self.editable()) else { return };
        let row = &mut self.draft.columns[ix];
        row.primary = !row.primary;
        if row.primary {
            row.column.nullable = false;
        }
        self.changed(window, cx);
    }

    fn set_identity(&mut self, key: u64, identity: Identity, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.row_of(key).filter(|_| self.editable()) else { return };
        self.draft.columns[ix].column.identity = identity;
        self.changed(window, cx);
    }

    // An added row goes. A loaded one is marked, and comes back with a second click.
    fn drop_row(&mut self, key: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.row_of(key).filter(|_| self.editable()) else { return };
        if self.draft.columns[ix].origin.is_none() {
            self.draft.columns.remove(ix);
            self.editors.remove(ix);
        } else {
            let row = &mut self.draft.columns[ix];
            row.dropped = !row.dropped;
        }
        self.changed(window, cx);
    }

    pub fn add_column(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.editable() {
            return;
        }
        let column = StructColumn { nullable: true, ..Default::default() };
        let key = self.key();
        let editors = columns::editors(key, &column, window, cx);
        editors.name.update(cx, |state, cx| state.focus(window, cx));
        self.draft.columns.push(ColumnRow { origin: None, dropped: false, column, primary: false });
        self.editors.push(editors);
        self.changed(window, cx);
    }

    fn can_add(&self) -> bool {
        match self.section {
            Section::Columns | Section::Table => true,
            Section::Indexes => self.connection.driver.capabilities().structure.indexes,
            Section::Checks => self.connection.driver.capabilities().structure.checks,
            Section::ForeignKeys => self.connection.driver.capabilities().structure.foreign_keys,
        }
    }

    fn add_row(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.section {
            Section::Columns | Section::Table => self.add_column(window, cx),
            Section::Indexes => self.add_index(window, cx),
            Section::Checks => self.add_check(window, cx),
            Section::ForeignKeys => self.add_foreign_key(window, cx),
        }
    }

    pub fn revert(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.draft = match &self.original {
            Some(original) => Draft::from_structure(original),
            None => Draft::new_table(&self.connection.driver, &self.schema),
        };
        if let Some(field) = self.naming() {
            field.update(cx, |state, cx| state.set_value("", window, cx));
        }
        self.reset_editors(window, cx);
        self.changed(window, cx);
    }

    fn problems(&self, section: Section, row: usize) -> impl Iterator<Item = &ProblemKind> {
        let problems = self.plan.as_ref().err().map(Vec::as_slice).unwrap_or_default();
        problems.iter().filter(move |p| p.section == section && p.row == row).map(|p| &p.kind)
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
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.section = section;
                        cx.notify();
                    })),
            )
        });
        h_flex()
            .flex_none()
            .justify_between()
            .gap(rems(0.923))
            .px(rems(1.231))
            .py(rems(0.615))
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

    fn body(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
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
                let rows = match self.section {
                    Section::Columns | Section::Table => columns::rows(self, window, cx).into_any_element(),
                    Section::Indexes => indexes::rows(self, window, cx),
                    Section::ForeignKeys => foreign_keys::rows(self, window, cx),
                    Section::Checks => checks::rows(self, window, cx),
                };
                div()
                    .relative()
                    .size_full()
                    .child(
                        div()
                            .id("structure-body")
                            .size_full()
                            .overflow_y_scroll()
                            .track_scroll(&self.scroll)
                            .px(rems(1.231))
                            .py(rems(0.923))
                            .text_size(TEXT_SM)
                            .text_color(theme.foreground)
                            .child(rows),
                    )
                    .hover_scrollbar(&self.scroll, ScrollbarAxis::Vertical)
                    .into_any_element()
            }
        }
    }

    fn toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let (_, pending) = self.pending();
        let editable = self.editable();
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
                    Button::new("structure-add")
                        .debug_selector(|| "structure-add".into())
                        .tool_icon(Icon::new(IconName::Plus), ICON_SM)
                        .tooltip(t(cx, "structure.add"))
                        .disabled(!self.can_add())
                        .on_click(cx.listener(|this, _, window, cx| this.add_row(window, cx))),
                )
            })
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

fn line(selector: impl Fn() -> String + 'static, cx: &App) -> Div {
    h_flex().gap(rems(0.923)).py(rems(0.462)).border_b_1().border_color(cx.theme().border).debug_selector(selector)
}

fn cell(text: &str, width: f32) -> Div {
    div().flex_none().w(rems(width)).min_w_0().overflow_hidden().child(text.to_string())
}

fn input(value: &str, window: &mut Window, cx: &mut Context<StructureTab>) -> Entity<InputState> {
    let value = value.to_string();
    cx.new(|cx| InputState::new(window, cx).default_value(value))
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
            .children(
                self.problems(Section::Table, 0)
                    .map(|kind| form::error(problem_text(kind, cx), cx).mx(rems(1.231)).mt(rems(0.615)))
                    .collect::<Vec<_>>(),
            )
            .children(self.banner(cx))
            .child(div().flex_1().min_h_0().child(self.body(window, cx)))
            .when(self.editable(), |tab| tab.child(preview::panel(self, cx)))
            .child(self.toolbar(cx))
    }
}
