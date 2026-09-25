use std::fs::File;
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::ExportChunk;

// Progress moves once per write, so this also sets its granularity.
const WRITE_BYTES: usize = 256 << 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteOutcome {
    pub rows: usize,
    // If set, the file has `rows` rows and no closing text for the format.
    pub cancelled: bool,
}

// Truncates first so an empty export still replaces the file. `progress` gets the rows written so far.
pub fn write_chunks(
    path: &Path,
    chunks: impl IntoIterator<Item = ExportChunk>,
    stop: &AtomicBool,
    mut progress: impl FnMut(usize),
) -> io::Result<WriteOutcome> {
    if stop.load(Ordering::Relaxed) {
        return Ok(WriteOutcome { rows: 0, cancelled: true });
    }
    let mut file = File::create(path)?;
    let (mut pending, mut pending_rows, mut written) = (String::new(), 0, 0);
    let mut flush = |pending: &mut String, pending_rows: &mut usize, written: &mut usize| -> io::Result<()> {
        file.write_all(pending.as_bytes())?;
        *written += *pending_rows;
        pending.clear();
        *pending_rows = 0;
        progress(*written);
        Ok(())
    };
    for chunk in chunks {
        if stop.load(Ordering::Relaxed) {
            return Ok(WriteOutcome { rows: written, cancelled: true });
        }
        pending.push_str(&chunk.text);
        pending_rows += chunk.rows;
        if pending.len() >= WRITE_BYTES {
            flush(&mut pending, &mut pending_rows, &mut written)?;
        }
    }
    if !pending.is_empty() {
        flush(&mut pending, &mut pending_rows, &mut written)?;
    }
    Ok(WriteOutcome { rows: written, cancelled: false })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(text: &str, rows: usize) -> ExportChunk {
        ExportChunk { text: text.into(), rows }
    }

    #[test]
    fn chunks_land_in_a_fresh_file_with_progress() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.csv");
        std::fs::write(&path, "old contents that must go").unwrap();
        let big = "x".repeat(WRITE_BYTES);
        let mut seen = Vec::new();
        let outcome = write_chunks(
            &path,
            [chunk("a,b\n", 1), chunk(&big, 2), chunk("\nz", 1)],
            &AtomicBool::new(false),
            |rows| seen.push(rows),
        )
        .unwrap();
        assert_eq!(outcome, WriteOutcome { rows: 4, cancelled: false });
        assert_eq!(seen, [3, 4]);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), format!("a,b\n{big}\nz"));
    }

    #[test]
    fn an_empty_export_empties_the_file_and_a_stop_is_an_outcome() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.json");
        std::fs::write(&path, "old").unwrap();
        let outcome = write_chunks(&path, [], &AtomicBool::new(false), |_| {}).unwrap();
        assert_eq!(outcome, WriteOutcome { rows: 0, cancelled: false });
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "");
        let stopped = write_chunks(&path, [chunk("[", 0)], &AtomicBool::new(true), |_| {}).unwrap();
        assert_eq!(stopped, WriteOutcome { rows: 0, cancelled: true });
    }
}
