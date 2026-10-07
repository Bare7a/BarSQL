use std::rc::Rc;

use barsql_io::{ArrayKind, array_json, array_kind};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Editor, EditorState};
use gpui_kit::component::menu::{DropdownMenu, PopupMenu, PopupMenuItem};
use gpui_kit::component::{Disableable, Icon, Sizable, WindowExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::cell_content::{self, Kind, Mode, SELECTABLE};
use crate::form;
use crate::i18n::t;
use crate::modal::{self, Modal};
use crate::scrollbars::ScrollbarsOnHover as _;
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
    type_name: String,
    // Set for a column of arrays, maps or tuples, whose cells open as JSON.
    array: Option<ArrayKind>,
    // The cell's own text, which every kind but Array shows.
    raw: String,
}

// Array's label names what the column holds.
fn label_key(kind: Kind, array: Option<ArrayKind>) -> &'static str {
    match (kind, array) {
        (Kind::Array, Some(ArrayKind::Map)) => "cellViewer.kindMap",
        (Kind::Array, Some(ArrayKind::Tuple)) => "cellViewer.kindTuple",
        _ => kind.label_key(),
    }
}

fn line_count(text: &str) -> usize {
    text.matches('\n').count() + 1
}

impl CellViewer {
    fn new(
        value: Option<String>,
        type_name: String,
        on_save: Option<OnSave>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let null = value.is_none();
        let raw = value.unwrap_or_default();
        let json = (!null).then(|| array_json(&raw, &type_name)).flatten();
        let array = json.as_ref().and_then(|_| array_kind(&type_name));
        let (text, kind) = match json {
            Some(json) => (json, Kind::Array),
            None => cell_content::initial(&raw, null),
        };
        let lines = line_count(&text);
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .language(kind.language())
                .line_number(true)
                .soft_wrap(true)
                .folding(kind.foldable())
                .default_value(text)
        });
        Self { editor, kind, null, lines, error: None, on_save, type_name, array, raw }
    }

    fn editable(&self) -> bool {
        self.on_save.is_some() && !self.null
    }

    // The Editor element sets this on its state as it renders, so it's decided here rather than on the state.
    fn readonly(&self) -> bool {
        !self.editable() || self.kind == Kind::Array
    }

    // A blank editor saves NULL, as the inline editor does.
    fn save(&mut self, value: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(on_save) = self.on_save.clone() {
            window.close_dialog(cx);
            on_save(value.filter(|text| !text.is_empty()), window, cx);
        }
    }

    // Array shows the text as JSON, read-only. Leaving it brings back the cell's own text, with any edits.
    fn set_kind(&mut self, kind: Kind, window: &mut Window, cx: &mut Context<Self>) {
        if kind == Kind::Array && self.kind != Kind::Array {
            let current = self.editor.read(cx).value().to_string();
            let Some(json) = array_json(&current, &self.type_name) else {
                self.error = Some(t(cx, "cellViewer.arrayError"));
                cx.notify();
                return;
            };
            self.raw = current;
            self.show(json, window, cx);
        } else if self.kind == Kind::Array && kind != Kind::Array {
            self.show(self.raw.clone(), window, cx);
        }
        self.kind = kind;
        self.error = None;
        self.editor.update(cx, |state, cx| state.set_highlighter(kind.language(), cx));
        cx.notify();
    }

    fn show(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        self.lines = line_count(&text);
        self.editor.update(cx, |state, cx| state.set_value(text, window, cx));
    }

    fn format(&mut self, mode: Mode, window: &mut Window, cx: &mut Context<Self>) {
        let content = self.editor.read(cx).value();
        match cell_content::apply(&content, self.kind, mode) {
            Some((text, kind)) => {
                self.lines = line_count(&text);
                self.editor.update(cx, |state, cx| state.set_value(text, window, cx));
                self.set_kind(kind, window, cx);
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
        let array = self.array;
        let kinds = (!self.null).then(|| {
            form::select_small("cell-viewer-kind", t(cx, label_key(current, array)).to_uppercase(), cx)
                .debug_selector(|| "cell-viewer-kind".into())
                .tooltip(t(cx, "tooltip.cellViewerKind"))
                .dropdown_menu(move |mut menu: PopupMenu, _, cx| {
                    for kind in array.map(|_| Kind::Array).into_iter().chain(SELECTABLE) {
                        let this = this.clone();
                        let label = t(cx, label_key(kind, array)).to_uppercase();
                        menu = menu.item(PopupMenuItem::new(label).checked(kind == current).on_click(
                            move |_, window, cx| {
                                this.update(cx, |viewer, cx| viewer.set_kind(kind, window, cx)).ok();
                            },
                        ));
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
                el.child(
                    Button::new("cell-viewer-save")
                        .debug_selector(|| "cell-viewer-save".into())
                        .small()
                        .primary()
                        .label(t(cx, "common.save"))
                        .disabled(self.kind == Kind::Array)
                        .on_click(cx.listener(|viewer, _, window, cx| {
                            let value = viewer.editor.read(cx).value().to_string();
                            viewer.save(Some(value), window, cx);
                        })),
                )
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
                            .readonly(self.readonly())
                            .context_menu(crate::context_menu::editor(&self.editor))
                            .size_full()
                            .bordered(false)
                            .text_size(font_size)
                            .scrollbars_on_hover(),
                    ),
            )
            .when_some(self.error.clone(), |el, error| {
                el.child(div().px(rems(1.231)).pb(rems(0.615)).child(form::error(error, cx)))
            })
            .when(self.kind == Kind::Array && self.editable(), |el| {
                el.child(div().px(rems(1.231)).pb(rems(0.615)).child(form::hint(t(cx, "cellViewer.arrayReadOnly"), cx)))
            })
            .child(self.footer(cx))
    }
}

// `type_name` is the column's, which says whether the cell is an array, map or tuple.
pub fn open(column: String, type_name: String, value: Option<String>, window: &mut Window, cx: &mut App) {
    show(column, type_name, value, None, window, cx);
}

pub fn open_editor(
    column: String,
    type_name: String,
    value: Option<String>,
    on_save: OnSave,
    window: &mut Window,
    cx: &mut App,
) {
    show(column, type_name, value, Some(on_save), window, cx);
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

    pub(crate) fn shown(&self, cx: &App) -> (Kind, String) {
        (self.kind, self.editor.read(cx).value().to_string())
    }
}

fn show(
    column: String,
    type_name: String,
    value: Option<String>,
    on_save: Option<OnSave>,
    window: &mut Window,
    cx: &mut App,
) {
    let viewer = cx.new(|cx| CellViewer::new(value, type_name, on_save, window, cx));
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

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use gpui_kit::component::{Root, WindowExt as _};
    use gpui_kit::{
        AppContext as _, Context, Entity, IntoElement, Render, TestAppContext, VisualTestContext, Window, div,
    };

    use super::{CellViewer, OnSave, Opened, label_key, open, open_editor};
    use crate::cell_content::Kind;
    use crate::test_support::Env;

    struct Host;

    impl Render for Host {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
        }
    }

    fn window(cx: &mut TestAppContext) -> &mut VisualTestContext {
        let window = cx.add_window(|window, cx| Root::new(cx.new(|_| Host), window, cx));
        VisualTestContext::from_window(window.into(), cx).into_mut()
    }

    fn opened(cx: &mut VisualTestContext) -> Entity<CellViewer> {
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.update(|_, cx| cx.try_global::<Opened>().and_then(|opened| opened.0.upgrade())).expect("a viewer")
    }

    fn shown(viewer: &Entity<CellViewer>, cx: &mut VisualTestContext) -> (Kind, String) {
        viewer.read_with(cx, |viewer, cx| viewer.shown(cx))
    }

    fn set_kind(viewer: &Entity<CellViewer>, kind: Kind, cx: &mut VisualTestContext) {
        viewer.update_in(cx, |viewer, window, cx| viewer.set_kind(kind, window, cx));
        cx.run_until_parked();
    }

    fn set_text(viewer: &Entity<CellViewer>, text: &str, cx: &mut VisualTestContext) {
        let text = text.to_string();
        viewer.update_in(cx, |viewer, window, cx| {
            viewer.editor.update(cx, |state, cx| state.set_value(text, window, cx))
        });
    }

    #[gpui_kit::test]
    fn an_array_opens_as_json_and_text_shows_its_literal(cx: &mut TestAppContext) {
        let _env = Env::new(cx);
        let cx = window(cx);
        cx.update(|window, cx| open("tags".into(), "_TEXT".into(), Some(r#"{a,"b c"}"#.into()), window, cx));
        let viewer = opened(cx);
        assert_eq!(shown(&viewer, cx), (Kind::Array, "[\n  \"a\",\n  \"b c\"\n]".into()));
        set_kind(&viewer, Kind::Text, cx);
        assert_eq!(shown(&viewer, cx), (Kind::Text, r#"{a,"b c"}"#.into()));
        set_kind(&viewer, Kind::Array, cx);
        assert_eq!(shown(&viewer, cx).0, Kind::Array);
    }

    #[gpui_kit::test]
    fn an_editable_array_is_edited_as_its_literal(cx: &mut TestAppContext) {
        let _env = Env::new(cx);
        let cx = window(cx);
        let saved = Rc::new(RefCell::new(None));
        let on_save: OnSave = {
            let saved = saved.clone();
            Rc::new(move |value, _, _| *saved.borrow_mut() = Some(value))
        };
        cx.update(|window, cx| open_editor("ids".into(), "_INT4".into(), Some("{1,2}".into()), on_save, window, cx));
        let viewer = opened(cx);
        let json = "[\n  1,\n  2\n]".to_string();
        cx.simulate_input("9");
        assert_eq!(shown(&viewer, cx), (Kind::Array, json), "the JSON can't be typed into");
        set_kind(&viewer, Kind::Text, cx);
        cx.simulate_input("9");
        assert!(shown(&viewer, cx).1.contains('9'), "the literal can");
        set_text(&viewer, "{1,2,3}", cx);
        set_kind(&viewer, Kind::Array, cx);
        assert_eq!(shown(&viewer, cx), (Kind::Array, "[\n  1,\n  2,\n  3\n]".into()), "edits show");
        set_kind(&viewer, Kind::Text, cx);
        assert_eq!(shown(&viewer, cx), (Kind::Text, "{1,2,3}".into()), "and are kept");
        set_text(&viewer, "{1,", cx);
        set_kind(&viewer, Kind::Array, cx);
        assert_eq!(shown(&viewer, cx).0, Kind::Text, "a broken literal stays text");
        assert!(viewer.read_with(cx, |viewer, _| viewer.error.is_some()));
        viewer.update_in(cx, |viewer, window, cx| {
            let value = viewer.editor.read(cx).value().to_string();
            viewer.save(Some(value), window, cx)
        });
        assert_eq!(*saved.borrow(), Some(Some("{1,".to_string())));
    }

    #[gpui_kit::test]
    fn maps_say_so_and_other_text_stays_text(cx: &mut TestAppContext) {
        let _env = Env::new(cx);
        let cx = window(cx);
        cx.update(|window, cx| open("attrs".into(), "Map(String, UInt8)".into(), Some("{'k':1}".into()), window, cx));
        let viewer = opened(cx);
        assert_eq!(viewer.read_with(cx, |viewer, _| label_key(viewer.kind, viewer.array)), "cellViewer.kindMap");
        cx.update(|window, cx| window.close_dialog(cx));
        cx.update(|window, cx| open("note".into(), "TEXT".into(), Some("{a,b}".into()), window, cx));
        let viewer = opened(cx);
        assert_eq!(shown(&viewer, cx), (Kind::Text, "{a,b}".into()));
        assert_eq!(viewer.read_with(cx, |viewer, _| viewer.array), None);
        cx.simulate_input("9");
        assert_eq!(shown(&viewer, cx).1, "{a,b}", "a results cell is only viewed");
    }
}
