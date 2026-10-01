mod about_dialog;
mod actions;
mod assets;
mod cell_content;
mod cell_viewer;
mod completion;
mod connection_dialog;
mod connections_panel;
mod context_menu;
mod dialogs;
mod editor_commands;
mod export_dialog;
mod file_dialogs;
mod form;
mod fuzzy;
mod grid;
mod history_panel;
mod hover_card;
pub mod i18n;
mod import_dialog;
mod insert_row;
mod json_panel;
mod list_nav;
mod modal;
mod plan_tree;
mod plan_view;
mod query_tab;
mod quick_search;
mod relative_time;
mod results;
mod saved_panel;
mod saved_queries;
#[cfg(test)]
mod scenarios;
mod schema;
mod schema_actions;
mod schema_objects;
mod schema_tree;
mod screenshots;
mod scrollbars;
mod shortcuts;
mod shortcuts_dialog;
mod sidebar;
mod snapshot;
mod spinner;
mod sql_language;
pub mod state;
mod status_bar;
mod syntax;
mod table_edits;
mod table_tab;
#[cfg(test)]
mod test_support;
mod theme;
mod tips_dialog;
mod title_bar;
mod toast;
mod tokens;
mod update_dialog;
mod window_state;
mod workspace;

use std::path::PathBuf;
use std::time::Duration;

use barsql_app::BarApp;

#[derive(Debug, Clone, Default)]
pub struct LaunchOptions {
    // Save a PNG here once the window settles, then quit.
    pub snapshot: Option<PathBuf>,
    // Run the active tab's SQL before the snapshot.
    pub snapshot_run: bool,
    // Sidebar panel or dialog to show. workspace::open has the list.
    pub snapshot_panel: Option<String>,
    // In points. Defaults to 80% of the screen.
    pub snapshot_size: Option<(f32, f32)>,
    // Play the README scenes against the forum database, save 1.png to 12.png here and quit.
    pub screenshots: Option<PathBuf>,
    // Look for a newer release 5 s after launch. Production builds only.
    pub check_updates: bool,
}

// Open transactions roll back on quit, best effort within GPUI's quit budget. Servers roll back abandoned
// sessions anyway.
pub fn run(bar: BarApp, options: LaunchOptions) {
    let app = gpui_kit::application().with_assets(assets::AppAssets);
    // Finder opens arrive as URLs, at launch and while running.
    let opener = bar.clone();
    app.on_open_urls(move |urls| {
        let path =
            urls.iter().filter_map(|url| barsql_app::path_from_file_url(url)).find(|p| barsql_app::is_sqlite_file(p));
        if let Some(path) = path {
            opener.open_sqlite(&path);
        }
    });
    app.run(move |cx| {
        gpui_kit::init(cx);
        if options.screenshots.is_some() {
            screenshots::begin(cx);
        }
        // No animations, so dialogs, menus, tooltips and toasts open and close at once.
        cx.set_reduce_motion(true);
        syntax::init();
        assets::load_fonts(cx);
        state::init(bar.clone(), cx);
        schema::init(cx);
        saved_queries::init(cx);
        theme::init(cx);
        actions::init(cx);
        grid::init(cx);
        plan_view::init(cx);
        schema_tree::init(cx);
        list_nav::init(cx);
        editor_commands::init(cx);
        completion::init(cx);
        shortcuts::init(cx);
        cx.on_app_quit(move |_| {
            let bar = bar.clone();
            let runtime = bar.runtime().clone();
            let done = runtime.spawn(async move {
                let _ = tokio::time::timeout(Duration::from_millis(150), bar.shutdown()).await;
            });
            async move {
                let _ = done.await;
            }
        })
        .detach();
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        workspace::open(options, cx);
    });
}
