use std::ops::Range;

use gpui_kit::component::input::{Enter, GoToDefinition, InputEvent, InputState, MoveDown, MoveUp};
use gpui_kit::component::kbd::Kbd;
use gpui_kit::component::scroll::ScrollbarAxis;
use gpui_kit::component::{ActiveTheme, WindowExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::actions::*;
use crate::editor_commands::{SelectAllOccurrences, ToggleComment};
use crate::form;
use crate::fuzzy::rank_candidate;
use crate::i18n::t;
use crate::scrollbars::HoverScrollbar as _;
use crate::tokens::{RADIUS_LG, RADIUS_SM, TEXT_BASE, TEXT_XS};

pub struct Command {
    pub label: String,
    pub action: Box<dyn Action>,
    // As the keymap has it where the palette was opened.
    pub shortcut: Option<String>,
}

// The menus' actions and the editor's, in menu order, labelled as the menus and the shortcuts list label them.
fn catalog(cx: &App) -> Vec<(String, Box<dyn Action>)> {
    let item = |key: &str| t(cx, key).to_string();
    let theme = |key: &str| format!("{}: {}", t(cx, "viewSections.theme"), t(cx, key));
    let language = |key: &str| format!("{}: {}", t(cx, "viewSections.language"), t(cx, key));
    vec![
        (item("shortcuts.items.runSelection"), Box::new(RunSelection)),
        (item("shortcuts.items.runAll"), Box::new(RunAll)),
        (item("shortcuts.items.explainQuery"), Box::new(ExplainQuery)),
        (item("shortcuts.items.explainAnalyze"), Box::new(ExplainAnalyze)),
        (item("editor.contextFormat"), Box::new(FormatQuery)),
        (item("editor.contextToggleComment"), Box::new(ToggleComment)),
        (item("editor.contextGoToDefinition"), Box::new(GoToDefinition)),
        (item("editor.contextChangeAll"), Box::new(SelectAllOccurrences)),
        (item("shortcuts.items.saveQuery"), Box::new(SaveQuery)),
        (item("shortcuts.items.renameSavedQuery"), Box::new(RenameSavedQuery)),
        (item("editor.beginTxn"), Box::new(BeginTransaction)),
        (item("commandPalette.commitTxn"), Box::new(CommitTransaction)),
        (item("commandPalette.rollbackTxn"), Box::new(RollbackTransaction)),
        (item("shortcuts.items.newTab"), Box::new(NewTab)),
        (item("shortcuts.items.closeTab"), Box::new(CloseTab)),
        (item("shortcuts.items.reopenClosedTab"), Box::new(ReopenClosedTab)),
        (item("shortcuts.items.nextTab"), Box::new(NextTab)),
        (item("shortcuts.items.prevTab"), Box::new(PrevTab)),
        (item("shortcuts.items.quickSearch"), Box::new(QuickSearch)),
        (item("shortcuts.items.toggleSidebar"), Box::new(ToggleSidebar)),
        (item("shortcuts.items.toggleJsonPanel"), Box::new(ToggleJsonPanel)),
        (item("menu.stripedRows"), Box::new(ToggleGridStripes)),
        (item("shortcuts.items.toggleFullscreen"), Box::new(ToggleFullscreen)),
        (item("shortcuts.items.zoomIn"), Box::new(ZoomIn)),
        (item("shortcuts.items.zoomOut"), Box::new(ZoomOut)),
        (item("shortcuts.items.resetZoom"), Box::new(ResetZoom)),
        (item("shortcuts.items.increaseEditorFontSize"), Box::new(IncreaseEditorFontSize)),
        (item("shortcuts.items.decreaseEditorFontSize"), Box::new(DecreaseEditorFontSize)),
        (item("editor.resetFontSize"), Box::new(ResetEditorFontSize)),
        (theme("theme.dark"), Box::new(ThemeDark)),
        (theme("theme.light"), Box::new(ThemeLight)),
        (theme("theme.system"), Box::new(ThemeSystem)),
        (language("language.en"), Box::new(LanguageEn)),
        (language("language.de"), Box::new(LanguageDe)),
        (language("language.bg"), Box::new(LanguageBg)),
        (item("menu.keyboardShortcuts"), Box::new(KeyboardShortcuts)),
        (item("menu.keyboardTips"), Box::new(KeyboardTips)),
        (item("menu.about"), Box::new(About)),
    ]
}

// What `focus` can run: an editor command only shows from the editor, Commit only while a transaction is open.
pub fn commands(focus: &FocusHandle, window: &Window, cx: &App) -> Vec<Command> {
    catalog(cx)
        .into_iter()
        .filter(|(_, action)| window.is_action_available_in(action.as_ref(), focus))
        .map(|(label, action)| {
            let shortcut = window.highest_precedence_binding_for_action_in(action.as_ref(), focus).map(|binding| {
                binding.keystrokes().iter().map(|key| Kbd::format(key.as_keystroke())).collect::<Vec<_>>().join(" ")
            });
            Command { label, action, shortcut }
        })
        .collect()
}

// Indices into `commands` with the matched character ranges, best first. An empty query keeps menu order.
pub fn rank(query: &str, commands: &[Command]) -> Vec<(usize, Vec<Range<usize>>)> {
    let query = query.trim().to_lowercase();
    let mut matches: Vec<(usize, f64, Vec<Range<usize>>)> = commands
        .iter()
        .enumerate()
        .filter_map(|(ix, command)| rank_candidate(&query, &command.label, &[]).map(|m| (ix, m.score, m.ranges)))
        .collect();
    // Stable, so equal scores keep menu order.
    matches.sort_by(|a, b| b.1.total_cmp(&a.1));
    matches.into_iter().map(|(ix, _, ranges)| (ix, ranges)).collect()
}

// Converts character ranges to byte ranges.
fn byte_ranges(text: &str, ranges: &[Range<usize>]) -> Vec<Range<usize>> {
    let offsets: Vec<usize> = text.char_indices().map(|(at, _)| at).chain([text.len()]).collect();
    ranges.iter().filter_map(|range| Some(*offsets.get(range.start)?..*offsets.get(range.end)?)).collect()
}

pub struct CommandPalette {
    input: Entity<InputState>,
    commands: Vec<Command>,
    items: Vec<(usize, Vec<Range<usize>>)>,
    active: usize,
    scroll: ScrollHandle,
    _subscription: Subscription,
}

impl CommandPalette {
    fn new(commands: Vec<Command>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(t(cx, "commandPalette.placeholder")));
        let subscription = cx.subscribe(&input, |this, input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                let query = input.read(cx).value().to_string();
                this.set_query(&query, cx);
            }
        });
        let items = rank("", &commands);
        Self { input, commands, items, active: 0, scroll: ScrollHandle::new(), _subscription: subscription }
    }

    fn set_query(&mut self, query: &str, cx: &mut Context<Self>) {
        self.items = rank(query, &self.commands);
        self.active = 0;
        self.scroll.scroll_to_item(0);
        cx.notify();
    }

    #[cfg(test)]
    pub fn labels(&self) -> Vec<String> {
        self.items.iter().map(|(ix, _)| self.commands[*ix].label.clone()).collect()
    }

    fn active_ix(&self) -> usize {
        self.active.min(self.items.len().saturating_sub(1))
    }

    fn step(&mut self, down: bool, cx: &mut Context<Self>) {
        if self.items.is_empty() {
            return;
        }
        let current = self.active_ix();
        self.active = if down { (current + 1).min(self.items.len() - 1) } else { current.saturating_sub(1) };
        self.scroll.scroll_to_item(self.active);
        cx.notify();
    }

    // Closing gives focus back to where the palette was opened from, which then takes the action.
    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((ix, _)) = self.items.get(self.active_ix()) else { return };
        let action = self.commands[*ix].action.boxed_clone();
        window.close_dialog(cx);
        window.dispatch_action(action, cx);
    }

    fn row(&self, row: usize, active: bool, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let (ix, ranges) = &self.items[row];
        let command = &self.commands[*ix];
        let highlight =
            HighlightStyle { color: Some(theme.foreground.blend(theme.primary.opacity(0.7))), ..Default::default() };
        let label = StyledText::new(command.label.clone())
            .with_highlights(byte_ranges(&command.label, ranges).into_iter().map(|range| (range, highlight)));
        h_flex()
            .id(("command-palette-item", row))
            .debug_selector(move || format!("command-palette-item-{row}"))
            .gap(rems(0.923))
            .px(rems(0.923))
            .py(rems(0.462))
            .cursor_pointer()
            .when(active, |el| el.bg(theme.accent))
            .hover(|style| style.bg(theme.accent))
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                if *hovered && this.active != row {
                    this.active = row;
                    cx.notify();
                }
            }))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.active = row;
                this.confirm(window, cx);
            }))
            .child(div().flex_1().min_w_0().truncate().text_size(TEXT_BASE).child(label))
            .when_some(command.shortcut.clone(), |el, shortcut| {
                el.child(
                    div()
                        .flex_none()
                        .px(rems(0.462))
                        .rounded(RADIUS_SM)
                        .border_1()
                        .border_color(theme.border)
                        .text_size(TEXT_XS)
                        .text_color(theme.muted_foreground)
                        .child(shortcut),
                )
            })
    }
}

impl Render for CommandPalette {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let rem = window.rem_size();
        let max_height = (rem * 32.308).min(window.viewport_size().height - rem * 16.923).max(px(0.));
        let active = self.active_ix();
        let list = if self.items.is_empty() {
            div()
                .px(rems(0.923))
                .py(rems(1.077))
                .text_color(theme.muted_foreground)
                .child(t(cx, "quickSearch.noResults"))
                .into_any_element()
        } else {
            let rows: Vec<_> = (0..self.items.len()).map(|row| self.row(row, row == active, cx)).collect();
            let list = v_flex()
                .id("command-palette-list")
                .max_h(max_height)
                .overflow_y_scroll()
                .track_scroll(&self.scroll)
                .children(rows);
            div().relative().child(list).hover_scrollbar(&self.scroll, ScrollbarAxis::Vertical).into_any_element()
        };
        v_flex()
            .key_context("CommandPalette")
            .capture_action(cx.listener(|this, _: &MoveUp, _, cx| {
                cx.stop_propagation();
                this.step(false, cx);
            }))
            .capture_action(cx.listener(|this, _: &MoveDown, _, cx| {
                cx.stop_propagation();
                this.step(true, cx);
            }))
            .on_action(cx.listener(|this, _: &Enter, window, cx| this.confirm(window, cx)))
            .child(
                div()
                    .p(rems(0.769))
                    .border_b_1()
                    .border_color(theme.border)
                    .child(Styled::h(form::input(&self.input, window, cx), rems(2.462))),
            )
            .child(list)
    }
}

pub fn open(commands: Vec<Command>, window: &mut Window, cx: &mut App) -> Entity<CommandPalette> {
    let view = cx.new(|cx| CommandPalette::new(commands, window, cx));
    let dialog = view.clone();
    window.open_dialog(cx, move |modal, window, cx| {
        let rem = window.rem_size();
        modal
            .w(rem * 46.154)
            .margin_top(rem * 5.5)
            .p_0()
            .bg(cx.theme().sidebar)
            .rounded(RADIUS_LG)
            .close_button(false)
            .on_ok(|_, _, _| false)
            .child(dialog.clone())
    });
    let input = view.read(cx).input.clone();
    window.defer(cx, move |window, cx| input.update(cx, |state, cx| state.focus(window, cx)));
    view
}

#[cfg(test)]
mod tests {
    use super::{Command, rank};
    use crate::actions::NewTab;

    fn command(label: &str) -> Command {
        Command { label: label.into(), action: Box::new(NewTab), shortcut: None }
    }

    #[test]
    fn an_empty_query_keeps_menu_order_and_typing_ranks_by_match() {
        let commands = [command("Run all"), command("Toggle sidebar"), command("Zoom in")];
        let order = |query: &str| rank(query, &commands).into_iter().map(|(ix, _)| ix).collect::<Vec<_>>();
        assert_eq!(order(""), [0, 1, 2]);
        assert_eq!(order("zoom"), [2]);
        assert_eq!(order("tsb"), [1], "initials match");
        assert!(order("xyz").is_empty());
    }
}
