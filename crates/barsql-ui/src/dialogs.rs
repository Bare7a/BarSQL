use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::{ActiveTheme, Disableable, Sizable, StyledExt, WindowExt, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::form;
use crate::i18n::t;
use crate::modal::{self, Modal};
use crate::tokens::TEXT_SM;

type OnText = Rc<dyn Fn(String, &mut Window, &mut App)>;

// Passes on the trimmed value, never an empty one.
pub struct Prompt {
    pub title: SharedString,
    pub description: Option<SharedString>,
    pub label: SharedString,
    pub placeholder: SharedString,
    pub initial: String,
    pub confirm: SharedString,
}

struct PromptForm {
    input: Entity<InputState>,
    description: Option<SharedString>,
    label: SharedString,
    confirm: SharedString,
    on_confirm: OnText,
    _subscriptions: Vec<Subscription>,
}

impl PromptForm {
    fn value(&self, cx: &App) -> Option<(String, OnText)> {
        let value = self.input.read(cx).value().trim().to_string();
        (!value.is_empty()).then(|| (value, self.on_confirm.clone()))
    }
}

fn submit(entry: Option<(String, OnText)>, window: &mut Window, cx: &mut App) {
    if let Some((value, on_confirm)) = entry {
        window.close_dialog(cx);
        on_confirm(value, window, cx);
    }
}

impl Render for PromptForm {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let empty = self.value(cx).is_none();
        v_flex()
            .child(
                modal::body().children(self.description.clone().map(|d| modal::description(d, cx))).child(form::group(
                    self.label.clone(),
                    form::input(&self.input, window, cx),
                    cx,
                )),
            )
            .child(
                modal::footer(cx)
                    .child(
                        Button::new("prompt-cancel")
                            .large()
                            .label(t(cx, "common.cancel"))
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("prompt-confirm")
                            .large()
                            .primary()
                            .label(self.confirm.clone())
                            .disabled(empty)
                            .on_click(cx.listener(|form, _, window, cx| submit(form.value(cx), window, cx))),
                    ),
            )
    }
}

pub fn prompt(
    prompt: Prompt,
    window: &mut Window,
    cx: &mut App,
    on_confirm: impl Fn(String, &mut Window, &mut App) + 'static,
) {
    let on_confirm: OnText = Rc::new(on_confirm);
    let form = cx.new(|cx| {
        let input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(prompt.placeholder.clone()).default_value(prompt.initial.clone())
        });
        let subscriptions = vec![cx.subscribe(&input, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        })];
        PromptForm {
            input,
            description: prompt.description.clone(),
            label: prompt.label.clone(),
            confirm: prompt.confirm.clone(),
            on_confirm,
            _subscriptions: subscriptions,
        }
    });
    let input = form.read(cx).input.clone();
    let title = prompt.title;
    window.open_dialog(cx, move |dialog, window, cx| {
        let entry = form.clone();
        Modal::new("prompt", title.clone()).size(modal::Size::Sm).build(dialog, form.clone(), window, cx).on_ok(
            move |_, window, cx| {
                submit(entry.read(cx).value(cx), window, cx);
                false
            },
        )
    });
    window.defer(cx, move |window, cx| input.update(cx, |state, cx| state.focus(window, cx)));
}

// Enter confirms, except a danger confirmation, which takes a click so a stray Enter can't delete anything.
pub struct Confirm {
    pub title: SharedString,
    pub description: SharedString,
    pub detail: Option<SharedString>,
    pub confirm: SharedString,
    pub danger: bool,
}

pub fn confirm(
    confirm: Confirm,
    window: &mut Window,
    cx: &mut App,
    on_confirm: impl Fn(&mut Window, &mut App) + 'static,
) {
    let on_confirm = Rc::new(on_confirm);
    let scroll = ScrollHandle::new();
    window.open_dialog(cx, move |dialog, window, cx| {
        let (on_ok, on_click) = (on_confirm.clone(), on_confirm.clone());
        let detail = confirm.detail.clone().map(|detail| modal::detail("confirm-detail", detail, &scroll, cx));
        let content = v_flex()
            .child(modal::body().child(modal::description(confirm.description.clone(), cx)).children(detail))
            .child(
                modal::footer(cx)
                    .child(
                        Button::new("confirm-cancel")
                            .large()
                            .label(t(cx, "common.cancel"))
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("confirm-ok")
                            .large()
                            .debug_selector(|| "confirm-ok".into())
                            .map(|button| if confirm.danger { button.danger().outline() } else { button.primary() })
                            .label(confirm.confirm.clone())
                            .on_click(move |_, window, cx| {
                                window.close_dialog(cx);
                                on_click(window, cx);
                            }),
                    ),
            );
        let dialog = Modal::new("confirm", confirm.title.clone())
            .size(modal::Size::Sm)
            .danger(confirm.danger)
            .build(dialog, content, window, cx)
            .overlay_closable(false);
        if confirm.danger {
            return dialog;
        }
        dialog.on_ok(move |_, window, cx| {
            window.close_dialog(cx);
            on_ok(window, cx);
            false
        })
    });
}

pub fn alert(title: SharedString, message: SharedString, window: &mut Window, cx: &mut App) {
    window.open_dialog(cx, move |dialog, window, cx| {
        let content = v_flex().child(modal::body().child(modal::description(message.clone(), cx))).child(
            modal::footer(cx).child(
                Button::new("alert-ok")
                    .large()
                    .primary()
                    .label(t(cx, "common.ok"))
                    .on_click(|_, window, cx| window.close_dialog(cx)),
            ),
        );
        Modal::new("alert", title.clone())
            .size(modal::Size::Sm)
            .build(dialog, content, window, cx)
            .overlay_closable(false)
    });
}

pub fn unsaved(
    name: SharedString,
    window: &mut Window,
    cx: &mut App,
    on_save: impl Fn(&mut Window, &mut App) + 'static,
    on_discard: impl Fn(&mut Window, &mut App) + 'static,
) {
    let (on_save, on_discard) = (Rc::new(on_save), Rc::new(on_discard));
    window.open_dialog(cx, move |dialog, window, cx| {
        let (save, discard) = (on_save.clone(), on_discard.clone());
        let content = v_flex()
            .child(modal::body().child(modal::description(t(cx, "dialog.unsavedDescription"), cx)).when(
                !name.is_empty(),
                |el| {
                    el.child(
                        div()
                            .text_size(TEXT_SM)
                            .font_semibold()
                            .text_color(cx.theme().muted_foreground)
                            .child(name.clone()),
                    )
                },
            ))
            .child(
                modal::footer(cx)
                    .flex_wrap()
                    .child(
                        Button::new("unsaved-cancel")
                            .large()
                            .label(t(cx, "dialog.unsavedCancel"))
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("unsaved-discard")
                            .large()
                            .danger()
                            .outline()
                            .label(t(cx, "dialog.unsavedDiscard"))
                            .on_click(move |_, window, cx| {
                                window.close_dialog(cx);
                                discard(window, cx);
                            }),
                    )
                    .child(Button::new("unsaved-save").large().primary().label(t(cx, "dialog.unsavedSave")).on_click(
                        move |_, window, cx| {
                            window.close_dialog(cx);
                            save(window, cx);
                        },
                    )),
            );
        Modal::new("unsaved", t(cx, "dialog.unsavedTitle")).size(modal::Size::Sm).build(dialog, content, window, cx)
    });
}
