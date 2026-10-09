use std::collections::HashMap;
use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::InputState;
use gpui_kit::component::{ActiveTheme, Sizable, WindowExt, v_flex};
use gpui_kit::*;

use crate::form;
use crate::i18n::t;
use crate::modal::{self, Modal};
use crate::state::{set_setting_json, setting_json};

// Most recent first, one entry per name.
const VALUES_KEY: &str = "barsql-query-params";
const REMEMBERED: usize = 100;

pub type Values = HashMap<String, String>;
type OnRun = Rc<dyn Fn(Values, &mut Window, &mut App)>;

fn remembered(cx: &App) -> Vec<(String, String)> {
    setting_json(cx, VALUES_KEY)
}

fn remember(values: &[(String, String)], cx: &App) {
    let mut kept = remembered(cx);
    kept.retain(|(name, _)| !values.iter().any(|(given, _)| given == name));
    let mut all = values.to_vec();
    all.extend(kept);
    all.truncate(REMEMBERED);
    set_setting_json(cx, VALUES_KEY, &all);
}

struct ParamsForm {
    inputs: Vec<(String, Entity<InputState>)>,
    scroll: ScrollHandle,
    on_run: OnRun,
}

impl ParamsForm {
    // An empty value runs as NULL, and is remembered as empty.
    fn submit(&self, window: &mut Window, cx: &mut App) {
        let typed: Vec<(String, String)> =
            self.inputs.iter().map(|(name, input)| (name.clone(), input.read(cx).value().trim().to_string())).collect();
        remember(&typed, cx);
        let values = typed
            .into_iter()
            .map(|(name, value)| (name, if value.is_empty() { "NULL".to_string() } else { value }))
            .collect();
        let on_run = self.on_run.clone();
        window.close_dialog(cx);
        on_run(values, window, cx);
    }
}

impl Render for ParamsForm {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mono = cx.theme().mono_font_family.clone();
        let fields: Vec<Div> = self
            .inputs
            .iter()
            .map(|(name, input)| {
                let label = form::label(format!(":{name}"), cx).font_family(mono.clone());
                v_flex().min_w_0().child(label).child(form::input(input, window, cx))
            })
            .collect();
        let body = modal::body().child(modal::description(t(cx, "params.description"), cx)).children(fields);
        modal::scroll_content().child(modal::scroll_body(&self.scroll, body)).child(
            modal::footer(cx)
                .child(
                    Button::new("params-cancel")
                        .large()
                        .label(t(cx, "common.cancel"))
                        .on_click(|_, window, cx| window.close_dialog(cx)),
                )
                .child(
                    Button::new("params-run")
                        .large()
                        .debug_selector(|| "params-run".into())
                        .primary()
                        .label(t(cx, "editor.run"))
                        .on_click(cx.listener(|form, _, window, cx| form.submit(window, cx))),
                ),
        )
    }
}

// Asks for the values of a run's `:name` placeholders, prefilled with the ones given last time. Enter runs.
pub fn open(
    names: Vec<String>,
    window: &mut Window,
    cx: &mut App,
    on_run: impl Fn(Values, &mut Window, &mut App) + 'static,
) {
    let last = remembered(cx);
    let form = cx.new(|cx| {
        let inputs = names
            .into_iter()
            .map(|name| {
                let value = last.iter().find(|(given, _)| *given == name).map(|(_, value)| value.clone());
                let input = cx
                    .new(|cx| InputState::new(window, cx).placeholder("NULL").default_value(value.unwrap_or_default()));
                (name, input)
            })
            .collect();
        ParamsForm { inputs, scroll: ScrollHandle::new(), on_run: Rc::new(on_run) }
    });
    let first = form.read(cx).inputs.first().map(|(_, input)| input.clone());
    window.open_dialog(cx, move |dialog, window, cx| {
        let entry = form.clone();
        Modal::new("params", t(cx, "params.title")).size(modal::Size::Sm).build(dialog, form.clone(), window, cx).on_ok(
            move |_, window, cx| {
                entry.update(cx, |form, cx| form.submit(window, cx));
                false
            },
        )
    });
    if let Some(first) = first {
        window.defer(cx, move |window, cx| {
            first.update(cx, |state, cx| {
                state.focus(window, cx);
                state.select_all(window, cx);
            })
        });
    }
}
