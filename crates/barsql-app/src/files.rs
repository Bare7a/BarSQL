use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

use barsql_core::QueryError;

use crate::events::AppEvent;
use crate::{BarApp, lock};

const SQLITE_EXTENSIONS: &[&str] = &["db", "sqlite", "sqlite3", "s3db", "sl3"];

pub fn is_sqlite_file(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| SQLITE_EXTENSIONS.contains(&e.to_lowercase().as_str()))
}

// Windows and Linux file associations pass the file as an argument. macOS opens arrive as URLs instead.
pub fn find_sqlite_arg<S: AsRef<str>>(args: &[S]) -> Option<String> {
    args.iter().map(AsRef::as_ref).find(|a| is_sqlite_file(a) && Path::new(a).exists()).map(str::to_string)
}

pub use barsql_core::conn_url::path_from_file_url;

// (path, file name without extension)
pub fn sqlite_file_payload(path: &str) -> (String, String) {
    let name = Path::new(path).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    (path.to_string(), name)
}

fn open_options(truncate: bool) -> OpenOptions {
    let mut options = OpenOptions::new();
    options.write(true).create(true);
    if truncate {
        options.truncate(true);
    } else {
        options.append(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}

impl BarApp {
    pub fn save_text_file(&self, path: &str, content: &str) -> Result<(), QueryError> {
        self.append_text_file(path, content, true)
    }

    // Opens and closes the file on every call, so nothing leaks if the caller stops part-way.
    pub fn append_text_file(&self, path: &str, chunk: &str, truncate: bool) -> Result<(), QueryError> {
        if path.is_empty() {
            return Err(QueryError::message("path is empty"));
        }
        let mut file = open_options(truncate).open(path).map_err(|e| QueryError::message(e.to_string()))?;
        file.write_all(chunk.as_bytes()).map_err(|e| QueryError::message(e.to_string()))
    }

    pub fn set_pending_file(&self, path: &str) {
        *lock(&self.inner.pending_file) = Some(path.to_string());
    }

    // Launch-time file to open, handed out once.
    pub fn take_pending_file(&self) -> Option<(String, String)> {
        lock(&self.inner.pending_file).take().filter(|p| !p.is_empty()).map(|p| sqlite_file_payload(&p))
    }

    pub fn open_sqlite(&self, path: &str) {
        if path.is_empty() {
            return;
        }
        let (file_path, name) = sqlite_file_payload(path);
        self.emit(AppEvent::OpenSqlite { file_path, name });
    }

    pub fn activate(&self) {
        self.emit(AppEvent::Activate);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_sqlite_files() {
        let cases = [
            ("data.db", true),
            ("foo.sqlite", true),
            ("foo.sqlite3", true),
            ("foo.s3db", true),
            ("foo.sl3", true),
            ("FOO.SQLITE", true),
            ("path/to/x.DB", true),
            ("foo.txt", false),
            ("foo", false),
            ("sqlite", false),
            ("foo.sql", false),
            ("foo.db.bak", false),
            ("", false),
        ];
        for (path, want) in cases {
            assert_eq!(is_sqlite_file(path), want, "{path}");
        }
    }

    #[test]
    fn finds_existing_sqlite_arguments() {
        let dir = tempfile::tempdir().unwrap();
        let existing = dir.path().join("real.sqlite").display().to_string();
        std::fs::write(&existing, "x").unwrap();
        let args = ["--flag".to_string(), "missing.db".into(), existing.clone(), "ignored.txt".into()];
        assert_eq!(find_sqlite_arg(&args), Some(existing));
        assert_eq!(find_sqlite_arg(&["--flag", "missing.db"]), None);
        assert_eq!(find_sqlite_arg::<&str>(&[]), None);
        assert_eq!(sqlite_file_payload("/tmp/notes.db"), ("/tmp/notes.db".into(), "notes".into()));
    }
}
