use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use barsql_io::{EXPORT_FORMATS, ExportFormat, WriteOutcome, write_chunks};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, Disableable, Sizable, WindowExt, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::file_dialogs::pick_export_path;
use crate::form::{self, SelectMenu};
use crate::grid::copy::{self, CopyTarget};
use crate::grid::format_label;
use crate::grid::{ExportSource, copy_format, set_copy_format};
use crate::i18n::{count, t, t_with};
use crate::modal::{self, Modal};
use crate::toast;
use crate::tokens::TEXT_SM;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowScope {
    All,
    Selected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ColumnScope {
    All,
    Visible,
    Selected,
}

pub struct ExportDialog {
    source: ExportSource,
    rows: RowScope,
    columns: ColumnScope,
    busy: bool,
    progress: Option<usize>,
    stop: Arc<AtomicBool>,
    task: Option<Task<()>>,
}

impl ExportDialog {
    fn new(source: ExportSource) -> Self {
        let rows = if source.selected_rows.is_empty() { RowScope::All } else { RowScope::Selected };
        let columns = if !source.selected_columns.is_empty() {
            ColumnScope::Selected
        } else if source.visible.len() < source.set.columns.len() {
            ColumnScope::Visible
        } else {
            ColumnScope::All
        };
        Self { source, rows, columns, busy: false, progress: None, stop: Arc::default(), task: None }
    }

    fn target(&self) -> CopyTarget {
        let source = &self.source;
        let rows = match self.rows {
            RowScope::Selected if !source.selected_rows.is_empty() => source.selected_rows.clone(),
            _ => source.order.clone(),
        };
        let columns = match self.columns {
            ColumnScope::Selected if !source.selected_columns.is_empty() => source.selected_columns.clone(),
            ColumnScope::All => (0..source.set.columns.len()).collect(),
            _ => source.visible.clone(),
        };
        CopyTarget { columns, rows }
    }

    fn copy(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.busy = true;
        cx.notify();
        let (set, staged, table) = (self.source.set.clone(), self.source.staged.clone(), self.source.table.clone());
        let (target, format, dialect) = (self.target(), copy_format(cx), self.source.dialect);
        let text = cx.background_spawn(async move {
            copy::export(&set, staged.as_ref(), format, table.as_deref(), dialect, &target)
        });
        self.task = Some(cx.spawn_in(window, async move |this, cx| {
            let text = text.await;
            this.update_in(cx, |dialog, window, cx| {
                dialog.busy = false;
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                toast::success(t(cx, "toast.exportCopied"), cx);
                window.close_dialog(cx);
            })
            .ok();
        }));
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let format = copy_format(cx);
        self.busy = true;
        cx.notify();
        let picked = pick_export_path(format.ext(), cx);
        self.task = Some(cx.spawn_in(window, async move |this, cx| {
            let Some(path) = picked.await else {
                this.update(cx, |dialog, cx| {
                    dialog.busy = false;
                    cx.notify();
                })
                .ok();
                return;
            };
            this.update_in(cx, |dialog, window, cx| dialog.save_to(path, format, window, cx)).ok();
        }));
    }

    fn save_to(&mut self, path: PathBuf, format: ExportFormat, window: &mut Window, cx: &mut Context<Self>) {
        let write = self.write(path.clone(), format, cx);
        self.task = Some(cx.spawn_in(window, async move |this, cx| {
            let outcome = write.await;
            let file_name =
                path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
            this.update_in(cx, |dialog, window, cx| dialog.finish(outcome, &file_name, window, cx)).ok();
        }));
    }

    // Progress updates after each chunk is written.
    fn write(
        &mut self,
        path: PathBuf,
        format: ExportFormat,
        cx: &mut Context<Self>,
    ) -> Task<std::io::Result<WriteOutcome>> {
        self.stop.store(false, Ordering::Relaxed);
        self.progress = Some(0);
        cx.notify();
        let (set, staged, table) = (self.source.set.clone(), self.source.staged.clone(), self.source.table.clone());
        let (target, stop, dialect) = (self.target(), self.stop.clone(), self.source.dialect);
        let (sender, receiver) = async_channel::unbounded();
        let write = cx.background_spawn(async move {
            write_chunks(&path, copy::chunks(set, staged, format, table, dialect, target), &stop, |rows| {
                sender.try_send(rows).ok();
            })
        });
        cx.spawn(async move |this, cx| {
            while let Ok(rows) = receiver.recv().await {
                this.update(cx, |dialog, cx| {
                    dialog.progress = Some(rows);
                    cx.notify();
                })
                .ok();
            }
            write.await
        })
    }

    fn finish(
        &mut self,
        outcome: std::io::Result<WriteOutcome>,
        file_name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.busy = false;
        self.progress = None;
        cx.notify();
        match outcome {
            Ok(WriteOutcome { cancelled: false, .. }) => {
                let message = t_with(cx, "toast.savedFile", &[("fileName", file_name)]);
                toast::success(message, cx);
                window.close_dialog(cx);
            }
            // A partial file stays on disk, so don't report it as a finished export.
            Ok(WriteOutcome { rows, cancelled: true }) => {
                let message =
                    t_with(cx, "toast.exportStopped", &[("fileName", file_name), ("count", &rows.to_string())]);
                toast::error(message, cx);
            }
            Err(error) => {
                let message = format!("{}: {error}", t(cx, "errors.exportFailed"));
                toast::error(message, cx);
            }
        }
    }

    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.progress.is_some() {
            self.stop.store(true, Ordering::Relaxed);
            return;
        }
        window.close_dialog(cx);
    }

    fn writing(&self) -> bool {
        self.progress.is_some()
    }

    fn footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let writing = self.writing();
        modal::footer(cx)
            .child(
                Button::new("export-dismiss")
                    .large()
                    .debug_selector(|| "export-dismiss".into())
                    .label(t(cx, if writing { "export.stop" } else { "common.cancel" }))
                    .disabled(self.busy && !writing)
                    .on_click(cx.listener(|dialog, _, window, cx| dialog.dismiss(window, cx))),
            )
            .child(
                Button::new("export-copy")
                    .large()
                    .debug_selector(|| "export-copy".into())
                    .label(t(cx, "export.copyToClipboard"))
                    .disabled(self.busy)
                    .on_click(cx.listener(|dialog, _, window, cx| dialog.copy(window, cx))),
            )
            .child(
                Button::new("export-save")
                    .large()
                    .debug_selector(|| "export-save".into())
                    .primary()
                    .label(t(cx, "export.saveToFile"))
                    .disabled(self.busy)
                    .on_click(cx.listener(|dialog, _, window, cx| dialog.save(window, cx))),
            )
    }
}

impl ExportDialog {
    fn set_rows(&mut self, rows: RowScope, cx: &mut Context<Self>) {
        self.rows = rows;
        cx.notify();
    }
}

impl Render for ExportDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let format = copy_format(cx);
        let target = self.target();
        let (total, selected_rows) = (self.source.order.len(), self.source.selected_rows.len());
        let (all_columns, visible, selected_columns) =
            (self.source.set.columns.len(), self.source.visible.len(), self.source.selected_columns.len());
        let this = cx.entity().downgrade();
        let format_menu = form::select("export-format", format_label(format, cx), cx)
            .debug_selector(|| "export-format".into())
            .select_menu(window, cx, move |mut menu: PopupMenu, _, cx| {
                for option in EXPORT_FORMATS {
                    let this = this.clone();
                    menu = menu.item(PopupMenuItem::new(format_label(option, cx)).checked(option == format).on_click(
                        move |_, _, cx| {
                            set_copy_format(option, cx);
                            this.update(cx, |_, cx| cx.notify()).ok();
                        },
                    ));
                }
                menu
            });
        let rows = form::toggle_group()
            .child(
                form::toggle(
                    "export-rows-all",
                    t_with(cx, "export.rowsAll", &[("count", &total.to_string())]),
                    self.rows == RowScope::All,
                )
                .on_click(cx.listener(|dialog, _, _, cx| dialog.set_rows(RowScope::All, cx))),
            )
            .child(
                form::toggle(
                    "export-rows-selected",
                    t_with(cx, "export.rowsSelected", &[("count", &selected_rows.to_string())]),
                    self.rows == RowScope::Selected,
                )
                .debug_selector(|| "export-rows-selected".into())
                .disabled(selected_rows == 0)
                .when(selected_rows == 0, |button| button.tooltip(t(cx, "tooltip.exportSelectRows")))
                .on_click(cx.listener(|dialog, _, _, cx| dialog.set_rows(RowScope::Selected, cx))),
            );
        let mut column_scopes =
            vec![(ColumnScope::All, t_with(cx, "export.colsAll", &[("count", &all_columns.to_string())]))];
        if visible < all_columns {
            column_scopes
                .push((ColumnScope::Visible, t_with(cx, "export.colsVisible", &[("count", &visible.to_string())])));
        }
        column_scopes.push((
            ColumnScope::Selected,
            t_with(cx, "export.colsSelected", &[("count", &selected_columns.to_string())]),
        ));
        let columns =
            form::toggle_group().children(column_scopes.into_iter().enumerate().map(|(ix, (scope, label))| {
                let empty = scope == ColumnScope::Selected && selected_columns == 0;
                form::toggle(("export-columns", ix), label, self.columns == scope)
                    .disabled(empty)
                    .when(empty, |button| button.tooltip(t(cx, "tooltip.exportSelectCols")))
                    .on_click(cx.listener(move |dialog, _, _, cx| {
                        dialog.columns = scope;
                        cx.notify();
                    }))
            }));
        let status = match self.progress {
            None => div()
                .mt(rems(0.308))
                .text_size(TEXT_SM)
                .text_color(cx.theme().muted_foreground)
                .child(t_with(
                    cx,
                    "export.summary",
                    &[
                        ("rows", &target.rows.len().to_string()),
                        ("cols", &target.columns.len().to_string()),
                        ("format", &format_label(format, cx)),
                    ],
                ))
                .into_any_element(),
            Some(done) => {
                let total = target.rows.len();
                let label = t_with(cx, "export.progress", &[("done", &count(cx, done)), ("total", &count(cx, total))]);
                form::progress(label, if total == 0 { 1. } else { done as f32 / total as f32 }, cx).into_any_element()
            }
        };
        v_flex()
            .child(
                modal::body()
                    .child(form::group(t(cx, "export.format"), format_menu, cx))
                    .child(form::group(t(cx, "export.rows"), rows, cx))
                    .child(form::group(t(cx, "export.columns"), columns, cx))
                    .child(status),
            )
            .child(self.footer(cx))
    }
}

// Last opened dialog, for scenarios to read.
#[cfg(test)]
pub(crate) struct Opened(pub WeakEntity<ExportDialog>);

#[cfg(test)]
impl Global for Opened {}

#[cfg(test)]
impl ExportDialog {
    pub(crate) fn summary(&self, cx: &App) -> String {
        let target = self.target();
        let format = format_label(copy_format(cx), cx);
        let (rows, cols) = (target.rows.len().to_string(), target.columns.len().to_string());
        t_with(cx, "export.summary", &[("rows", &rows), ("cols", &cols), ("format", &format)]).to_string()
    }

    // Whether Selected can be picked, and whether it is.
    pub(crate) fn selected_rows(&self) -> (bool, bool) {
        (!self.source.selected_rows.is_empty(), self.rows == RowScope::Selected)
    }

    // Offered column scopes and the chosen one.
    pub(crate) fn column_scopes(&self) -> (Vec<&'static str>, &'static str) {
        let name = |scope: ColumnScope| match scope {
            ColumnScope::All => "all",
            ColumnScope::Visible => "visible",
            ColumnScope::Selected => "selected",
        };
        let mut scopes = vec!["all"];
        if self.source.visible.len() < self.source.set.columns.len() {
            scopes.push("visible");
        }
        scopes.push("selected");
        (scopes, name(self.columns))
    }
}

pub fn open(source: ExportSource, window: &mut Window, cx: &mut App) {
    let dialog = cx.new(|_| ExportDialog::new(source));
    #[cfg(test)]
    cx.set_global(Opened(dialog.downgrade()));
    // Enter saves to a file, the main button.
    window.open_dialog(cx, move |frame, window, cx| {
        let writing = dialog.read(cx).writing();
        let entry = dialog.clone();
        Modal::new("export", t(cx, "export.title"))
            .size(modal::Size::Sm)
            .closable(!writing)
            .build(frame, dialog.clone(), window, cx)
            .keyboard(!writing)
            .overlay_closable(!writing)
            .on_ok(move |_, window, cx| {
                entry.update(cx, |dialog, cx| dialog.save(window, cx));
                false
            })
    });
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use barsql_db::{ChunkBuilder, ColumnMeta, ResultSet};
    use barsql_io::ExportFormat;
    use gpui_kit::component::Root;
    use gpui_kit::{AppContext as _, Context, IntoElement, Render, TestAppContext, VisualTestContext, Window, div};

    use super::{ColumnScope, ExportDialog, RowScope};
    use crate::grid::ExportSource;
    use crate::grid::copy::CopyTarget;
    use crate::test_support::Env;

    struct Blank;

    impl Render for Blank {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
        }
    }

    fn source() -> ExportSource {
        let columns: Arc<[ColumnMeta]> = ["id", "name", "note"]
            .map(|name| ColumnMeta { name: name.into(), type_name: "TEXT".into() })
            .to_vec()
            .into();
        let mut builder = ChunkBuilder::new(3, 3);
        for (id, name) in [("1", "ann"), ("2", "bob"), ("3", "cy")] {
            builder.push_number(|s| s.push_str(id));
            builder.push_text(|s| s.push_str(name));
            builder.push_null();
            builder.end_row();
        }
        let mut set = ResultSet::new(columns);
        set.push(Arc::new(builder.finish()));
        ExportSource {
            set,
            staged: None,
            table: None,
            dialect: None,
            order: vec![2, 0, 1],
            selected_rows: vec![1],
            visible: vec![1, 0],
            selected_columns: vec![],
        }
    }

    #[gpui_kit::test]
    fn exports_default_to_the_selection_and_copy_or_write_it(cx: &mut TestAppContext) {
        let _env = Env::new(cx);
        let window = cx.add_window(|window, cx| Root::new(cx.new(|_| Blank), window, cx));
        let cx = VisualTestContext::from_window(window.into(), cx).into_mut();
        let dialog = cx.new(|_| ExportDialog::new(source()));
        let (rows, columns, target) = dialog.read_with(cx, |d, _| (d.rows, d.columns, d.target()));
        assert_eq!((rows, columns), (RowScope::Selected, ColumnScope::Visible));
        assert_eq!(target, CopyTarget { columns: vec![1, 0], rows: vec![1] });

        dialog.update_in(cx, |dialog, window, cx| dialog.copy(window, cx));
        cx.run_until_parked();
        assert_eq!(cx.read_from_clipboard().and_then(|item| item.text()).as_deref(), Some("name,id\nbob,2"));

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("export.json");
        dialog.update_in(cx, |dialog, window, cx| {
            dialog.rows = RowScope::All;
            dialog.columns = ColumnScope::All;
            dialog.save_to(path.clone(), ExportFormat::Json, window, cx);
        });
        cx.run_until_parked();
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.starts_with("[\n  {\n    \"id\": 3,"), "{written}");
        assert_eq!(written.matches("\"note\": null").count(), 3);
        assert_eq!(dialog.read_with(cx, |d, _| (d.busy, d.progress)), (false, None));
    }
}
