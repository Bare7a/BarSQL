// The screenshots for `cargo xtask screenshots`: plays the scenes against the forum database, saves each as `<n>.png`.
use std::path::PathBuf;

use gpui_kit::component::Root;
use gpui_kit::*;

// Screenshots draw the Windows/Linux title bar, with the menus and window controls in the window.
#[cfg(feature = "snapshot")]
pub fn windows_title_bar(cx: &App) -> bool {
    cx.has_global::<script::WindowsTitleBar>()
}

#[cfg(not(feature = "snapshot"))]
pub fn windows_title_bar(_: &App) -> bool {
    false
}

#[cfg(feature = "snapshot")]
pub fn begin(video: bool, cx: &mut App) {
    cx.set_global(script::WindowsTitleBar);
    if video {
        video::watch_inputs(cx);
    }
}

#[cfg(not(feature = "snapshot"))]
pub fn begin(_: bool, _: &mut App) {}

#[cfg(feature = "snapshot")]
pub fn run(window: WindowHandle<Root>, out: PathBuf, cx: &mut AsyncApp) {
    script::run(window, out, cx);
}

#[cfg(not(feature = "snapshot"))]
pub fn run(_: WindowHandle<Root>, _: PathBuf, cx: &mut AsyncApp) {
    eprintln!("BARSQL_SCREENSHOTS needs a build with --features snapshot");
    cx.update(|cx| cx.quit());
}

#[cfg(feature = "snapshot")]
pub fn record(window: WindowHandle<Root>, out: PathBuf, cx: &mut AsyncApp) {
    video::run(window.into(), out, cx);
}

#[cfg(not(feature = "snapshot"))]
pub fn record(_: WindowHandle<Root>, _: PathBuf, cx: &mut AsyncApp) {
    eprintln!("BARSQL_DEMO_VIDEO needs a build with --features snapshot");
    cx.update(|cx| cx.quit());
}

#[cfg(feature = "snapshot")]
pub fn overlay(window: &mut Window, cx: &mut App) -> Option<AnyElement> {
    video::overlay(window, cx)
}

#[cfg(not(feature = "snapshot"))]
pub fn overlay(_: &mut Window, _: &mut App) -> Option<AnyElement> {
    None
}

#[cfg(feature = "snapshot")]
mod video;

#[cfg(feature = "snapshot")]
mod script {
    use std::io::BufWriter;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    use barsql_core::{ConnectionConfig, DriverType, StructColumn, StructIndex};
    use barsql_sql::lang::TxnControl;
    use barsql_sql::structure::{ColumnRow, Draft, Row};
    use gpui_kit::component::{Root, WindowExt};
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::*;
    use image::codecs::png::{CompressionType, FilterType, PngEncoder};
    use image::{DynamicImage, RgbaImage};

    use crate::grid::Grid;
    use crate::schema_objects::Group;
    use crate::workspace::Workspace;
    use crate::{connection_dialog, export_dialog, import_dialog, query_tab, theme};

    // In points; shots come out at 1920 × 1080 pixels on any display.
    pub(super) const SIZE: (f32, f32) = (1920., 1080.);
    const SIDEBAR_WIDTH: f32 = 364.;
    const JSON_WIDTH: f32 = 410.;
    pub(super) const WAIT: Duration = Duration::from_secs(30);
    const POSTS_SQL: &str = "SELECT * FROM posts p ORDER BY p.id;";
    pub(super) const PLAN_SQL: &str = "SELECT c.name AS category, u.display_name AS author,
       COUNT(*) AS posts, SUM(p.views) AS views
FROM posts p
JOIN users u ON u.id = p.author_id
JOIN categories c ON c.id = p.category_id
WHERE p.score > 10
GROUP BY c.name, u.display_name
ORDER BY views DESC
LIMIT 25;";
    const TXN_SQL: &str = "INSERT INTO categories
  (name, description, settings) VALUES
  ('Go', 'Talk about anything related to Go.', '{\"allowLinks\": true}'),
  ('TypeScript', 'Talk about anything related to TS.', '{\"allowLinks\": true}')
RETURNING *;

INSERT INTO categories
  (name, description, settings) VALUES
  ('Java', 'Talk about anything related to Java.', '{\"allowLinks\": true}'),
  ('Kotlin', 'Talk about anything related to Kotlin.', '{\"allowLinks\": true}')
RETURNING *;
";
    pub(super) const CHART_SQL: &str = "SELECT date_trunc('month', created_at)::date AS month,
       COUNT(*) AS posts,
       COUNT(*) FILTER (WHERE score >= 500) AS popular,
       COUNT(*) FILTER (WHERE score >= 900) AS top_rated
FROM posts
WHERE created_at >= '2025-01-01' AND created_at < '2026-06-01'
GROUP BY 1
ORDER BY 1;";
    // Column indexes in `public.users`.
    const USERNAME: usize = 1;
    const DISPLAY_NAME: usize = 3;
    const PREFERENCES: usize = 10;
    // Relative so the dialog shows a short path. The xtask runs the app from the repo root.
    pub(super) const IMPORT_CSV: &str = "fixtures/screenshots/users.csv";

    pub struct WindowsTitleBar;

    impl Global for WindowsTitleBar {}

    pub(super) type Res<T> = Result<T, String>;

    pub fn run(window: WindowHandle<Root>, out: PathBuf, cx: &mut AsyncApp) {
        cx.spawn(async move |cx| {
            let robot = Robot { window: window.into() };
            if let Err(error) = scenes(&robot, &out, cx).await {
                eprintln!("screenshots failed: {error}");
                std::process::exit(1);
            }
            println!("screenshots saved to {}", out.display());
            cx.update(|cx| cx.quit());
        })
        .detach();
    }

    // A window behind others gets no frames, so every step draws one and input lands on what it showed. The
    // handle is untyped so nothing holds the root view, which dialogs update.
    pub(super) struct Robot {
        pub(super) window: AnyWindowHandle,
    }

    impl Robot {
        pub(super) fn act<R>(
            &self,
            cx: &mut AsyncApp,
            f: impl FnOnce(&Entity<Workspace>, &mut Window, &mut App) -> R,
        ) -> Res<R> {
            self.window
                .update(cx, |root, window, cx| {
                    let root = root.downcast::<Root>().map_err(|_| "no root")?;
                    let workspace = root.read(cx).view().clone().downcast::<Workspace>().map_err(|_| "no workspace")?;
                    Ok(f(&workspace, window, cx))
                })
                .map_err(|error| error.to_string())?
        }

        pub(super) fn workspace<R>(
            &self,
            cx: &mut AsyncApp,
            f: impl FnOnce(&mut Workspace, &mut Window, &mut Context<Workspace>) -> R,
        ) -> Res<R> {
            self.act(cx, |workspace, window, cx| workspace.update(cx, |workspace, cx| f(workspace, window, cx)))
        }

        async fn pause(&self, cx: &mut AsyncApp, ms: u64) {
            cx.background_executor().timer(Duration::from_millis(ms)).await;
        }

        async fn draw(&self, cx: &mut AsyncApp) {
            let _ = self.window.update(cx, |_, window, cx| {
                window.refresh();
                window.draw(cx).clear(cx);
                // Windows sends a hidden window no frames, so its next-frame callbacks wait on these.
                window.simulate_next_frame(cx);
            });
            self.pause(cx, 40).await;
        }

        pub(super) async fn settle(
            &self,
            cx: &mut AsyncApp,
            what: &str,
            check: impl Fn(&Entity<Workspace>, &mut Window, &mut App) -> bool,
        ) -> Res<()> {
            let deadline = Instant::now() + WAIT;
            loop {
                self.draw(cx).await;
                // What it waited for may have arrived after that frame, so it gets one of its own before use.
                if self.act(cx, &check)? {
                    self.draw(cx).await;
                    return Ok(());
                }
                if Instant::now() > deadline {
                    return Err(format!("timed out waiting for {what}"));
                }
                self.pause(cx, 60).await;
            }
        }

        async fn click(&self, cx: &mut AsyncApp, position: Point<Pixels>, modifiers: Modifiers, count: usize) {
            self.draw(cx).await;
            let _ = self.window.update(cx, |_, window, cx| {
                let moved = MouseMoveEvent { position, pressed_button: None, modifiers };
                window.dispatch_event(PlatformInput::MouseMove(moved), cx);
                for click_count in 1..=count {
                    let button = MouseButton::Left;
                    let down = MouseDownEvent { button, position, modifiers, click_count, first_mouse: false };
                    window.dispatch_event(PlatformInput::MouseDown(down), cx);
                    let up = MouseUpEvent { button, position, modifiers, click_count };
                    window.dispatch_event(PlatformInput::MouseUp(up), cx);
                }
            });
            self.draw(cx).await;
        }

        // Space-separated keystrokes, with `mod` as the platform's command key.
        async fn keys(&self, cx: &mut AsyncApp, keys: &str) -> Res<()> {
            let os = cx.update(|cx| cx.platform_info().os);
            for key in keys.split_whitespace() {
                let key = key.replace("mod-", "secondary-");
                let keystroke = Keystroke::parse_with_os(&key, os).map_err(|error| error.to_string())?;
                self.draw(cx).await;
                let _ = self.window.update(cx, |_, window, cx| window.dispatch_keystroke(keystroke, cx));
            }
            self.draw(cx).await;
            Ok(())
        }

        async fn type_text(&self, cx: &mut AsyncApp, text: &str) {
            for ch in text.chars() {
                let key = if ch == ' ' { "space".to_string() } else { ch.to_lowercase().to_string() };
                let keystroke = Keystroke { modifiers: Modifiers::none(), key, key_char: Some(ch.to_string()) };
                let _ = self.window.update(cx, |_, window, cx| window.dispatch_keystroke(keystroke, cx));
            }
            self.draw(cx).await;
        }

        // Opaque and compressed hard, since the pictures live in the repository.
        async fn capture(&self, cx: &mut AsyncApp, path: &Path) -> Res<()> {
            self.pause(cx, 300).await;
            self.draw(cx).await;
            let image = self
                .window
                .update(cx, |_, window, cx| {
                    window.draw(cx).clear(cx);
                    window.render_to_image().map_err(|error| error.to_string())
                })
                .map_err(|error| error.to_string())??;
            let (width, height) = (SIZE.0 as u32, SIZE.1 as u32);
            let image = match image.dimensions() == (width, height) {
                true => image,
                false => shrink(&image, width, height),
            };
            let file = std::fs::File::create(path).map_err(|error| format!("{}: {error}", path.display()))?;
            let encoder =
                PngEncoder::new_with_quality(BufWriter::new(file), CompressionType::Best, FilterType::Adaptive);
            DynamicImage::ImageRgba8(image)
                .into_rgb8()
                .write_with_encoder(encoder)
                .map_err(|error| error.to_string())?;
            println!("saved {}", path.display());
            Ok(())
        }

        // In the last frame. Two matches are an error, not try_find's panic, which would abort the app.
        pub(super) fn bounds(&self, cx: &mut AsyncApp, id: impl Into<ElementId>) -> Res<Bounds<Pixels>> {
            let id = id.into();
            let found = self.act(cx, |_, window, _| window.find_all(id.clone()))?;
            match found.as_slice() {
                [element] => Ok(element.bounds()),
                [] => Err(format!("{id} was not drawn")),
                _ => Err(format!("{id} was drawn {} times", found.len())),
            }
        }

        pub(super) fn center(&self, cx: &mut AsyncApp, id: impl Into<ElementId>) -> Res<Point<Pixels>> {
            Ok(self.bounds(cx, id)?.center())
        }

        // Inputs all register under one id, and a password under its own role, so take the scope's first.
        pub(super) fn field(&self, cx: &mut AsyncApp, scope: &'static str) -> Res<Point<Pixels>> {
            let found = self
                .act(cx, |_, window, _| window.within(scope).elements().first().map(|input| input.bounds().center()))?;
            found.ok_or_else(|| format!("no input in {scope}"))
        }

        // A schema tree row by its scenario id, such as `t:public.users`.
        pub(super) fn tree_row(&self, cx: &mut AsyncApp, row: &str) -> Res<Point<Pixels>> {
            let id = self.act(cx, |workspace, _, cx| {
                let tree = workspace.read(cx).sidebar().read(cx).panels().0;
                tree.read(cx).row_id(row)
            })?;
            self.center(cx, id.ok_or_else(|| format!("no tree row {row}"))?)
        }

        async fn close_dialog(&self, cx: &mut AsyncApp) -> Res<()> {
            let _ = self.window.update(cx, |_, window, cx| window.close_dialog(cx));
            self.settle(cx, "the dialog to close", |_, window, cx| !window.has_active_dialog(cx)).await
        }

        fn grid(&self, cx: &mut AsyncApp) -> Res<Entity<Grid>> {
            self.act(cx, |workspace, _, cx| workspace.read(cx).active_grid(cx))?.ok_or_else(|| "no grid".into())
        }

        pub(super) fn cell(&self, cx: &mut AsyncApp, row: usize, col: usize) -> Res<Point<Pixels>> {
            let grid = self.grid(cx)?;
            Ok(cx.update(|cx| grid.read(cx).cell_point(row, col)))
        }

        pub(super) fn header(&self, cx: &mut AsyncApp, col: usize) -> Res<Point<Pixels>> {
            let grid = self.grid(cx)?;
            Ok(cx.update(|cx| grid.read(cx).header_point(col)))
        }

        pub(super) fn gutter(&self, cx: &mut AsyncApp, row: usize) -> Res<Point<Pixels>> {
            let grid = self.grid(cx)?;
            Ok(cx.update(|cx| grid.read(cx).gutter_point(row)))
        }

        pub(super) fn structure_cell(&self, cx: &mut AsyncApp, row: usize, col: usize) -> Res<Point<Pixels>> {
            let grid =
                self.act(cx, |workspace, _, cx| workspace.read(cx).structure_tab().map(|tab| tab.read(cx).grid()))?;
            let grid = grid.ok_or("no Structure tab")?;
            Ok(cx.update(|cx| grid.read(cx).cell_point(row, col)))
        }

        // Waits out the scrollbars' idle hold, so they are gone from the shot.
        async fn scroll_grid(&self, cx: &mut AsyncApp, row: usize, col: usize) -> Res<()> {
            let grid = self.grid(cx)?;
            cx.update(|cx| grid.update(cx, |grid, cx| grid.scroll_to_cell(row, col, cx)));
            self.draw(cx).await;
            self.pause(cx, 2200).await;
            Ok(())
        }

        pub(super) async fn fit_window(&self, cx: &mut AsyncApp) -> Res<()> {
            let target = size(px(SIZE.0), px(SIZE.1));
            let deadline = Instant::now() + WAIT;
            while self.act(cx, |_, window, _| window.viewport_size())? != target {
                if Instant::now() > deadline {
                    return Err("the window would not grow to 1920 × 1080".into());
                }
                self.act(cx, |_, window, _| window.resize(target))?;
                self.pause(cx, 250).await;
            }
            self.draw(cx).await;
            Ok(())
        }

        // Toggling a side panel rescales the others, so put them back to the demo widths.
        async fn panel_widths(&self, cx: &mut AsyncApp) -> Res<()> {
            self.draw(cx).await;
            self.workspace(cx, |workspace, window, cx| {
                workspace.set_panel_widths(px(SIDEBAR_WIDTH), px(JSON_WIDTH), window, cx)
            })?;
            self.draw(cx).await;
            Ok(())
        }

        async fn open_query(&self, cx: &mut AsyncApp, title: &str, sql: &str) -> Res<()> {
            self.workspace(cx, |workspace, window, cx| {
                let forum = workspace.sidebar().read(cx).connection("forum").cloned().ok_or("no forum connection")?;
                workspace.open_query(forum, title, sql, window, cx);
                Ok::<(), String>(())
            })??;
            self.draw(cx).await;
            Ok(())
        }

        pub(super) async fn activate(&self, cx: &mut AsyncApp, title: &str) -> Res<()> {
            let found = self.workspace(cx, |workspace, window, cx| workspace.activate_title(title, window, cx))?;
            if !found {
                return Err(format!("no {title} tab"));
            }
            self.draw(cx).await;
            Ok(())
        }

        async fn users_loaded(&self, cx: &mut AsyncApp) -> Res<()> {
            self.settle(cx, "the users table", |workspace, _, cx| {
                workspace.read(cx).table_tab().is_some_and(|tab| tab.read(cx).idle())
            })
            .await
        }

        async fn tree_lists(&self, cx: &mut AsyncApp, what: &str, ids: &'static [&'static str]) -> Res<()> {
            self.settle(cx, what, move |workspace, _, cx| {
                let tree = workspace.read(cx).sidebar().read(cx).panels().0;
                let listed = tree.read(cx).listed();
                ids.iter().all(|id| listed.iter().any(|(row, ..)| row == id))
            })
            .await
        }
    }

    fn query_running(workspace: &Entity<Workspace>, cx: &App) -> bool {
        workspace.read(cx).query_tab().is_some_and(|tab| tab.read(cx).is_running())
    }

    // Area averaging: a 2x render comes down in exact 2 × 2 blocks, a 125% one weighs pixels by overlap.
    fn shrink(image: &RgbaImage, width: u32, height: u32) -> RgbaImage {
        let (columns, rows) = (areas(image.width(), width), areas(image.height(), height));
        let (from, to) = (image.width() as usize * 4, width as usize * 4);
        let mut out = RgbaImage::new(width, height);
        let mut line = vec![0f32; from];
        for ((first, shares), target) in rows.iter().zip(out.chunks_exact_mut(to)) {
            line.fill(0.);
            for (row, share) in image.chunks_exact(from).skip(*first).zip(shares) {
                for (sum, &value) in line.iter_mut().zip(row) {
                    *sum += share * f32::from(value);
                }
            }
            for ((first, shares), pixel) in columns.iter().zip(target.as_chunks_mut::<4>().0) {
                for (channel, value) in pixel.iter_mut().enumerate() {
                    let sum: f32 =
                        shares.iter().enumerate().map(|(i, share)| share * line[(first + i) * 4 + channel]).sum();
                    *value = (sum + 0.5) as u8;
                }
            }
        }
        out
    }

    // Along one axis, each output pixel's first source pixel and the shares of those it covers.
    fn areas(from: u32, to: u32) -> Vec<(usize, Vec<f32>)> {
        let ratio = f64::from(from) / f64::from(to);
        (0..to)
            .map(|ix| {
                let (start, end) = (f64::from(ix) * ratio, f64::from(ix + 1) * ratio);
                let (first, past) = (start.floor() as usize, (end.ceil() as usize).min(from as usize));
                let share = |px: usize| ((end.min(px as f64 + 1.) - start.max(px as f64)) / ratio) as f32;
                (first, (first..past).map(share).collect())
            })
            .collect()
    }

    // A slug column with a unique index, a longer title and is_locked dropped, typed as the catalog spells them.
    fn stage_posts(draft: &mut Draft) {
        let mut text = String::new();
        for row in &mut draft.columns {
            match row.column.name.as_str() {
                "title" => {
                    text = row.column.data_type.clone();
                    row.column.data_type = text.replace("255", "300");
                }
                "is_locked" => row.dropped = true,
                _ => {}
            }
        }
        let slug = StructColumn { name: "slug".into(), data_type: text, nullable: true, ..Default::default() };
        draft.columns.push(ColumnRow { origin: None, dropped: false, column: slug, primary: false });
        let index = StructIndex {
            name: "posts_slug_idx".into(),
            columns: vec!["slug".into()],
            unique: true,
            ..Default::default()
        };
        draft.indexes.push(Row::added(index));
    }

    fn new_connection() -> ConnectionConfig {
        ConnectionConfig {
            name: "My Database".into(),
            driver: DriverType::Postgres,
            database: "blog".into(),
            read_only: true,
            ..connection_dialog::new_connection()
        }
    }

    async fn scenes(robot: &Robot, out: &Path, cx: &mut AsyncApp) -> Res<()> {
        std::fs::create_dir_all(out).map_err(|error| format!("{}: {error}", out.display()))?;
        let shot = |n: u8| out.join(format!("{n}.png"));
        robot.fit_window(cx).await?;
        robot.panel_widths(cx).await?;
        robot.users_loaded(cx).await?;

        // 1: saved query rows with suggestions after `p.`.
        robot.activate(cx, "Get All Posts").await?;
        robot.act(cx, |workspace, window, cx| {
            let tab = workspace.read(cx).query_tab()?;
            tab.update(cx, |tab, cx| tab.run_all(window, cx));
            Some(())
        })?;
        robot
            .settle(cx, "the posts", |workspace, _, cx| {
                !query_running(workspace, cx)
                    && workspace.read(cx).active_grid(cx).is_some_and(|grid| grid.read(cx).rows_shown() == 500)
            })
            .await?;
        let first = robot.cell(cx, 0, 0)?;
        robot.click(cx, first, Modifiers::none(), 1).await;
        robot.act(cx, |workspace, window, cx| {
            let sidebar = workspace.read(cx).sidebar();
            sidebar.update(cx, |sidebar, cx| sidebar.show_panel("saved", window, cx));
            let completion = workspace.read(cx).query_tab()?.read(cx).completion();
            completion.update(cx, |completion, cx| completion.preview("SELECT * FROM posts p ORDER BY p.", window, cx));
            Some(())
        })?;
        robot
            .settle(cx, "the suggestions", |workspace, _, cx| {
                workspace.read(cx).query_tab().is_some_and(|tab| tab.read(cx).completion().read(cx).is_open(cx))
            })
            .await?;
        robot.capture(cx, &shot(1)).await?;
        robot.keys(cx, "escape").await?;
        robot.act(cx, |workspace, window, cx| {
            let tab = workspace.read(cx).query_tab()?;
            tab.update(cx, |tab, cx| tab.set_sql(POSTS_SQL.into(), window, cx));
            Some(())
        })?;

        // 2: a transaction with two inserts that each return rows.
        robot.open_query(cx, "Query 3 · Forum", "").await?;
        robot.act(cx, |workspace, window, cx| {
            let tab = workspace.read(cx).query_tab()?;
            tab.update(cx, |tab, cx| tab.transaction(TxnControl::Begin, false, window, cx));
            Some(())
        })?;
        robot
            .settle(cx, "the transaction", |workspace, _, cx| {
                workspace.read(cx).query_tab().is_some_and(|tab| tab.read(cx).in_transaction())
            })
            .await?;
        robot.act(cx, |workspace, window, cx| {
            let tab = workspace.read(cx).query_tab()?;
            tab.update(cx, |tab, cx| {
                tab.set_sql(TXN_SQL.into(), window, cx);
                tab.run_all(window, cx);
            });
            Some(())
        })?;
        robot
            .settle(cx, "both results", |workspace, _, cx| {
                !query_running(workspace, cx)
                    && workspace
                        .read(cx)
                        .query_tab()
                        .is_some_and(|tab| tab.read(cx).results().read(cx).result_count() == 2)
            })
            .await?;
        robot.act(cx, |workspace, window, cx| {
            let tab = workspace.read(cx).query_tab()?;
            tab.read(cx).results().update(cx, |results, cx| results.select_result(1, cx));
            let sidebar = workspace.read(cx).sidebar();
            sidebar.update(cx, |sidebar, cx| sidebar.show_panel("schema", window, cx));
            Some(())
        })?;
        robot.draw(cx).await;
        let first = robot.cell(cx, 0, 0)?;
        robot.click(cx, first, Modifiers::none(), 1).await;
        robot.capture(cx, &shot(2)).await?;
        robot.act(cx, |workspace, window, cx| {
            let tab = workspace.read(cx).query_tab()?;
            tab.update(cx, |tab, cx| tab.transaction(TxnControl::Rollback, false, window, cx));
            Some(())
        })?;
        robot
            .settle(cx, "the rollback", |workspace, _, cx| {
                workspace.read(cx).query_tab().is_some_and(|tab| !tab.read(cx).in_transaction())
            })
            .await?;
        robot.workspace(cx, |workspace, window, cx| workspace.close_active(window, cx))?;

        // 3: users with a staged edit and two deletes, a column search, a JSON filter and filter suggestions.
        robot.activate(cx, "users").await?;
        robot.users_loaded(cx).await?;
        robot.act(cx, |workspace, window, cx| {
            let tree = workspace.read(cx).sidebar().read(cx).panels().0;
            tree.update(cx, |tree, cx| tree.search_for("id", window, cx));
        })?;
        robot
            .tree_lists(
                cx,
                "the column search",
                &["c:public.posts.author_id", "c:public.post_reactions.user_id", "c:public.users.id"],
            )
            .await?;
        let edited = robot.cell(cx, 3, USERNAME)?;
        robot.click(cx, edited, Modifiers::none(), 2).await;
        robot.keys(cx, "mod-a").await?;
        robot.type_text(cx, "maria_young82").await;
        robot.keys(cx, "enter").await?;
        for row in [5, 6] {
            let at = robot.cell(cx, row, 0)?;
            robot.click(cx, at, Modifiers::none(), 1).await;
            robot.keys(cx, "delete").await?;
        }
        robot
            .settle(cx, "the staged changes", |workspace, _, cx| {
                workspace.read(cx).table_tab().is_some_and(|tab| tab.read(cx).pending() == (1, 2))
            })
            .await?;
        let focused = robot.cell(cx, 0, DISPLAY_NAME)?;
        robot.click(cx, focused, Modifiers::none(), 1).await;
        robot.act(cx, |workspace, window, cx| {
            let json = workspace.read(cx).json_panel()?;
            json.update(cx, |json, cx| json.set_filter("la", window, cx));
            let completion = workspace.read(cx).table_tab()?.read(cx).completion();
            completion.update(cx, |completion, cx| completion.preview("id < 20 AND di", window, cx));
            Some(())
        })?;
        robot
            .settle(cx, "the filter suggestions", |workspace, _, cx| {
                workspace.read(cx).table_tab().is_some_and(|tab| tab.read(cx).completion().read(cx).is_open(cx))
            })
            .await?;
        robot.capture(cx, &shot(3)).await?;
        robot.keys(cx, "escape").await?;
        robot.act(cx, |workspace, window, cx| {
            let json = workspace.read(cx).json_panel()?;
            json.update(cx, |json, cx| json.set_filter("", window, cx));
            let tree = workspace.read(cx).sidebar().read(cx).panels().0;
            tree.update(cx, |tree, cx| tree.search_for("", window, cx));
            let tab = workspace.read(cx).table_tab()?;
            tab.update(cx, |tab, cx| tab.start_over(window, cx));
            Some(())
        })?;
        robot
            .settle(cx, "the users table again", |workspace, _, cx| {
                workspace
                    .read(cx)
                    .table_tab()
                    .is_some_and(|tab| tab.read(cx).idle() && tab.read(cx).pending() == (0, 0))
            })
            .await?;
        robot.tree_lists(cx, "the whole tree", &["c:public.users.preferences", "t:public.posts"]).await?;

        // 4: a JSON cell in the cell editor.
        robot.scroll_grid(cx, 0, PREFERENCES).await?;
        let preferences = robot.cell(cx, 32, PREFERENCES)?;
        robot.click(cx, preferences, Modifiers::none(), 1).await;
        robot.keys(cx, "shift-enter").await?;
        robot.settle(cx, "the cell editor", |_, window, cx| window.has_active_dialog(cx)).await?;
        robot.capture(cx, &shot(4)).await?;
        robot.close_dialog(cx).await?;

        // 5: a 14x3 selection and the copy formats.
        robot.scroll_grid(cx, 0, 0).await?;
        let from = robot.cell(cx, 4, USERNAME)?;
        robot.click(cx, from, Modifiers::none(), 1).await;
        let to = robot.cell(cx, 17, DISPLAY_NAME)?;
        robot.click(cx, to, Modifiers::shift(), 1).await;
        robot
            .settle(cx, "the selection", |workspace, _, cx| {
                workspace.read(cx).active_grid(cx).is_some_and(|grid| grid.read(cx).selection_counts() == (14, 3))
            })
            .await?;
        let formats = robot.center(cx, "copy-format")?;
        robot.click(cx, formats, Modifiers::none(), 1).await;
        robot.capture(cx, &shot(5)).await?;
        robot.keys(cx, "escape").await?;

        // 6: exporting the selection.
        robot.act(cx, |workspace, window, cx| {
            let source = workspace.read(cx).active_grid(cx)?.read(cx).export_source();
            export_dialog::open(source, window, cx);
            Some(())
        })?;
        robot.settle(cx, "the export dialog", |_, window, cx| window.has_active_dialog(cx)).await?;
        robot.capture(cx, &shot(6)).await?;
        robot.close_dialog(cx).await?;

        // 7: New Connection over the connections panel with its folders.
        robot.act(cx, |workspace, window, cx| {
            let sidebar = workspace.read(cx).sidebar();
            sidebar.update(cx, |sidebar, cx| sidebar.show_panel("connections", window, cx));
            connection_dialog::open(Some(new_connection()), window, cx, |_, _, _| {});
        })?;
        robot.settle(cx, "the connection dialog", |_, window, cx| window.has_active_dialog(cx)).await?;
        robot.capture(cx, &shot(7)).await?;
        robot.close_dialog(cx).await?;
        robot.act(cx, |workspace, _, cx| {
            let sidebar = workspace.read(cx).sidebar();
            sidebar.update(cx, |sidebar, cx| sidebar.close_switcher(cx));
        })?;

        // 8: Quick Search on the users tab.
        robot.workspace(cx, |workspace, window, cx| {
            let dialog = workspace.open_quick_search(window, cx)?;
            dialog.update(cx, |dialog, cx| dialog.highlight("users", cx));
            Some(())
        })?;
        robot.settle(cx, "Quick Search", |_, window, cx| window.has_active_dialog(cx)).await?;
        robot.capture(cx, &shot(8)).await?;
        robot.close_dialog(cx).await?;

        // 9: users DDL in a tab, with its indexes and constraints open in the tree.
        robot.workspace(cx, |workspace, window, cx| workspace.set_json_open(false, window, cx))?;
        robot.panel_widths(cx).await?;
        robot.act(cx, |workspace, _, cx| {
            let tree = workspace.read(cx).sidebar().read(cx).panels().0;
            tree.update(cx, |tree, cx| {
                tree.expand_group("public", "users", Group::Indexes, cx);
                tree.expand_group("public", "users", Group::Constraints, cx);
            });
        })?;
        robot
            .tree_lists(
                cx,
                "the indexes and constraints",
                &["o:public:users:indexes:users_pkey", "o:public:users:constraints:users_pkey"],
            )
            .await?;
        robot.act(cx, |workspace, window, cx| {
            let tree = workspace.read(cx).sidebar().read(cx).panels().0;
            tree.update(cx, |tree, cx| tree.open_table_ddl("public", "users", window, cx));
        })?;
        robot
            .settle(cx, "the DDL tab", |workspace, _, cx| {
                let workspace = workspace.read(cx);
                workspace.active_title(cx) == "DDL: users"
                    && workspace.active_sql(cx).is_some_and(|sql| sql.contains("CREATE TABLE"))
            })
            .await?;
        robot.act(cx, |_, _, cx| query_tab::set_results_share(15., cx))?;
        robot.capture(cx, &shot(9)).await?;
        robot.workspace(cx, |workspace, window, cx| workspace.close_active(window, cx))?;

        // 10: EXPLAIN ANALYZE plan with the hottest node selected.
        robot.activate(cx, "users").await?;
        robot.open_query(cx, "Query 3 · Forum", PLAN_SQL).await?;
        robot.act(cx, |workspace, window, cx| {
            let tab = workspace.read(cx).query_tab()?;
            tab.update(cx, |tab, cx| tab.explain(true, window, cx));
            Some(())
        })?;
        robot
            .settle(cx, "the plan", |workspace, _, cx| {
                !query_running(workspace, cx)
                    && workspace
                        .read(cx)
                        .query_tab()
                        .is_some_and(|tab| tab.read(cx).results().read(cx).active_plan().is_some())
            })
            .await?;
        robot.act(cx, |workspace, _, cx| {
            query_tab::set_results_share(58., cx);
            let plan = workspace.read(cx).query_tab()?.read(cx).results().read(cx).active_plan()?;
            plan.update(cx, |plan, cx| plan.select_hottest(cx));
            Some(())
        })?;
        robot.capture(cx, &shot(10)).await?;
        robot.act(cx, |_, _, cx| query_tab::set_results_share(40., cx))?;
        robot.workspace(cx, |workspace, window, cx| workspace.close_active(window, cx))?;

        // 11: a CSV loaded into the importer but not run.
        robot.workspace(cx, |workspace, window, cx| workspace.set_json_open(true, window, cx))?;
        robot.activate(cx, "users").await?;
        robot.panel_widths(cx).await?;
        robot.scroll_grid(cx, 17, 0).await?;
        robot.act(cx, |workspace, window, cx| {
            let tree = workspace.read(cx).sidebar().read(cx).panels().0;
            tree.update(cx, |tree, cx| tree.import_into("public", "users", window, cx));
        })?;
        robot.act(cx, |_, window, cx| {
            let dialog = cx.try_global::<import_dialog::Opened>().and_then(|opened| opened.0.upgrade())?;
            dialog.update(cx, |dialog, cx| dialog.choose(IMPORT_CSV.into(), window, cx));
            Some(())
        })?;
        robot
            .settle(cx, "the CSV preview", |_, _, cx| {
                let dialog = cx.try_global::<import_dialog::Opened>().and_then(|opened| opened.0.upgrade());
                dialog.is_some_and(|dialog| dialog.read(cx).preview_loaded())
            })
            .await?;
        robot.capture(cx, &shot(11)).await?;
        robot.close_dialog(cx).await?;

        // 12: posts in a Structure tab, with changes staged in its columns and indexes.
        robot.workspace(cx, |workspace, window, cx| workspace.set_json_open(false, window, cx))?;
        robot.panel_widths(cx).await?;
        robot.workspace(cx, |workspace, window, cx| {
            let forum = workspace.sidebar().read(cx).connection("forum").cloned().ok_or("no forum connection")?;
            workspace.open_structure(forum, "public".into(), Some("posts".into()), window, cx);
            Ok::<(), String>(())
        })??;
        robot
            .settle(cx, "the posts structure", |workspace, _, cx| {
                workspace.read(cx).structure_tab().is_some_and(|tab| tab.read(cx).editable())
            })
            .await?;
        robot.act(cx, |workspace, window, cx| {
            let tab = workspace.read(cx).structure_tab()?;
            tab.update(cx, |tab, cx| tab.stage(stage_posts, window, cx));
            Some(())
        })?;
        robot
            .settle(cx, "the staged changes", |workspace, _, cx| {
                workspace.read(cx).structure_tab().is_some_and(|tab| tab.read(cx).has_changes())
            })
            .await?;
        robot.capture(cx, &shot(12)).await?;
        robot.act(cx, |workspace, window, cx| {
            let tab = workspace.read(cx).structure_tab()?;
            tab.update(cx, |tab, cx| tab.revert(window, cx));
            Some(())
        })?;
        robot.workspace(cx, |workspace, window, cx| workspace.close_active(window, cx))?;

        // 13: the public schema's ER diagram, zoomed in, with posts' keys lit.
        robot.act(cx, |workspace, _, cx| {
            let tree = workspace.read(cx).sidebar().read(cx).panels().0;
            tree.update(cx, |tree, cx| tree.open_er_diagram("public".into(), cx));
        })?;
        robot
            .settle(cx, "the diagram", |workspace, _, cx| {
                workspace.read(cx).diagram_tab().is_some_and(|diagram| diagram.read(cx).fitted())
            })
            .await?;
        robot.act(cx, |workspace, window, cx| {
            let diagram = workspace.read(cx).diagram_tab()?;
            diagram.update(cx, |diagram, cx| {
                diagram.select_table("posts", cx);
                diagram.set_zoom(1.75, window, cx);
            });
            Some(())
        })?;
        robot.capture(cx, &shot(13)).await?;
        robot.workspace(cx, |workspace, window, cx| workspace.close_active(window, cx))?;

        // 14: posts a month as grouped bars, beside the popular and top-rated ones.
        robot.open_query(cx, "Query 3 · Forum", CHART_SQL).await?;
        robot.act(cx, |workspace, window, cx| {
            let tab = workspace.read(cx).query_tab()?;
            tab.update(cx, |tab, cx| tab.run_all(window, cx));
            Some(())
        })?;
        robot
            .settle(cx, "the monthly posts", |workspace, _, cx| {
                !query_running(workspace, cx)
                    && workspace.read(cx).active_grid(cx).is_some_and(|grid| grid.read(cx).rows_shown() == 17)
            })
            .await?;
        robot.act(cx, |workspace, window, cx| {
            query_tab::set_results_share(62., cx);
            let results = workspace.read(cx).query_tab()?.read(cx).results();
            results.update(cx, |results, cx| results.toggle_chart(window, cx));
            let chart = results.read(cx).chart()?;
            chart.update(cx, |chart, cx| chart.plot_bars(0, &[1, 2, 3], cx));
            Some(())
        })?;
        robot.capture(cx, &shot(14)).await?;
        robot.act(cx, |_, _, cx| query_tab::set_results_share(40., cx))?;
        robot.workspace(cx, |workspace, window, cx| workspace.close_active(window, cx))?;

        // 15: light theme with View > Theme > Dark theme open, over the users table.
        robot.activate(cx, "users").await?;
        robot.workspace(cx, |workspace, window, cx| workspace.set_json_open(true, window, cx))?;
        robot.panel_widths(cx).await?;
        robot.act(cx, |_, window, cx| theme::set_choice(theme::ThemeChoice::Light, window, cx))?;
        robot.draw(cx).await;
        // File is at (44, 17), past the bar's padding, logo and gap. The keys then open View > Theme > Dark theme.
        robot.click(cx, point(px(44.), px(17.)), Modifiers::none(), 1).await;
        robot.keys(cx, "right right down down right down down down right").await?;
        robot.capture(cx, &shot(15)).await?;
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use image::{Rgba, RgbaImage, imageops};

        use super::shrink;

        #[test]
        fn a_2x_render_comes_down_in_whole_pixel_blocks() {
            let render =
                RgbaImage::from_fn(64, 36, |x, y| Rgba([(x * 37 + y * 11) as u8, (x * y) as u8, (x ^ y) as u8, 255]));
            assert_eq!(shrink(&render, 32, 18), imageops::thumbnail(&render, 32, 18));
        }

        #[test]
        fn a_125_percent_render_weighs_pixels_by_area() {
            let stripes = RgbaImage::from_fn(5, 1, |x, _| {
                let shade = if x % 2 == 1 { 255 } else { 0 };
                Rgba([shade, shade, shade, 255])
            });
            let shades: Vec<u8> = shrink(&stripes, 4, 1).pixels().map(|pixel| pixel[0]).collect();
            assert_eq!(shades, [51, 153, 153, 51]);
        }
    }
}
