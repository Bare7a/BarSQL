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
// GPUI Kit asks for the menu while it is updating the input, so `build` runs once that's done and may read the input.
// `hold` gets the open menu's focus, which keeps the input's selection showing and tells a cell editor that the
// blur it sees is its own menu's.
pub fn open(
    native: NativeMenu,
    window: &mut Window,
    cx: &mut App,
    build: impl FnOnce(&mut Window, &mut App) -> Option<(Vec<Entry>, FocusHandle)> + 'static,
    hold: impl FnOnce(FocusHandle, &mut App) + 'static,
) -> NativeMenu {
    let position = window.mouse_position();
    window.defer(cx, move |window, cx| {
        let Some((entries, focus)) = build(window, cx) else { return };
        if let Some(overlay) = overlay(window, cx) {
            overlay.update(cx, |overlay, cx| overlay.show(entries, focus, position, hold, window, cx));
        }
    });
    native
}

pub fn input(state: &Entity<InputState>) -> impl Fn(NativeMenu, &mut Window, &mut App) -> NativeMenu + 'static {
    let state = state.clone();
    move |native, window, cx| {
        let (read, hold) = (state.clone(), state.clone());
        open(
            native,
            window,
            cx,
            move |_, cx| {
                let input = read.read(cx);
                Some((text_entries(input.is_copyable(), input.is_editable(), cx), input.focus_handle(cx)))
            },
            move |menu, cx| hold.update(cx, |input, cx| input.set_selection_focus(Some(menu), cx)),
        )
    }
}

pub fn editor(state: &Entity<EditorState>) -> impl Fn(NativeMenu, &mut Window, &mut App) -> NativeMenu + 'static {
    let state = state.clone();
    move |native, window, cx| {
        let (read, hold) = (state.clone(), state.clone());
        open(
            native,
            window,
            cx,
            move |_, cx| {
                let editor = read.read(cx);
                Some((text_entries(editor.is_copyable(), editor.is_editable(), cx), editor.focus_handle(cx)))
            },
            move |menu, cx| hold.update(cx, |editor, cx| editor.set_selection_focus(Some(menu), cx)),
        )
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
        hold: impl FnOnce(FocusHandle, &mut App),
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
        // Before the menu takes the focus, so the input never sees an unexplained blur.
        hold(menu.focus_handle(cx), cx);
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
