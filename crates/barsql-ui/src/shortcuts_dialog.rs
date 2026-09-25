use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{ActiveTheme, Sizable, WindowExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::form;
use crate::i18n::{t, t_with};
use crate::modal::{self, Modal};
use crate::shortcuts::{self, Binding, Category, SHORTCUTS, ShortcutDef};
use crate::tokens::{RADIUS, RADIUS_SM, TEXT_SM, TINT, TINT_BORDER};

pub struct ShortcutsDialog {
    recording: Option<&'static str>,
    error: Option<SharedString>,
    interceptor: Option<Subscription>,
    scroll: ScrollHandle,
}

impl ShortcutsDialog {
    fn new() -> Self {
        Self { recording: None, error: None, interceptor: None, scroll: ScrollHandle::new() }
    }

    // Every keystroke goes to the recorder first, so no shortcut fires while it listens.
    fn record(&mut self, id: &'static str, cx: &mut Context<Self>) {
        self.recording = Some(id);
        self.error = None;
        let this = cx.entity().downgrade();
        self.interceptor = Some(cx.intercept_keystrokes(move |event, _, cx| {
            cx.stop_propagation();
            let keystroke = event.keystroke.clone();
            let _ = this.update(cx, |this, cx| this.pressed(&keystroke, cx));
        }));
        cx.notify();
    }

    fn pressed(&mut self, keystroke: &Keystroke, cx: &mut Context<Self>) {
        let Some(id) = self.recording else { return };
        if keystroke.key == "escape" && !keystroke.modifiers.modified() {
            self.stop(cx);
            return;
        }
        if let Some(binding) = shortcuts::binding_from_keystroke(keystroke) {
            self.apply(id, &binding, cx);
        }
    }

    fn apply(&mut self, id: &str, binding: &Binding, cx: &mut Context<Self>) {
        if let Some(conflict) = shortcuts::find_conflict(id, binding, cx) {
            self.error = Some(t_with(cx, "shortcuts.conflict", &[("label", &conflict.label(cx))]));
            cx.notify();
            return;
        }
        shortcuts::set_binding(id, binding, cx);
        self.stop(cx);
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        self.recording = None;
        self.error = None;
        self.interceptor = None;
        cx.notify();
    }

    pub fn is_recording(&self) -> bool {
        self.recording.is_some()
    }

    fn row(&self, def: &'static ShortcutDef, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        let binding = shortcuts::effective(def, cx);
        let is_default = shortcuts::format(&binding) == shortcuts::format(&def.default_binding());
        let recording = self.recording == Some(def.id);
        h_flex()
            .py(rems(0.462))
            .gap(rems(0.615))
            .child(div().flex_1().min_w_0().child(def.label(cx)))
            .when(!is_default, |el| {
                el.child(
                    Button::new(SharedString::from(format!("reset-{}", def.id)))
                        .small()
                        .label(t(cx, "shortcuts.reset"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            shortcuts::reset_binding(def.id, cx);
                            this.error = None;
                            cx.notify();
                        })),
                )
            })
            .child(
                div()
                    .id(SharedString::from(format!("keys-{}", def.id)))
                    .flex_none()
                    .min_w(rems(11.))
                    .px(rems(0.769))
                    .py(rems(0.308))
                    .rounded(RADIUS_SM)
                    .border_1()
                    .border_color(if recording { theme.primary } else { theme.border })
                    .when(recording, |el| {
                        el.shadow(vec![BoxShadow {
                            color: theme.primary,
                            offset: point(px(0.), px(0.)),
                            blur_radius: px(0.),
                            spread_radius: px(1.),
                            inset: false,
                        }])
                    })
                    .bg(theme.background)
                    .font_family(theme.mono_font_family.clone())
                    .text_size(TEXT_SM)
                    .text_center()
                    .cursor_pointer()
                    .child(shortcuts::format(&binding))
                    .on_click(cx.listener(move |this, _, _, cx| this.record(def.id, cx))),
            )
    }
}

impl Render for ShortcutsDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let muted = theme.muted_foreground;
        let mut sections: Vec<(Category, Vec<&'static ShortcutDef>)> = Vec::new();
        for def in &SHORTCUTS {
            match sections.iter_mut().find(|(category, _)| *category == def.category) {
                Some((_, defs)) => defs.push(def),
                None => sections.push((def.category, vec![def])),
            }
        }
        let sections: Vec<AnyElement> = sections
            .into_iter()
            .map(|(category, defs)| {
                let rows: Vec<_> = defs.into_iter().map(|def| self.row(def, cx)).collect();
                v_flex()
                    .mt(rems(1.231))
                    .child(
                        form::caption(t(cx, &format!("shortcuts.categories.{}", category.key())), cx).mb(rems(0.615)),
                    )
                    .children(rows)
                    .into_any_element()
            })
            .collect();
        let body = modal::body()
            .child(modal::description(t(cx, "shortcuts.intro"), cx))
            .when_some(self.recording.and_then(shortcuts::find), |el, def| {
                el.child(
                    div()
                        .px(rems(0.769))
                        .py(rems(0.615))
                        .rounded(RADIUS)
                        .bg(theme.primary.opacity(TINT))
                        .border_1()
                        .border_color(theme.primary.opacity(TINT_BORDER))
                        .text_size(TEXT_SM)
                        .child(t_with(cx, "shortcuts.recording", &[("label", &def.label(cx))])),
                )
            })
            .when_some(self.error.clone(), |el, error| el.child(form::error(error, cx)))
            .children(sections)
            .child(div().text_size(TEXT_SM).text_color(muted).child(t(cx, "shortcuts.note")));
        modal::scroll_body(&self.scroll, body)
    }
}

pub fn open(window: &mut Window, cx: &mut App) -> Entity<ShortcutsDialog> {
    let view = cx.new(|_| ShortcutsDialog::new());
    let dialog = view.clone();
    window.open_dialog(cx, move |modal, window, cx| {
        let recording = dialog.read(cx).is_recording();
        let reset = dialog.downgrade();
        let content = modal::scroll_content().child(dialog.clone()).child(
            modal::footer(cx)
                .child(Button::new("shortcuts-reset-all").label(t(cx, "shortcuts.resetAll")).on_click(
                    move |_, _, cx| {
                        shortcuts::reset_all(cx);
                        let _ = reset.update(cx, |dialog, cx| {
                            dialog.error = None;
                            cx.notify();
                        });
                    },
                ))
                .child(
                    Button::new("shortcuts-close")
                        .primary()
                        .label(t(cx, "common.close"))
                        .on_click(|_, window, cx| window.close_dialog(cx)),
                ),
        );
        Modal::new("shortcuts", t(cx, "shortcuts.title")).build(modal, content, window, cx).keyboard(!recording)
    });
    view
}

#[cfg(test)]
mod tests;
