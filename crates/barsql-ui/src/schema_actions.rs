use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use barsql_app::{BackupOutcome, BackupRequest};
use barsql_core::{ConnectionConfig, DriverType, ObjectKind, ObjectRef, QueryError};
use barsql_sql::Dialect;
use barsql_sql::alter::{self, ConstraintKind};
use barsql_sql::ddl::terminate_statement;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::{ActiveTheme, Disableable, Sizable, WindowExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::file_dialogs::pick_save_path;
use crate::form;
use crate::i18n::{count, t, t_count, t_with};
use crate::modal::{self, Modal};
use crate::spinner::Spinner;
use crate::state;
use crate::toast;
use crate::tokens::{ICON_XS, TEXT_SM};

type OnDone = Rc<dyn Fn(&Applied, &mut App)>;

pub struct Applied {
    pub connection_id: String,
    pub driver: DriverType,
    pub change: Change,
    // Empty unless the change is a rename.
    pub new_name: String,
    pub options: Options,
}

fn job_id(kind: &str) -> String {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    format!("{kind}-{}", SEQ.fetch_add(1, Ordering::Relaxed) + 1)
}

#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    RenameTable { schema: String, table: String, kind: ObjectKind },
    DropTable { schema: String, table: String, kind: ObjectKind },
    Truncate { schema: String, table: String },
    RenameColumn { schema: String, table: String, column: String },
    DropColumn { schema: String, table: String, column: String },
    DropObject { object: ObjectRef, constraint: ConstraintKind },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Options {
    pub cascade: bool,
    pub restart_identity: bool,
}

impl Change {
    fn name(&self) -> &str {
        match self {
            Self::RenameTable { table, .. } | Self::DropTable { table, .. } | Self::Truncate { table, .. } => table,
            Self::RenameColumn { column, .. } | Self::DropColumn { column, .. } => column,
            Self::DropObject { object, .. } => &object.name,
        }
    }

    fn renames(&self) -> bool {
        matches!(self, Self::RenameTable { .. } | Self::RenameColumn { .. })
    }

    // None where the engine has no statement for it.
    pub fn sql(&self, driver: &DriverType, new_name: &str, options: Options) -> Option<String> {
        let Options { cascade, restart_identity } = options;
        match self {
            Self::RenameTable { schema, table, kind } => alter::rename_relation(driver, kind, schema, table, new_name),
            Self::DropTable { schema, table, kind } => Some(alter::drop_relation(driver, kind, schema, table, cascade)),
            Self::Truncate { schema, table } => {
                Some(alter::truncate_table(driver, schema, table, restart_identity, cascade))
            }
            Self::RenameColumn { schema, table, column } => {
                Some(alter::rename_column(driver, schema, table, column, new_name))
            }
            Self::DropColumn { schema, table, column } => {
                Some(alter::drop_column(driver, schema, table, column, cascade))
            }
            Self::DropObject { object, constraint } => alter::drop_object(driver, object, *constraint, cascade),
        }
    }

    pub fn supported(&self, driver: &DriverType) -> bool {
        self.sql(driver, self.name(), Options::default()).is_some()
    }

    fn title(&self, cx: &App) -> SharedString {
        let view = |kind: &ObjectKind| matches!(kind, ObjectKind::View | ObjectKind::MaterializedView);
        let key = match self {
            Self::RenameTable { kind, .. } if view(kind) => "renameView",
            Self::RenameTable { .. } => "renameTable",
            Self::DropTable { kind, .. } if view(kind) => "dropView",
            Self::DropTable { .. } => "dropTable",
            Self::Truncate { .. } => "truncateTable",
            Self::RenameColumn { .. } => "renameColumn",
            Self::DropColumn { .. } => "dropColumn",
            Self::DropObject { object, .. } => match object.kind {
                ObjectKind::Index => "dropIndex",
                ObjectKind::Constraint => "dropConstraint",
                ObjectKind::Trigger => "dropTrigger",
                ObjectKind::Procedure => "dropProcedure",
                _ => "dropFunction",
            },
        };
        t(cx, &format!("schemaChange.{key}"))
    }

    fn description(&self, cx: &App) -> Option<SharedString> {
        let name = self.name();
        Some(match self {
            Self::RenameTable { .. } | Self::RenameColumn { .. } => return None,
            Self::DropTable { kind: ObjectKind::View | ObjectKind::MaterializedView, .. } => {
                t_with(cx, "schemaChange.dropViewDescription", &[("name", name)])
            }
            Self::DropTable { .. } => t_with(cx, "schemaChange.dropTableDescription", &[("name", name)]),
            Self::Truncate { .. } => t_with(cx, "schemaChange.truncateDescription", &[("name", name)]),
            Self::DropColumn { table, .. } => {
                t_with(cx, "schemaChange.dropColumnDescription", &[("name", name), ("table", table)])
            }
            Self::DropObject { .. } => t_with(cx, "schemaChange.dropObjectDescription", &[("name", name)]),
        })
    }

    fn confirm(&self, cx: &App) -> SharedString {
        match self {
            Self::RenameTable { .. } | Self::RenameColumn { .. } => t(cx, "common.rename"),
            Self::Truncate { .. } => t(cx, "schemaChange.truncate"),
            _ => t(cx, "schemaChange.drop"),
        }
    }

    fn done(&self, new_name: &str, cx: &App) -> SharedString {
        let name = self.name();
        match self {
            Self::RenameTable { .. } | Self::RenameColumn { .. } => {
                t_with(cx, "schemaChange.renamed", &[("name", name), ("newName", new_name)])
            }
            Self::Truncate { .. } => t_with(cx, "schemaChange.truncated", &[("name", name)]),
            _ => t_with(cx, "schemaChange.dropped", &[("name", name)]),
        }
    }
}

struct ChangeDialog {
    connection: ConnectionConfig,
    change: Change,
    name: Option<Entity<InputState>>,
    options: Options,
    running: bool,
    error: Option<SharedString>,
    job: String,
    on_done: OnDone,
    sql_scroll: ScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl ChangeDialog {
    fn new_name(&self, cx: &App) -> String {
        self.name.as_ref().map(|name| name.read(cx).value().trim().to_string()).unwrap_or_default()
    }

    // None while a rename's new name is empty or unchanged.
    fn sql(&self, cx: &App) -> Option<String> {
        let new_name = self.new_name(cx);
        if self.change.renames() && (new_name.is_empty() || new_name == self.change.name()) {
            return None;
        }
        self.change.sql(&self.connection.driver, &new_name, self.options)
    }

    // A rename previews with the current name until a new one is typed.
    fn preview(&self, cx: &App) -> Option<String> {
        let new_name = Some(self.new_name(cx)).filter(|n| !n.is_empty()).unwrap_or_else(|| self.change.name().into());
        self.change.sql(&self.connection.driver, &new_name, self.options).map(|sql| terminate_statement(&sql))
    }

    // The tree catches up even if the dialog was closed while the statement ran.
    fn run(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.running {
            return;
        }
        let Some(sql) = self.sql(cx) else { return };
        self.running = true;
        self.error = None;
        cx.notify();
        let bar = state::bar(cx);
        let (id, job) = (self.connection.id.clone(), self.job.clone());
        let task = state::spawn(cx, async move { bar.execute_statement(&id, &job, &sql).await });
        let applied = Applied {
            connection_id: self.connection.id.clone(),
            driver: self.connection.driver.clone(),
            change: self.change.clone(),
            new_name: self.new_name(cx),
            options: self.options,
        };
        let on_done = self.on_done.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            if let Some(Ok(())) = &result {
                cx.update(|_, cx| {
                    on_done(&applied, cx);
                    toast::success(applied.change.done(&applied.new_name, cx), cx);
                })
                .ok();
            }
            this.update_in(cx, |dialog, window, cx| {
                dialog.running = false;
                match result {
                    Some(Ok(())) => window.close_dialog(cx),
                    Some(Err(error)) if !error.cancelled => dialog.error = Some(message(&error).into()),
                    _ => {}
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.running {
            state::bar(cx).cancel_query(&self.job);
        }
        window.close_dialog(cx);
    }

    fn option(
        &self,
        id: &'static str,
        checked: bool,
        label: &str,
        hint: &str,
        flip: fn(&mut Options),
        cx: &mut Context<Self>,
    ) -> Div {
        v_flex()
            .child(
                form::checkbox(id, checked, t(cx, label), self.running, cx).debug_selector(move || id.into()).on_click(
                    cx.listener(move |dialog, _, _, cx| {
                        if !dialog.running {
                            flip(&mut dialog.options);
                            dialog.error = None;
                            cx.notify();
                        }
                    }),
                ),
            )
            .child(form::hint(t(cx, hint), cx).pl(rems(2.)))
    }
}

// Adds the detail, which often names the object in the way.
fn message(error: &QueryError) -> String {
    if error.detail.is_empty() { error.message.clone() } else { format!("{}\n{}", error.message, error.detail) }
}

impl Render for ChangeDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let dialect = Dialect::for_driver(&self.connection.driver);
        let truncate = matches!(self.change, Change::Truncate { .. });
        let mut body = modal::body().children(self.change.description(cx).map(|d| modal::description(d, cx)));
        if let Some(name) = &self.name {
            let field = div().debug_selector(|| "change-name".into()).child(form::input(name, window, cx));
            body = body.child(form::group(t(cx, "schemaChange.newName"), field, cx));
        }
        if dialect.cascade && !self.change.renames() {
            let hint = if truncate { "schemaChange.cascadeTruncateHint" } else { "schemaChange.cascadeHint" };
            let cascade = self.option(
                "change-cascade",
                self.options.cascade,
                "schemaChange.cascade",
                hint,
                |options| options.cascade = !options.cascade,
                cx,
            );
            body = body.child(cascade);
        }
        if dialect.restart_identity && truncate {
            body = body.child(self.option(
                "change-restart-identity",
                self.options.restart_identity,
                "schemaChange.restartIdentity",
                "schemaChange.restartIdentityHint",
                |options| options.restart_identity = !options.restart_identity,
                cx,
            ));
        }
        if let Some(sql) = self.preview(cx) {
            let detail = modal::detail("change-sql", sql, &self.sql_scroll, cx);
            body = body.child(form::group(t(cx, "schemaChange.sql"), detail, cx));
        }
        let error = self.error.clone().map(|error| form::error(error, cx).debug_selector(|| "change-error".into()));
        let body = body.children(error);
        let ready = self.sql(cx).is_some() && !self.running;
        let danger = !self.change.renames();
        v_flex().child(body).child(
            modal::footer(cx)
                .child(
                    Button::new("change-cancel")
                        .large()
                        .label(t(cx, "common.cancel"))
                        .on_click(cx.listener(|dialog, _, window, cx| dialog.dismiss(window, cx))),
                )
                .child(
                    Button::new("change-confirm")
                        .large()
                        .debug_selector(|| "change-confirm".into())
                        .map(|button| if danger { button.danger().outline() } else { button.primary() })
                        .label(self.change.confirm(cx))
                        .disabled(!ready)
                        .on_click(cx.listener(|dialog, _, window, cx| dialog.run(window, cx))),
                ),
        )
    }
}

// `on_done` runs after the statement succeeds. The dialog stays open while it runs, and Cancel stops it.
pub fn open_change(
    connection: ConnectionConfig,
    change: Change,
    window: &mut Window,
    cx: &mut App,
    on_done: impl Fn(&Applied, &mut App) + 'static,
) {
    let current = change.name().to_string();
    let renames = change.renames();
    let dialog = cx.new(|cx| {
        let name = renames.then(|| cx.new(|cx| InputState::new(window, cx).default_value(current.clone())));
        let subscriptions = name
            .iter()
            .map(|name| {
                cx.subscribe(name, |dialog: &mut ChangeDialog, _, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        dialog.error = None;
                        cx.notify();
                    }
                })
            })
            .collect();
        ChangeDialog {
            connection,
            change,
            name,
            options: Options::default(),
            running: false,
            error: None,
            job: job_id("schema-change"),
            on_done: Rc::new(on_done),
            sql_scroll: ScrollHandle::new(),
            _subscriptions: subscriptions,
        }
    });
    let (title, danger) = {
        let dialog = dialog.read(cx);
        (dialog.change.title(cx), !dialog.change.renames())
    };
    let input = dialog.read(cx).name.clone();
    window.open_dialog(cx, move |modal_dialog, window, cx| {
        let entry = dialog.clone();
        let running = dialog.read(cx).running;
        Modal::new("schema-change", title.clone())
            .size(modal::Size::Sm)
            .danger(danger)
            .closable(!running)
            .build(modal_dialog, dialog.clone(), window, cx)
            .keyboard(!running)
            .overlay_closable(false)
            .on_ok(move |_, window, cx| {
                entry.update(cx, |dialog, cx| dialog.run(window, cx));
                false
            })
    });
    if let Some(input) = input {
        window.defer(cx, move |window, cx| {
            input.update(cx, |state, cx| {
                state.focus(window, cx);
                state.select_all(window, cx);
            })
        });
    }
}

struct BackupDialog {
    connection: ConnectionConfig,
    schema: String,
    table: String,
    structure: bool,
    data: bool,
    picking: bool,
    progress: Option<usize>,
    job: String,
}

impl BackupDialog {
    fn ready(&self) -> bool {
        (self.structure || self.data) && !self.picking && self.progress.is_none()
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.ready() {
            return;
        }
        self.picking = true;
        cx.notify();
        let picked = pick_save_path(&format!("{}.sql", self.table), cx);
        cx.spawn_in(window, async move |this, cx| {
            let path = picked.await;
            this.update_in(cx, |dialog, window, cx| {
                dialog.picking = false;
                match path {
                    Some(path) => dialog.write(path, window, cx),
                    None => cx.notify(),
                }
            })
            .ok();
        })
        .detach();
    }

    // Toasts even if the dialog was closed while writing.
    fn write(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.progress = Some(0);
        cx.notify();
        let request = BackupRequest {
            schema: self.schema.clone(),
            table: self.table.clone(),
            structure: self.structure,
            data: self.data,
            path: path.clone(),
        };
        let bar = state::bar(cx);
        let (id, job) = (self.connection.id.clone(), self.job.clone());
        let (sender, receiver) = async_channel::unbounded();
        let backup = state::spawn(cx, async move { bar.backup_table(&id, &job, request, sender).await });
        let file_name =
            path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
        cx.spawn_in(window, async move |this, cx| {
            while let Ok(rows) = receiver.recv().await {
                this.update(cx, |dialog, cx| {
                    dialog.progress = Some(rows);
                    cx.notify();
                })
                .ok();
            }
            let outcome = backup.await;
            cx.update(|_, cx| finished(outcome.as_ref(), &file_name, cx)).ok();
            this.update_in(cx, |dialog, window, cx| {
                dialog.progress = None;
                if let Some(Ok(BackupOutcome { cancelled: false, .. })) = outcome {
                    window.close_dialog(cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.progress.is_some() {
            state::bar(cx).cancel_query(&self.job);
            return;
        }
        window.close_dialog(cx);
    }

    fn part(
        &self,
        id: &'static str,
        checked: bool,
        label: &str,
        hint: &str,
        flip: fn(&mut Self),
        cx: &mut Context<Self>,
    ) -> Div {
        let busy = self.progress.is_some();
        v_flex()
            .child(form::checkbox(id, checked, t(cx, label), busy, cx).debug_selector(move || id.into()).on_click(
                cx.listener(move |dialog, _, _, cx| {
                    if dialog.progress.is_none() {
                        flip(dialog);
                        cx.notify();
                    }
                }),
            ))
            .child(form::hint(t(cx, hint), cx).pl(rems(2.)))
    }
}

fn finished(outcome: Option<&Result<BackupOutcome, QueryError>>, file_name: &str, cx: &mut App) {
    match outcome {
        Some(Ok(BackupOutcome { rows, cancelled: false })) => {
            let formatted = count(cx, *rows);
            let vars = [("formatted", formatted.as_str()), ("fileName", file_name)];
            toast::success(t_count(cx, "schemaChange.backedUp", *rows as i64, &vars), cx);
        }
        Some(Ok(BackupOutcome { cancelled: true, .. })) => toast::info(t(cx, "schemaChange.backupStopped"), cx),
        Some(Err(error)) => toast::error_in(t(cx, "schemaChange.backupFailed"), &error.message, cx),
        None => {}
    }
}

impl Render for BackupDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let writing = self.progress.is_some();
        let theme = cx.theme();
        let status = self.progress.map(|rows| {
            h_flex()
                .gap(rems(0.462))
                .text_size(TEXT_SM)
                .text_color(theme.muted_foreground)
                .child(Spinner::new(ICON_XS))
                .child(t_with(cx, "schemaChange.backupProgress", &[("formatted", &count(cx, rows))]))
        });
        let description = t_with(cx, "schemaChange.backupDescription", &[("name", &self.table)]);
        let structure = self.part(
            "backup-structure",
            self.structure,
            "schemaChange.backupStructure",
            "schemaChange.backupStructureHint",
            |dialog| dialog.structure = !dialog.structure,
            cx,
        );
        let data = self.part(
            "backup-data",
            self.data,
            "schemaChange.backupData",
            "schemaChange.backupDataHint",
            |dialog| dialog.data = !dialog.data,
            cx,
        );
        let ready = self.ready();
        v_flex()
            .child(
                modal::body().child(modal::description(description, cx)).child(structure).child(data).children(status),
            )
            .child(
                modal::footer(cx)
                    .child(
                        Button::new("backup-dismiss")
                            .large()
                            .debug_selector(|| "backup-dismiss".into())
                            .label(t(cx, if writing { "export.stop" } else { "common.cancel" }))
                            .on_click(cx.listener(|dialog, _, window, cx| dialog.dismiss(window, cx))),
                    )
                    .child(
                        Button::new("backup-save")
                            .large()
                            .debug_selector(|| "backup-save".into())
                            .primary()
                            .label(t(cx, "schemaChange.backupSave"))
                            .disabled(!ready)
                            .on_click(cx.listener(|dialog, _, window, cx| dialog.save(window, cx))),
                    ),
            )
    }
}

pub fn open_backup(connection: ConnectionConfig, schema: String, table: String, window: &mut Window, cx: &mut App) {
    let dialog = cx.new(|_| BackupDialog {
        connection,
        schema,
        table,
        structure: true,
        data: true,
        picking: false,
        progress: None,
        job: job_id("backup"),
    });
    window.open_dialog(cx, move |modal_dialog, window, cx| {
        let busy = dialog.read(cx).progress.is_some();
        let entry = dialog.clone();
        Modal::new("backup", t(cx, "schemaChange.backupTitle"))
            .size(modal::Size::Sm)
            .closable(!busy)
            .build(modal_dialog, dialog.clone(), window, cx)
            .keyboard(!busy)
            .overlay_closable(!busy)
            .on_ok(move |_, window, cx| {
                entry.update(cx, |dialog, cx| dialog.save(window, cx));
                false
            })
    });
}

#[cfg(test)]
mod tests {
    use barsql_core::{DriverType, ObjectKind, ObjectRef};
    use barsql_sql::alter::ConstraintKind;

    use super::{Change, Options};

    fn object(kind: ObjectKind) -> ObjectRef {
        ObjectRef { schema: "main".into(), name: "thing".into(), kind, table: "t".into(), args: String::new() }
    }

    #[test]
    fn the_menus_offer_only_what_the_engine_can_do() {
        let lite = DriverType::Sqlite;
        let view = Change::RenameTable { schema: "main".into(), table: "v".into(), kind: ObjectKind::View };
        assert!(!view.supported(&lite) && view.supported(&DriverType::Postgres));
        let constraint =
            Change::DropObject { object: object(ObjectKind::Constraint), constraint: ConstraintKind::Check };
        assert!(!constraint.supported(&lite) && constraint.supported(&DriverType::MySql));
        let index = Change::DropObject { object: object(ObjectKind::Index), constraint: ConstraintKind::Other };
        assert!(index.supported(&lite));
        let truncate = Change::Truncate { schema: "main".into(), table: "t".into() };
        assert_eq!(truncate.sql(&lite, "", Options::default()).as_deref(), Some(r#"DELETE FROM "t""#));
    }
}
