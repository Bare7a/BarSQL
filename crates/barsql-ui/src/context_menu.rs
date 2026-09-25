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

// Windows' native menu ignores the app theme and Linux has none, so outside macOS, and always in tests, the menu
// is drawn here and GPUI Kit gets an empty native menu, which it doesn't show.
pub fn open(
    native: NativeMenu,
    entries: Vec<Entry>,
    focus: &FocusHandle,
    window: &mut Window,
    cx: &mut App,
) -> NativeMenu {
    if cfg!(target_os = "macos") && !cfg!(test) {
        return entries.into_iter().fold(native, |menu, entry| match entry {
            Entry::Item { label, action, disabled } => menu.menu_with_disabled(label, disabled, action),
            Entry::Separator => menu.separator(),
        });
    }
    if let Some(overlay) = overlay(window, cx) {
        let (focus, position) = (focus.clone(), window.mouse_position());
        overlay.update(cx, |overlay, cx| overlay.show(entries, focus, position, window, cx));
    }
    native
}

pub fn input(state: &Entity<InputState>) -> impl Fn(NativeMenu, &mut Window, &mut App) -> NativeMenu + 'static {
    let state = state.clone();
    text(move |cx| {
        let input = state.read(cx);
        (!input.selected_range().is_empty(), input.is_editable(), input.focus_handle(cx))
    })
}

pub fn editor(state: &Entity<EditorState>) -> impl Fn(NativeMenu, &mut Window, &mut App) -> NativeMenu + 'static {
    let state = state.clone();
    text(move |cx| {
        let editor = state.read(cx);
        (!editor.selected_range().is_empty(), editor.is_editable(), editor.focus_handle(cx))
    })
}

// `read` returns whether text is selected, whether it's editable, and where to send the actions.
fn text(
    read: impl Fn(&App) -> (bool, bool, FocusHandle) + 'static,
) -> impl Fn(NativeMenu, &mut Window, &mut App) -> NativeMenu + 'static {
    move |native, window, cx| {
        let (selected, editable, focus) = read(cx);
        let pastable = editable && cx.read_from_clipboard().is_some();
        let entries = vec![
            Entry::item(t(cx, "editor.contextCut"), Cut, !(editable && selected)),
            Entry::item(t(cx, "editor.contextCopy"), Copy, !selected),
            Entry::item(t(cx, "editor.contextPaste"), Paste, !pastable),
            Entry::Separator,
            Entry::item(t(cx, "editor.contextSelectAll"), SelectAll, false),
        ];
        open(native, entries, &focus, window, cx)
    }
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
