#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod single_instance;

use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::Path;

use anyhow::Context as _;
use barsql_app::{BarApp, find_sqlite_arg};
use barsql_core::clock::now_rfc3339;
use barsql_ui::LaunchOptions;
use single_instance::Instance;

// Past this the log starts over, so a run of panics can't fill the disk.
const CRASH_LOG_LIMIT: u64 = 1 << 20;

fn main() -> anyhow::Result<()> {
    barsql_app::install_default_provider();
    if let Some(code) = barsql_app::update::helper_mode() {
        std::process::exit(code);
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let data_dir = barsql_core::paths::ensure_data_dir().context("data folder")?;
    log_panics(&data_dir);
    // Started by an AI app: no window, and never handed to a running BarSQL.
    if args.first().is_some_and(|arg| arg == "--mcp") {
        let runtime = runtime()?;
        return barsql_app::mcp::serve(&data_dir, runtime.handle().clone()).context("mcp");
    }
    let snapshot = std::env::var_os("BARSQL_SNAPSHOT").filter(|p| !p.is_empty()).map(Into::into);
    let screenshots = std::env::var_os("BARSQL_SCREENSHOTS").filter(|p| !p.is_empty()).map(Into::into);
    let demo_video = std::env::var_os("BARSQL_DEMO_VIDEO").filter(|p| !p.is_empty()).map(Into::into);
    let listener = match snapshot.as_ref().or(screenshots.as_ref()).or(demo_video.as_ref()) {
        Some(_) => None,
        None => match single_instance::claim(&data_dir, &args) {
            Instance::First(listener) => listener,
            Instance::Later => return Ok(()),
        },
    };
    barsql_app::update::sweep_update_leftovers();
    let runtime = runtime()?;
    let bar = BarApp::open(&data_dir, runtime.handle().clone()).context("open data folder")?;
    if let Some(path) = find_sqlite_arg(&args) {
        bar.set_pending_file(&path);
    }
    if let Some(listener) = listener {
        let bar = bar.clone();
        single_instance::serve(listener, move |args| {
            if let Some(path) = find_sqlite_arg(&args) {
                bar.open_sqlite(&path);
            }
            bar.activate();
        });
    }
    let snapshot_run = std::env::var_os("BARSQL_SNAPSHOT_RUN").is_some_and(|v| v == "1");
    let snapshot_panel = std::env::var("BARSQL_SNAPSHOT_PANEL").ok();
    // "1920x1080"
    let snapshot_size = std::env::var("BARSQL_SNAPSHOT_SIZE").ok().and_then(|size| {
        let (width, height) = size.split_once('x')?;
        Some((width.trim().parse().ok()?, height.trim().parse().ok()?))
    });
    let check_updates =
        cfg!(feature = "production") && snapshot.is_none() && screenshots.is_none() && demo_video.is_none();
    let options =
        LaunchOptions { snapshot, snapshot_run, snapshot_panel, snapshot_size, screenshots, demo_video, check_updates };
    barsql_ui::run(bar, options);
    Ok(())
}

// A window launch has nowhere to print a panic, so it goes to crash.log in the data folder. Appended: a panic in an
// OS callback panics again on its way out ("cannot unwind"), and that report mustn't replace the first.
fn log_panics(data_dir: &Path) {
    let path = data_dir.join("crash.log");
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let backtrace = std::backtrace::Backtrace::force_capture();
        let thread = std::thread::current();
        let report = format!(
            "{} BarSQL {}: thread '{}' {info}\n\n{backtrace:#}\n",
            now_rfc3339(),
            env!("CARGO_PKG_VERSION"),
            thread.name().unwrap_or("<unnamed>")
        );
        let _ = record(&path, &report);
        default(info);
    }));
}

fn record(path: &Path, report: &str) -> std::io::Result<()> {
    if std::fs::metadata(path).is_ok_and(|meta| meta.len() > CRASH_LOG_LIMIT) {
        return std::fs::write(path, report);
    }
    OpenOptions::new().create(true).append(true).open(path)?.write_all(report.as_bytes())
}

fn runtime() -> anyhow::Result<tokio::runtime::Runtime> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .thread_stack_size(barsql_app::WORKER_STACK)
        .enable_all()
        .build()
        .context("tokio runtime")?;
    Ok(runtime)
}

#[cfg(test)]
mod tests {
    use super::{CRASH_LOG_LIMIT, log_panics, record};

    #[test]
    fn crash_log_starts_over_once_it_passes_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("crash.log");
        record(&path, &"x".repeat(CRASH_LOG_LIMIT as usize + 1)).unwrap();
        record(&path, "latest\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "latest\n");
    }

    #[test]
    fn a_second_panic_adds_to_crash_log_instead_of_replacing_the_first() {
        let dir = tempfile::tempdir().unwrap();
        log_panics(dir.path());
        let _ = std::panic::catch_unwind(|| panic!("the real panic"));
        let _ = std::panic::catch_unwind(|| panic!("panic in a function that cannot unwind"));
        let log = std::fs::read_to_string(dir.path().join("crash.log")).unwrap();
        let (first, second) = (log.find("the real panic"), log.find("cannot unwind"));
        assert!(first.is_some() && first < second, "{log}");
    }
}
