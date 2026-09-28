use std::rc::Rc;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Editor, EditorState};
use gpui_kit::component::menu::{DropdownMenu, PopupMenu, PopupMenuItem};
use gpui_kit::component::{Icon, Sizable, WindowExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::cell_content::{self, Kind, Mode, SELECTABLE};
use crate::form;
use crate::i18n::t;
use crate::modal::{self, Modal};
use crate::theme;
use crate::toast;

// `None` sets the cell to NULL.
pub type OnSave = Rc<dyn Fn(Option<String>, &mut Window, &mut App)>;

// Only editable from a table view, and never for a NULL cell.
pub struct CellViewer {
    editor: Entity<EditorState>,
    kind: Kind,
    null: bool,
    lines: usize,
    error: Option<SharedString>,
    on_save: Option<OnSave>,
}

fn line_count(text: &str) -> usize {
    text.matches('\n').count() + 1
}

impl CellViewer {
    fn new(value: Option<String>, on_save: Option<OnSave>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let null = value.is_none();
        let editable = on_save.is_some() && !null;
        let (text, kind) = cell_content::initial(value.as_deref().unwrap_or_default(), null);
        let lines = line_count(&text);
        let editor = cx.new(|cx| {
            let mut state = EditorState::new(window, cx)
                .language(kind.language())
                .line_number(true)
                .soft_wrap(true)
                .folding(kind.structured())
                .default_value(text);
            state.set_readonly(!editable, cx);
            state
        });
        Self { editor, kind, null, lines, error: None, on_save }
    }

    fn editable(&self) -> bool {
        self.on_save.is_some() && !self.null
    }

    // A blank editor saves NULL, as the inline editor does.
    fn save(&mut self, value: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(on_save) = self.on_save.clone() {
            window.close_dialog(cx);
            on_save(value.filter(|text| !text.is_empty()), window, cx);
        }
    }

    fn set_kind(&mut self, kind: Kind, cx: &mut Context<Self>) {
        self.kind = kind;
        self.error = None;
        self.editor.update(cx, |state, cx| state.set_highlighter(kind.language(), cx));
        cx.notify();
    }

    fn format(&mut self, mode: Mode, window: &mut Window, cx: &mut Context<Self>) {
        let content = self.editor.read(cx).value();
        match cell_content::apply(&content, self.kind, mode) {
            Some((text, kind)) => {
                self.lines = line_count(&text);
                self.editor.update(cx, |state, cx| state.set_value(text, window, cx));
                self.set_kind(kind, cx);
            }
            None => {
                let key = if mode == Mode::Beautify { "cellViewer.beautifyError" } else { "cellViewer.minifyError" };
                self.error = Some(t(cx, key));
                cx.notify();
            }
        }
    }

    fn copy(&self, cx: &mut App) {
        cx.write_to_clipboard(ClipboardItem::new_string(self.editor.read(cx).value().to_string()));
        toast::success(t(cx, "toast.copiedClipboard"), cx);
    }

    fn hint(&self, cx: &App) -> SharedString {
        if self.null {
            return t(cx, Kind::Null.label_key());
        }
        let unit = if self.lines == 1 { t(cx, "cellViewer.line") } else { t(cx, "cellViewer.lines") };
        format!("{} {unit}", self.lines).into()
    }

    fn footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let this = cx.entity().downgrade();
        let current = self.kind.selectable();
        let structured = self.kind.structured();
        let kinds = (!self.null).then(|| {
            form::select_small("cell-viewer-kind", t(cx, current.label_key()).to_uppercase(), cx)
                .debug_selector(|| "cell-viewer-kind".into())
                .tooltip(t(cx, "tooltip.cellViewerKind"))
                .dropdown_menu(move |mut menu: PopupMenu, _, cx| {
                    for kind in SELECTABLE {
                        let this = this.clone();
                        let label = t(cx, kind.label_key()).to_uppercase();
                        menu =
                            menu.item(PopupMenuItem::new(label).checked(kind == current).on_click(move |_, _, cx| {
                                this.update(cx, |viewer, cx| viewer.set_kind(kind, cx)).ok();
                            }));
                    }
                    menu
                })
        });
        let format_button =
            |id: &'static str, icon: Lucide, key: &str, tooltip: &str, mode: Mode, cx: &mut Context<Self>| {
                Button::new(id)
                    .debug_selector(move || id.into())
                    .small()
                    .icon(Icon::new(icon))
                    .label(t(cx, key))
                    .tooltip(t(cx, tooltip))
                    .on_click(cx.listener(move |viewer, _, window, cx| viewer.format(mode, window, cx)))
            };
        let editable = self.editable();
        let left = h_flex()
            .flex_wrap()
            .gap(rems(0.615))
            .children(kinds)
            .when(structured, |el| {
                el.child(format_button(
                    "cell-viewer-beautify",
                    Lucide::TextAlignStart,
                    "cellViewer.beautify",
                    "tooltip.prettify",
                    Mode::Beautify,
                    cx,
                ))
                .child(format_button(
                    "cell-viewer-minify",
                    Lucide::Minimize2,
                    "cellViewer.minify",
                    "tooltip.minify",
                    Mode::Minify,
                    cx,
                ))
            })
            .child(
                Button::new("cell-viewer-copy")
                    .small()
                    .icon(Icon::new(Lucide::Copy))
                    .label(t(cx, "common.copy"))
                    .on_click(cx.listener(|viewer, _, _, cx| viewer.copy(cx))),
            );
        let right = h_flex()
            .flex_wrap()
            .gap(rems(0.615))
            .when(editable, |el| {
                el.child(
                    Button::new("cell-viewer-set-null")
                        .debug_selector(|| "cell-viewer-set-null".into())
                        .small()
                        .danger()
                        .outline()
                        .icon(Icon::new(Lucide::Ban))
                        .label(t(cx, "cellViewer.setNull"))
                        .on_click(cx.listener(|viewer, _, window, cx| viewer.save(None, window, cx))),
                )
            })
            .child(
                Button::new("cell-viewer-close")
                    .debug_selector(|| "cell-viewer-close".into())
                    .small()
                    .when(!editable, |button| button.primary())
                    .label(t(cx, if editable { "common.cancel" } else { "common.close" }))
                    .on_click(|_, window, cx| window.close_dialog(cx)),
            )
            .when(editable, |el| {
                el.child(Button::new("cell-viewer-save").small().primary().label(t(cx, "common.save")).on_click(
                    cx.listener(|viewer, _, window, cx| {
                        let value = viewer.editor.read(cx).value().to_string();
                        viewer.save(Some(value), window, cx);
                    }),
                ))
            });
        modal::footer(cx).flex_wrap().justify_between().child(left).child(right)
    }
}

impl Render for CellViewer {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let font_size = px(theme::editor_font_size(cx));
        v_flex()
            .flex_1()
            .min_h_0()
            .child(
                crate::editor_commands::handlers(div(), &self.editor, None)
                    .key_context(crate::editor_commands::CONTEXT)
                    .flex_1()
                    .min_h_0()
                    .child(
                        Editor::new(&self.editor)
                            .context_menu(crate::context_menu::editor(&self.editor))
                            .size_full()
                            .bordered(false)
                            .text_size(font_size),
                    ),
            )
            .when_some(self.error.clone(), |el, error| {
                el.child(div().px(rems(1.231)).pb(rems(0.615)).child(form::error(error, cx)))
            })
            .child(self.footer(cx))
    }
}

pub fn open(column: String, value: Option<String>, window: &mut Window, cx: &mut App) {
    show(column, value, None, window, cx);
}

pub fn open_editor(column: String, value: Option<String>, on_save: OnSave, window: &mut Window, cx: &mut App) {
    show(column, value, Some(on_save), window, cx);
}

// Last opened viewer, for scenarios to read.
#[cfg(test)]
pub(crate) struct Opened(pub WeakEntity<CellViewer>);

#[cfg(test)]
impl Global for Opened {}

#[cfg(test)]
impl CellViewer {
    pub(crate) fn lines(&self) -> usize {
        self.lines
    }
}

fn show(column: String, value: Option<String>, on_save: Option<OnSave>, window: &mut Window, cx: &mut App) {
    let viewer = cx.new(|cx| CellViewer::new(value, on_save, window, cx));
    #[cfg(test)]
    cx.set_global(Opened(viewer.downgrade()));
    let editor = viewer.read(cx).editor.clone();
    window.open_dialog(cx, move |dialog, window, cx| {
        let hint = viewer.read(cx).hint(cx);
        let height = (window.viewport_size().height * 0.78).min(window.rem_size() * 55.385);
        Modal::new("cell-viewer", column.clone()).extra(hint).size(modal::Size::Xl).height(height).build(
            dialog,
            viewer.clone(),
            window,
            cx,
        )
    });
    window.defer(cx, move |window, cx| editor.update(cx, |state, cx| state.focus(window, cx)));
}
