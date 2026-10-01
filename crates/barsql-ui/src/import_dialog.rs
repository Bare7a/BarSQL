use std::rc::Rc;
use std::time::Duration;

use barsql_app::{
    CsvImportRequest, CsvOptions, ImportEvent, ImportPreview, ImportProgress, ImportResult, SqlImportRequest,
};
use barsql_core::ConnectionConfig;
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme, Disableable, Icon, WindowExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::file_dialogs::pick_import_file;
use crate::form::{self, SelectMenu};
use crate::i18n::{count, t, t_with};
use crate::modal::{self, Modal};
use crate::query_tab::new_tab_id;
use crate::scrollbars::ScrollbarsOnHover as _;
use crate::spinner::Spinner;
use crate::tokens::{DIMMED, ICON_XS, RADIUS, TEXT_2XS, TEXT_BASE, TEXT_SM, TEXT_XS};
use crate::{schema, state};

const PREVIEW_DEBOUNCE: Duration = Duration::from_millis(200);
const DELIMITERS: [(&str, &str); 5] = [
    ("", "import.delimiterAuto"),
    (",", "import.delimiterComma"),
    (";", "import.delimiterSemicolon"),
    ("\t", "import.delimiterTab"),
    ("|", "import.delimiterPipe"),
];
const COLUMN_TYPES: [&str; 6] = ["text", "int", "float", "bool", "date", "timestamp"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Csv,
    Sql,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    New,
    Existing,
}

// Drops the extension and turns anything but ASCII letters, digits and _ into _.
pub fn table_name_for(path: &str) -> String {
    let base = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let stem = match base.rfind('.') {
        Some(dot) if dot + 1 < base.len() => &base[..dot],
        _ => base,
    };
    stem.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' }).collect()
}

// Stopping an import keeps whatever was already loaded.
pub struct ImportDialog {
    connection: ConnectionConfig,
    schema: String,
    tables: Vec<String>,
    kind: Kind,
    path: String,
    preview: Option<ImportPreview>,
    preview_error: Option<SharedString>,
    previewing: bool,
    has_header: bool,
    delimiter: &'static str,
    trim_space: bool,
    null_literal: Entity<InputState>,
    skip_rows: Entity<InputState>,
    target: Target,
    existing: String,
    new_table: Entity<InputState>,
    truncate: bool,
    stop_on_error: bool,
    mapping: Vec<Entity<InputState>>,
    column_types: Vec<String>,
    import_id: Option<String>,
    running: Option<Task<()>>,
    progress: Option<ImportProgress>,
    result: Option<ImportResult>,
    error: Option<SharedString>,
    preview_task: Option<Task<()>>,
    on_imported: Rc<dyn Fn(&mut App)>,
    scroll: ScrollHandle,
    errors_scroll: ScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl ImportDialog {
    fn new(
        connection: ConnectionConfig,
        schema: String,
        tables: Vec<String>,
        table: Option<String>,
        on_imported: Rc<dyn Fn(&mut App)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let null_literal = cx.new(|cx| InputState::new(window, cx).placeholder(t(cx, "import.nullPlaceholder")));
        let skip_rows = cx.new(|cx| InputState::new(window, cx).default_value("0"));
        let new_table = cx.new(|cx| InputState::new(window, cx));
        let reload = |this: &mut Self,
                      _: &Entity<InputState>,
                      event: &InputEvent,
                      window: &mut Window,
                      cx: &mut Context<Self>| {
            if matches!(event, InputEvent::Change) {
                this.schedule_preview(window, cx);
            }
        };
        let subscriptions =
            vec![cx.subscribe_in(&null_literal, window, reload), cx.subscribe_in(&skip_rows, window, reload)];
        Self {
            connection,
            schema,
            target: if table.is_some() { Target::Existing } else { Target::New },
            existing: table.or_else(|| tables.first().cloned()).unwrap_or_default(),
            tables,
            kind: Kind::Csv,
            path: String::new(),
            preview: None,
            preview_error: None,
            previewing: false,
            has_header: true,
            delimiter: "",
            trim_space: false,
            null_literal,
            skip_rows,
            new_table,
            truncate: false,
            stop_on_error: false,
            mapping: Vec::new(),
            column_types: Vec::new(),
            import_id: None,
            running: None,
            progress: None,
            result: None,
            error: None,
            preview_task: None,
            on_imported,
            scroll: ScrollHandle::new(),
            errors_scroll: ScrollHandle::new(),
            _subscriptions: subscriptions,
        }
    }

    // Follows the schema cache, which may still be loading.
    fn sync_tables(&mut self, cx: &App) {
        let Some(live) = schema::get(cx, &self.connection.id).and_then(|entry| entry.tables(&self.schema)) else {
            return;
        };
        let names: Vec<String> = live.iter().map(|table| table.name.clone()).collect();
        if names != self.tables {
            self.tables = names;
            if self.existing.is_empty() {
                self.existing = self.tables.first().cloned().unwrap_or_default();
            }
        }
    }

    fn options(&self, cx: &App) -> CsvOptions {
        let skip = self.skip_rows.read(cx).value().trim().parse::<usize>().unwrap_or(0);
        CsvOptions {
            delimiter: self.delimiter.to_string(),
            has_header: self.has_header,
            null_literal: self.null_literal.read(cx).value().to_string(),
            skip_rows: skip,
            trim_space: self.trim_space,
        }
    }

    fn reset_run(&mut self) {
        self.progress = None;
        self.result = None;
        self.error = None;
    }

    fn set_kind(&mut self, kind: Kind, cx: &mut Context<Self>) {
        self.kind = kind;
        self.path.clear();
        self.preview = None;
        self.preview_error = None;
        self.reset_run();
        cx.notify();
    }

    fn pick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picked = pick_import_file(self.kind, cx);
        cx.spawn_in(window, async move |this, cx| {
            let Some(path) = picked.await else { return };
            this.update_in(cx, |this, window, cx| this.choose(path.display().to_string(), window, cx)).ok();
        })
        .detach();
    }

    pub(crate) fn choose(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        self.reset_run();
        self.path = path;
        self.preview = None;
        if self.kind == Kind::Csv {
            self.load_preview(window, cx);
        }
        cx.notify();
    }

    #[cfg(feature = "snapshot")]
    pub(crate) fn preview_loaded(&self) -> bool {
        self.preview.is_some()
    }

    fn schedule_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.kind != Kind::Csv || self.path.is_empty() {
            return;
        }
        self.preview_task = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(PREVIEW_DEBOUNCE).await;
            this.update_in(cx, |this, window, cx| this.load_preview(window, cx)).ok();
        }));
    }

    // Same reader and options as the import, so the sample matches what it will read.
    fn load_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.previewing = true;
        self.preview_error = None;
        let bar = state::bar(cx);
        let (connection, path, options) = (self.connection.id.clone(), self.path.clone(), self.options(cx));
        let load = state::spawn(cx, async move { bar.preview_import_file(&connection, &path, &options) });
        self.preview_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = load.await;
            this.update_in(cx, |this, window, cx| {
                this.previewing = false;
                match result {
                    Some(Ok(preview)) => this.show_preview(preview, window, cx),
                    Some(Err(error)) => {
                        this.preview = None;
                        this.preview_error = Some(error.message.into());
                    }
                    None => {}
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn show_preview(&mut self, preview: ImportPreview, window: &mut Window, cx: &mut Context<Self>) {
        self.mapping = preview
            .columns
            .iter()
            .map(|column| {
                let column = column.clone();
                cx.new(|cx| InputState::new(window, cx).placeholder(t(cx, "import.skipColumn")).default_value(column))
            })
            .collect();
        self.column_types = preview.inferred_types.clone();
        if self.new_table.read(cx).value().is_empty() && !preview.columns.is_empty() {
            let name = table_name_for(&self.path);
            self.new_table.update(cx, |state, cx| state.set_value(name, window, cx));
        }
        self.preview = Some(preview);
    }

    fn target_table(&self, cx: &App) -> String {
        match self.target {
            Target::New => self.new_table.read(cx).value().trim().to_string(),
            Target::Existing => self.existing.clone(),
        }
    }

    fn mapping_values(&self, cx: &App) -> Vec<String> {
        self.mapping.iter().map(|input| input.read(cx).value().to_string()).collect()
    }

    fn can_run(&self, cx: &App) -> bool {
        if self.running.is_some() || self.path.is_empty() {
            return false;
        }
        if self.kind == Kind::Sql {
            return true;
        }
        let mapped = self.mapping_values(cx).iter().filter(|m| !m.trim().is_empty()).count();
        self.preview.is_some() && !self.target_table(cx).is_empty() && mapped > 0
    }

    fn run(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_run(cx) {
            return;
        }
        self.reset_run();
        let id = new_tab_id();
        self.import_id = Some(id.clone());
        let bar = state::bar(cx);
        let connection = self.connection.id.clone();
        let start = match self.kind {
            Kind::Sql => {
                let request = SqlImportRequest { path: self.path.clone(), stop_on_error: self.stop_on_error };
                state::spawn(cx, async move { bar.import_sql(&connection, &id, request).await })
            }
            Kind::Csv => {
                let request = CsvImportRequest {
                    path: self.path.clone(),
                    schema: self.schema.clone(),
                    table: self.target_table(cx),
                    create_table: self.target == Target::New,
                    truncate: self.target == Target::Existing && self.truncate,
                    options: self.options(cx),
                    mapping: self.mapping_values(cx),
                    column_types: self.column_types.clone(),
                    batch_size: 0,
                    stop_on_error: self.stop_on_error,
                };
                state::spawn(cx, async move { bar.import_csv(&connection, &id, request).await })
            }
        };
        self.running = Some(cx.spawn_in(window, async move |this, cx| {
            match start.await {
                Some(Ok(handle)) => {
                    while let Ok(event) = handle.events.recv().await {
                        let done = matches!(event, ImportEvent::Done(_));
                        if this.update(cx, |this, cx| this.apply(event, cx)).is_err() || done {
                            break;
                        }
                    }
                }
                Some(Err(error)) => {
                    this.update(cx, |this, _| this.error = Some(error.message.into())).ok();
                }
                None => {}
            }
            this.update(cx, |this, cx| {
                this.running = None;
                this.import_id = None;
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn apply(&mut self, event: ImportEvent, cx: &mut Context<Self>) {
        match event {
            ImportEvent::Progress(progress) => self.progress = Some(progress),
            ImportEvent::Done(done) if !done.error.is_empty() => self.error = Some(done.error.into()),
            ImportEvent::Done(done) => {
                if let Some(result) = &done.result
                    && !result.cancelled
                    && (result.inserted > 0 || result.statements > 0)
                {
                    (self.on_imported)(cx);
                }
                self.result = done.result;
            }
        }
        cx.notify();
    }

    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match &self.import_id {
            Some(id) if self.running.is_some() => {
                state::bar(cx).cancel_query(id);
            }
            _ => window.close_dialog(cx),
        }
    }

    fn progress_view(&self, cx: &App) -> impl IntoElement {
        let p = self.progress.clone().unwrap_or(ImportProgress {
            total_bytes: self.preview.as_ref().map_or(0, |p| p.total_bytes as i64),
            total_rows: if self.kind == Kind::Csv {
                self.preview.as_ref().map_or(0, |p| p.total_rows as i64)
            } else {
                0
            },
            ..Default::default()
        });
        let n = |value: i64| count(cx, value.max(0) as usize);
        let (label, value, max) = match self.kind {
            Kind::Sql => (
                t_with(cx, "import.progressStatements", &[("done", &n(p.processed)), ("total", &n(p.total_bytes))]),
                p.processed,
                p.total_bytes,
            ),
            Kind::Csv if p.total_rows > 0 => (
                t_with(
                    cx,
                    "import.progressRowsOf",
                    &[("done", &n(p.processed.min(p.total_rows))), ("total", &n(p.total_rows))],
                ),
                p.processed.min(p.total_rows),
                p.total_rows,
            ),
            Kind::Csv => (t_with(cx, "import.progressRows", &[("rows", &n(p.processed))]), p.bytes_read, p.total_bytes),
        };
        form::progress(label, if max > 0 { value as f32 / max as f32 } else { 0. }, cx)
    }

    fn checkbox(
        &self,
        id: &'static str,
        key: &str,
        checked: bool,
        set: fn(&mut Self, bool),
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let running = self.running.is_some();
        form::checkbox(id, checked, t(cx, key), running, cx).debug_selector(move || id.into()).when(!running, |el| {
            el.on_click(cx.listener(move |this, _, window, cx| {
                set(this, !checked);
                this.schedule_preview(window, cx);
                cx.notify();
            }))
        })
    }

    fn csv_fields(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let running = self.running.is_some();
        let target: AnyElement = match self.target {
            Target::New => form::group(
                t(cx, "import.newTableName"),
                form::input(&self.new_table, window, cx).disabled(running),
                cx,
            )
            .child(form::hint(t(cx, "import.newTableHint"), cx))
            .into_any_element(),
            Target::Existing => {
                let this = cx.entity().downgrade();
                let (tables, current) = (self.tables.clone(), self.existing.clone());
                let table = form::select("import-existing-table", current.clone(), cx)
                    .debug_selector(|| "import-existing-table".into())
                    .disabled(running)
                    .select_menu(window, cx, move |mut menu: PopupMenu, _, _| {
                        for table in tables.clone() {
                            let this = this.clone();
                            let checked = table == current;
                            menu = menu.item(PopupMenuItem::new(table.clone()).checked(checked).on_click(
                                move |_, _, cx| {
                                    this.update(cx, |dialog, cx| {
                                        dialog.existing = table.clone();
                                        cx.notify();
                                    })
                                    .ok();
                                },
                            ));
                        }
                        menu.scrollable(true)
                    });
                v_flex()
                    .gap(rems(0.923))
                    .child(form::group(t(cx, "import.existingTable"), table, cx))
                    .child(
                        v_flex()
                            .child(self.checkbox(
                                "import-truncate",
                                "import.truncate",
                                self.truncate,
                                |d, v| d.truncate = v,
                                cx,
                            ))
                            .child(form::hint(t(cx, "import.truncateHint"), cx).pl(rems(2.))),
                    )
                    .into_any_element()
            }
        };
        let this = cx.entity().downgrade();
        let current = self.delimiter;
        let delimiter_label = DELIMITERS.iter().find(|(value, _)| *value == current).map_or("", |(_, key)| key);
        let delimiter = form::select("import-delimiter", t(cx, delimiter_label), cx)
            .debug_selector(|| "import-delimiter".into())
            .disabled(running)
            .select_menu(window, cx, move |mut menu: PopupMenu, _, cx| {
                for (value, key) in DELIMITERS {
                    let this = this.clone();
                    menu = menu.item(PopupMenuItem::new(t(cx, key)).checked(value == current).on_click(
                        move |_, window, cx| {
                            this.update(cx, |dialog, cx| {
                                dialog.delimiter = value;
                                dialog.schedule_preview(window, cx);
                                cx.notify();
                            })
                            .ok();
                        },
                    ));
                }
                menu
            });
        let fluid = |children: Vec<AnyElement>| {
            h_flex()
                .flex_wrap()
                .items_start()
                .gap(rems(0.769))
                .children(children.into_iter().map(|child| div().flex_1().min_w_0().child(child)))
        };
        let options = form::section(cx)
            .child(fluid(vec![
                form::group(t(cx, "import.delimiter"), delimiter, cx).into_any_element(),
                form::group(t(cx, "import.skipRows"), form::input(&self.skip_rows, window, cx).disabled(running), cx)
                    .into_any_element(),
                form::group(
                    t(cx, "import.nullLiteral"),
                    form::input(&self.null_literal, window, cx).disabled(running),
                    cx,
                )
                .into_any_element(),
            ]))
            .child(fluid(vec![
                self.checkbox("import-header", "import.hasHeader", self.has_header, |d, v| d.has_header = v, cx)
                    .into_any_element(),
                self.checkbox("import-trim", "import.trimSpace", self.trim_space, |d, v| d.trim_space = v, cx)
                    .into_any_element(),
                self.checkbox("import-stop", "import.stopOnError", self.stop_on_error, |d, v| d.stop_on_error = v, cx)
                    .into_any_element(),
            ]));
        v_flex().gap(rems(0.923)).child(target).child(options).child(self.preview_table(window, cx))
    }

    fn preview_table(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        if self.previewing {
            return h_flex()
                .gap(rems(0.385))
                .text_size(TEXT_XS)
                .text_color(theme.muted_foreground)
                .child(Spinner::new(ICON_XS))
                .child(t(cx, "import.reading"))
                .into_any_element();
        }
        if let Some(error) = &self.preview_error {
            return form::error(error.clone(), cx).into_any_element();
        }
        let Some(preview) = self.preview.clone() else { return div().into_any_element() };
        let delimiter = if preview.delimiter == "\t" { "\\t".to_string() } else { preview.delimiter.clone() };
        let mut meta = String::new();
        if preview.total_rows > 0 {
            meta =
                format!("{} · ", t_with(cx, "import.fileRows", &[("rows", &count(cx, preview.total_rows as usize))]));
        }
        meta.push_str(&t_with(cx, "import.detectedDelimiter", &[("delimiter", &delimiter)]));
        let new_table = self.target == Target::New;
        let running = self.running.is_some();
        let mono = theme.mono_font_family.clone();
        let cell = || div().flex_1().min_w_0().px(rems(0.615)).py(rems(0.308));
        let heading = |key: &str, cx: &App| cell().child(form::caption(t(cx, key), cx));
        let header = h_flex()
            .bg(theme.secondary)
            .child(heading("import.sourceColumn", cx))
            .child(heading("import.sample", cx))
            .child(heading("import.targetColumn", cx))
            .when(new_table, |el| el.child(heading("import.columnType", cx)));
        let rows: Vec<AnyElement> =
            preview
                .columns
                .iter()
                .enumerate()
                .map(|(ix, column)| {
                    let skipped = self.mapping.get(ix).is_some_and(|input| input.read(cx).value().trim().is_empty());
                    let sample = preview.rows.first().and_then(|row| row.get(ix)).cloned().unwrap_or_default();
                    let kind = self.column_types.get(ix).cloned().unwrap_or_else(|| "text".into());
                    let this = cx.entity().downgrade();
                    let picker = form::select(("import-type", ix), t(cx, &format!("import.type.{kind}")), cx)
                        .disabled(running)
                        .select_menu(window, cx, move |mut menu: PopupMenu, _, cx| {
                            for option in COLUMN_TYPES {
                                let this = this.clone();
                                menu = menu.item(
                                    PopupMenuItem::new(t(cx, &format!("import.type.{option}")))
                                        .checked(option == kind)
                                        .on_click(move |_, _, cx| {
                                            this.update(cx, |dialog, cx| {
                                                if let Some(slot) = dialog.column_types.get_mut(ix) {
                                                    *slot = option.to_string();
                                                }
                                                cx.notify();
                                            })
                                            .ok();
                                        }),
                                );
                            }
                            menu
                        });
                    h_flex()
                        .text_size(TEXT_SM)
                        .border_t_1()
                        .border_color(theme.border)
                        .child(
                            cell()
                                .truncate()
                                .font_family(mono.clone())
                                .when(skipped, |el| el.opacity(DIMMED))
                                .child(column.clone()),
                        )
                        .child(
                            cell()
                                .truncate()
                                .font_family(mono.clone())
                                .text_size(TEXT_2XS)
                                .text_color(theme.muted_foreground)
                                .when(skipped, |el| el.opacity(DIMMED))
                                .child(sample),
                        )
                        .child(cell().children(
                            self.mapping.get(ix).map(|input| form::input(input, window, cx).disabled(running)),
                        ))
                        .when(new_table, |el| el.child(cell().child(picker)))
                        .into_any_element()
                })
                .collect();
        v_flex()
            .flex_none()
            .border_1()
            .border_color(theme.border)
            .rounded(RADIUS)
            .overflow_hidden()
            .child(
                h_flex()
                    .justify_between()
                    .gap(rems(0.615))
                    .px(rems(0.615))
                    .py(rems(0.385))
                    .bg(theme.secondary)
                    .border_b_1()
                    .border_color(theme.border)
                    .text_size(TEXT_SM)
                    .child(t(cx, "import.columnsHeading"))
                    .child(div().text_size(TEXT_2XS).text_color(theme.muted_foreground).child(meta)),
            )
            .child(header)
            .children(rows)
            .into_any_element()
    }

    fn footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let running = self.running.is_some();
        modal::footer(cx)
            .child(
                Button::new("import-dismiss")
                    .debug_selector(|| "import-dismiss".into())
                    .label(t(cx, if running { "import.stop" } else { "common.close" }))
                    .on_click(cx.listener(|this, _, window, cx| this.dismiss(window, cx))),
            )
            .child(
                Button::new("import-run")
                    .debug_selector(|| "import-run".into())
                    .primary()
                    .label(t(cx, if running { "import.running" } else { "import.run" }))
                    .disabled(!self.can_run(cx))
                    .on_click(cx.listener(|this, _, window, cx| this.run(window, cx))),
            )
    }

    fn result_view(&self, result: &ImportResult, cx: &App) -> impl IntoElement {
        let theme = cx.theme();
        let n = |value: i64| count(cx, value.max(0) as usize);
        let text = if self.kind == Kind::Sql {
            t_with(cx, "import.doneStatements", &[("count", &n(result.statements)), ("skipped", &n(result.skipped))])
        } else {
            t_with(cx, "import.doneRows", &[("count", &n(result.inserted)), ("skipped", &n(result.skipped))])
        };
        let tint = if result.skipped > 0 { theme.warning } else { theme.success };
        form::alert(tint)
            .flex()
            .flex_col()
            .gap(rems(0.385))
            .child(
                h_flex()
                    .gap(rems(0.385))
                    .child(
                        Icon::new(if result.cancelled { Lucide::CircleAlert } else { Lucide::CircleCheck })
                            .size(ICON_XS),
                    )
                    .child(text),
            )
            .when(result.cancelled, |el| {
                el.child(
                    div().text_size(TEXT_2XS).text_color(theme.muted_foreground).child(t(cx, "import.cancelledNote")),
                )
            })
            .when(!result.errors.is_empty(), |el| {
                let errors = v_flex()
                    .id("import-errors")
                    .max_h(px(120.))
                    .overflow_y_scroll()
                    .track_scroll(&self.errors_scroll)
                    .pl(rems(1.231))
                    .font_family(theme.mono_font_family.clone())
                    .text_size(TEXT_2XS)
                    .text_color(theme.muted_foreground)
                    .children(result.errors.iter().map(|error| div().child(format!("• {error}"))));
                el.child(div().relative().child(errors).vertical_scrollbar(&self.errors_scroll).scrollbars_on_hover())
            })
    }
}

impl Render for ImportDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_tables(cx);
        let running = self.running.is_some();
        let csv = self.kind == Kind::Csv;
        let kinds = form::toggle_group()
            .child(
                form::toggle("import-kind-csv", t(cx, "import.kindCSV"), csv)
                    .debug_selector(|| "import-kind-csv".into())
                    .icon(Icon::new(Lucide::Sheet))
                    .disabled(running)
                    .on_click(cx.listener(|this, _, _, cx| {
                        if this.kind != Kind::Csv {
                            this.set_kind(Kind::Csv, cx);
                        }
                    })),
            )
            .child(
                form::toggle("import-kind-sql", t(cx, "import.kindSQL"), !csv)
                    .debug_selector(|| "import-kind-sql".into())
                    .icon(Icon::new(Lucide::FileCode))
                    .disabled(running)
                    .on_click(cx.listener(|this, _, _, cx| {
                        if this.kind != Kind::Sql {
                            this.set_kind(Kind::Sql, cx);
                        }
                    })),
            );
        let no_tables = self.tables.is_empty();
        let targets = form::toggle_group()
            .child(
                form::toggle("import-target-new", t(cx, "import.targetNew"), self.target == Target::New)
                    .debug_selector(|| "import-target-new".into())
                    .disabled(running)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.target = Target::New;
                        cx.notify();
                    })),
            )
            .child(
                form::toggle("import-target-existing", t(cx, "import.targetExisting"), self.target == Target::Existing)
                    .debug_selector(|| "import-target-existing".into())
                    .disabled(running || no_tables)
                    .when(no_tables, |button| button.tooltip(t(cx, "import.noTables")))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.target = Target::Existing;
                        cx.notify();
                    })),
            );
        let theme = cx.theme().clone();
        let path: SharedString = if self.path.is_empty() { t(cx, "import.noFile") } else { self.path.clone().into() };
        let path_field = div()
            .flex_1()
            .min_w_0()
            .h(rems(2.308))
            .flex()
            .items_center()
            .px(rems(0.769))
            .rounded(RADIUS)
            .border_1()
            .border_color(theme.border)
            .bg(theme.background)
            .text_size(TEXT_BASE)
            .text_color(if self.path.is_empty() { theme.muted_foreground } else { theme.foreground })
            .child(div().min_w_0().truncate().child(path));
        let file_row = h_flex().gap(rems(0.615)).child(path_field).child(
            Button::new("import-browse")
                .debug_selector(|| "import-browse".into())
                .icon(Icon::new(Lucide::FolderOpen))
                .label(t(cx, "common.browse"))
                .disabled(running)
                .on_click(cx.listener(|this, _, window, cx| this.pick(window, cx))),
        );
        let kind_row = h_flex()
            .flex_wrap()
            .items_start()
            .gap(rems(0.769))
            .child(div().flex_1().min_w_0().child(form::group(t(cx, "import.kind"), kinds, cx)))
            .when(csv, |el| el.child(div().flex_1().min_w_0().child(form::group(t(cx, "import.target"), targets, cx))));
        let body = modal::body()
            .child(kind_row)
            .child(form::group(t(cx, "import.file"), file_row, cx))
            .when(csv, |el| el.child(self.csv_fields(window, cx)))
            .when(!csv, |el| {
                el.child(self.checkbox(
                    "import-stop-sql",
                    "import.stopOnError",
                    self.stop_on_error,
                    |d, v| d.stop_on_error = v,
                    cx,
                ))
            })
            .when(running, |el| el.child(self.progress_view(cx)))
            .when_some(self.error.clone(), |el, error| el.child(form::error(error, cx)))
            .when_some(self.result.clone(), |el, result| el.child(self.result_view(&result, cx)));
        modal::scroll_content().child(modal::scroll_body(&self.scroll, body)).child(self.footer(cx))
    }
}

// Last opened dialog, for scenarios to read.
#[cfg(any(test, feature = "snapshot"))]
pub(crate) struct Opened(pub WeakEntity<ImportDialog>);

#[cfg(any(test, feature = "snapshot"))]
impl Global for Opened {}

#[cfg(test)]
pub(crate) struct Shown {
    pub csv: bool,
    pub has_header: bool,
    pub can_run: bool,
    pub existing: Option<String>,
    pub tables: Vec<String>,
    pub truncate: bool,
    pub path: String,
    pub preview_rows: Option<usize>,
    pub running: bool,
    pub result: Option<(i64, i64)>,
    pub error: Option<String>,
}

#[cfg(test)]
impl ImportDialog {
    pub(crate) fn shown(&self, cx: &App) -> Shown {
        Shown {
            csv: self.kind == Kind::Csv,
            has_header: self.has_header,
            can_run: self.can_run(cx),
            existing: (self.target == Target::Existing).then(|| self.existing.clone()),
            tables: self.tables.clone(),
            truncate: self.truncate,
            path: self.path.clone(),
            preview_rows: self.preview.as_ref().map(|preview| preview.total_rows as usize),
            running: self.running.is_some(),
            result: self.result.as_ref().map(|result| (result.inserted, result.skipped)),
            error: self.error.as_ref().map(|error| error.to_string()),
        }
    }
}

pub fn open(
    connection: ConnectionConfig,
    schema: String,
    tables: Vec<String>,
    table: Option<String>,
    window: &mut Window,
    cx: &mut App,
    on_imported: impl Fn(&mut App) + 'static,
) {
    let dialog = cx.new(|cx| ImportDialog::new(connection, schema, tables, table, Rc::new(on_imported), window, cx));
    #[cfg(any(test, feature = "snapshot"))]
    cx.set_global(Opened(dialog.downgrade()));
    window.open_dialog(cx, move |frame, window, cx| {
        let running = dialog.read(cx).running.is_some();
        Modal::new("import", t(cx, "import.title"))
            .size(modal::Size::Lg)
            .closable(!running)
            .build(frame, dialog.clone(), window, cx)
            .keyboard(!running)
            .overlay_closable(!running)
            .on_ok(|_, _, _| false)
    });
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use barsql_core::Value;
    use gpui_kit::component::Root;
    use gpui_kit::{AppContext as _, Entity, TestAppContext, VisualTestContext};

    use super::{ImportDialog, Kind, table_name_for};
    use crate::test_support::{Env, settle};

    fn open<'a>(
        env: &Env,
        cx: &'a mut TestAppContext,
        imported: Rc<Cell<usize>>,
    ) -> (Entity<ImportDialog>, &'a mut VisualTestContext) {
        let connection = env.connection.clone();
        let slot = Rc::new(std::cell::RefCell::new(None));
        let out = slot.clone();
        let window = cx.add_window(move |window, cx| {
            let on_imported = Rc::new(move |_: &mut gpui_kit::App| imported.set(imported.get() + 1));
            let view = cx.new(|cx| {
                ImportDialog::new(connection, "main".into(), vec!["things".into()], None, on_imported, window, cx)
            });
            *out.borrow_mut() = Some(view.clone());
            Root::new(view, window, cx)
        });
        let cx = VisualTestContext::from_window(window.into(), cx).into_mut();
        let view = slot.borrow_mut().take().unwrap();
        (view, cx)
    }

    #[gpui_kit::test]
    fn a_csv_previews_then_loads_into_a_new_table(cx: &mut TestAppContext) {
        let env = Env::new(cx);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("people list.csv");
        std::fs::write(&path, "name,age,active\nann,31,true\nbob,,false\n").unwrap();
        let imported = Rc::new(Cell::new(0));
        let (view, cx) = open(&env, cx, imported.clone());
        view.update_in(cx, |dialog, window, cx| {
            dialog.path = path.display().to_string();
            dialog.load_preview(window, cx);
        });
        settle(cx, |cx| view.read_with(cx, |d, _| d.preview.is_some()));
        let (columns, types, table) = view.read_with(cx, |d, cx| {
            let preview = d.preview.clone().unwrap();
            (preview.columns, d.column_types.clone(), d.new_table.read(cx).value().to_string())
        });
        assert_eq!(columns, ["name", "age", "active"]);
        assert_eq!(types, ["text", "int", "bool"]);
        assert_eq!(table, "people_list");
        view.update_in(cx, |dialog, window, cx| dialog.run(window, cx));
        settle(cx, |cx| view.read_with(cx, |d, _| d.running.is_none() && (d.result.is_some() || d.error.is_some())));
        let result = view.read_with(cx, |d, _| d.result.clone()).expect("a result");
        assert_eq!((result.inserted, result.skipped, result.cancelled), (2, 0, false));
        assert_eq!(imported.get(), 1);
        let rows = env
            .runtime
            .block_on(env.bar.execute_query(&env.connection.id, "SELECT name, age FROM people_list ORDER BY name"))
            .unwrap()
            .rows;
        assert_eq!(
            rows,
            [vec![Value::Text("ann".into()), Value::Int(31)], vec![Value::Text("bob".into()), Value::Null]]
        );
    }

    #[gpui_kit::test]
    fn a_sql_script_runs_its_statements(cx: &mut TestAppContext) {
        let env = Env::new(cx);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("seed.sql");
        std::fs::write(
            &path,
            "CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT);\nINSERT INTO notes (body) VALUES ('a'), ('b');\n",
        )
        .unwrap();
        let imported = Rc::new(Cell::new(0));
        let (view, cx) = open(&env, cx, imported.clone());
        view.update_in(cx, |dialog, window, cx| {
            dialog.set_kind(Kind::Sql, cx);
            dialog.path = path.display().to_string();
            dialog.run(window, cx);
        });
        settle(cx, |cx| view.read_with(cx, |d, _| d.running.is_none() && (d.result.is_some() || d.error.is_some())));
        let result = view.read_with(cx, |d, _| d.result.clone()).expect("a result");
        assert_eq!((result.statements, result.skipped), (2, 0));
        assert_eq!(imported.get(), 1);
        let count =
            env.runtime.block_on(env.bar.execute_query(&env.connection.id, "SELECT count(*) FROM notes")).unwrap().rows;
        assert_eq!(count[0][0], Value::Int(2));
    }

    #[test]
    fn file_names_become_table_names() {
        assert_eq!(table_name_for("/data/Sales 2024.csv"), "Sales_2024");
        assert_eq!(table_name_for("C:\\exports\\orders.v2.tsv"), "orders_v2");
        assert_eq!(table_name_for("/tmp/.hidden"), "");
        assert_eq!(table_name_for("plain"), "plain");
        assert_eq!(table_name_for("file."), "file_");
    }
}
