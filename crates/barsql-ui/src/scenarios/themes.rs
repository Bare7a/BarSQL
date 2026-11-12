use std::cell::Cell;
use std::fs;
use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::component::{ActiveTheme as _, Theme, ThemeMode, try_parse_color};
use gpui_kit::{Entity, TestAppContext};
use serde_json::Value;

use super::driver::{self, Driver};
use crate::actions::{CustomizeTheme, OpenThemesFolder, SelectTheme, SetTheme, ThemeDark, ThemeLight, ThemeSystem};
use crate::command_palette::CommandPalette;
use crate::state::{set_setting, setting};
use crate::theme::{self, DARK_KEY, LIGHT_KEY, MenuEntry, ThemeChoice, choice};
use crate::theme_files::{self, Source, Themes, parse_set};
use crate::theme_picker;

const TEST_THEMES: &str = r##"{"name": "Test", "themes": [
    {"name": "Test Dark", "mode": "dark", "colors": {"background": "#101010"}},
    {"name": "Test Light", "mode": "light", "colors": {"background": "#fafafa"}}
]}"##;

// Theme tests start in Dark. Match System would follow the test platform, which is always light.
fn open(cx: &mut TestAppContext) -> Driver<'_> {
    let mut app = driver::open(cx);
    app.dispatch(ThemeDark);
    app
}

fn add_user_themes(app: &mut Driver, json: &str) {
    let entries = parse_set(json, Source::File("t.json".into())).unwrap();
    app.cx.update(|_, cx| cx.global_mut::<Themes>().set_user(entries));
}

fn shown(app: &mut Driver) -> (String, ThemeMode) {
    app.cx.update(|_, cx| (cx.theme().theme_name().to_string(), cx.theme().mode))
}

fn saved(app: &mut Driver, key: &str) -> Option<String> {
    app.cx.update(|_, cx| setting(cx, key))
}

#[gpui_kit::test]
fn unset_picks_show_barsql_themes(cx: &mut TestAppContext) {
    let mut app = open(cx);
    assert_eq!(shown(&mut app), ("BarSQL Dark".into(), ThemeMode::Dark));
    app.dispatch(ThemeLight);
    assert_eq!(shown(&mut app), ("BarSQL Light".into(), ThemeMode::Light));
}

#[gpui_kit::test]
fn picking_a_theme_of_the_other_mode_switches_the_mode(cx: &mut TestAppContext) {
    let mut app = open(cx);
    add_user_themes(&mut app, TEST_THEMES);
    app.cx.update(|window, cx| theme::pick("Test Light", window, cx));
    assert_eq!(app.cx.update(|_, cx| choice(cx)), ThemeChoice::Light);
    assert_eq!(shown(&mut app), ("Test Light".into(), ThemeMode::Light));
    assert_eq!(saved(&mut app, LIGHT_KEY).as_deref(), Some("Test Light"));
    assert_eq!(saved(&mut app, DARK_KEY), None);
}

// The test platform reports a light system appearance.
#[gpui_kit::test]
fn under_match_system_a_pick_waits_for_the_system(cx: &mut TestAppContext) {
    let mut app = open(cx);
    add_user_themes(&mut app, TEST_THEMES);
    app.dispatch(ThemeSystem);
    app.cx.update(|window, cx| theme::pick("Test Dark", window, cx));
    assert_eq!(app.cx.update(|_, cx| choice(cx)), ThemeChoice::System);
    assert_eq!(shown(&mut app), ("BarSQL Light".into(), ThemeMode::Light));
    assert_eq!(saved(&mut app, DARK_KEY).as_deref(), Some("Test Dark"));
    assert_eq!(app.info_toasts(), ["Test Dark will show when your system switches to dark mode"]);
}

#[gpui_kit::test]
fn a_missing_pick_falls_back_and_keeps_its_setting(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.cx.update(|window, cx| {
        set_setting(cx, DARK_KEY, "Gone");
        theme::reapply(window, cx);
    });
    assert_eq!(shown(&mut app), ("BarSQL Dark".into(), ThemeMode::Dark));
    assert_eq!(saved(&mut app, DARK_KEY).as_deref(), Some("Gone"));
}

#[gpui_kit::test]
fn a_pick_edited_to_the_other_mode_falls_back(cx: &mut TestAppContext) {
    let mut app = open(cx);
    add_user_themes(&mut app, r#"{"name": "F", "themes": [{"name": "Flip", "mode": "dark"}]}"#);
    app.cx.update(|window, cx| theme::pick("Flip", window, cx));
    assert_eq!(shown(&mut app), ("Flip".into(), ThemeMode::Dark));
    add_user_themes(&mut app, r#"{"name": "F", "themes": [{"name": "Flip", "mode": "light"}]}"#);
    app.cx.update(theme::reapply);
    assert_eq!(shown(&mut app), ("BarSQL Dark".into(), ThemeMode::Dark));
    assert_eq!(saved(&mut app, DARK_KEY).as_deref(), Some("Flip"));
}

#[gpui_kit::test]
fn a_preview_changes_nothing_saved(cx: &mut TestAppContext) {
    let mut app = open(cx);
    add_user_themes(&mut app, TEST_THEMES);
    app.cx.update(|window, cx| theme::preview("Test Light", window, cx));
    assert_eq!(shown(&mut app), ("Test Light".into(), ThemeMode::Light));
    nothing_saved(&mut app);
    app.cx.update(theme::end_preview);
    assert_eq!(shown(&mut app), ("BarSQL Dark".into(), ThemeMode::Dark));
}

const LIST_THEMES: &str = r##"{"name": "L", "themes": [
    {"name": "zeta dark", "mode": "dark"},
    {"name": "Alpha Dark", "mode": "dark"}
]}"##;

fn menu_names(entries: &[MenuEntry]) -> Vec<String> {
    let name = |entry: &MenuEntry| match entry {
        MenuEntry::Theme { name, .. } => name.to_string(),
        MenuEntry::Separator => "---".into(),
    };
    entries.iter().map(name).collect()
}

#[gpui_kit::test]
fn the_theme_lists_put_barsql_first_and_user_themes_last(cx: &mut TestAppContext) {
    let mut app = open(cx);
    add_user_themes(&mut app, LIST_THEMES);
    let (entries, mut others) = app.cx.update(|_, cx| {
        let built_in = theme_files::themes(cx).all().filter(|entry| entry.source == Source::BuiltIn);
        let dark = built_in.filter(|entry| entry.config.mode == ThemeMode::Dark && entry.config.name != "BarSQL Dark");
        (theme::menu_entries(ThemeMode::Dark, cx), dark.map(|entry| entry.config.name.to_string()).collect::<Vec<_>>())
    });
    others.sort_by_key(|name| name.to_lowercase());
    let mut expected = vec!["BarSQL Dark".to_string()];
    expected.extend(others);
    expected.extend(["---".into(), "Alpha Dark".into(), "zeta dark".into()]);
    assert_eq!(menu_names(&entries), expected);
}

#[gpui_kit::test]
fn the_check_mark_follows_the_theme_showing(cx: &mut TestAppContext) {
    let mut app = open(cx);
    add_user_themes(&mut app, LIST_THEMES);
    let checked = |app: &mut Driver| {
        let entries = app.cx.update(|_, cx| theme::menu_entries(ThemeMode::Dark, cx));
        let checked = entries.into_iter().filter(|entry| matches!(entry, MenuEntry::Theme { checked: true, .. }));
        menu_names(&checked.collect::<Vec<_>>())
    };
    app.cx.update(|window, cx| {
        set_setting(cx, DARK_KEY, "Gone");
        theme::reapply(window, cx);
    });
    assert_eq!(checked(&mut app), ["BarSQL Dark"]);
    app.cx.update(|window, cx| theme::pick("Alpha Dark", window, cx));
    assert_eq!(checked(&mut app), ["Alpha Dark"]);
}

#[gpui_kit::test]
fn the_theme_menu_picks_a_theme(cx: &mut TestAppContext) {
    let mut app = open(cx);
    add_user_themes(&mut app, LIST_THEMES);
    app.dispatch(SetTheme { name: "Alpha Dark".into() });
    assert_eq!(shown(&mut app), ("Alpha Dark".into(), ThemeMode::Dark));
    assert_eq!(saved(&mut app, DARK_KEY).as_deref(), Some("Alpha Dark"));
}

fn nothing_saved(app: &mut Driver) {
    let keys = ["barsql-theme", DARK_KEY, LIGHT_KEY];
    assert_eq!(keys.map(|key| saved(app, key)), [Some("dark".into()), None, None]);
}

// The rows are read from the list, so built-in themes can come and go.
fn open_picker(app: &mut Driver) -> Entity<CommandPalette> {
    add_user_themes(app, LIST_THEMES);
    let palette = app.cx.update(theme_picker::open);
    app.cx.run_until_parked();
    palette
}

fn rows(app: &mut Driver, palette: &Entity<CommandPalette>) -> Vec<String> {
    app.cx.update(|_, cx| palette.read(cx).labels())
}

fn cursor(app: &mut Driver, palette: &Entity<CommandPalette>) -> String {
    app.cx.update(|_, cx| palette.read(cx).active_label())
}

#[gpui_kit::test]
fn select_theme_opens_on_the_theme_showing_now(cx: &mut TestAppContext) {
    let mut app = open(cx);
    add_user_themes(&mut app, LIST_THEMES);
    app.cx.update(|window, cx| theme::pick("Alpha Dark", window, cx));
    app.dispatch(SelectTheme);
    assert!(app.dialog_open());
    app.keys("escape");
    let palette = app.cx.update(theme_picker::open);
    app.cx.run_until_parked();
    assert_eq!(app.cx.update(|_, cx| palette.read(cx).active_label()), "Alpha Dark");
    assert_eq!(shown(&mut app), ("Alpha Dark".into(), ThemeMode::Dark));
}

#[gpui_kit::test]
fn arrows_and_typing_preview_without_saving(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let palette = open_picker(&mut app);
    let second = rows(&mut app, &palette)[1].clone();
    app.keys("down");
    assert_eq!(shown(&mut app), (second, ThemeMode::Dark));
    app.type_text("barsql light");
    assert_eq!(shown(&mut app), ("BarSQL Light".into(), ThemeMode::Light));
    app.type_text("zzz");
    assert_eq!(shown(&mut app), ("BarSQL Light".into(), ThemeMode::Light), "no match keeps the preview");
    nothing_saved(&mut app);
}

#[gpui_kit::test]
fn hovering_doesnt_preview(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let palette = open_picker(&mut app);
    let second = rows(&mut app, &palette)[1].clone();
    app.hover("command-palette-item-1");
    assert_eq!(cursor(&mut app, &palette), second, "the highlight moves");
    assert_eq!(shown(&mut app), ("BarSQL Dark".into(), ThemeMode::Dark));
}

#[gpui_kit::test]
fn enter_keeps_the_previewed_theme(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let palette = open_picker(&mut app);
    let second = rows(&mut app, &palette)[1].clone();
    app.keys("down");
    app.keys("enter");
    app.settle_dialog_closed();
    assert_eq!(shown(&mut app), (second.clone(), ThemeMode::Dark));
    assert_eq!(saved(&mut app, DARK_KEY), Some(second));
}

#[gpui_kit::test]
fn escape_restores_the_old_theme(cx: &mut TestAppContext) {
    let mut app = open(cx);
    open_picker(&mut app);
    app.keys("down");
    app.keys("escape");
    app.settle_dialog_closed();
    assert_eq!(shown(&mut app), ("BarSQL Dark".into(), ThemeMode::Dark));
    nothing_saved(&mut app);
}

const MINE: &str =
    r##"{"name": "M", "themes": [{"name": "Mine Dark", "mode": "dark", "colors": {"background": "#101010"}}]}"##;

fn themes_dir(app: &mut Driver) -> PathBuf {
    app.env.bar.data_dir().join("themes")
}

fn write_theme(app: &mut Driver, file: &str, text: &str) {
    let dir = themes_dir(app);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(file), text).unwrap();
}

fn refocus(app: &mut Driver) {
    app.cx.deactivate_window();
    app.cx.update(|window, _| window.activate_window());
    app.cx.run_until_parked();
}

fn lists_mine(app: &mut Driver) -> bool {
    let entries = app.cx.update(|_, cx| theme::menu_entries(ThemeMode::Dark, cx));
    menu_names(&entries).iter().any(|name| name == "Mine Dark")
}

#[gpui_kit::test]
fn user_themes_load_at_launch_and_bad_files_say_so(cx: &mut TestAppContext) {
    let mut app = open(cx);
    write_theme(&mut app, "mine.json", MINE);
    write_theme(&mut app, "broken.json", "{");
    app.cx.update(|_, cx| theme_files::init(cx));
    assert!(lists_mine(&mut app));
    let errors = app.error_toasts();
    assert!(errors.len() == 1 && errors[0].starts_with("broken.json: line 1, column "), "{errors:?}");
}

#[gpui_kit::test]
fn a_changed_folder_reloads_when_the_window_regains_focus(cx: &mut TestAppContext) {
    let mut app = open(cx);
    write_theme(&mut app, "mine.json", MINE);
    assert!(!lists_mine(&mut app));
    refocus(&mut app);
    assert!(lists_mine(&mut app));
}

#[gpui_kit::test]
fn an_unchanged_folder_isnt_reread_or_reported_again(cx: &mut TestAppContext) {
    let mut app = open(cx);
    write_theme(&mut app, "broken.json", "{");
    refocus(&mut app);
    assert_eq!(app.error_toasts().len(), 1);
    refocus(&mut app);
    assert_eq!(app.error_toasts().len(), 1);
}

#[gpui_kit::test]
fn editing_the_theme_in_use_updates_it(cx: &mut TestAppContext) {
    let mut app = open(cx);
    write_theme(&mut app, "mine.json", MINE);
    refocus(&mut app);
    app.cx.update(|window, cx| theme::pick("Mine Dark", window, cx));
    let background = |app: &mut Driver| app.cx.update(|_, cx| cx.theme().background);
    assert_eq!(background(&mut app), try_parse_color("#101010").unwrap());
    write_theme(&mut app, "mine.json", &format!("{}\n", MINE.replace("#101010", "#202020")));
    refocus(&mut app);
    assert_eq!(background(&mut app), try_parse_color("#202020").unwrap());
}

#[gpui_kit::test]
fn a_preview_isnt_cancelled_by_a_reload(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.dispatch(SelectTheme);
    app.keys("down");
    let previewed = shown(&mut app);
    assert_ne!(previewed.0, "BarSQL Dark");
    write_theme(&mut app, "mine.json", MINE);
    refocus(&mut app);
    assert_eq!(shown(&mut app), previewed, "the preview stays");
    assert!(!lists_mine(&mut app), "the reload waits for the list to close");
    app.keys("escape");
    app.settle_dialog_closed();
    assert_eq!(shown(&mut app), ("BarSQL Dark".into(), ThemeMode::Dark));
    assert!(lists_mine(&mut app));
}

fn opened(app: &mut Driver) -> Vec<PathBuf> {
    app.cx.update(|_, cx| cx.try_global::<theme_files::Opened>().map(|opened| opened.0.clone()).unwrap_or_default())
}

fn read_json(path: PathBuf) -> Value {
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

#[gpui_kit::test]
fn customize_writes_a_copy_and_switches_to_it(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.dispatch(CustomizeTheme);
    let file = themes_dir(&mut app).join("barsql-dark-custom.json");
    assert_eq!(shown(&mut app), ("BarSQL Dark (custom)".into(), ThemeMode::Dark));
    assert_eq!(saved(&mut app, DARK_KEY).as_deref(), Some("BarSQL Dark (custom)"));
    let copy = read_json(file);
    let original = app.cx.update(|_, cx| theme_files::themes(cx).barsql(ThemeMode::Dark).json.clone());
    assert_eq!(copy["themes"][0]["name"], "BarSQL Dark (custom)");
    assert_eq!(copy["author"], "Bare7a", "the copy keeps the theme's author");
    assert_eq!(copy["themes"][0]["colors"], original["colors"]);
    assert_eq!(copy["themes"][0]["highlight"], original["highlight"]);
    assert_eq!(opened(&mut app), [themes_dir(&mut app).join("barsql-dark-custom.json")], "the copy opens");
    assert_eq!(app.error_toasts(), Vec::<String>::new(), "the copy loads without problems");
    let reloaded = app.cx.update(|_, cx| crate::theme::slot(ThemeMode::Dark, cx).json);
    assert_eq!(reloaded["colors"], original["colors"]);
    assert_eq!(reloaded["highlight"], original["highlight"], "the copy keeps its editor colors");
}

// Customizing the copy only opens it, so the second one starts from BarSQL Dark again.
#[gpui_kit::test]
fn customize_numbers_the_next_copy(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.dispatch(CustomizeTheme);
    app.cx.update(|window, cx| theme::pick("BarSQL Dark", window, cx));
    app.dispatch(CustomizeTheme);
    assert!(themes_dir(&mut app).join("barsql-dark-custom-2.json").exists());
    assert_eq!(shown(&mut app).0, "BarSQL Dark (custom 2)");
}

#[gpui_kit::test]
fn customizing_your_own_theme_opens_its_file(cx: &mut TestAppContext) {
    let mut app = open(cx);
    write_theme(&mut app, "mine.json", MINE);
    refocus(&mut app);
    app.cx.update(|window, cx| theme::pick("Mine Dark", window, cx));
    app.dispatch(CustomizeTheme);
    assert_eq!(opened(&mut app), [themes_dir(&mut app).join("mine.json")]);
    assert_eq!(shown(&mut app).0, "Mine Dark");
    assert_eq!(fs::read_dir(themes_dir(&mut app)).unwrap().count(), 1, "no copy is made");
}

#[gpui_kit::test]
fn clearing_the_search_goes_back_to_the_theme_in_use(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.cx.update(|window, cx| theme::pick("Nord", window, cx));
    let palette = app.cx.update(theme_picker::open);
    app.cx.run_until_parked();
    app.type_text("z");
    assert_ne!(shown(&mut app).0, "Nord");
    app.keys("backspace");
    assert_eq!(cursor(&mut app, &palette), "Nord");
    assert_eq!(shown(&mut app), ("Nord".into(), ThemeMode::Dark));
}

#[gpui_kit::test]
fn searching_dark_lists_every_dark_theme(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let palette = open_picker(&mut app);
    app.type_text("dark");
    let mut found = rows(&mut app, &palette);
    let says = |name: &String| name.to_lowercase().contains("dark");
    let others = found.iter().position(|name| !says(name)).unwrap_or(found.len());
    assert!(!found[others..].iter().any(says), "names that say it come first: {found:?}");
    let dark = app.cx.update(|_, cx| theme::ordered(ThemeMode::Dark, cx));
    let mut dark: Vec<String> = dark.iter().map(|entry| entry.config.name.to_string()).collect();
    found.sort();
    dark.sort();
    assert_eq!(found, dark);
}

#[gpui_kit::test]
fn open_themes_folder_creates_it(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let dir = themes_dir(&mut app);
    assert!(!dir.exists());
    app.dispatch(OpenThemesFolder);
    assert!(dir.exists());
    assert_eq!(opened(&mut app), [dir]);
}

// Counts changes to GPUI Kit's theme from here on.
fn theme_changes(app: &mut Driver) -> Rc<Cell<usize>> {
    let count = Rc::new(Cell::new(0));
    let counter = count.clone();
    app.cx.update(|_, cx| cx.observe_global::<Theme>(move |_| counter.set(counter.get() + 1)).detach());
    count
}

#[gpui_kit::test]
fn picking_a_missing_theme_ends_the_preview(cx: &mut TestAppContext) {
    let mut app = open(cx);
    add_user_themes(&mut app, TEST_THEMES);
    app.cx.update(|window, cx| theme::preview("Test Light", window, cx));
    app.cx.update(|window, cx| theme::pick("Gone", window, cx));
    assert!(!app.cx.update(|_, cx| theme::previewing(cx)));
    assert_eq!(shown(&mut app), ("BarSQL Dark".into(), ThemeMode::Dark));
}

#[gpui_kit::test]
fn select_theme_asks_for_a_theme(cx: &mut TestAppContext) {
    let app = open(cx);
    let palette = app.cx.update(theme_picker::open);
    app.cx.run_until_parked();
    assert_eq!(app.cx.update(|_, cx| palette.read(cx).placeholder(cx)), "Search themes…");
}

#[gpui_kit::test]
fn typing_that_keeps_the_match_doesnt_reapply(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let _palette = open_picker(&mut app);
    app.type_text("barsql l");
    assert_eq!(shown(&mut app).0, "BarSQL Light");
    let changes = theme_changes(&mut app);
    app.type_text("ight");
    assert_eq!(shown(&mut app).0, "BarSQL Light");
    assert_eq!(changes.get(), 0);
}

#[gpui_kit::test]
fn a_new_theme_file_doesnt_reapply_the_one_showing(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let changes = theme_changes(&mut app);
    write_theme(&mut app, "mine.json", MINE);
    app.cx.update(theme_files::refresh);
    assert!(lists_mine(&mut app));
    assert_eq!(changes.get(), 0);
}

#[gpui_kit::test]
fn theme_lookups_share_their_data(cx: &mut TestAppContext) {
    let app = open(cx);
    let (slot, listed) = app.cx.update(|_, cx| {
        (theme::slot(ThemeMode::Dark, cx).json, theme_files::themes(cx).barsql(ThemeMode::Dark).json.clone())
    });
    assert!(Rc::ptr_eq(&slot, &listed));
}
