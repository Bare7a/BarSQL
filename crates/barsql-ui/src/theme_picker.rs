use std::rc::Rc;

use gpui_kit::component::{ActiveTheme as _, ThemeMode};
use gpui_kit::{App, Entity, Window};

use crate::actions::SetTheme;
use crate::command_palette::{self, Command, CommandPalette, Hooks};
use crate::i18n::t;
use crate::theme;

// Select theme…: every theme, dark then light, each shown while the cursor is on it.
pub fn open(window: &mut Window, cx: &mut App) -> Entity<CommandPalette> {
    let showing = cx.theme().theme_name().clone();
    let mut commands = Vec::new();
    for (mode, tag) in [(ThemeMode::Dark, t(cx, "theme.dark")), (ThemeMode::Light, t(cx, "theme.light"))] {
        for entry in theme::ordered(mode, cx) {
            let name = entry.config.name.clone();
            let label = name.to_string();
            let tag = Some(tag.to_string());
            commands.push(Command { label, action: Box::new(SetTheme { name }), shortcut: None, tag });
        }
    }
    let active = commands.iter().position(|command| command.label == showing.as_ref()).unwrap_or(0);
    let hooks = Hooks {
        on_cursor: Some(Rc::new(|command: &Command, window: &mut Window, cx: &mut App| {
            theme::preview(&command.label, window, cx)
        })),
        on_dismiss: Some(Rc::new(theme::end_preview)),
    };
    command_palette::open_with(commands, active, t(cx, "theme.search"), hooks, window, cx)
}
