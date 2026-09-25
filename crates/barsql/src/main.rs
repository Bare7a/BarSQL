#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod single_instance;

use anyhow::Context as _;
use barsql_app::{BarApp, find_sqlite_arg};
use barsql_ui::LaunchOptions;
use single_instance::Instance;

fn main() -> anyhow::Result<()> {
    if let Some(code) = barsql_app::update::helper_mode() {
        std::process::exit(code);
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let data_dir = barsql_core::paths::ensure_data_dir().context("data folder")?;
    let snapshot = std::env::var_os("BARSQL_SNAPSHOT").filter(|p| !p.is_empty()).map(Into::into);
    let screenshots = std::env::var_os("BARSQL_SCREENSHOTS").filter(|p| !p.is_empty()).map(Into::into);
    let listener = match snapshot.as_ref().or(screenshots.as_ref()) {
        Some(_) => None,
        None => match single_instance::claim(&data_dir, &args) {
            Instance::First(listener) => listener,
            Instance::Later => return Ok(()),
        },
    };
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().context("tokio runtime")?;
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
    // "2048x1152"
    let snapshot_size = std::env::var("BARSQL_SNAPSHOT_SIZE").ok().and_then(|size| {
        let (width, height) = size.split_once('x')?;
        Some((width.trim().parse().ok()?, height.trim().parse().ok()?))
    });
    let check_updates = cfg!(feature = "production") && snapshot.is_none() && screenshots.is_none();
    let options = LaunchOptions { snapshot, snapshot_run, snapshot_panel, snapshot_size, screenshots, check_updates };
    barsql_ui::run(bar, options);
    Ok(())
}
