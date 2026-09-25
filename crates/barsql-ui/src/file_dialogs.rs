use std::future::Future;
use std::path::PathBuf;

use gpui_kit::{App, Task};

// Tests answer the pickers themselves.
#[cfg(test)]
pub(crate) struct Stub(pub Option<PathBuf>);

#[cfg(test)]
impl gpui_kit::Global for Stub {}

#[cfg(test)]
fn stubbed(cx: &App) -> Option<Task<Option<PathBuf>>> {
    cx.try_global::<Stub>().map(|stub| Task::ready(stub.0.clone()))
}

#[cfg(not(test))]
fn stubbed(_: &App) -> Option<Task<Option<PathBuf>>> {
    None
}

// Uses rfd since GPUI's prompt has no filters or start folder. macOS panels have no filter menu, so filters
// are skipped there to keep every file pickable.
pub fn pick_sqlite_file(cx: &App) -> Task<Option<PathBuf>> {
    if let Some(stub) = stubbed(cx) {
        return stub;
    }
    let dialog = rfd::AsyncFileDialog::new().set_title("Select SQLite database");
    #[cfg(not(target_os = "macos"))]
    let dialog = dialog.add_filter("SQLite Database", &["db", "sqlite", "sqlite3"]).add_filter("All Files", &["*"]);
    run(dialog.pick_file(), cx)
}

// Keys and known_hosts live in ~/.ssh, where the picker starts.
pub fn pick_ssh_file(title: &str, cx: &App) -> Task<Option<PathBuf>> {
    if let Some(stub) = stubbed(cx) {
        return stub;
    }
    let mut dialog = rfd::AsyncFileDialog::new().set_title(title);
    if let Some(ssh) = dirs::home_dir().map(|home| home.join(".ssh")).filter(|dir| dir.is_dir()) {
        dialog = dialog.set_directory(ssh);
    }
    run(dialog.pick_file(), cx)
}

pub fn pick_import_file(kind: crate::import_dialog::Kind, cx: &App) -> Task<Option<PathBuf>> {
    if let Some(stub) = stubbed(cx) {
        return stub;
    }
    let dialog = rfd::AsyncFileDialog::new().set_title("Select file to import");
    #[cfg(not(target_os = "macos"))]
    let dialog = match kind {
        crate::import_dialog::Kind::Sql => dialog.add_filter("SQL script", &["sql"]),
        crate::import_dialog::Kind::Csv => dialog.add_filter("Delimited text", &["csv", "tsv", "txt"]),
    }
    .add_filter("All Files", &["*"]);
    #[cfg(target_os = "macos")]
    let _ = kind;
    run(dialog.pick_file(), cx)
}

pub fn pick_export_path(ext: &str, cx: &App) -> Task<Option<PathBuf>> {
    pick_save_path(&format!("export.{ext}"), cx)
}

pub fn pick_save_path(file_name: &str, cx: &App) -> Task<Option<PathBuf>> {
    if let Some(stub) = stubbed(cx) {
        return stub;
    }
    let dialog = rfd::AsyncFileDialog::new().set_file_name(file_name);
    #[cfg(not(target_os = "macos"))]
    let dialog = match file_name.rsplit_once('.') {
        Some((_, ext)) => dialog.add_filter(format!("{} files", ext.to_uppercase()), &[ext]),
        None => dialog,
    }
    .add_filter("All Files", &["*"]);
    let pick = dialog.save_file();
    cx.foreground_executor().spawn(async move { pick.await.map(|file| file.path().to_path_buf()) })
}

// macOS panels must run on the main thread.
fn run(pick: impl Future<Output = Option<rfd::FileHandle>> + 'static, cx: &App) -> Task<Option<PathBuf>> {
    cx.foreground_executor().spawn(async move { pick.await.map(|file| file.path().to_path_buf()) })
}
