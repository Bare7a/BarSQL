use barsql_core::StructIndex;
use barsql_sql::structure::{NameKind, Row, Section, changes, suggested_name};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, Disableable, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::columns::{check, field};
use super::{StructureTab, cell, drop_button, input, line, problem_text, row_select};
use crate::form::{self, SelectMenu as _};
use crate::i18n::t;
use crate::tokens::{RADIUS, TEXT_XS, TINT};

pub(super) struct IndexEditors {
    pub key: u64,
    pub name: Entity<InputState>,
    _subscription: Subscription,
}

pub(super) fn editors(
    key: u64,
    index: &StructIndex,
    window: &mut Window,
    cx: &mut Context<StructureTab>,
) -> IndexEditors {
    let name = input(&index.name, window, cx);
    let subscription = cx.subscribe_in(&name, window, move |tab, state, event: &InputEvent, window, cx| {
        if matches!(event, InputEvent::Change) {
            let value = state.read(cx).value().trim().to_string();
            tab.rename_index(key, value, window, cx);
        }
    });
    IndexEditors { key, name, _subscription: subscription }
}

impl StructureTab {
    fn indexes_editable(&self) -> bool {
        self.editable() && self.connection.driver.capabilities().structure.indexes
    }

    fn index_row(&self, key: u64) -> Option<usize> {
        self.index_editors.iter().position(|editors| editors.key == key).filter(|_| self.indexes_editable())
    }

    fn rename_index(&mut self, key: u64, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.index_row(key) else { return };
        if self.draft.indexes[ix].value.name != name {
            self.draft.indexes[ix].value.name = name;
            self.changed(window, cx);
        }
    }

    // A new index is named after its columns until it's named by hand.
    fn toggle_index_column(&mut self, key: u64, column: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.index_row(key) else { return };
        let driver = self.connection.driver.clone();
        let table = if self.table.is_empty() { self.draft.name.clone() } else { self.table.clone() };
        let suggested = |columns: &[String]| match columns.is_empty() {
            true => String::new(),
            false => suggested_name(&driver, NameKind::Index, &table, columns, ""),
        };
        let row = &mut self.draft.indexes[ix];
        let follows = row.origin.is_none() && row.value.name == suggested(&row.value.columns);
        match row.value.columns.iter().position(|named| *named == column) {
            Some(at) => {
                row.value.columns.remove(at);
            }
            None => row.value.columns.push(column),
        }
        if follows {
            let name = suggested(&row.value.columns);
            row.value.name = name.clone();
            self.index_editors[ix].name.update(cx, |state, cx| state.set_value(name, window, cx));
        }
        self.changed(window, cx);
    }

    fn toggle_unique(&mut self, key: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.index_row(key) else { return };
        let index = &mut self.draft.indexes[ix].value;
        index.unique = !index.unique;
        self.changed(window, cx);
    }

    fn set_index_method(&mut self, key: u64, method: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.index_row(key) else { return };
        self.draft.indexes[ix].value.method = method.to_string();
        self.changed(window, cx);
    }

    fn drop_index(&mut self, key: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.index_row(key) else { return };
        if self.draft.indexes[ix].origin.is_none() {
            self.draft.indexes.remove(ix);
            self.index_editors.remove(ix);
        } else {
            let row = &mut self.draft.indexes[ix];
            row.dropped = !row.dropped;
        }
        self.changed(window, cx);
    }

    pub(super) fn add_index(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.indexes_editable() {
            return;
        }
        let key = self.key();
        let index = StructIndex::default();
        self.index_editors.push(editors(key, &index, window, cx));
        self.draft.indexes.push(Row::added(index));
        self.changed(window, cx);
    }
}

pub(super) fn rows(tab: &StructureTab, window: &mut Window, cx: &mut Context<StructureTab>) -> AnyElement {
    if tab.draft.indexes.is_empty() {
        return form::empty_state(None, t(cx, "structure.noIndexes"), cx).into_any_element();
    }
    let theme = cx.theme().clone();
    let methods = tab.connection.driver.capabilities().structure.index_methods;
    let editable = tab.indexes_editable();
    let heading = |key: &str, width: f32| cell(&t(cx, key), width);
    let header = line(|| "structure:indexes-header".into(), cx)
        .text_size(TEXT_XS)
        .text_color(theme.muted_foreground)
        .child(heading("structure.index.name", 14.))
        .child(heading("structure.index.columns", 16.))
        .child(heading("structure.index.unique", 5.))
        .when(!methods.is_empty(), |row| row.child(heading("structure.index.method", 8.)));
    // A changed index is dropped and created again, so its old self is among the dropped ones.
    let replaced: Vec<String> = match &tab.original {
        Some(original) => changes(original, &tab.draft).dropped_indexes.into_iter().map(|index| index.name).collect(),
        None => Vec::new(),
    };
    let columns: Vec<String> =
        tab.draft.columns.iter().filter(|row| !row.dropped).map(|row| row.column.name.clone()).collect();
    let this = cx.entity().downgrade();
    let mut list = v_flex().child(header);
    for (ix, (row, editors)) in tab.draft.indexes.iter().zip(&tab.index_editors).enumerate() {
        let index = &row.value;
        let old = row.origin.and_then(|origin| tab.original.as_ref()?.indexes.get(origin));
        let stable = old.map_or_else(|| format!("new{}", editors.key), |old| old.name.clone());
        let selector = |part: &str| format!("structure:index:{stable}:{part}");
        let changed = !row.dropped && old.is_some_and(|old| replaced.contains(&old.name));
        let locked = !editable || row.dropped;
        let key = editors.key;
        let mut line = line(
            {
                let id = format!("structure:index:{stable}");
                move || id.clone()
            },
            cx,
        )
        .px(rems(0.462))
        .rounded(RADIUS)
        .when(old.is_none(), |el| el.bg(theme.success.opacity(TINT)))
        .when(changed, |el| el.bg(theme.warning.opacity(TINT)))
        .when(row.dropped, |el| el.bg(theme.danger.opacity(TINT)).line_through());
        if index.definition.is_empty() {
            let label: SharedString = match index.columns.is_empty() {
                true => t(cx, "structure.index.columns"),
                false => index.columns.join(", ").into(),
            };
            let picker = row_select(selector("columns"), label, cx).disabled(locked).select_menu(window, cx, {
                let (this, columns, chosen) = (this.clone(), columns.clone(), index.columns.clone());
                move |mut menu: PopupMenu, _, _| {
                    for column in &columns {
                        let (this, column) = (this.clone(), column.clone());
                        let item = PopupMenuItem::new(column.clone()).checked(chosen.contains(&column));
                        menu = menu.item(item.on_click(move |_, window, cx| {
                            let _ = this.update(cx, |tab, cx| tab.toggle_index_column(key, column.clone(), window, cx));
                        }));
                    }
                    menu
                }
            });
            let method = (!methods.is_empty()).then(|| {
                let current = if index.method.is_empty() { methods[0].to_string() } else { index.method.clone() };
                let select =
                    row_select(selector("method"), current.clone(), cx).disabled(locked).select_menu(window, cx, {
                        let this = this.clone();
                        move |mut menu: PopupMenu, _, _| {
                            for &method in methods {
                                let this = this.clone();
                                let item = PopupMenuItem::new(method).checked(method == current);
                                menu = menu.item(item.on_click(move |_, window, cx| {
                                    let _ = this.update(cx, |tab, cx| tab.set_index_method(key, method, window, cx));
                                }));
                            }
                            menu
                        }
                    });
                div().flex_none().w(rems(8.)).child(select)
            });
            line = line
                .child(field(&editors.name, selector("name"), false, locked, 14., window, cx))
                .child(div().flex_none().w(rems(16.)).child(picker))
                .child(check(selector("unique"), index.unique, locked, key, StructureTab::toggle_unique, cx))
                .children(method);
        } else {
            let definition = format!("{}: {}", t(cx, "structure.index.definition"), index.definition);
            line = line
                .child(cell(&index.name, 14.))
                .child(div().flex_1().min_w_0().text_color(theme.muted_foreground).child(definition));
        }
        let drop = editable.then(|| {
            drop_button(selector("drop"), row.dropped, cx)
                .on_click(cx.listener(move |tab, _, window, cx| tab.drop_index(key, window, cx)))
        });
        line = line
            .when(index.constraint, |line| line.child(form::badge(t(cx, "structure.index.constraint"), theme.primary)))
            .children(drop);
        list = list.child(line);
        for kind in tab.problems(Section::Indexes, ix) {
            let id = format!("structure:problem:indexes:{ix}");
            list = list.child(form::error(problem_text(kind, cx), cx).debug_selector(move || id.clone()));
        }
    }
    list.into_any_element()
}
