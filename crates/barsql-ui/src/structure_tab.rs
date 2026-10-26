mod columns;
#[cfg(test)]
mod tests;

use barsql_app::{EditorTab, StructureRef};
use barsql_core::{ConnectionConfig, TableStructure};
use barsql_sql::structure::{Draft, Section, changes};
use gpui_kit::component::button::Button;
use gpui_kit::component::scroll::ScrollbarAxis;
use gpui_kit::component::{ActiveTheme, Sizable, StyledExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::form;
use crate::i18n::t;
use crate::scrollbars::HoverScrollbar as _;
use crate::state;
use crate::tokens::{TEXT_SM, TEXT_XS};

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
    section: Section,
    scroll: ScrollHandle,
    _load: Task<()>,
}

impl EventEmitter<StructureTabEvent> for StructureTab {}

impl StructureTab {
    pub fn new(tab: EditorTab, connection: ConnectionConfig, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let target = tab.structure.unwrap_or_default();
        let draft = Draft::new_table(&connection.driver, &target.schema);
        let load = if target.table.is_empty() { Load::Ready } else { Load::Loading };
        Self {
            id: tab.id,
            connection,
            schema: target.schema,
            table: target.table,
            focus: cx.focus_handle(),
            load,
            original: None,
            draft,
            section: Section::Columns,
            scroll: ScrollHandle::new(),
            _load: Task::ready(()),
        }
    }

    pub fn load(&mut self, cx: &mut Context<Self>) {
        if self.table.is_empty() {
            return;
        }
        self.load = Load::Loading;
        let bar = state::bar(cx);
        let (id, schema, table) = (self.connection.id.clone(), self.schema.clone(), self.table.clone());
        let loaded = state::spawn(cx, async move { bar.table_structure(&id, &schema, &table).await });
        self._load = cx.spawn(async move |this, cx| {
            let result = loaded.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Some(Ok(structure)) => {
                        this.draft = Draft::from_structure(&structure);
                        this.original = Some(structure);
                        this.load = Load::Ready;
                    }
                    Some(Err(error)) => this.load = Load::Failed(error.message),
                    None => {}
                }
                cx.notify();
            });
        });
    }

    pub fn title(&self, _: &App) -> SharedString {
        self.table.clone().into()
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
        window.focus(&self.focus, cx);
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

    fn count(&self, section: Section) -> usize {
        let draft = &self.draft;
        match section {
            Section::Columns => draft.columns.iter().filter(|row| !row.dropped).count(),
            Section::Indexes => draft.indexes.iter().filter(|row| !row.dropped).count(),
            Section::ForeignKeys => draft.foreign_keys.iter().filter(|row| !row.dropped).count(),
            Section::Checks | Section::Table => draft.checks.iter().filter(|row| !row.dropped).count(),
        }
    }

    fn header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let switch = SECTIONS.iter().enumerate().fold(form::toggle_group(), |group, (ix, (section, key))| {
            let section = *section;
            let label = format!("{} {}", t(cx, key), self.count(section));
            group.child(
                form::toggle(("structure-section", ix), label, self.section == section)
                    .debug_selector(move || format!("structure-section:{ix}"))
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
            .child(
                div()
                    .text_size(TEXT_SM)
                    .font_semibold()
                    .debug_selector(|| "structure-title".into())
                    .child(format!("{}.{}", self.schema, self.table)),
            )
            .child(div().w(rems(30.769)).child(switch))
    }

    fn body(&self, cx: &mut Context<Self>) -> AnyElement {
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
                        .on_click(cx.listener(|this, _, _, cx| this.load(cx))),
                )
                .into_any_element(),
            Load::Ready => {
                let rows = match self.section {
                    Section::Columns | Section::Table => columns::read_only(&self.draft, cx).into_any_element(),
                    Section::Indexes => self.indexes(cx).into_any_element(),
                    Section::ForeignKeys => self.foreign_keys(cx).into_any_element(),
                    Section::Checks => self.checks(cx).into_any_element(),
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

    fn indexes(&self, cx: &App) -> impl IntoElement {
        let rows = self.draft.indexes.iter().filter(|row| !row.dropped).map(|row| {
            let index = &row.value;
            let name = index.name.clone();
            let what = if index.definition.is_empty() { index.columns.join(", ") } else { index.definition.clone() };
            line(move || format!("structure:index:{name}"), cx)
                .child(cell(&index.name, 14.))
                .child(div().flex_1().min_w_0().child(what))
                .when(index.unique, |row| row.child(form::badge("UNIQUE", cx.theme().primary)))
        });
        list(rows, "structure.noIndexes", cx)
    }

    fn foreign_keys(&self, cx: &App) -> impl IntoElement {
        let rows = self.draft.foreign_keys.iter().enumerate().filter(|(_, row)| !row.dropped).map(|(ix, row)| {
            let key = &row.value;
            let target = format!("{} → {}({})", key.columns.join(", "), key.ref_table, key.ref_columns.join(", "));
            let actions = format!("ON UPDATE {} · ON DELETE {}", key.on_update.sql(), key.on_delete.sql());
            line(move || format!("structure:foreign-key:{ix}"), cx)
                .child(cell(&key.name, 14.))
                .child(div().flex_1().min_w_0().child(target))
                .child(div().text_size(TEXT_XS).text_color(cx.theme().muted_foreground).child(actions))
        });
        list(rows, "structure.noForeignKeys", cx)
    }

    fn checks(&self, cx: &App) -> impl IntoElement {
        let rows = self.draft.checks.iter().enumerate().filter(|(_, row)| !row.dropped).map(|(ix, row)| {
            line(move || format!("structure:check:{ix}"), cx)
                .child(cell(&row.value.name, 14.))
                .child(div().flex_1().min_w_0().child(row.value.expression.clone()))
        });
        list(rows, "structure.noChecks", cx)
    }
}

// One row of a section, laid out like the others.
fn line(selector: impl Fn() -> String + 'static, cx: &App) -> Div {
    h_flex().gap(rems(0.923)).py(rems(0.462)).border_b_1().border_color(cx.theme().border).debug_selector(selector)
}

fn cell(text: &str, width: f32) -> Div {
    div().flex_none().w(rems(width)).min_w_0().overflow_hidden().child(text.to_string())
}

fn list(rows: impl Iterator<Item = Div>, empty: &str, cx: &App) -> Div {
    let rows: Vec<Div> = rows.collect();
    match rows.is_empty() {
        true => form::empty_state(None, t(cx, empty), cx),
        false => v_flex().children(rows),
    }
}

impl Render for StructureTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        v_flex()
            .size_full()
            .track_focus(&self.focus)
            .bg(theme.background)
            .debug_selector(|| "structure-tab".into())
            .child(self.header(cx))
            .child(div().flex_1().min_h_0().child(self.body(cx)))
    }
}
