use std::time::Duration;

use barsql_app::BarApp;
use barsql_core::{ConnectionConfig, DriverType};
use gpui_kit::component::Root;
use gpui_kit::{AppContext as _, Context, Entity, Render, TestAppContext, VisualTestContext, Window};

use crate::{actions, grid, list_nav, plan_view, saved_queries, schema, schema_tree, shortcuts, state, theme};

// "mod" means the platform's command key, as in the default shortcuts.
pub fn chord(keys: &str) -> String {
    keys.replace("mod-", if cfg!(target_os = "macos") { "cmd-" } else { "ctrl-" })
}

// A window with `build`'s view inside Root, like the app's, so dialogs can open.
pub fn in_root<V: Render>(
    cx: &mut TestAppContext,
    build: impl FnOnce(&mut Window, &mut Context<V>) -> V,
) -> (Entity<V>, &mut VisualTestContext) {
    let mut view = None;
    let handle = cx.add_window(|window, cx| {
        let entity = cx.new(|cx| build(window, cx));
        view = Some(entity.clone());
        Root::new(entity, window, cx)
    });
    (view.unwrap(), VisualTestContext::from_window(handle.into(), cx).into_mut())
}

pub struct Env {
    pub runtime: tokio::runtime::Runtime,
    dir: tempfile::TempDir,
    pub bar: BarApp,
    pub connection: ConnectionConfig,
}

impl Env {
    pub fn new(cx: &mut TestAppContext) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_stack_size(barsql_app::WORKER_STACK)
            .enable_all()
            .build()
            .unwrap();
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
        Self { runtime, dir, bar, connection }
    }

    pub fn block_writes(&self, file: &str) {
        let path = self.dir.path().join("data").join(file);
        let _ = std::fs::remove_file(&path);
        std::fs::create_dir(&path).unwrap();
    }

    pub fn unblock_writes(&self, file: &str) {
        std::fs::remove_dir(self.dir.path().join("data").join(file)).unwrap();
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
