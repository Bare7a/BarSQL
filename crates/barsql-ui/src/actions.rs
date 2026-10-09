use gpui_kit::component::GlobalState;
use gpui_kit::component::input::{Copy, Cut, Paste, Redo, SelectAll, Undo};
use gpui_kit::component::kbd::Kbd;
use gpui_kit::component::menu::AppMenuBar;
use gpui_kit::{
    Action, App, AsKeystroke, Entity, Global, KeyBinding, Menu, MenuItem, OsAction, SystemMenuType, WeakEntity, Window,
    actions,
};

use crate::i18n::{I18n, t};
use crate::state::setting_bool;
use crate::theme::ThemeChoice;

actions!(
    barsql,
    [
        Quit,
        About,
        NewTab,
        CloseTab,
        ReopenClosedTab,
        QuickSearch,
        CommandPalette,
        ThemeDark,
        ThemeLight,
        ThemeSystem,
        LanguageEn,
        LanguageDe,
        LanguageBg,
        ZoomIn,
        ZoomOut,
        ResetZoom,
        IncreaseEditorFontSize,
        DecreaseEditorFontSize,
        ResetEditorFontSize,
        ToggleSidebar,
        ToggleJsonPanel,
        ToggleGridStripes,
        ToggleFullscreen,
        KeyboardTips,
        KeyboardShortcuts,
        RunSelection,
        RunAll,
        ExplainQuery,
        ExplainAnalyze,
        NextTab,
        PrevTab,
        FormatQuery,
        TriggerSuggest,
        SaveQuery,
        RenameSavedQuery,
        BeginTransaction,
        CommitTransaction,
        RollbackTransaction,
        HideApp,
        HideOtherApps,
        ShowAllApps,
        MinimizeWindow,
        ZoomWindow,
        BringAllToFront,
    ]
);

#[cfg(target_os = "macos")]
const MOD: &str = "cmd";
#[cfg(not(target_os = "macos"))]
const MOD: &str = "ctrl";

// Quit isn't remappable. Remappable shortcuts and their menu accelerators come from `shortcuts`.
pub fn init(cx: &mut App) {
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &HideApp, cx| cx.hide());
    cx.on_action(|_: &HideOtherApps, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAllApps, cx| cx.unhide_other_apps());
    cx.on_action(|_: &BringAllToFront, cx| cx.activate(true));
    // Suggestions open with Ctrl+Space on every platform, macOS included.
    cx.bind_keys([
        KeyBinding::new(&format!("{MOD}-q"), Quit, None),
        KeyBinding::new("ctrl-space", TriggerSuggest, None),
    ]);
    if cfg!(target_os = "macos") {
        cx.bind_keys([
            KeyBinding::new("cmd-h", HideApp, None),
            KeyBinding::new("alt-cmd-h", HideOtherApps, None),
            KeyBinding::new("cmd-m", MinimizeWindow, None),
            KeyBinding::new("ctrl-cmd-f", ToggleFullscreen, None),
        ]);
    }
}

// Key glyphs on macOS, Ctrl+J style text elsewhere.
pub fn shortcut_label(action: &dyn Action, window: &Window) -> Option<String> {
    let binding = window.highest_precedence_binding_for_action(action)?;
    Some(Kbd::format(binding.keystrokes().first()?.as_keystroke()))
}

fn menu(name: impl Into<gpui_kit::SharedString>, items: Vec<MenuItem>) -> Menu {
    Menu { name: name.into(), items, disabled: false }
}

// In-window menu bars on Windows and Linux, reloaded by set_menus.
#[derive(Default)]
struct MenuBars(Vec<WeakEntity<AppMenuBar>>);

impl Global for MenuBars {}

pub fn register_menu_bar(bar: &Entity<AppMenuBar>, cx: &mut App) {
    cx.default_global::<MenuBars>().0.push(bar.downgrade());
}

// Rerun when the language or a checked item changes. GPUI Kit's AppMenuBar on Windows and Linux reads its own copy
// of the menus, so that copy is set too.
pub fn set_menus(cx: &mut App) {
    let owned = menus(cx).into_iter().map(Menu::owned).collect();
    GlobalState::global_mut(cx).set_app_menus(owned);
    let menus = menus(cx);
    cx.set_menus(menus);
    let bars = cx.try_global::<MenuBars>().map(|bars| bars.0.clone()).unwrap_or_default();
    for bar in bars.iter().filter_map(WeakEntity::upgrade) {
        bar.update(cx, |bar, cx| bar.reload(cx));
    }
}

// README screenshots show the Windows menus, even on macOS.
fn menus(cx: &App) -> Vec<Menu> {
    let mac = cfg!(target_os = "macos") && !crate::screenshots::windows_title_bar(cx);
    let mut file = vec![
        MenuItem::action(t(cx, "menu.newTab"), NewTab),
        MenuItem::action(t(cx, "menu.closeTab"), CloseTab),
        MenuItem::action(t(cx, "menu.reopenClosedTab"), ReopenClosedTab),
        MenuItem::separator(),
        MenuItem::action(t(cx, "menu.quickSearch"), QuickSearch),
    ];
    if !mac {
        file.extend([MenuItem::separator(), MenuItem::action(t(cx, "menu.exit"), Quit)]);
    }
    let edit = vec![
        MenuItem::os_action(t(cx, "menu.undo"), Undo, OsAction::Undo),
        MenuItem::os_action(t(cx, "menu.redo"), Redo, OsAction::Redo),
        MenuItem::separator(),
        MenuItem::os_action(t(cx, "menu.cut"), Cut, OsAction::Cut),
        MenuItem::os_action(t(cx, "menu.copy"), Copy, OsAction::Copy),
        MenuItem::os_action(t(cx, "menu.paste"), Paste, OsAction::Paste),
        MenuItem::os_action(t(cx, "menu.selectAll"), SelectAll, OsAction::SelectAll),
    ];
    let choice = crate::theme::choice(cx);
    let theme = menu(
        t(cx, "viewSections.theme"),
        vec![
            MenuItem::action(t(cx, "theme.dark"), ThemeDark).checked(choice == ThemeChoice::Dark),
            MenuItem::action(t(cx, "theme.light"), ThemeLight).checked(choice == ThemeChoice::Light),
            MenuItem::action(t(cx, "theme.system"), ThemeSystem).checked(choice == ThemeChoice::System),
        ],
    );
    let lang = cx.global::<I18n>().lang();
    let language = menu(
        t(cx, "viewSections.language"),
        vec![
            MenuItem::action(t(cx, "language.en"), LanguageEn).checked(lang == "en"),
            MenuItem::action(t(cx, "language.de"), LanguageDe).checked(lang == "de"),
            MenuItem::action(t(cx, "language.bg"), LanguageBg).checked(lang == "bg"),
        ],
    );
    let (sidebar_open, json_open) =
        (setting_bool(cx, "barsql-sidebar-open", true), setting_bool(cx, "barsql-json-open", false));
    let stripes = setting_bool(cx, crate::grid::STRIPES_KEY, false);
    let view = vec![
        MenuItem::action(t(cx, "menu.commandPalette"), CommandPalette),
        MenuItem::separator(),
        MenuItem::submenu(theme),
        MenuItem::submenu(language),
        MenuItem::separator(),
        MenuItem::action(t(cx, "shortcuts.items.zoomIn"), ZoomIn),
        MenuItem::action(t(cx, "shortcuts.items.zoomOut"), ZoomOut),
        MenuItem::action(t(cx, "shortcuts.items.resetZoom"), ResetZoom),
        MenuItem::separator(),
        MenuItem::action(t(cx, "editor.increaseFontSize"), IncreaseEditorFontSize),
        MenuItem::action(t(cx, "editor.decreaseFontSize"), DecreaseEditorFontSize),
        MenuItem::action(t(cx, "editor.resetFontSize"), ResetEditorFontSize),
        MenuItem::separator(),
        MenuItem::action(t(cx, "menu.toggleSidebar"), ToggleSidebar).checked(sidebar_open),
        MenuItem::action(t(cx, "menu.toggleJsonPanel"), ToggleJsonPanel).checked(json_open),
        MenuItem::action(t(cx, "menu.stripedRows"), ToggleGridStripes).checked(stripes),
        MenuItem::separator(),
        MenuItem::action(t(cx, "menu.toggleFullscreen"), ToggleFullscreen),
    ];
    let help = vec![
        MenuItem::action(t(cx, "menu.keyboardTips"), KeyboardTips),
        MenuItem::action(t(cx, "menu.keyboardShortcuts"), KeyboardShortcuts),
        MenuItem::separator(),
        MenuItem::action(t(cx, "menu.about"), About),
    ];
    let mut menus = Vec::new();
    if mac {
        menus.push(menu(
            "BarSQL",
            vec![
                MenuItem::action(t(cx, "menu.about"), About),
                MenuItem::separator(),
                MenuItem::os_submenu(t(cx, "menu.services"), SystemMenuType::Services),
                MenuItem::separator(),
                MenuItem::action(t(cx, "menu.hideApp"), HideApp),
                MenuItem::action(t(cx, "menu.hideOthers"), HideOtherApps),
                MenuItem::action(t(cx, "menu.showAll"), ShowAllApps),
                MenuItem::separator(),
                MenuItem::action(t(cx, "menu.quitApp"), Quit),
            ],
        ));
    }
    menus.extend([menu(t(cx, "menu.file"), file), menu(t(cx, "menu.edit"), edit), menu(t(cx, "menu.view"), view)]);
    if mac {
        menus.push(menu(
            t(cx, "menu.window"),
            vec![
                MenuItem::action(t(cx, "menu.minimize"), MinimizeWindow),
                MenuItem::action(t(cx, "menu.zoom"), ZoomWindow),
                MenuItem::separator(),
                MenuItem::action(t(cx, "menu.bringAllToFront"), BringAllToFront),
            ],
        ));
    }
    menus.push(menu(t(cx, "menu.help"), help));
    menus
}
