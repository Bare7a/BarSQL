use gpui_kit::component::Root;
use gpui_kit::component::input::{Copy, Cut, EditorState, InputState, Paste, SelectAll};
use gpui_kit::component::menu::PopupMenu;
use gpui_kit::component::native_menu::NativeMenu;
use gpui_kit::*;

use crate::i18n::t;
use crate::workspace::Workspace;

// Same as gpui_base::POPUP_PRIORITY.
const PRIORITY: usize = 100;

pub enum Entry {
    Item { label: SharedString, action: Box<dyn Action>, disabled: bool },
    Separator,
}

impl Entry {
    pub fn item(label: impl Into<SharedString>, action: impl Action, disabled: bool) -> Self {
        Self::Item { label: label.into(), action: Box::new(action), disabled }
    }
}

// Every text menu is drawn here, as the app's other menus are, so they look alike on every platform and show their
// shortcuts. GPUI Kit gets an empty native menu back, which it doesn't show.
//
// GPUI Kit asks for the menu once it has let go of the input, so the entries can come from reading it. The menu takes
// the focus at once, and GPUI Kit ties it to the input: the selection keeps showing, and a cell editor can tell that
// the blur it sees is its own menu's.
pub fn open(
    native: NativeMenu,
    entries: Vec<Entry>,
    focus: FocusHandle,
    window: &mut Window,
    cx: &mut App,
) -> NativeMenu {
    if let Some(overlay) = overlay(window, cx) {
        let position = window.mouse_position();
        overlay.update(cx, |overlay, cx| overlay.show(entries, focus, position, window, cx));
    }
    native
}

pub fn input(state: &Entity<InputState>) -> impl Fn(NativeMenu, &mut Window, &mut App) -> NativeMenu + 'static {
    let state = state.clone();
    move |native, window, cx| {
        let input = state.read(cx);
        let entries = text_entries(input.is_copyable(), input.is_editable(), cx);
        let focus = input.focus_handle(cx);
        open(native, entries, focus, window, cx)
    }
}

pub fn editor(state: &Entity<EditorState>) -> impl Fn(NativeMenu, &mut Window, &mut App) -> NativeMenu + 'static {
    let state = state.clone();
    move |native, window, cx| {
        let editor = state.read(cx);
        let entries = text_entries(editor.is_copyable(), editor.is_editable(), cx);
        let focus = editor.focus_handle(cx);
        open(native, entries, focus, window, cx)
    }
}

// Cut and Copy need a selection that isn't masked, Paste something to paste. A read-only field only copies.
fn text_entries(copyable: bool, editable: bool, cx: &App) -> Vec<Entry> {
    let copy = Entry::item(t(cx, "editor.contextCopy"), Copy, !copyable);
    let select_all = Entry::item(t(cx, "editor.contextSelectAll"), SelectAll, false);
    if !editable {
        return vec![copy, Entry::Separator, select_all];
    }
    let pastable = cx.read_from_clipboard().is_some();
    vec![
        Entry::item(t(cx, "editor.contextCut"), Cut, !copyable),
        copy,
        Entry::item(t(cx, "editor.contextPaste"), Paste, !pastable),
        Entry::Separator,
        select_all,
    ]
}

fn overlay(window: &Window, cx: &App) -> Option<Entity<Overlay>> {
    let root = window.root::<Root>()??;
    let workspace = root.read(cx).view().clone().downcast::<Workspace>().ok()?;
    Some(workspace.read(cx).context_menu())
}

pub struct Overlay {
    active: Option<Active>,
}

struct Active {
    menu: Entity<PopupMenu>,
    position: Point<Pixels>,
    _dismiss: Subscription,
}

impl Overlay {
    pub fn new() -> Self {
        Self { active: None }
    }

    fn show(
        &mut self,
        entries: Vec<Entry>,
        focus: FocusHandle,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let menu = PopupMenu::build(window, cx, move |menu, _, _| {
            entries.into_iter().fold(menu.action_context(focus), |menu, entry| match entry {
                Entry::Item { label, action, disabled } => menu.menu_with_disabled(label, action, disabled),
                Entry::Separator => menu.separator(),
            })
        });
        let dismiss = cx.subscribe(&menu, |overlay, _, _: &DismissEvent, cx| {
            overlay.active = None;
            cx.notify();
        });
        menu.focus_handle(cx).focus(window, cx);
        self.active = Some(Active { menu, position, _dismiss: dismiss });
        cx.notify();
    }
}

impl Render for Overlay {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().children(self.active.as_ref().map(|active| {
            deferred(anchored().position(active.position).snap_to_window_with_margin(px(8.)).child(active.menu.clone()))
                .with_priority(PRIORITY)
        }))
    }
}
