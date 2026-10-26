use barsql_core::capabilities::IdentityStyle;
use barsql_core::{Identity, StructColumn};
use barsql_sql::structure::{Section, type_suggestions};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, Disableable, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::{FIELD, StructureTab, cell, drop_button, input, line, problem_text, row_select};
use crate::form::{self, SelectMenu as _};
use crate::i18n::{t, t_with};
use crate::schema;
use crate::tokens::{RADIUS, TEXT_XS, TINT};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Field {
    Name,
    Type,
    Default,
    Comment,
}

pub(super) struct Editors {
    pub key: u64,
    pub name: Entity<InputState>,
    pub data_type: Entity<InputState>,
    pub default: Entity<InputState>,
    pub comment: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

fn watch(
    state: &Entity<InputState>,
    key: u64,
    field: Field,
    window: &mut Window,
    cx: &mut Context<StructureTab>,
) -> Subscription {
    cx.subscribe_in(state, window, move |tab, state, event: &InputEvent, window, cx| {
        if matches!(event, InputEvent::Change) {
            let value = state.read(cx).value().to_string();
            tab.edit(key, field, value, window, cx);
        }
    })
}

pub(super) fn editors(key: u64, column: &StructColumn, window: &mut Window, cx: &mut Context<StructureTab>) -> Editors {
    let (name, data_type) = (input(&column.name, window, cx), input(&column.data_type, window, cx));
    let (default, comment) = (input(&column.default, window, cx), input(&column.comment, window, cx));
    let subscriptions = vec![
        watch(&name, key, Field::Name, window, cx),
        watch(&data_type, key, Field::Type, window, cx),
        watch(&default, key, Field::Default, window, cx),
        watch(&comment, key, Field::Comment, window, cx),
    ];
    Editors { key, name, data_type, default, comment, _subscriptions: subscriptions }
}

pub(super) fn identity_label(identity: Identity, cx: &App) -> SharedString {
    match identity {
        Identity::None => t(cx, "structure.identity.none"),
        Identity::ByDefault => t(cx, "structure.identity.byDefault"),
        Identity::Always => t(cx, "structure.identity.always"),
        Identity::AutoIncrement => t(cx, "structure.identity.autoIncrement"),
        Identity::Seeded { seed, increment } => {
            let (seed, increment) = (seed.to_string(), increment.to_string());
            t_with(cx, "structure.identity.seeded", &[("seed", &seed), ("increment", &increment)])
        }
    }
}

fn identity_choices(style: IdentityStyle) -> &'static [Identity] {
    match style {
        IdentityStyle::Generated => &[Identity::None, Identity::ByDefault, Identity::Always],
        IdentityStyle::AutoIncrement => &[Identity::None, Identity::AutoIncrement],
        IdentityStyle::Seeded => &[Identity::None, Identity::Seeded { seed: 1, increment: 1 }],
        IdentityStyle::None => &[],
    }
}

// The engine's usual types first, then the ones the loaded tables use.
fn suggestions(tab: &StructureTab, cx: &App) -> Vec<String> {
    let mut out: Vec<String> = type_suggestions(&tab.connection.driver).iter().map(|t| t.to_string()).collect();
    let used = schema::column_types(cx, &tab.connection.id);
    let here = tab.draft.columns.iter().map(|row| row.column.data_type.clone());
    for data_type in used.into_iter().chain(here).filter(|t| !t.trim().is_empty()) {
        if !out.iter().any(|known| known.eq_ignore_ascii_case(&data_type)) {
            out.push(data_type);
        }
    }
    out
}

pub(super) fn field(
    state: &Entity<InputState>,
    selector: String,
    changed: bool,
    locked: bool,
    width: f32,
    window: &Window,
    cx: &App,
) -> Div {
    // The input paints its own background, so a change tints the input itself.
    let input = form::input(state, window, cx).disabled(locked);
    let input = if changed { input.bg(cx.theme().warning.opacity(TINT)) } else { input };
    div().flex_none().w(rems(width)).debug_selector(move || selector.clone()).child(input)
}

pub(super) type Toggle = fn(&mut StructureTab, u64, &mut Window, &mut Context<StructureTab>);

pub(super) fn check(
    id: String,
    checked: bool,
    locked: bool,
    key: u64,
    toggle: Toggle,
    cx: &mut Context<StructureTab>,
) -> Div {
    let selector = id.clone();
    let mut checkbox = form::checkbox(id, checked, "", locked, cx);
    if !locked {
        checkbox = checkbox.on_click(cx.listener(move |tab, _, window, cx| toggle(tab, key, window, cx)));
    }
    div().flex_none().w(rems(5.)).debug_selector(move || selector.clone()).child(checkbox)
}

pub(super) fn rows(tab: &StructureTab, window: &mut Window, cx: &mut Context<StructureTab>) -> impl IntoElement {
    let theme = cx.theme().clone();
    let editing = &tab.connection.driver.capabilities().structure;
    let editable = tab.editable();
    let heading = |key: &str, width: f32| cell(&t(cx, key), width);
    let header = line(|| "structure:columns-header".into(), cx)
        .text_size(TEXT_XS)
        .text_color(theme.muted_foreground)
        .child(heading("structure.name", 12.))
        .child(heading("structure.type", 12.))
        .child(heading("structure.nullable", 5.))
        .child(heading("structure.primaryKey", 5.))
        .child(heading("structure.default", 11.))
        .when(editing.identity != IdentityStyle::None, |row| row.child(heading("structure.identity.label", 9.)))
        .when(editing.comments, |row| row.child(div().flex_1().child(t(cx, "structure.comment"))));
    let this = cx.entity().downgrade();
    let mut list = v_flex().child(header);
    for (ix, (row, editors)) in tab.draft.columns.iter().zip(&tab.editors).enumerate() {
        let column = &row.column;
        let old = row.origin.and_then(|origin| tab.original.as_ref()?.columns.get(origin));
        let stable = old.map_or_else(|| format!("new{}", editors.key), |old| old.name.clone());
        let selector = |part: &str| format!("structure:column:{stable}:{part}");
        let changed = |differs: fn(&StructColumn, &StructColumn) -> bool| old.is_some_and(|old| differs(old, column));
        let locked = !editable || !column.generated.is_empty() || row.dropped;
        let key = editors.key;
        let identity_shown = editing.identity != IdentityStyle::None && (old.is_none() || editing.identity_on_existing);
        // A chevron alone, so it fits beside the type in its cell.
        let picker = row_select(selector("types"), SharedString::default(), cx)
            .min_w(rems(FIELD))
            .w(rems(FIELD))
            .px_0()
            .disabled(locked)
            .select_menu(window, cx, {
                let this = this.clone();
                move |mut menu: PopupMenu, _, cx| {
                    let types = this.upgrade().map(|tab| suggestions(tab.read(cx), cx)).unwrap_or_default();
                    for data_type in types {
                        let this = this.clone();
                        menu = menu.item(PopupMenuItem::new(data_type.clone()).on_click(move |_, window, cx| {
                            let _ = this.update(cx, |tab, cx| tab.pick_type(key, data_type.clone(), window, cx));
                        }));
                    }
                    menu
                }
            });
        let default = match column.generated.as_str() {
            "" => field(
                &editors.default,
                selector("default"),
                changed(|a, b| a.default != b.default),
                locked,
                11.,
                window,
                cx,
            ),
            generated => cell(&format!("{} {generated}", t(cx, "structure.generated")), 11.),
        };
        let identity = identity_shown.then(|| {
            let current = column.identity;
            let choices = identity_choices(editing.identity);
            let label = identity_label(current, cx);
            div().flex_none().w(rems(9.)).child(
                row_select(selector("identity"), label, cx).disabled(locked).select_menu(window, cx, {
                    let this = this.clone();
                    move |mut menu: PopupMenu, _, cx| {
                        for &choice in choices {
                            let this = this.clone();
                            let item = PopupMenuItem::new(identity_label(choice, cx)).checked(choice == current);
                            menu = menu.item(item.on_click(move |_, window, cx| {
                                let _ = this.update(cx, |tab, cx| tab.set_identity(key, choice, window, cx));
                            }));
                        }
                        menu
                    }
                }),
            )
        });
        let comment = editing.comments.then(|| {
            field(&editors.comment, selector("comment"), changed(|a, b| a.comment != b.comment), locked, 0., window, cx)
                .flex_1()
                .w_auto()
        });
        let drop = editable.then(|| {
            drop_button(selector("drop"), row.dropped, cx)
                .disabled(!column.generated.is_empty())
                .on_click(cx.listener(move |tab, _, window, cx| tab.drop_row(key, window, cx)))
        });
        let line = line(
            {
                let id = format!("structure:column:{stable}");
                move || id.clone()
            },
            cx,
        )
        .px(rems(0.462))
        .rounded(RADIUS)
        .when(old.is_none(), |el| el.bg(theme.success.opacity(TINT)))
        .when(row.dropped, |el| el.bg(theme.danger.opacity(TINT)).line_through())
        .child(field(&editors.name, selector("name"), changed(|a, b| a.name != b.name), locked, 12., window, cx))
        .child(
            h_flex()
                .flex_none()
                .w(rems(12.))
                .gap(rems(0.308))
                .child(field(
                    &editors.data_type,
                    selector("type"),
                    changed(|a, b| a.data_type != b.data_type),
                    locked,
                    9.384,
                    window,
                    cx,
                ))
                .child(picker),
        )
        .child(check(selector("nullable"), column.nullable, locked, key, StructureTab::toggle_nullable, cx))
        .child(check(selector("primary"), row.primary, locked, key, StructureTab::toggle_primary, cx))
        .child(default)
        .children(identity)
        .children(comment)
        .children(drop);
        list = list.child(line);
        for kind in tab.problems(Section::Columns, ix) {
            let id = format!("structure:problem:columns:{ix}");
            list = list.child(form::error(problem_text(kind, cx), cx).debug_selector(move || id.clone()));
        }
    }
    list
}
