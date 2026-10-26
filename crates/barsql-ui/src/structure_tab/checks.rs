use barsql_core::Check;
use barsql_sql::structure::{Row, Section};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::Button;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::{ActiveTheme, Icon, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::columns::field;
use super::{StructureTab, cell, input, line, problem_text};
use crate::form::{self, ToolButton as _};
use crate::i18n::t;
use crate::tokens::{ICON_SM, RADIUS, TEXT_XS, TINT};

#[derive(Debug, Clone, Copy)]
enum Part {
    Name,
    Expression,
}

pub(super) struct CheckEditors {
    pub key: u64,
    pub name: Entity<InputState>,
    pub expression: Entity<InputState>,
    _subscriptions: [Subscription; 2],
}

fn watch(
    state: &Entity<InputState>,
    key: u64,
    part: Part,
    window: &mut Window,
    cx: &mut Context<StructureTab>,
) -> Subscription {
    cx.subscribe_in(state, window, move |tab, state, event: &InputEvent, window, cx| {
        if matches!(event, InputEvent::Change) {
            let value = state.read(cx).value().trim().to_string();
            tab.edit_check(key, part, value, window, cx);
        }
    })
}

pub(super) fn editors(key: u64, check: &Check, window: &mut Window, cx: &mut Context<StructureTab>) -> CheckEditors {
    let (name, expression) = (input(&check.name, window, cx), input(&check.expression, window, cx));
    let subscriptions =
        [watch(&name, key, Part::Name, window, cx), watch(&expression, key, Part::Expression, window, cx)];
    CheckEditors { key, name, expression, _subscriptions: subscriptions }
}

impl StructureTab {
    fn checks_editable(&self) -> bool {
        self.editable() && self.connection.driver.capabilities().structure.checks
    }

    fn check_row(&self, key: u64) -> Option<usize> {
        self.check_editors.iter().position(|editors| editors.key == key).filter(|_| self.checks_editable())
    }

    fn edit_check(&mut self, key: u64, part: Part, value: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.check_row(key) else { return };
        let check = &mut self.draft.checks[ix].value;
        let field = match part {
            Part::Name => &mut check.name,
            Part::Expression => &mut check.expression,
        };
        if *field != value {
            *field = value;
            self.changed(window, cx);
        }
    }

    fn drop_check(&mut self, key: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.check_row(key) else { return };
        if self.draft.checks[ix].origin.is_none() {
            self.draft.checks.remove(ix);
            self.check_editors.remove(ix);
        } else {
            let row = &mut self.draft.checks[ix];
            row.dropped = !row.dropped;
        }
        self.changed(window, cx);
    }

    pub(super) fn add_check(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.checks_editable() {
            return;
        }
        let key = self.key();
        let check = Check::default();
        let editors = editors(key, &check, window, cx);
        editors.expression.update(cx, |state, cx| state.focus(window, cx));
        self.check_editors.push(editors);
        self.draft.checks.push(Row::added(check));
        self.changed(window, cx);
    }
}

pub(super) fn rows(tab: &StructureTab, window: &mut Window, cx: &mut Context<StructureTab>) -> AnyElement {
    if tab.draft.checks.is_empty() {
        return form::empty_state(None, t(cx, "structure.noChecks"), cx).into_any_element();
    }
    let theme = cx.theme().clone();
    let editable = tab.checks_editable();
    let header = line(|| "structure:checks-header".into(), cx)
        .text_size(TEXT_XS)
        .text_color(theme.muted_foreground)
        .child(cell(&t(cx, "structure.check.name"), 14.))
        .child(div().flex_1().child(t(cx, "structure.check.expression")));
    let mut list = v_flex().child(header);
    for (ix, (row, editors)) in tab.draft.checks.iter().zip(&tab.check_editors).enumerate() {
        let check = &row.value;
        let old = row.origin.and_then(|origin| tab.original.as_ref()?.checks.get(origin));
        // Checks may have no name, so a loaded one goes by its place.
        let stable = row.origin.map_or_else(|| format!("new{}", editors.key), |origin| origin.to_string());
        let selector = |part: &str| format!("structure:check:{stable}:{part}");
        let changed = |differs: fn(&Check, &Check) -> bool| old.is_some_and(|old| differs(old, check));
        let locked = !editable || row.dropped;
        let key = editors.key;
        let drop = editable.then(|| {
            let icon = if row.dropped { Lucide::Undo2 } else { Lucide::Trash };
            let tip = if row.dropped { "structure.restore" } else { "structure.drop" };
            Button::new(SharedString::from(selector("drop")))
                .debug_selector({
                    let id = selector("drop");
                    move || id.clone()
                })
                .tool_icon(Icon::new(icon), ICON_SM)
                .tooltip(t(cx, tip))
                .on_click(cx.listener(move |tab, _, window, cx| tab.drop_check(key, window, cx)))
        });
        let name = field(&editors.name, selector("name"), changed(|a, b| a.name != b.name), locked, 14., window, cx);
        let expression = field(
            &editors.expression,
            selector("expression"),
            changed(|a, b| a.expression != b.expression),
            locked,
            0.,
            window,
            cx,
        )
        .flex_1()
        .w_auto();
        let line = line(
            {
                let id = format!("structure:check:{stable}");
                move || id.clone()
            },
            cx,
        )
        .px(rems(0.462))
        .rounded(RADIUS)
        .when(old.is_none(), |el| el.bg(theme.success.opacity(TINT)))
        .when(row.dropped, |el| el.bg(theme.danger.opacity(TINT)).line_through())
        .child(name)
        .child(expression)
        .children(drop);
        list = list.child(line);
        for kind in tab.problems(Section::Checks, ix) {
            let id = format!("structure:problem:checks:{ix}");
            list = list.child(form::error(problem_text(kind, cx), cx).debug_selector(move || id.clone()));
        }
    }
    list.into_any_element()
}
