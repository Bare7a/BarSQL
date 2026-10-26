use barsql_core::{ForeignKey, RefAction};
use barsql_sql::structure::{NameKind, Row, Section, changes, suggested_name};
use gpui_kit::component::button::Button;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, Disableable, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::columns::field;
use super::{StructureTab, cell, drop_button, input, line, problem_text, row_select};
use crate::form::{self, SelectMenu as _};
use crate::i18n::t;
use crate::schema;
use crate::tokens::{RADIUS, TEXT_XS, TINT};

pub(super) struct KeyEditors {
    pub key: u64,
    pub name: Entity<InputState>,
    _subscription: Subscription,
}

pub(super) fn editors(key: u64, fk: &ForeignKey, window: &mut Window, cx: &mut Context<StructureTab>) -> KeyEditors {
    let name = input(&fk.name, window, cx);
    let subscription = cx.subscribe_in(&name, window, move |tab, state, event: &InputEvent, window, cx| {
        if matches!(event, InputEvent::Change) {
            let value = state.read(cx).value().trim().to_string();
            tab.rename_foreign_key(key, value, window, cx);
        }
    });
    KeyEditors { key, name, _subscription: subscription }
}

fn action_label(action: RefAction, cx: &App) -> SharedString {
    let key = match action {
        RefAction::NoAction => "noAction",
        RefAction::Restrict => "restrict",
        RefAction::Cascade => "cascade",
        RefAction::SetNull => "setNull",
        RefAction::SetDefault => "setDefault",
    };
    t(cx, &format!("structure.action.{key}"))
}

fn toggle(names: &mut Vec<String>, name: String) {
    match names.iter().position(|listed| *listed == name) {
        Some(at) => {
            names.remove(at);
        }
        None => names.push(name),
    }
}

impl StructureTab {
    fn keys_editable(&self) -> bool {
        self.editable() && self.connection.driver.capabilities().structure.foreign_keys
    }

    fn key_row(&self, key: u64) -> Option<usize> {
        self.key_editors.iter().position(|editors| editors.key == key).filter(|_| self.keys_editable())
    }

    fn target_schema(&self, fk: &ForeignKey) -> String {
        if fk.ref_schema.is_empty() { self.schema.clone() } else { fk.ref_schema.clone() }
    }

    fn rename_foreign_key(&mut self, key: u64, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.key_row(key) else { return };
        if self.draft.foreign_keys[ix].value.name != name {
            self.draft.foreign_keys[ix].value.name = name;
            self.changed(window, cx);
        }
    }

    // A new key is named after its columns and target until it's named by hand.
    fn edit_foreign_key(
        &mut self,
        key: u64,
        edit: impl FnOnce(&mut ForeignKey),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ix) = self.key_row(key) else { return };
        let driver = self.connection.driver.clone();
        let table = if self.table.is_empty() { self.draft.name.clone() } else { self.table.clone() };
        let suggested = |fk: &ForeignKey| match fk.columns.is_empty() || fk.ref_table.is_empty() {
            true => String::new(),
            false => suggested_name(&driver, NameKind::ForeignKey, &table, &fk.columns, &fk.ref_table),
        };
        let row = &mut self.draft.foreign_keys[ix];
        let follows = row.origin.is_none() && row.value.name == suggested(&row.value);
        edit(&mut row.value);
        if follows {
            let name = suggested(&row.value);
            row.value.name = name.clone();
            self.key_editors[ix].name.update(cx, |state, cx| state.set_value(name, window, cx));
        }
        self.changed(window, cx);
    }

    // Loads the referenced table's columns, and fills an empty column list with its primary key.
    fn pick_target(&mut self, key: u64, table: String, window: &mut Window, cx: &mut Context<Self>) {
        let schema = self.schema.clone();
        let target = (schema.clone(), table.clone());
        self.edit_foreign_key(
            key,
            |fk| {
                (fk.ref_schema, fk.ref_table) = target;
                fk.ref_columns.clear();
            },
            window,
            cx,
        );
        let load = schema::columns(&self.connection.id, &schema, &table, cx);
        cx.spawn_in(window, async move |this, cx| {
            let Ok(columns) = load.await else { return };
            let keys: Vec<String> = columns.iter().filter(|c| c.is_primary).map(|c| c.name.clone()).collect();
            let _ = this.update_in(cx, |tab, window, cx| {
                let empty = tab.key_row(key).is_some_and(|ix| tab.draft.foreign_keys[ix].value.ref_columns.is_empty());
                if empty && !keys.is_empty() {
                    tab.edit_foreign_key(key, |fk| fk.ref_columns = keys, window, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn drop_foreign_key(&mut self, key: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.key_row(key) else { return };
        if self.draft.foreign_keys[ix].origin.is_none() {
            self.draft.foreign_keys.remove(ix);
            self.key_editors.remove(ix);
        } else {
            let row = &mut self.draft.foreign_keys[ix];
            row.dropped = !row.dropped;
        }
        self.changed(window, cx);
    }

    pub(super) fn add_foreign_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.keys_editable() {
            return;
        }
        let key = self.key();
        let fk = ForeignKey::default();
        self.key_editors.push(editors(key, &fk, window, cx));
        self.draft.foreign_keys.push(Row::added(fk));
        self.changed(window, cx);
    }

    // So the referenced-column menus have their lists when first opened.
    pub(super) fn load_targets(&self, cx: &mut App) {
        for fk in self.draft.foreign_keys.iter().map(|row| &row.value) {
            drop(schema::columns(&self.connection.id, &self.target_schema(fk), &fk.ref_table, cx));
        }
    }
}

// A menu that toggles names in or out of a list, in the order they're picked.
fn names_menu(
    this: WeakEntity<StructureTab>,
    names: Option<Vec<String>>,
    chosen: Vec<String>,
    key: u64,
    edit: fn(&mut ForeignKey, String),
) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
    move |mut menu, _, cx| {
        let Some(names) = &names else {
            return menu.item(PopupMenuItem::new(t(cx, "tableView.loading")).disabled(true));
        };
        for name in names {
            let (this, name) = (this.clone(), name.clone());
            let item = PopupMenuItem::new(name.clone()).checked(chosen.contains(&name));
            menu = menu.item(item.on_click(move |_, window, cx| {
                let name = name.clone();
                let _ = this.update(cx, |tab, cx| tab.edit_foreign_key(key, |fk| edit(fk, name), window, cx));
            }));
        }
        menu
    }
}

fn joined(names: &[String], empty: &str, cx: &App) -> SharedString {
    if names.is_empty() { t(cx, empty) } else { names.join(", ").into() }
}

#[allow(clippy::too_many_arguments)]
fn action_select(
    this: WeakEntity<StructureTab>,
    id: String,
    current: RefAction,
    actions: &'static [RefAction],
    locked: bool,
    key: u64,
    set: fn(&mut ForeignKey, RefAction),
    window: &mut Window,
    cx: &mut Context<StructureTab>,
) -> Div {
    let (slot, picker) = select(id, action_label(current, cx), locked, 8., cx);
    slot.child(picker.select_menu(window, cx, move |mut menu: PopupMenu, _, cx| {
        for &action in actions {
            let this = this.clone();
            let item = PopupMenuItem::new(action_label(action, cx)).checked(action == current);
            menu = menu.item(item.on_click(move |_, window, cx| {
                let _ = this.update(cx, |tab, cx| tab.edit_foreign_key(key, |fk| set(fk, action), window, cx));
            }));
        }
        menu
    }))
}

fn select(id: String, label: SharedString, locked: bool, width: f32, cx: &App) -> (Div, Button) {
    (div().flex_none().w(rems(width)), row_select(id, label, cx).disabled(locked))
}

pub(super) fn rows(tab: &StructureTab, window: &mut Window, cx: &mut Context<StructureTab>) -> AnyElement {
    if tab.draft.foreign_keys.is_empty() {
        return form::empty_state(None, t(cx, "structure.noForeignKeys"), cx).into_any_element();
    }
    let theme = cx.theme().clone();
    let actions = tab.connection.driver.capabilities().structure.ref_actions;
    let editable = tab.keys_editable();
    let heading = |key: &str, width: f32| cell(&t(cx, key), width);
    let header = line(|| "structure:foreign-keys-header".into(), cx)
        .text_size(TEXT_XS)
        .text_color(theme.muted_foreground)
        .child(heading("structure.foreignKey.name", 12.))
        .child(heading("structure.foreignKey.columns", 10.))
        .child(heading("structure.foreignKey.table", 10.))
        .child(heading("structure.foreignKey.refColumns", 10.))
        .child(heading("structure.foreignKey.onUpdate", 8.))
        .child(heading("structure.foreignKey.onDelete", 8.));
    // A changed key is dropped and added again, so its old self is among the dropped ones.
    let replaced = tab.original.as_ref().map(|original| changes(original, &tab.draft).dropped_foreign_keys);
    // The schema's tables, and each key's referenced columns once they're loaded.
    let (tables, targets): (Vec<String>, Vec<Option<Vec<String>>>) = {
        let cached = schema::get(cx, &tab.connection.id);
        let tables = cached
            .map(|cached| cached.loaded_tables())
            .unwrap_or_default()
            .into_iter()
            .filter(|(schema, _)| *schema == tab.schema)
            .flat_map(|(_, tables)| tables.iter().filter(|t| !schema::is_view_kind(&t.kind)).map(|t| t.name.clone()))
            .collect();
        let target = |fk: &ForeignKey| {
            let columns = cached?.columns(&tab.target_schema(fk), &fk.ref_table)?;
            Some(columns.iter().map(|c| c.name.clone()).collect())
        };
        (tables, tab.draft.foreign_keys.iter().map(|row| target(&row.value)).collect())
    };
    let columns: Vec<String> =
        tab.draft.columns.iter().filter(|row| !row.dropped).map(|row| row.column.name.clone()).collect();
    let this = cx.entity().downgrade();
    let mut list = v_flex().child(header);
    for (ix, ((row, editors), names)) in tab.draft.foreign_keys.iter().zip(&tab.key_editors).zip(targets).enumerate() {
        let fk = &row.value;
        let old = row.origin.and_then(|origin| tab.original.as_ref()?.foreign_keys.get(origin));
        // A key may have no name, so a loaded one goes by its place.
        let stable = row.origin.map_or_else(|| format!("new{}", editors.key), |origin| origin.to_string());
        let selector = |part: &str| format!("structure:foreign-key:{stable}:{part}");
        let changed = !row.dropped && old.is_some_and(|old| replaced.as_ref().is_some_and(|gone| gone.contains(old)));
        let locked = !editable || row.dropped;
        let key = editors.key;
        let label = joined(&fk.columns, "structure.foreignKey.columns", cx);
        let (slot, picker) = select(selector("columns"), label, locked, 10., cx);
        let edit_columns: fn(&mut ForeignKey, String) = |fk, name| toggle(&mut fk.columns, name);
        let menu = names_menu(this.clone(), Some(columns.clone()), fk.columns.clone(), key, edit_columns);
        let column_pick = slot.child(picker.select_menu(window, cx, menu));

        let label =
            if fk.ref_table.is_empty() { t(cx, "structure.foreignKey.table") } else { fk.ref_table.clone().into() };
        let (slot, picker) = select(selector("table"), label, locked, 10., cx);
        let table_pick = slot.child(picker.select_menu(window, cx, {
            let (this, tables, current) = (this.clone(), tables.clone(), fk.ref_table.clone());
            move |mut menu: PopupMenu, _, _| {
                for table in &tables {
                    let (this, table) = (this.clone(), table.clone());
                    let item = PopupMenuItem::new(table.clone()).checked(*table == current);
                    menu = menu.item(item.on_click(move |_, window, cx| {
                        let _ = this.update(cx, |tab, cx| tab.pick_target(key, table.clone(), window, cx));
                    }));
                }
                menu
            }
        }));

        let label = joined(&fk.ref_columns, "structure.foreignKey.refColumns", cx);
        let (slot, picker) = select(selector("ref-columns"), label, locked || fk.ref_table.is_empty(), 10., cx);
        let edit_targets: fn(&mut ForeignKey, String) = |fk, name| toggle(&mut fk.ref_columns, name);
        let menu = names_menu(this.clone(), names, fk.ref_columns.clone(), key, edit_targets);
        let target_pick = slot.child(picker.select_menu(window, cx, menu));

        let on_update = action_select(
            this.clone(),
            selector("on-update"),
            fk.on_update,
            actions,
            locked,
            key,
            |fk, action| fk.on_update = action,
            window,
            cx,
        );
        let on_delete = action_select(
            this.clone(),
            selector("on-delete"),
            fk.on_delete,
            actions,
            locked,
            key,
            |fk, action| fk.on_delete = action,
            window,
            cx,
        );

        let drop = editable.then(|| {
            drop_button(selector("drop"), row.dropped, cx)
                .on_click(cx.listener(move |tab, _, window, cx| tab.drop_foreign_key(key, window, cx)))
        });
        let line = line(
            {
                let id = format!("structure:foreign-key:{stable}");
                move || id.clone()
            },
            cx,
        )
        .px(rems(0.462))
        .rounded(RADIUS)
        .when(old.is_none(), |el| el.bg(theme.success.opacity(TINT)))
        .when(changed, |el| el.bg(theme.warning.opacity(TINT)))
        .when(row.dropped, |el| el.bg(theme.danger.opacity(TINT)).line_through())
        .child(field(&editors.name, selector("name"), false, locked, 12., window, cx))
        .child(column_pick)
        .child(table_pick)
        .child(target_pick)
        .child(on_update)
        .child(on_delete)
        .children(drop);
        list = list.child(line);
        for kind in tab.problems(Section::ForeignKeys, ix) {
            let id = format!("structure:problem:foreign-keys:{ix}");
            list = list.child(form::error(problem_text(kind, cx), cx).debug_selector(move || id.clone()));
        }
    }
    list.into_any_element()
}
