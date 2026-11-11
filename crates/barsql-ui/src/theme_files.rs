use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::LazyLock;
use std::time::SystemTime;

use gpui_kit::component::highlighter::{HighlightThemeStyle, SyntaxColors};
use gpui_kit::component::{
    Theme, ThemeConfig, ThemeConfigColors, ThemeMode, ThemeSet, try_parse_background, try_parse_color,
};
use gpui_kit::{App, Global, SharedString, Window};
use serde_json::{Map, Value, json};

use crate::i18n::{t, t_count, t_with};
use crate::state::{self, AppState};
use crate::toast::{self, ToastAction, ToastKind};

pub const BARSQL_DARK: &str = "BarSQL Dark";
pub const BARSQL_LIGHT: &str = "BarSQL Light";

const BUILT_IN: &[&str] = &[
    include_str!("../themes/barsql.json"),
    include_str!("../themes/catppuccin.json"),
    include_str!("../themes/one.json"),
    include_str!("../themes/dracula.json"),
    include_str!("../themes/tokyo-night.json"),
    include_str!("../themes/nord.json"),
    include_str!("../themes/gruvbox.json"),
    include_str!("../themes/solarized.json"),
    include_str!("../themes/vscode.json"),
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    BuiltIn,
    File(PathBuf),
}

#[derive(Clone)]
pub struct ThemeEntry {
    pub config: Rc<ThemeConfig>,
    pub json: Rc<Value>,
    pub author: Option<String>,
    pub url: Option<String>,
    pub source: Source,
}

pub struct Themes {
    built_in: Vec<ThemeEntry>,
    user: Vec<ThemeEntry>,
    stamp: Stamp,
}

impl Global for Themes {}

impl Themes {
    pub fn all(&self) -> impl Iterator<Item = &ThemeEntry> {
        self.built_in.iter().chain(&self.user)
    }

    pub fn find(&self, name: &str, mode: ThemeMode) -> Option<&ThemeEntry> {
        self.all().find(|entry| entry.config.name == name && entry.config.mode == mode)
    }

    pub fn barsql(&self, mode: ThemeMode) -> &ThemeEntry {
        let name = if mode.is_dark() { BARSQL_DARK } else { BARSQL_LIGHT };
        self.built_in.iter().find(|entry| entry.config.name == name).expect("BarSQL's own themes are built in")
    }

    pub fn user(&self) -> &[ThemeEntry] {
        &self.user
    }

    #[cfg(test)]
    pub(crate) fn set_user(&mut self, user: Vec<ThemeEntry>) {
        self.user = user;
    }
}

pub(crate) fn built_in() -> Themes {
    let built_in = BUILT_IN.iter().flat_map(|text| parse_set(text, Source::BuiltIn).expect("built-in theme")).collect();
    Themes { built_in, user: Vec::new(), stamp: Stamp::default() }
}

pub fn init(cx: &mut App) {
    let mut themes = built_in();
    let reports = if cx.has_global::<AppState>() { load_user(&mut themes, &folder(cx)) } else { Vec::new() };
    cx.set_global(themes);
    show_reports(&reports, cx);
}

pub fn folder(cx: &App) -> PathBuf {
    state::bar(cx).data_dir().join("themes")
}

fn load_user(themes: &mut Themes, dir: &Path) -> Vec<FileReport> {
    let mut load = load_folder(dir, themes);
    keep_last_good(&themes.user, &mut load);
    themes.user = load.entries;
    themes.stamp = load.stamp;
    load.reports
}

// When the window comes back, in case a theme file changed meanwhile. A preview defers it until the list closes.
pub fn refresh(window: &mut Window, cx: &mut App) {
    let dir = folder(cx);
    if crate::theme::previewing(cx) || stamp(&dir) == themes(cx).stamp {
        return;
    }
    let showing = |cx: &App| [ThemeMode::Dark, ThemeMode::Light].map(|mode| crate::theme::slot(mode, cx).json);
    let before = showing(cx);
    let reports = load_user(cx.global_mut::<Themes>(), &dir);
    show_reports(&reports, cx);
    // The lists change either way; the themes in use only when their own files did.
    if showing(cx) == before {
        crate::actions::set_menus(cx);
    } else {
        crate::theme::reapply(window, cx);
    }
}

fn show_reports(reports: &[FileReport], cx: &mut App) {
    for report in reports {
        let Some(first) = report.problems.first() else { continue };
        let mut message = format!("{}: {}", file_name(&report.path), problem_text(first, cx));
        if report.problems.len() > 1 {
            let more = t_count(cx, "theme.problems.more", report.problems.len() as i64 - 1, &[]);
            message = format!("{message} {more}");
        }
        let path = report.path.clone();
        let open = ToastAction { label: t(cx, "theme.openFile"), on_click: Rc::new(move |_, cx| open_path(&path, cx)) };
        toast::push(message, ToastKind::Error, None, Some(open), cx);
    }
}

fn problem_text(problem: &Problem, cx: &App) -> SharedString {
    let key = |key: &str| format!("theme.problems.{key}");
    match problem {
        Problem::Unreadable(error) => t_with(cx, &key("unreadable"), &[("error", error)]),
        Problem::NotJson { line, column, message } => {
            let (line, column) = (line.to_string(), column.to_string());
            t_with(cx, &key("notJson"), &[("line", &line), ("column", &column), ("error", message)])
        }
        Problem::TooBig => t(cx, &key("tooBig")),
        Problem::NoThemes => t(cx, &key("noThemes")),
        Problem::MissingName => t(cx, &key("missingName")),
        Problem::MissingMode(theme) => t_with(cx, &key("missingMode"), &[("theme", theme)]),
        Problem::BuiltInName(theme) => t_with(cx, &key("builtInName"), &[("theme", theme)]),
        Problem::DuplicateName(theme) => t_with(cx, &key("duplicateName"), &[("theme", theme)]),
        Problem::BadValue { theme, key: name } => t_with(cx, &key("badValue"), &[("theme", theme), ("key", name)]),
        Problem::UnknownKey { theme, key: name } => t_with(cx, &key("unknownKey"), &[("theme", theme), ("key", name)]),
    }
}

pub fn themes(cx: &App) -> &Themes {
    cx.global::<Themes>()
}

pub fn parse_set(text: &str, source: Source) -> anyhow::Result<Vec<ThemeEntry>> {
    let set: ThemeSet = serde_json::from_str(text)?;
    let raw: Value = serde_json::from_str(text)?;
    let objects = raw.get("themes").and_then(Value::as_array).cloned().unwrap_or_default();
    let (author, url) = (set.author.map(|a| a.to_string()), set.url.map(|u| u.to_string()));
    let entries = set.themes.into_iter().zip(objects).map(|(mut config, json)| {
        with_barsql_shape(&mut config);
        let json = Rc::new(json);
        ThemeEntry { config: Rc::new(config), json, author: author.clone(), url: url.clone(), source: source.clone() }
    });
    Ok(entries.collect())
}

// Themes set colors only, so every theme keeps BarSQL's fonts and corners.
pub(crate) fn with_barsql_shape(config: &mut ThemeConfig) {
    config.font_family = Some("Inter".into());
    config.font_size = Some(13.);
    config.mono_font_family = Some("Fira Code".into());
    config.mono_font_size = Some(13.);
    config.radius = Some(6);
    config.radius_lg = Some(6);
    config.shadow = Some(false);
}

pub const MAX_FILE_BYTES: u64 = 1024 * 1024;

const IGNORED_KEYS: [&str; 8] =
    ["font.size", "font.family", "mono_font.family", "mono_font.size", "radius", "radius.lg", "shadow", "is_default"];

#[derive(Clone, Debug, PartialEq)]
pub enum Problem {
    Unreadable(String),
    NotJson { line: usize, column: usize, message: String },
    TooBig,
    NoThemes,
    MissingName,
    MissingMode(String),
    BuiltInName(String),
    DuplicateName(String),
    BadValue { theme: String, key: String },
    UnknownKey { theme: String, key: String },
}

#[derive(Clone, Debug, PartialEq)]
pub struct FileReport {
    pub path: PathBuf,
    pub problems: Vec<Problem>,
    pub loaded: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stamp(Vec<(String, u64, Option<SystemTime>)>);

pub struct FolderLoad {
    pub entries: Vec<ThemeEntry>,
    pub reports: Vec<FileReport>,
    pub stamp: Stamp,
}

struct KnownKeys {
    colors: HashSet<String>,
    highlight: HashSet<String>,
    syntax: HashSet<String>,
}

// Every key GPUI Kit reads, taken from its own defaults so the list follows its version.
static KNOWN: LazyLock<KnownKeys> = LazyLock::new(|| {
    fn keys(value: serde_json::Result<Value>) -> HashSet<String> {
        value
            .ok()
            .and_then(|value| value.as_object().map(|object| object.keys().cloned().collect()))
            .unwrap_or_default()
    }
    let mut highlight = keys(serde_json::to_value(HighlightThemeStyle::default()));
    highlight.remove("syntax");
    KnownKeys {
        colors: keys(serde_json::to_value(ThemeConfigColors::default())),
        highlight,
        syntax: keys(serde_json::to_value(SyntaxColors::default())),
    }
});

fn file_name(path: &Path) -> String {
    path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default()
}

// `*.json` files right in `dir`, by lowercase name. Editor leftovers like `x.json~` and hidden files like macOS's
// `._x.json` don't count.
fn json_files(dir: &Path) -> Vec<(PathBuf, fs::Metadata)> {
    let Ok(read) = fs::read_dir(dir) else { return Vec::new() };
    let json = |path: &PathBuf| {
        let extension = path.extension().and_then(|ext| ext.to_str());
        extension.is_some_and(|ext| ext.eq_ignore_ascii_case("json")) && !file_name(path).starts_with('.')
    };
    let mut files: Vec<_> = read
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(json)
        .filter_map(|path| fs::metadata(&path).ok().filter(fs::Metadata::is_file).map(|meta| (path, meta)))
        .collect();
    files.sort_by_key(|(path, _)| file_name(path).to_lowercase());
    files
}

fn stamp_of(files: &[(PathBuf, fs::Metadata)]) -> Stamp {
    Stamp(files.iter().map(|(path, meta)| (file_name(path), meta.len(), meta.modified().ok())).collect())
}

pub fn stamp(dir: &Path) -> Stamp {
    stamp_of(&json_files(dir))
}

fn read_set(path: &Path, meta: &fs::Metadata) -> Result<Value, Problem> {
    if meta.len() > MAX_FILE_BYTES {
        return Err(Problem::TooBig);
    }
    let text = fs::read_to_string(path).map_err(|error| Problem::Unreadable(error.to_string()))?;
    serde_json::from_str(text.strip_prefix('\u{feff}').unwrap_or(&text)).map_err(|error| {
        let message = error.to_string();
        let message = message.rsplit_once(" at line ").map_or(message.as_str(), |(head, _)| head).to_string();
        Problem::NotJson { line: error.line(), column: error.column(), message }
    })
}

pub fn load_folder(dir: &Path, themes: &Themes) -> FolderLoad {
    let built_in: HashSet<String> = themes.built_in.iter().map(|entry| entry.config.name.to_lowercase()).collect();
    let mut taken = HashSet::new();
    let files = json_files(dir);
    let mut load = FolderLoad { entries: Vec::new(), reports: Vec::new(), stamp: stamp_of(&files) };
    for (path, meta) in files {
        let mut problems = Vec::new();
        // An unreadable file, or one whose themes went missing mid-edit, keeps its last themes.
        let mut loaded = true;
        match read_set(&path, &meta) {
            Err(problem) => {
                loaded = false;
                problems.push(problem);
            }
            Ok(set) => match set.get("themes").and_then(Value::as_array).filter(|list| !list.is_empty()) {
                None => {
                    loaded = false;
                    problems.push(Problem::NoThemes);
                }
                Some(list) => {
                    let text = |key: &str| set.get(key).and_then(Value::as_str).map(str::to_string);
                    let (author, url) = (text("author"), text("url"));
                    for raw in list {
                        let Some((name, json)) = clean_theme(raw, &built_in, &mut taken, themes, &mut problems) else {
                            continue;
                        };
                        match serde_json::from_value::<ThemeConfig>(json.clone()) {
                            Ok(mut config) => {
                                with_barsql_shape(&mut config);
                                let (author, url, source) = (author.clone(), url.clone(), Source::File(path.clone()));
                                let (config, json) = (Rc::new(config), Rc::new(json));
                                load.entries.push(ThemeEntry { config, json, author, url, source });
                            }
                            Err(error) => problems.push(Problem::Unreadable(format!("{name}: {error}"))),
                        }
                    }
                }
            },
        }
        if !problems.is_empty() {
            load.reports.push(FileReport { path, problems, loaded });
        }
    }
    load
}

fn clean_theme(
    raw: &Value,
    built_in: &HashSet<String>,
    taken: &mut HashSet<String>,
    themes: &Themes,
    problems: &mut Vec<Problem>,
) -> Option<(String, Value)> {
    let object = raw.as_object();
    let name = object.and_then(|object| object.get("name")).and_then(Value::as_str).map(str::trim);
    let (Some(object), Some(name)) = (object, name.filter(|name| !name.is_empty())) else {
        problems.push(Problem::MissingName);
        return None;
    };
    let name = name.to_string();
    let mode = match object.get("mode").and_then(Value::as_str) {
        Some("dark") => ThemeMode::Dark,
        Some("light") => ThemeMode::Light,
        _ => {
            problems.push(Problem::MissingMode(name));
            return None;
        }
    };
    let lower = name.to_lowercase();
    if built_in.contains(&lower) {
        problems.push(Problem::BuiltInName(name));
        return None;
    }
    if !taken.insert(lower) {
        problems.push(Problem::DuplicateName(name));
        return None;
    }
    let mut clean = Map::new();
    clean.insert("name".into(), Value::String(name.clone()));
    clean.insert("mode".into(), object["mode"].clone());
    for (key, value) in object {
        match key.as_str() {
            "name" | "mode" => {}
            key if IGNORED_KEYS.contains(&key) => {}
            "colors" => {
                let valid = |key: &str, value: &Value| {
                    let color = |color: &str| try_parse_color(color).is_ok() || paints_gradient(key, color, mode);
                    value.is_null() || value.as_str().is_some_and(color)
                };
                let colors = clean_section(value, "colors", &KNOWN.colors, valid, &name, problems);
                clean.insert(key.into(), Value::Object(colors));
            }
            "highlight" => {
                clean.insert(key.into(), clean_highlight(value, &name, problems));
            }
            _ => problems.push(Problem::UnknownKey { theme: name.clone(), key: key.clone() }),
        }
    }
    fill_syntax(&mut clean, &themes.barsql(mode).json["highlight"]["syntax"]);
    Some((name, Value::Object(clean)))
}

// A gradient counts only on a key GPUI Kit paints one for. On any other it would quietly become the default color.
fn paints_gradient(key: &str, value: &str, mode: ThemeMode) -> bool {
    let tokens = |colors: Value| {
        let config = serde_json::from_value::<ThemeConfig>(json!({ "mode": mode, "colors": colors })).ok()?;
        let mut theme = Theme::default();
        theme.apply_config(&Rc::new(config));
        Some(theme.tokens)
    };
    try_parse_background(value).is_ok() && tokens(json!({ key: value })) != tokens(json!({}))
}

// Keeps the known keys whose values pass `valid`; reports the rest as `section["key"]`.
fn clean_section(
    value: &Value,
    section: &str,
    known: &HashSet<String>,
    valid: impl Fn(&str, &Value) -> bool,
    theme: &str,
    problems: &mut Vec<Problem>,
) -> Map<String, Value> {
    let Some(object) = value.as_object() else {
        problems.push(Problem::BadValue { theme: theme.into(), key: section.into() });
        return Map::new();
    };
    let mut clean = Map::new();
    for (key, value) in object {
        let label = format!(r#"{section}["{key}"]"#);
        if !known.contains(key) {
            problems.push(Problem::UnknownKey { theme: theme.into(), key: label });
        } else if valid(key, value) {
            clean.insert(key.clone(), value.clone());
        } else {
            problems.push(Problem::BadValue { theme: theme.into(), key: label });
        }
    }
    clean
}

// Highlight values deserialize strictly, so each one is checked on its own.
fn clean_highlight(value: &Value, theme: &str, problems: &mut Vec<Problem>) -> Value {
    let Some(object) = value.as_object() else {
        problems.push(Problem::BadValue { theme: theme.into(), key: "highlight".into() });
        return json!({});
    };
    let rest =
        Value::Object(object.iter().filter(|(key, _)| *key != "syntax").map(|(k, v)| (k.clone(), v.clone())).collect());
    let one = |key: &str, value: &Value| json!({ key: value });
    // `syntax` isn't optional in GPUI Kit's struct, so each entry is checked beside an empty one.
    let valid = |key: &str, value: &Value| {
        serde_json::from_value::<HighlightThemeStyle>(json!({ key: value, "syntax": {} })).is_ok()
    };
    let mut clean = clean_section(&rest, "highlight", &KNOWN.highlight, valid, theme, problems);
    if let Some(syntax) = object.get("syntax") {
        let valid = |key: &str, value: &Value| serde_json::from_value::<SyntaxColors>(one(key, value)).is_ok();
        let syntax = clean_section(syntax, "syntax", &KNOWN.syntax, valid, theme, problems);
        clean.insert("syntax".into(), Value::Object(syntax));
    }
    Value::Object(clean)
}

// Syntax colors a theme leaves out come from BarSQL's; GPUI Kit would keep the previous theme's. Not the ones
// under a key it set, which GPUI Kit colors like that key: `punctuation.bracket` as `punctuation`.
fn fill_syntax(theme: &mut Map<String, Value>, base: &Value) {
    let highlight = theme.entry("highlight").or_insert_with(|| json!({}));
    let Some(syntax) = highlight.as_object_mut().map(|h| h.entry("syntax").or_insert_with(|| json!({}))) else {
        return;
    };
    if let (Some(syntax), Some(base)) = (syntax.as_object_mut(), base.as_object()) {
        let set: HashSet<String> = syntax.keys().cloned().collect();
        for (key, style) in base {
            let parent = key.split(['.', '_']).next().unwrap_or(key);
            if !set.contains(key) && !set.contains(parent) {
                syntax.insert(key.clone(), style.clone());
            }
        }
    }
}

pub fn slug(name: &str) -> String {
    let mut slug = String::new();
    for c in name.chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            slug.push(c);
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_end_matches('-');
    if slug.is_empty() { "theme".into() } else { slug.into() }
}

// Saves the theme showing now as an editable copy in the folder, switches to it and opens the file. A theme
// from the folder is editable as it is, so its own file opens.
pub fn customize(window: &mut Window, cx: &mut App) -> anyhow::Result<PathBuf> {
    use gpui_kit::component::ActiveTheme as _;
    let source = crate::theme::slot(cx.theme().mode, cx);
    if let Source::File(path) = &source.source {
        open_path(path, cx);
        return Ok(path.clone());
    }
    let dir = folder(cx);
    let base = source.config.name.to_string();
    let names: HashSet<String> = themes(cx).all().map(|entry| entry.config.name.to_lowercase()).collect();
    let copy = |n: usize| match n {
        1 => t_with(cx, "theme.customName", &[("name", &base)]).to_string(),
        n => t_with(cx, "theme.customNameN", &[("name", &base), ("n", &n.to_string())]).to_string(),
    };
    let (name, path) = (1..)
        .map(copy)
        .map(|name| (dir.join(format!("{}.json", slug(&name))), name))
        .find(|(path, name)| !names.contains(&name.to_lowercase()) && !path.exists())
        .map(|(path, name)| (name, path))
        .expect("some number is free");
    let mut theme = source.json.as_object().cloned().unwrap_or_default();
    for key in IGNORED_KEYS {
        theme.remove(key);
    }
    theme.insert("name".into(), Value::String(name.clone()));
    let mut set = Map::new();
    set.insert("name".into(), Value::String(name.clone()));
    for (key, value) in [("author", &source.author), ("url", &source.url)] {
        if let Some(value) = value {
            set.insert(key.into(), Value::String(value.clone()));
        }
    }
    set.insert("themes".into(), json!([theme]));
    fs::create_dir_all(&dir)?;
    fs::write(&path, serde_json::to_string_pretty(&Value::Object(set))?)?;
    refresh(window, cx);
    crate::theme::pick(&name, window, cx);
    open_path(&path, cx);
    Ok(path)
}

pub fn open_folder(cx: &mut App) -> std::io::Result<PathBuf> {
    let dir = folder(cx);
    fs::create_dir_all(&dir)?;
    open_path(&dir, cx);
    Ok(dir)
}

// The test platform can't open files, so tests read what would have opened here.
#[cfg(test)]
#[derive(Default)]
pub struct Opened(pub Vec<PathBuf>);

#[cfg(test)]
impl Global for Opened {}

fn open_path(path: &Path, cx: &mut App) {
    #[cfg(test)]
    return cx.default_global::<Opened>().0.push(path.to_path_buf());
    #[cfg(not(test))]
    cx.open_with_system(path);
}

// A file that can't be read mid-edit keeps the themes it last loaded.
pub fn keep_last_good(previous: &[ThemeEntry], load: &mut FolderLoad) {
    let failed: Vec<&PathBuf> =
        load.reports.iter().filter(|report| !report.loaded).map(|report| &report.path).collect();
    let mut names: HashSet<String> = load.entries.iter().map(|entry| entry.config.name.to_lowercase()).collect();
    let kept = previous.iter().filter(|entry| matches!(&entry.source, Source::File(path) if failed.contains(&path)));
    let kept: Vec<ThemeEntry> = kept.filter(|entry| names.insert(entry.config.name.to_lowercase())).cloned().collect();
    load.entries.extend(kept);
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use std::fs;
    use std::path::Path;

    use gpui_kit::component::{Theme, ThemeMode, try_parse_color};
    use serde_json::Value;

    use super::{
        BUILT_IN, FolderLoad, KNOWN, MAX_FILE_BYTES, Problem, Source, built_in, keep_last_good, load_folder, parse_set,
        slug, stamp,
    };

    fn keys(value: &Value) -> HashSet<String> {
        value.as_object().map(|object| object.keys().cloned().collect()).unwrap_or_default()
    }

    #[test]
    fn built_in_themes_parse_with_unique_names() {
        let mut names = HashSet::new();
        for text in BUILT_IN {
            for entry in parse_set(text, Source::BuiltIn).unwrap() {
                assert!(names.insert(entry.config.name.to_lowercase()), "{} is there twice", entry.config.name);
            }
        }
        let themes = built_in();
        let dark = &themes.barsql(ThemeMode::Dark).config;
        assert_eq!((dark.name.as_ref(), dark.mode), ("BarSQL Dark", ThemeMode::Dark));
        let light = &themes.barsql(ThemeMode::Light).config;
        assert_eq!((light.name.as_ref(), light.mode), ("BarSQL Light", ThemeMode::Light));
    }

    #[test]
    fn built_in_names_match_the_spec() {
        let names: Vec<String> = built_in().all().map(|entry| entry.config.name.to_string()).collect();
        let expected = [
            "BarSQL Dark",
            "BarSQL Light",
            "Catppuccin Latte",
            "Catppuccin Frappé",
            "Catppuccin Macchiato",
            "Catppuccin Mocha",
            "One Dark",
            "One Light",
            "Dracula",
            "Tokyo Night",
            "Tokyo Night Light",
            "Nord",
            "Gruvbox Dark",
            "Gruvbox Light",
            "Solarized Dark",
            "Solarized Light",
            "VS Code Dark Modern",
            "VS Code Dark+",
            "VS Code Light Modern",
            "VS Code Light+",
        ];
        assert_eq!(names, expected);
    }

    #[test]
    fn built_in_themes_set_the_same_keys_as_barsql() {
        let themes = built_in();
        let highlight = |json: &Value| {
            let mut keys = keys(&json["highlight"]);
            keys.remove("syntax");
            keys
        };
        for entry in themes.all() {
            let (name, base) = (&entry.config.name, &themes.barsql(entry.config.mode).json);
            assert_eq!(keys(&entry.json["colors"]), keys(&base["colors"]), "{name}: colors");
            assert_eq!(highlight(&entry.json), highlight(base), "{name}: highlight");
            let syntax = |json: &Value| keys(&json["highlight"]["syntax"]);
            assert_eq!(syntax(&entry.json), syntax(base), "{name}: syntax");
        }
    }

    #[test]
    fn built_in_themes_only_use_keys_gpui_kit_reads() {
        for entry in built_in().all() {
            let unknown = |section: &Value, known: &HashSet<String>| {
                let mut unknown: Vec<String> = keys(section).into_iter().filter(|key| !known.contains(key)).collect();
                unknown.retain(|key| key != "syntax");
                unknown.sort();
                unknown
            };
            let (name, json) = (&entry.config.name, &entry.json);
            assert_eq!(unknown(&json["colors"], &KNOWN.colors), Vec::<String>::new(), "{name}: colors");
            assert_eq!(unknown(&json["highlight"], &KNOWN.highlight), Vec::<String>::new(), "{name}: highlight");
            let syntax = unknown(&json["highlight"]["syntax"], &KNOWN.syntax);
            assert_eq!(syntax, Vec::<String>::new(), "{name}: syntax");
        }
    }

    // What Customize current theme… writes has to load back as the same theme.
    #[test]
    fn built_in_themes_load_back_unchanged() {
        for entry in built_in().all() {
            let mut json = (*entry.json).clone();
            json["name"] = "Copy".into();
            let dir = folder(&[("copy.json", &serde_json::json!({"name": "Copy", "themes": [json]}).to_string())]);
            let load = load(dir.path());
            assert!(load.reports.is_empty(), "{}: {:?}", entry.config.name, load.reports);
            assert_eq!(*load.entries[0].json, json, "{}", entry.config.name);
        }
    }

    fn rgba(value: &Value) -> [f32; 4] {
        let color = try_parse_color(value.as_str().expect("a color string")).expect("a color").to_rgb();
        [color.r, color.g, color.b, color.a]
    }

    fn over(top: [f32; 4], base: [f32; 4]) -> [f32; 4] {
        let mix = |i: usize| top[i] * top[3] + base[i] * (1. - top[3]);
        [mix(0), mix(1), mix(2), 1.]
    }

    // WCAG 2 relative luminance and contrast ratio.
    fn luminance(color: [f32; 4]) -> f32 {
        let channel = |v: f32| if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
        0.2126 * channel(color[0]) + 0.7152 * channel(color[1]) + 0.0722 * channel(color[2])
    }

    fn ratio(text: [f32; 4], background: [f32; 4]) -> f32 {
        let (a, b) = (luminance(over(text, background)), luminance(background));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    const SURFACES: [&str; 3] = ["background", "sidebar.background", "popover.background"];

    #[test]
    fn built_in_themes_are_readable() {
        for entry in built_in().all() {
            let (colors, highlight) = (&entry.json["colors"], &entry.json["highlight"]);
            let c = |key: &str| rgba(&colors[key]);
            let editor = rgba(&highlight["editor.background"]);
            // GPUI Kit shows a selection at 30% opacity at most.
            let mut selection = c("selection.background");
            selection[3] = selection[3].min(0.3);
            let mut pairs = vec![
                ("foreground on background".to_string(), c("foreground"), c("background"), 7.),
                ("sidebar text".into(), c("sidebar.foreground"), c("sidebar.background"), 6.),
                ("popover text".into(), c("popover.foreground"), c("popover.background"), 6.),
                ("table header text".into(), c("table.head.foreground"), c("table.head.background"), 6.),
                (
                    "text on an active list row".into(),
                    c("foreground"),
                    over(c("list.active.background"), c("list.background")),
                    4.5,
                ),
                (
                    "text on an active table row".into(),
                    c("foreground"),
                    over(c("table.active.background"), c("table.background")),
                    4.5,
                ),
                ("editor text".into(), rgba(&highlight["editor.foreground"]), editor, 7.),
                ("primary as text on background".into(), c("primary.background"), c("background"), 3.),
                ("primary as text on a popover".into(), c("primary.background"), c("popover.background"), 3.),
                (
                    "primary as text on a selected completion".into(),
                    c("primary.background"),
                    over(c("list.active.background"), c("popover.background")),
                    2.5,
                ),
                ("muted text on background".into(), c("muted.foreground"), c("background"), 4.5),
                ("muted text on the sidebar".into(), c("muted.foreground"), c("sidebar.background"), 4.),
                ("muted text on a popover".into(), c("muted.foreground"), c("popover.background"), 4.),
                ("an inactive tab's text".into(), c("tab.foreground"), c("tab_bar.background"), 4.),
                ("line numbers".into(), rgba(&highlight["editor.line_number"]), editor, 3.),
                // BarSQL draws its panels, grid and buttons with 1px lines, so those have to show too.
                ("borders on background".into(), c("border"), c("background"), 1.4),
                ("borders on the sidebar".into(), c("sidebar.border"), c("sidebar.background"), 1.3),
                ("borders on a popover".into(), c("border"), c("popover.background"), 1.3),
                ("a hovered row on background".into(), c("list.hover.background"), c("background"), 1.1),
                ("the scrollbar thumb".into(), c("scrollbar.thumb.background"), c("background"), 1.5),
                ("a selection in the editor".into(), selection, editor, 1.3),
                (
                    "editor text on a selection".into(),
                    rgba(&highlight["editor.foreground"]),
                    over(selection, editor),
                    3.,
                ),
            ];
            // Status and icon colors are also drawn as text and icons, on any of the three surfaces.
            let marks = [
                "danger.background",
                "warning.background",
                "success.background",
                "base.yellow",
                "base.blue",
                "base.magenta",
                "link",
            ];
            for (mark, surface) in marks.iter().flat_map(|mark| SURFACES.map(|surface| (mark, surface))) {
                pairs.push((format!("{mark} as text on {surface}"), c(mark), c(surface), 3.));
            }
            // A badge is its color as text over a tint of the same color.
            let badges = ["danger.background", "warning.background", "success.background", "primary.background"];
            for (badge, surface) in badges.iter().flat_map(|badge| SURFACES.map(|surface| (badge, surface))) {
                let [r, g, b, _] = c(badge);
                let tinted = over([r, g, b, crate::tokens::TINT], c(surface));
                pairs.push((format!("{badge} as a badge on {surface}"), c(badge), tinted, 3.));
            }
            for button in ["primary", "danger", "secondary"] {
                let text = c(&format!("{button}.foreground"));
                for state in ["", "hover.", "active."] {
                    let fill = c(&format!("{button}.{state}background"));
                    // Most buttons are secondary ones, so at rest theirs reads as body text does.
                    let floor = if button == "secondary" && state.is_empty() { 4.5 } else { 3. };
                    pairs.push((format!("{button} button text on its {state}background"), text, fill, floor));
                }
            }
            for (key, style) in highlight["syntax"].as_object().expect("syntax colors") {
                pairs.push((format!("syntax {key}"), rgba(&style["color"]), editor, 3.));
            }
            // Rows and the current line are fills under text, so text has to hold on them too.
            let rows = [
                over(c("list.hover.background"), c("background")),
                over(c("table.hover.background"), c("table.background")),
                over(c("sidebar.accent.background"), c("sidebar.background")),
            ];
            let accents = SURFACES.map(|surface| over(c("accent.background"), c(surface)));
            for row in rows.into_iter().chain(accents) {
                pairs.push(("text on a hovered row".into(), c("foreground"), row, 4.));
                pairs.push(("muted text on a hovered row".into(), c("muted.foreground"), row, 3.5));
            }
            for surface in ["table.background", "table.head.background", "secondary.background"] {
                pairs.push((format!("muted text on {surface}"), c("muted.foreground"), c(surface), 4.));
            }
            for surface in ["sidebar.background", "table.background", "popover.background"] {
                let thumb = c("scrollbar.thumb.background");
                pairs.push((format!("the scrollbar thumb on {surface}"), thumb, c(surface), 1.4));
            }
            let current_line = over(rgba(&highlight["editor.active_line.background"]), editor);
            let comment = rgba(&highlight["syntax"]["comment"]["color"]);
            pairs.extend([
                ("the caret".to_string(), c("caret"), editor, 3.),
                ("the current line's number".into(), rgba(&highlight["editor.active_line_number"]), editor, 4.5),
                ("editor text on the current line".into(), rgba(&highlight["editor.foreground"]), current_line, 4.5),
                ("a comment on the current line".into(), comment, current_line, 3.),
            ]);
            for (pair, text, background, floor) in pairs {
                let ratio = ratio(text, background);
                assert!(ratio >= floor, "{}: {pair} is {ratio:.2}:1, under {floor}:1", entry.config.name);
            }
        }
    }

    // GPUI Kit caps some opacities and fills in what a theme leaves out, so this reads what it resolves.
    #[test]
    fn resolved_themes_keep_selections_and_button_states_readable() {
        use crate::test_support::contrast;
        for entry in built_in().all() {
            let mut theme = Theme::default();
            theme.apply_config(&entry.config);
            let name = &entry.config.name;
            let editor = theme.highlight_theme.style.editor_background.expect("an editor background");
            let selection = contrast(theme.selection, editor);
            assert!(selection >= 1.3, "{name}: a selection is {selection:.2}:1 against the editor");
            let buttons = [
                ("primary", theme.primary_foreground, [theme.primary, theme.primary_hover, theme.primary_active]),
                ("danger", theme.danger_foreground, [theme.danger, theme.danger_hover, theme.danger_active]),
                (
                    "secondary",
                    theme.secondary_foreground,
                    [theme.secondary, theme.secondary_hover, theme.secondary_active],
                ),
            ];
            for (button, text, fills) in buttons {
                for (state, fill) in ["at rest", "hovered", "pressed"].into_iter().zip(fills) {
                    let ratio = contrast(text, fill);
                    assert!(ratio >= 3., "{name}: {button} button text {state} is {ratio:.2}:1");
                }
            }
        }
    }

    #[test]
    fn every_theme_gets_barsql_fonts_and_shape() {
        let set = r##"{"name": "Odd", "themes": [{"name": "Odd", "mode": "dark", "font.family": "Comic Sans",
            "mono_font.size": 20, "radius": 0, "radius.lg": 12, "shadow": true}]}"##;
        let odd = parse_set(set, Source::BuiltIn).unwrap();
        let themes = built_in();
        for entry in odd.iter().chain(themes.all()) {
            let config = &entry.config;
            let fonts = (config.font_family.as_deref(), config.mono_font_family.as_deref());
            assert_eq!(fonts, (Some("Inter"), Some("Fira Code")), "{}", config.name);
            assert_eq!((config.font_size, config.mono_font_size), (Some(13.), Some(13.)), "{}", config.name);
            let shape = (config.radius, config.radius_lg, config.shadow);
            assert_eq!(shape, (Some(6), Some(6), Some(false)), "{}", config.name);
        }
    }

    fn folder(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (name, text) in files {
            fs::write(dir.path().join(name), text).unwrap();
        }
        dir
    }

    fn set(themes: &str) -> String {
        format!(r#"{{"name": "Set", "themes": [{themes}]}}"#)
    }

    fn load(dir: &Path) -> FolderLoad {
        load_folder(dir, &built_in())
    }

    fn names(load: &FolderLoad) -> Vec<String> {
        load.entries.iter().map(|entry| entry.config.name.to_string()).collect()
    }

    fn problems(load: &FolderLoad, file: &str) -> Vec<Problem> {
        let report = load.reports.iter().find(|report| report.path.file_name().unwrap() == file);
        report.map(|report| report.problems.clone()).unwrap_or_default()
    }

    fn barsql_syntax(mode: ThemeMode) -> Value {
        built_in().barsql(mode).json["highlight"]["syntax"].clone()
    }

    #[test]
    fn a_partial_theme_fills_syntax_from_barsql_and_ignores_fonts() {
        let theme = r##"{"name": "Mine", "mode": "dark", "colors": {"background": "#101010"},
            "highlight": {"syntax": {"keyword": {"color": "#ff0000"}}}, "font.family": "Comic Sans"}"##;
        let dir = folder(&[("mine.json", &set(theme))]);
        let load = load(dir.path());
        assert!(load.reports.is_empty(), "{:?}", load.reports);
        let entry = &load.entries[0];
        assert_eq!(entry.config.font_family.as_deref(), Some("Inter"));
        let mut expected = barsql_syntax(ThemeMode::Dark);
        expected["keyword"] = serde_json::json!({"color": "#ff0000"});
        assert_eq!(entry.json["highlight"]["syntax"], expected);
        assert_eq!(entry.source, Source::File(dir.path().join("mine.json")));
    }

    #[test]
    fn a_syntax_key_covers_the_keys_under_it() {
        let theme = r##"{"name": "P", "mode": "dark", "highlight": {"syntax": {
            "punctuation": {"color": "#ff0000"}, "comment": {"color": "#0000ff"}}}}"##;
        let dir = folder(&[("p.json", &set(theme))]);
        let load = load(dir.path());
        let syntax = &load.entries[0].config.highlight.as_ref().expect("a highlight section").syntax;
        let color = |name: &str| syntax.style(name).and_then(|style| style.color);
        let (red, blue) = (try_parse_color("#ff0000").ok(), try_parse_color("#0000ff").ok());
        assert_eq!((color("punctuation.bracket"), color("punctuation.delimiter")), (red, red));
        assert_eq!(color("comment.doc"), blue);
        let barsql = barsql_syntax(ThemeMode::Dark);
        assert_eq!(load.entries[0].json["highlight"]["syntax"]["string.special"], barsql["string.special"]);
    }

    #[test]
    fn a_gradient_loads_only_where_gpui_kit_paints_one() {
        let theme = r##"{"name": "G", "mode": "dark", "colors": {
            "primary.background": "linear-gradient(135deg, #000000, #ffffff)",
            "primary.foreground": "linear-gradient(135deg, #000000, #ffffff)"}}"##;
        let dir = folder(&[("g.json", &set(theme))]);
        let load = load(dir.path());
        let bad = Problem::BadValue { theme: "G".into(), key: r#"colors["primary.foreground"]"#.into() };
        assert_eq!(problems(&load, "g.json"), [bad]);
        assert_eq!(keys(&load.entries[0].json["colors"]), HashSet::from(["primary.background".to_string()]));
    }

    #[test]
    fn a_theme_without_highlight_gets_barsql_syntax() {
        let dir = folder(&[("plain.json", &set(r#"{"name": "Plain", "mode": "light"}"#))]);
        let load = load(dir.path());
        assert_eq!(load.entries[0].json["highlight"]["syntax"], barsql_syntax(ThemeMode::Light));
        assert!(load.entries[0].config.highlight.is_some());
    }

    #[test]
    fn broken_json_reports_line_and_column_and_others_still_load() {
        let broken = "{\n  \"name\": \"A\",\n  \"themes\": [}\n}\n";
        let dir = folder(&[("a.json", broken), ("b.json", &set(r#"{"name": "B", "mode": "dark"}"#))]);
        let load = load(dir.path());
        assert_eq!(names(&load), ["B"]);
        let [Problem::NotJson { line, message, .. }] = &problems(&load, "a.json")[..] else {
            panic!("{:?}", load.reports);
        };
        assert_eq!(*line, 3);
        assert!(!message.contains("at line"), "{message}");
        assert!(!load.reports[0].loaded);
    }

    #[test]
    fn bad_colors_and_unknown_keys_are_reported_and_removed() {
        let theme = r##"{"name": "Odd", "mode": "dark",
            "colors": {"sidebar.background": "#12345", "backgrund": "#000", "border": "#333333"},
            "highlight": {"syntax": {"keyword": {"color": "nope"}}}}"##;
        let dir = folder(&[("odd.json", &set(theme))]);
        let load = load(dir.path());
        let (theme, key) = ("Odd".to_string(), |key: &str| key.to_string());
        assert_eq!(
            problems(&load, "odd.json"),
            [
                Problem::BadValue { theme: theme.clone(), key: key(r#"colors["sidebar.background"]"#) },
                Problem::UnknownKey { theme: theme.clone(), key: key(r#"colors["backgrund"]"#) },
                Problem::BadValue { theme, key: key(r#"syntax["keyword"]"#) },
            ]
        );
        let entry = &load.entries[0];
        assert_eq!(keys(&entry.json["colors"]), HashSet::from(["border".to_string()]));
        assert_eq!(entry.json["highlight"]["syntax"]["keyword"], barsql_syntax(ThemeMode::Dark)["keyword"]);
        assert!(load.reports[0].loaded);
    }

    #[test]
    fn names_clash_with_built_ins_and_earlier_themes() {
        let a = set(r#"{"name": " barsql dark ", "mode": "dark"}, {"name": "Mine", "mode": "dark"},
            {"name": "mine", "mode": "light"}, {"name": "  ", "mode": "dark"}"#);
        let dir = folder(&[("a.json", &a), ("b.json", &set(r#"{"name": "MINE", "mode": "dark"}"#))]);
        let load = load(dir.path());
        assert_eq!(names(&load), ["Mine"]);
        assert_eq!(
            problems(&load, "a.json"),
            [Problem::BuiltInName("barsql dark".into()), Problem::DuplicateName("mine".into()), Problem::MissingName]
        );
        assert_eq!(problems(&load, "b.json"), [Problem::DuplicateName("MINE".into())]);
    }

    #[test]
    fn a_theme_without_mode_is_skipped() {
        let dir = folder(&[("m.json", &set(r#"{"name": "Moody"}, {"name": "Odd", "mode": "dim"}"#))]);
        let load = load(dir.path());
        assert!(load.entries.is_empty());
        let expected = [Problem::MissingMode("Moody".into()), Problem::MissingMode("Odd".into())];
        assert_eq!(problems(&load, "m.json"), expected);
    }

    #[test]
    fn editor_leftovers_and_subfolders_are_ignored() {
        let theme = set(r#"{"name": "Upper", "mode": "dark"}"#);
        let other = set(r#"{"name": "Leftover", "mode": "dark"}"#);
        let files =
            [("UPPER.JSON", &theme), ("mine.json~", &other), ("mine.json.tmp", &other), (".mine.json.swp", &other)];
        let dir = folder(&files.map(|(name, text)| (name, text.as_str())));
        fs::create_dir(dir.path().join("old")).unwrap();
        fs::write(dir.path().join("old").join("x.json"), &other).unwrap();
        let load = load(dir.path());
        assert_eq!(names(&load), ["Upper"]);
        assert!(load.reports.is_empty(), "{:?}", load.reports);
    }

    #[test]
    fn editor_colors_in_a_user_theme_load() {
        let theme = r##"{"name": "Ed", "mode": "dark",
            "highlight": {"editor.background": "#101010", "error.background": "#ff000022"}}"##;
        let dir = folder(&[("ed.json", &set(theme))]);
        let load = load(dir.path());
        assert!(load.reports.is_empty(), "{:?}", load.reports);
        let highlight = load.entries[0].config.highlight.as_ref().expect("a highlight section");
        assert_eq!(highlight.editor_background, Some(try_parse_color("#101010").unwrap()));
    }

    // macOS writes `._name.json` beside each file on FAT, exFAT and SMB volumes.
    #[test]
    fn hidden_files_are_ignored() {
        let dir = folder(&[("mine.json", &set(r#"{"name": "Mine", "mode": "dark"}"#))]);
        fs::write(dir.path().join("._mine.json"), [0u8, 5, 22, 7, 0xff, 0xfe]).unwrap();
        let load = load(dir.path());
        assert_eq!(names(&load), ["Mine"]);
        assert!(load.reports.is_empty(), "{:?}", load.reports);
    }

    #[test]
    fn a_file_with_a_bom_loads() {
        let dir = folder(&[("bom.json", &format!("\u{feff}{}", set(r#"{"name": "Bom", "mode": "dark"}"#)))]);
        let load = load(dir.path());
        assert_eq!(names(&load), ["Bom"]);
        assert!(load.reports.is_empty(), "{:?}", load.reports);
    }

    #[test]
    fn files_over_1_mb_are_skipped_with_a_notice() {
        let big = format!("{}{}", set(r#"{"name": "Big", "mode": "dark"}"#), " ".repeat(MAX_FILE_BYTES as usize));
        let dir = folder(&[("big.json", &big)]);
        let load = load(dir.path());
        assert!(load.entries.is_empty());
        assert_eq!(problems(&load, "big.json"), [Problem::TooBig]);
        assert!(!load.reports[0].loaded);
    }

    #[test]
    fn stamp_changes_when_a_file_is_added_edited_or_removed() {
        let dir = folder(&[("a.json", "{}")]);
        let first = stamp(dir.path());
        assert_eq!(stamp(dir.path()), first, "nothing changed");
        fs::write(dir.path().join("b.json"), "{}").unwrap();
        let added = stamp(dir.path());
        assert_ne!(added, first);
        fs::write(dir.path().join("a.json"), "{ }").unwrap();
        let edited = stamp(dir.path());
        assert_ne!(edited, added);
        fs::remove_file(dir.path().join("b.json")).unwrap();
        assert_ne!(stamp(dir.path()), edited);
    }

    #[test]
    fn last_good_version_stays_when_a_file_breaks() {
        let dir = folder(&[("a.json", &set(r#"{"name": "Keep", "mode": "dark"}"#))]);
        let before = load(dir.path());
        fs::write(dir.path().join("a.json"), "{ broken").unwrap();
        let mut after = load(dir.path());
        assert!(after.entries.is_empty() && !after.reports[0].loaded);
        keep_last_good(&before.entries, &mut after);
        assert_eq!(names(&after), ["Keep"]);
    }

    #[test]
    fn the_guide_example_loads_cleanly() {
        let guide = include_str!("../../../guide/themes.md");
        let example = guide.split("```json").nth(1).and_then(|rest| rest.split("```").next()).expect("a json example");
        let dir = folder(&[("example.json", example)]);
        let load = load(dir.path());
        assert!(load.reports.is_empty(), "{:?}", load.reports);
        assert_eq!(names(&load).len(), 1);
        let colors = &load.entries[0].json["colors"];
        let button = ratio(rgba(&colors["primary.foreground"]), rgba(&colors["primary.background"]));
        assert!(button >= 4.5, "the example's button text is {button:.2}:1");
    }

    #[test]
    fn slugs_keep_letters_and_digits() {
        assert_eq!(slug("Dracula (custom)"), "dracula-custom");
        assert_eq!(slug("Catppuccin Frappé (custom 2)"), "catppuccin-frappé-custom-2");
        assert_eq!(slug("  ** "), "theme");
    }

    #[test]
    fn an_emptied_file_keeps_its_last_themes() {
        let dir = folder(&[("a.json", &set(r#"{"name": "Keep", "mode": "dark"}"#))]);
        let before = load(dir.path());
        fs::write(dir.path().join("a.json"), r#"{"name": "A", "themes": []}"#).unwrap();
        let mut after = load(dir.path());
        assert_eq!(problems(&after, "a.json"), [Problem::NoThemes]);
        assert!(!after.reports[0].loaded);
        keep_last_good(&before.entries, &mut after);
        assert_eq!(names(&after), ["Keep"]);
    }

    #[test]
    fn no_folder_means_no_themes_and_no_problems() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("themes");
        let load = load(&missing);
        assert!(load.entries.is_empty() && load.reports.is_empty());
        assert!(!missing.exists(), "loading doesn't create the folder");
    }
}
