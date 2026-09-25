use std::time::Duration;

use barsql_app::BarApp;
use barsql_core::{ConnectionConfig, DriverType};
use gpui_kit::{TestAppContext, VisualTestContext};

use crate::{actions, grid, list_nav, plan_view, saved_queries, schema, schema_tree, shortcuts, state, theme};

pub struct Env {
    pub runtime: tokio::runtime::Runtime,
    _dir: tempfile::TempDir,
    pub bar: BarApp,
    pub connection: ConnectionConfig,
}

impl Env {
    pub fn new(cx: &mut TestAppContext) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        let bar = BarApp::open(&data, runtime.handle().clone()).unwrap();
        let config = ConnectionConfig {
            name: "Local".into(),
            driver: DriverType::Sqlite,
            file_path: dir.path().join("ui.db").display().to_string(),
            color: "#22c55e".into(),
            ..Default::default()
        };
        let connection = runtime.block_on(bar.save_connection(config)).unwrap();
        let seed = "CREATE TABLE things AS WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 3000) \
                    SELECT i AS id, 'thing ' || i AS name FROM n";
        runtime.block_on(bar.execute_query(&connection.id, seed)).unwrap();
        let app = bar.clone();
        // App work runs on tokio threads, which wake the test executor from outside.
        cx.executor().allow_parking();
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_reduce_motion(true);
            crate::syntax::init();
            crate::assets::load_fonts(cx);
            state::init(app, cx);
            schema::init(cx);
            saved_queries::init(cx);
            theme::init(cx);
            actions::init(cx);
            grid::init(cx);
            plan_view::init(cx);
            schema_tree::init(cx);
            list_nav::init(cx);
            crate::editor_commands::init(cx);
            crate::completion::init(cx);
            shortcuts::init(cx);
        });
        Self { runtime, _dir: dir, bar, connection }
    }
}

// Work runs on tokio, outside the test executor, so wait for it in real time.
pub fn settle(cx: &mut VisualTestContext, mut done: impl FnMut(&mut VisualTestContext) -> bool) {
    for _ in 0..1000 {
        cx.run_until_parked();
        if done(cx) {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("timed out waiting for the UI to settle");
}
