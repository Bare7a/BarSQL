// The demo video for `cargo xtask demo-video`: tours the app offscreen and saves the changed frames for ffmpeg.
use std::cell::{Cell, RefCell};
use std::fs::File;
use std::io::{BufWriter, Write as _};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use gpui_kit::base::input::{InputBaseState, InputModeKind};
use gpui_kit::component::input::{EditorState, InputState};
use gpui_kit::component::{ActiveTheme as _, WindowExt as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::test::{TestQueryExt as _, TestWindowExt as _};
use gpui_kit::*;
use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use image::{ExtendedColorType, ImageEncoder as _, RgbaImage, imageops};

use barsql_core::{ConnectionConfig, DriverType};

use super::script::{CHART_SQL, IMPORT_CSV, PLAN_SQL, Res, Robot, SIZE, WAIT};
use crate::assets::{LOGO, vector_img};
use crate::connection_dialog::{self, ConnectionForm};
use crate::i18n::t;
use crate::import_dialog::{self, ImportDialog};
use crate::plan_view::PlanView;
use crate::query_tab::{self, QueryTab};
use crate::schema_diff_view::{self, SchemaDiffView};
use crate::schema_objects::Group;
use crate::workspace::Workspace;
use crate::{state, theme};

const SIDEBAR_WIDTH: f32 = 360.;
const JSON_WIDTH: f32 = 440.;
const FRAME: Duration = Duration::from_micros(33_333);
const RING_FRAMES: usize = 8;
const POINTER_SIZE: f32 = 34.;
// The arrow's tip, from the SVG's 32-unit box.
const POINTER_TIP: (f32, f32) = (8. * POINTER_SIZE / 32., 4. * POINTER_SIZE / 32.);
// Above dialogs and menus.
const OVERLAY_PRIORITY: usize = 1 << 20;
// A dialog's caret: over GPUI Kit's dialogs, which draw at 10 and up, and under its popups at 100.
const DIALOG_MARKS_PRIORITY: usize = 99;
// Each half of the drawn caret's blink.
const BLINK: Duration = Duration::from_millis(530);
const FADE_OUT: Duration = Duration::from_millis(150);
const FADE_IN: Duration = Duration::from_millis(200);
const CROSSFADE_FRAMES: usize = 9;
// The query tabs' results split, set once so it never moves on camera.
const RESULTS_SHARE: f32 = 60.;
// Column indexes in `public.posts`, and its rows in a Structure tab.
const POST_CATEGORY: usize = 1;
const POST_TITLE: usize = 3;
const CONTENT_ROW: usize = 4;
const IS_LOCKED_ROW: usize = 8;
// From fixtures/demo-video/staging.sql.
const USERS_TRIGGER: &str = "users_normalize_email";
const POINTER: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32"><defs><filter id="s" x="-50%" y="-50%" width="200%" height="200%"><feDropShadow dx="0" dy="1" stdDeviation="1.2" flood-color="#000" flood-opacity="0.45"/></filter></defs><path d="M8 4 L8 26 L13.2 21.2 L16.8 29.4 L20.4 27.8 L16.9 19.8 L23.6 19.8 Z" fill="#16181d" stroke="#fff" stroke-width="2" stroke-linejoin="round" filter="url(#s)"/></svg>"##;

// What the overlay draws over the app.
#[derive(Default)]
struct Stage {
    // The last click and how far its ring has faded, 0 to 1.
    ring: Option<(Point<Pixels>, f32)>,
    caption: Option<Caption>,
    leaving: Option<Caption>,
    end_card: bool,
    // GPUI Kit paints carets and selections only in an active window, which the recording's never is.
    inputs: Vec<TextInput>,
    // Video time, and when the caret last moved, which restarts its blink.
    now: Duration,
    caret_moved: Option<Duration>,
}

impl Global for Stage {}

#[derive(Clone)]
struct Caption {
    text: SharedString,
    // Lifts the caption above this element, off the rows under it.
    above: Option<&'static str>,
    // When its fade starts: in while showing, out once leaving.
    fade_from: Duration,
}

// From launch, so the inputs the window opens with are watched too.
pub fn watch_inputs(cx: &mut App) {
    cx.observe_new(|_: &mut InputState, _, cx| add_input(TextInput::Line(cx.entity().downgrade()), cx)).detach();
    cx.observe_new(|_: &mut EditorState, _, cx| add_input(TextInput::Editor(cx.entity().downgrade()), cx)).detach();
}

fn add_input(input: TextInput, cx: &mut App) {
    let inputs = &mut cx.default_global::<Stage>().inputs;
    inputs.retain(TextInput::alive);
    inputs.push(input);
}

#[derive(Clone)]
enum TextInput {
    Line(WeakEntity<InputState>),
    Editor(WeakEntity<EditorState>),
}

impl TextInput {
    fn alive(&self) -> bool {
        match self {
            TextInput::Line(input) => input.upgrade().is_some(),
            TextInput::Editor(editor) => editor.upgrade().is_some(),
        }
    }

    fn marks(&self, window: &mut Window, cx: &mut App) -> Option<Marks> {
        match self {
            TextInput::Line(input) => marks(&input.upgrade()?, window, cx),
            TextInput::Editor(editor) => marks(&editor.upgrade()?, window, cx),
        }
    }
}

struct Marks {
    caret: Option<Bounds<Pixels>>,
    selection: Vec<Bounds<Pixels>>,
}

fn marks<M: InputModeKind>(input: &Entity<InputBaseState<M>>, window: &mut Window, cx: &mut App) -> Option<Marks> {
    let state = input.read(cx);
    if window.is_window_active() || !state.focus_handle(cx).is_focused(window) {
        return None;
    }
    // GPUI Kit's caret-anchored popups place themselves the same way.
    let caret = state.prepaint_caret_geometry().map(|geometry| {
        let bounds = geometry.cursor_bounds();
        Bounds::new(point(bounds.origin.x, bounds.origin.y + geometry.scroll_offset().y), bounds.size)
    });
    let (text, laid_out) = (state.value(), state.text_bounds());
    // The IME's range bounds stop at a line's end.
    let selection = input.update(cx, |state, cx| {
        let (selected, laid_out) = (state.selected_text_range(false, window, cx)?.range, laid_out?);
        let mut boxes = Vec::new();
        let mut start = 0;
        for line in text.split('\n') {
            let end = start + line.encode_utf16().count();
            let (from, to) = (selected.start.max(start), selected.end.min(end));
            if from < to {
                boxes.extend(state.bounds_for_range(from..to, laid_out, window, cx));
            }
            start = end + 1;
        }
        Some(boxes)
    });
    Some(Marks { caret, selection: selection.unwrap_or_default() })
}

pub fn overlay(window: &mut Window, cx: &mut App) -> Option<AnyElement> {
    let marks_priority = if window.has_active_dialog(cx) { DIALOG_MARKS_PRIORITY } else { 0 };
    let stage = cx.try_global::<Stage>()?;
    let blink_on = stage
        .caret_moved
        .is_none_or(|moved| (stage.now.saturating_sub(moved).as_millis() / BLINK.as_millis()).is_multiple_of(2));
    let inputs = stage.inputs.clone();
    let (caret, selection) = (cx.theme().caret, cx.theme().selection);
    // In prepaint, which deferred elements run after the inputs', so the caret is this frame's.
    let marks = canvas(
        move |_, window, cx| inputs.iter().find_map(|input| input.marks(window, cx)),
        move |_, marks, window, _| {
            let Some(marks) = marks else { return };
            for bounds in marks.selection {
                window.paint_quad(fill(bounds, selection));
            }
            if let Some(bounds) = marks.caret.filter(|_| blink_on) {
                window.paint_quad(fill(bounds, caret));
            }
        },
    )
    .absolute()
    .inset_0();
    let faded = |caption: &Caption, length: Duration| {
        (stage.now.saturating_sub(caption.fade_from).as_secs_f32() / length.as_secs_f32()).min(1.)
    };
    let leaving = stage.leaving.clone().filter(|leaving| faded(leaving, FADE_OUT) < 1.).map(|leaving| {
        let opacity = 1. - faded(&leaving, FADE_OUT);
        caption(leaving, opacity)
    });
    let showing = stage.caption.clone().map(|showing| {
        let opacity = faded(&showing, FADE_IN);
        caption(showing, opacity)
    });
    let layer = div()
        .absolute()
        .inset_0()
        .when(stage.end_card, |el| el.child(end_card(cx)))
        .children(leaving)
        .children(showing)
        .when_some(stage.ring, |el, (at, faded)| el.child(ring(at, faded, cx.theme().primary)));
    let overlay = div()
        .absolute()
        .inset_0()
        .child(deferred(marks).with_priority(marks_priority))
        .child(deferred(layer).with_priority(OVERLAY_PRIORITY));
    Some(overlay.into_any_element())
}

// Laid out in prepaint, once this frame has placed `above`. Moves beside a dialog it would cover.
fn caption(caption: Caption, opacity: f32) -> Canvas<AnyElement> {
    canvas(
        move |_, window, cx| {
            let viewport = window.viewport_size();
            // A tab switch leaves the old pane registered for a frame, in the same place.
            let above = caption.above.and_then(|id| window.find_all(id).first().map(|pane| pane.bounds()));
            let bottom = above.map_or(viewport.height - px(40.), |above| above.top() - px(22.));
            let (mut pill, mut measured) = caption_pill(&caption.text, opacity, None, window, cx);
            let mut origin = point((viewport.width - measured.width) / 2., bottom - measured.height);
            // Only dialogs register a surface.
            let dialog = window
                .find_all("surface")
                .into_iter()
                .map(|surface| surface.bounds())
                .find(|dialog| dialog.intersects(&Bounds::new(origin, measured)));
            if let Some(dialog) = dialog {
                let room = viewport.width - dialog.right();
                if measured.width > room - px(80.) {
                    let text = SharedString::from(balanced(&caption.text));
                    (pill, measured) = caption_pill(&text, opacity, Some(room - px(80.)), window, cx);
                }
                origin = point(dialog.right() + (room - measured.width) / 2., bottom - measured.height);
            }
            pill.prepaint_at(origin, window, cx);
            pill
        },
        |_, mut pill, window, cx| pill.paint(window, cx),
    )
    .absolute()
    .inset_0()
}

fn caption_pill(
    text: &SharedString,
    opacity: f32,
    max_width: Option<Pixels>,
    window: &mut Window,
    cx: &mut App,
) -> (AnyElement, Size<Pixels>) {
    let mut pill = div()
        .when_some(max_width, |el, width| el.max_w(width))
        .px(px(32.))
        .py(px(14.))
        .rounded(px(18.))
        .bg(black().opacity(0.82))
        .border_1()
        .border_color(white().opacity(0.12))
        .text_size(px(38.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(white())
        .text_center()
        .opacity(opacity)
        .child(text.clone())
        .into_any_element();
    let measured = pill.layout_as_root(size(AvailableSpace::MaxContent, AvailableSpace::MaxContent), window, cx);
    (pill, measured)
}

// Two lines of near-equal length, like CSS's `text-wrap: balance`.
fn balanced(text: &str) -> String {
    let words: Vec<&str> = text.split(' ').collect();
    let longer = |at: usize| words[..at].join(" ").len().max(words[at..].join(" ").len());
    match (1..words.len()).min_by_key(|&at| longer(at)) {
        Some(at) => format!("{}\n{}", words[..at].join(" "), words[at..].join(" ")),
        None => text.to_string(),
    }
}

// At the window's scale, in the straight alpha `imageops::overlay` expects.
fn arrow(window: &Window, cx: &App) -> Res<RgbaImage> {
    let side = DevicePixels((POINTER_SIZE * window.scale_factor()).round() as i32);
    let renderer = cx.svg_renderer();
    let raster = renderer
        .parse_svg(POINTER)
        .and_then(|svg| renderer.render_parsed(&svg, SvgSize::ExactSize(size(side, side))))
        .map_err(|error| error.to_string())?;
    let bgra = raster.as_bytes(0).ok_or("the pointer did not render")?.to_vec();
    let mut arrow = RgbaImage::from_raw(side.0 as u32, side.0 as u32, bgra).ok_or("the pointer did not render")?;
    for pixel in arrow.pixels_mut() {
        pixel.0.swap(0, 2);
    }
    Ok(arrow)
}

fn ring(at: Point<Pixels>, faded: f32, color: Hsla) -> Div {
    let radius = px(8. + 18. * faded);
    let alpha = 0.6 * (1. - faded);
    div()
        .absolute()
        .left(at.x - radius)
        .top(at.y - radius)
        .size(radius * 2.)
        .rounded_full()
        .border_2()
        .border_color(color.opacity(alpha))
        .bg(color.opacity(alpha * 0.35))
}

fn end_card(cx: &App) -> Div {
    let theme = cx.theme();
    div()
        .absolute()
        .inset_0()
        .bg(theme.background)
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(20.))
        .child(vector_img(LOGO, px(164.)))
        .child(div().mt(px(10.)).text_size(px(82.)).font_weight(FontWeight::BOLD).child("BarSQL"))
        .child(div().text_size(px(40.)).child("A fast, local-first SQL workbench"))
        .child(
            div()
                .text_size(px(32.))
                .text_color(theme.muted_foreground)
                .child("Free and open source for Windows, macOS and Linux"),
        )
        .child(
            div()
                .mt(px(44.))
                .flex()
                .gap(px(58.))
                .text_size(px(36.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.primary)
                .child("barsql.bare7a.eu")
                .child("github.com/Bare7a/BarSQL"),
        )
}

pub fn run(window: AnyWindowHandle, out: PathBuf, cx: &mut AsyncApp) {
    cx.spawn(async move |cx| {
        let robot = Robot { window };
        match record(&robot, &out, cx).await {
            Ok(length) => println!("{:.1} s of video saved to {}", length.as_secs_f64(), out.display()),
            Err(error) => {
                eprintln!("demo video failed: {error}");
                std::process::exit(1);
            }
        }
        cx.update(|cx| cx.quit());
    })
    .detach();
}

async fn record(robot: &Robot, out: &Path, cx: &mut AsyncApp) -> Res<Duration> {
    robot.fit_window(cx).await?;
    robot.workspace(cx, |workspace, window, cx| {
        workspace.set_panel_widths(px(SIDEBAR_WIDTH), px(JSON_WIDTH), window, cx)
    })?;
    open_forum(robot, cx).await?;
    robot.act(cx, |_, _, cx| query_tab::set_results_share(RESULTS_SHARE, cx))?;
    let arrow = robot.act(cx, |_, window, cx| arrow(window, cx))??;
    let camera = Camera::new(robot, out, arrow)?;
    camera.place(cx, point(px(SIZE.0 * 0.62), px(SIZE.1 * 0.7)));
    // The tour copies and pastes through the system clipboard, so it's restored afterwards.
    let clipboard = cx.update(|cx| cx.read_from_clipboard());
    let toured = tour(&camera, cx).await;
    if let Some(item) = clipboard {
        cx.update(|cx| cx.write_to_clipboard(item));
    }
    toured?;
    camera.recorder.into_inner().finish()
}

// Off camera, so the video opens on the forum's editor.
async fn open_forum(robot: &Robot, cx: &mut AsyncApp) -> Res<()> {
    let forum = ConnectionConfig {
        name: "Forum".into(),
        driver: DriverType::Postgres,
        color: connection_dialog::DEFAULT_COLOR.into(),
        host: "127.0.0.1".into(),
        port: 55432,
        username: "postgres".into(),
        password: "postgres".into(),
        database: "forum".into(),
        schema: "public".into(),
        ssl_mode: "disable".into(),
        ..Default::default()
    };
    let forum = save(cx, forum, None).await?;
    robot
        .workspace(cx, |workspace, window, cx| {
            workspace.sidebar().update(cx, |sidebar, cx| sidebar.reload_connections(cx));
            workspace.open_query(forum, "Query 1 · Forum", "", window, cx);
            let tab = workspace.query_tab()?;
            tab.update(cx, |tab, cx| tab.focus_editor(window, cx));
            Some(())
        })?
        .ok_or("no query tab")?;
    robot.settle(cx, "the forum's schemas", |workspace, _, cx| listed(workspace, cx, "s:public")).await
}

async fn save(cx: &mut AsyncApp, config: ConnectionConfig, seed: Option<&'static str>) -> Res<ConnectionConfig> {
    let bar = cx.update(|cx| state::bar(cx));
    let name = config.name.clone();
    let task = cx.update(|cx| {
        state::spawn(cx, async move {
            let saved = bar.save_connection(config).await?;
            if let Some(seed) = seed {
                bar.execute_statement(&saved.id, "demo-video-seed", seed).await?;
            }
            Ok::<_, barsql_core::QueryError>(saved)
        })
    });
    match task.await {
        Some(Ok(saved)) => Ok(saved),
        Some(Err(error)) => Err(format!("{name}: {}", error.message)),
        None => Err(format!("{name}: the setup stopped")),
    }
}

// The landing page's sections in its order, but with the chart before editing rows to keep the query tab together.
async fn tour(cam: &Camera<'_>, cx: &mut AsyncApp) -> Res<()> {
    sql(cam, cx).await?;
    plan(cam, cx).await?;
    chart(cam, cx).await?;
    search(cam, cx).await?;
    edit(cam, cx).await?;
    schema(cam, cx).await?;
    structure(cam, cx).await?;
    data(cam, cx).await?;
    engines(cam, cx).await?;
    end(cam, cx).await
}

async fn sql(cam: &Camera<'_>, cx: &mut AsyncApp) -> Res<()> {
    cam.caption(cx, Some("Write SQL at full speed"));
    cam.hold(cx, 1000).await?;
    cam.type_text(cx, "SELECT * FROM posts p\nWHERE p.").await?;
    cam.until(cx, "the column suggestions", suggesting).await?;
    cam.hold(cx, 1400).await?;
    cam.type_text(cx, "sco").await?;
    cam.hold(cx, 500).await?;
    cam.keys(cx, "enter").await?;
    cam.type_text(cx, " > 900\nORDER BY p.score DESC;").await?;
    cam.close_suggestions(cx).await?;
    cam.caption(cx, Some("Hover a table for its columns"));
    let posts = cam.word(cx, "posts")?;
    cam.glide(cx, posts).await?;
    cam.until(cx, "the hover card", hovering).await?;
    cam.hold(cx, 1800).await?;
    cam.click_on(cx, "run").await?;
    cam.until(cx, "the posts", |workspace, _, cx| {
        workspace.read(cx).active_grid(cx).is_some_and(|grid| grid.read(cx).rows_shown() > 0)
    })
    .await?;
    cam.caption(cx, Some("Click a row to see it as JSON"));
    cam.hold(cx, 700).await?;
    let title = cam.robot.cell(cx, 2, POST_TITLE)?;
    cam.click(cx, title).await?;
    cam.hold(cx, 1800).await
}

async fn plan(cam: &Camera<'_>, cx: &mut AsyncApp) -> Res<()> {
    cam.caption_over_results(cx, "See why a query is slow");
    // The plan wants the JSON viewer's width.
    cam.click_on(cx, "json-panel-close").await?;
    cam.hold(cx, 300).await?;
    let sql = cam.word(cx, "DESC")?;
    cam.click(cx, sql).await?;
    cam.keys(cx, "mod-a").await?;
    cam.paste(cx, PLAN_SQL)?;
    cam.hold(cx, 700).await?;
    cam.click_on(cx, "explain-menu").await?;
    cam.hold(cx, 500).await?;
    let analyze = label(cx, "editor.explainAnalyze");
    cam.pick(cx, &analyze).await?;
    cam.until(cx, "the plan", |workspace, _, cx| active_plan(workspace, cx).is_some()).await?;
    cam.hold(cx, 700).await?;
    let hottest = cam.robot.act(cx, |workspace, _, cx| active_plan(workspace, cx)?.read(cx).hottest())?;
    let hottest = hottest.ok_or("the plan has no hottest step")?;
    cam.click_on(cx, SharedString::from(format!("plan-node-{hottest}"))).await?;
    cam.hold(cx, 1800).await
}

async fn chart(cam: &Camera<'_>, cx: &mut AsyncApp) -> Res<()> {
    cam.caption_over_results(cx, "Chart any result in one click");
    let sql = cam.word(cx, "LIMIT")?;
    cam.click(cx, sql).await?;
    cam.keys(cx, "mod-a").await?;
    cam.paste(cx, CHART_SQL)?;
    cam.hold(cx, 900).await?;
    cam.click_on(cx, "run").await?;
    cam.until(cx, "the monthly posts", |workspace, _, cx| {
        workspace.read(cx).active_grid(cx).is_some_and(|grid| grid.read(cx).rows_shown() == 17)
    })
    .await?;
    cam.hold(cx, 700).await?;
    // Dated charts open as lines, so the bars go on before the click's first frame.
    let toggle = cam.robot.center(cx, "chart-toggle")?;
    cam.push(cx, toggle, MouseButton::Left, 1, Modifiers::none()).await?;
    let plotted = cam.robot.act(cx, |workspace, _, cx| {
        let results = workspace.read(cx).query_tab()?.read(cx).results();
        let chart = results.read(cx).chart()?;
        chart.update(cx, |chart, cx| chart.plot_bars(0, &[1, 2, 3], cx));
        Some(())
    })?;
    plotted.ok_or("the chart toggle opened no chart")?;
    cam.ring(cx, toggle).await?;
    cam.hold(cx, 2000).await
}

async fn search(cam: &Camera<'_>, cx: &mut AsyncApp) -> Res<()> {
    cam.caption(cx, Some("Jump to any table with Quick Search"));
    cam.hold(cx, 400).await?;
    cam.keys(cx, "mod-p").await?;
    cam.until(cx, "Quick Search", |_, window, cx| window.has_active_dialog(cx)).await?;
    cam.type_text(cx, "posts").await?;
    cam.hold(cx, 900).await?;
    cam.keys(cx, "enter").await?;
    cam.until(cx, "the posts table", |workspace, _, cx| {
        let workspace = workspace.read(cx);
        workspace.active_title(cx) == "posts" && workspace.table_tab().is_some()
    })
    .await?;
    cam.hold(cx, 700).await
}

// On posts, as deleting a user would trip the keys pointing at it.
async fn edit(cam: &Camera<'_>, cx: &mut AsyncApp) -> Res<()> {
    cam.caption(cx, Some("Filter with a WHERE that autocompletes"));
    cam.click_field(cx, "table-filter").await?;
    cam.type_text(cx, "sco").await?;
    cam.until(cx, "the filter suggestions", filtering).await?;
    cam.hold(cx, 600).await?;
    cam.keys(cx, "enter").await?;
    cam.type_text(cx, " > 900").await?;
    if cam.robot.act(cx, filtering)? {
        cam.keys(cx, "escape").await?;
    }
    cam.keys(cx, "enter").await?;
    cam.until(cx, "the filtered posts", |workspace, _, cx| {
        workspace
            .read(cx)
            .table_tab()
            .is_some_and(|tab| tab.read(cx).idle() && !tab.read(cx).applied_filter().is_empty())
    })
    .await?;
    cam.caption(cx, Some("Edit rows without writing UPDATE"));
    cam.hold(cx, 400).await?;
    let title = cam.robot.cell(cx, 0, POST_TITLE)?;
    cam.press(cx, title, MouseButton::Left, 2).await?;
    cam.hold(cx, 300).await?;
    cam.keys(cx, "mod-a").await?;
    cam.type_text(cx, "Go microservices in 2026").await?;
    cam.keys(cx, "enter").await?;
    cam.caption(cx, Some("Copy, paste and select like a spreadsheet"));
    let copied = cam.robot.cell(cx, 1, POST_TITLE)?;
    cam.click(cx, copied).await?;
    cam.keys(cx, "mod-c").await?;
    let pasted = cam.robot.cell(cx, 2, POST_TITLE)?;
    cam.click(cx, pasted).await?;
    cam.keys(cx, "mod-v").await?;
    cam.hold(cx, 400).await?;
    let (corner, far) = (cam.robot.cell(cx, 3, POST_CATEGORY)?, cam.robot.cell(cx, 6, POST_TITLE)?);
    cam.click(cx, corner).await?;
    cam.press_with(cx, far, Modifiers::shift()).await?;
    cam.hold(cx, 500).await?;
    let column = cam.robot.header(cx, POST_TITLE)?;
    cam.click(cx, column).await?;
    cam.hold(cx, 500).await?;
    cam.caption(cx, Some("Delete rows and apply every change at once"));
    let row = cam.robot.gutter(cx, 5)?;
    cam.click(cx, row).await?;
    cam.hold(cx, 300).await?;
    cam.keys(cx, "delete").await?;
    cam.until(cx, "the staged delete", |workspace, _, cx| {
        workspace.read(cx).table_tab().is_some_and(|tab| tab.read(cx).pending().1 == 1)
    })
    .await?;
    cam.hold(cx, 500).await?;
    cam.click_on(cx, "table-apply").await?;
    cam.until(cx, "the saved changes", |workspace, _, cx| {
        workspace.read(cx).table_tab().is_some_and(|tab| tab.read(cx).idle() && tab.read(cx).pending() == (0, 0))
    })
    .await?;
    cam.hold(cx, 1000).await
}

async fn schema(cam: &Camera<'_>, cx: &mut AsyncApp) -> Res<()> {
    cam.caption(cx, Some("Browse your schema down to the triggers"));
    let public = cam.robot.tree_row(cx, "s:public")?;
    cam.click(cx, public).await?;
    cam.until(cx, "the tables", |workspace, _, cx| listed(workspace, cx, "t:public.users")).await?;
    cam.hold(cx, 300).await?;
    let users = cam.robot.tree_row(cx, "t:public.users")?;
    cam.click(cx, users).await?;
    let group = |group: Group| format!("g:public:users:{}", group.key());
    let triggers = group(Group::Triggers);
    cam.until(cx, "the users table's groups", |workspace, _, cx| listed(workspace, cx, &triggers)).await?;
    cam.hold(cx, 500).await?;
    for (kind, object) in [(Group::Indexes, "users_pkey"), (Group::Triggers, USERS_TRIGGER)] {
        let row = cam.robot.tree_row(cx, &group(kind))?;
        cam.click(cx, row).await?;
        let object = format!("o:public:users:{}:{object}", kind.key());
        cam.until(cx, &object, |workspace, _, cx| listed(workspace, cx, &object)).await?;
        cam.hold(cx, 600).await?;
    }
    cam.hold(cx, 500).await?;
    let (public, diagram) = (cam.robot.tree_row(cx, "s:public")?, label(cx, "erDiagram.menu"));
    cam.context_pick(cx, public, &diagram).await?;
    cam.until(cx, "the diagram", |workspace, _, cx| workspace.read(cx).diagram_tab().is_some()).await?;
    cam.caption(cx, Some("See any schema as an ER diagram"));
    cam.hold(cx, 700).await?;
    let posts =
        cam.robot.act(cx, |workspace, _, cx| workspace.read(cx).diagram_tab()?.read(cx).table_index("posts"))?;
    cam.click_on(cx, ("er-table", posts.ok_or("no posts table in the diagram")?)).await?;
    cam.hold(cx, 1500).await?;
    let (public, compare) = (cam.robot.tree_row(cx, "s:public")?, label(cx, "schemaDiff.menu"));
    cam.context_pick(cx, public, &compare).await?;
    cam.until(cx, "the schema compare", |_, _, cx| diff_view(cx).is_some()).await?;
    cam.caption(cx, Some("Compare two schemas and get a sync script"));
    cam.hold(cx, 500).await?;
    cam.click_on(cx, "diff-target-schema").await?;
    cam.hold(cx, 500).await?;
    cam.pick(cx, "staging").await?;
    cam.until(cx, "staging to be picked", |_, _, cx| {
        diff_view(cx).is_some_and(|view| view.read(cx).target_schema() == Some("staging"))
    })
    .await?;
    cam.hold(cx, 300).await?;
    cam.click_on(cx, "diff-compare").await?;
    cam.until(cx, "the comparison", |_, _, cx| diff_view(cx).is_some_and(|view| view.read(cx).ready())).await?;
    cam.hold(cx, 1300).await?;
    // A shorter results pane shows the whole script; the dialog hides the change.
    cam.robot.act(cx, |_, _, cx| query_tab::set_results_share(40., cx))?;
    cam.click_on(cx, "diff-open-script").await?;
    cam.until(cx, "the script tab", |_, window, cx| !window.has_active_dialog(cx)).await?;
    cam.hold(cx, 1600).await
}

async fn structure(cam: &Camera<'_>, cx: &mut AsyncApp) -> Res<()> {
    let (posts, edit) = (cam.robot.tree_row(cx, "t:public.posts")?, label(cx, "sidebar.editStructure"));
    cam.context_pick(cx, posts, &edit).await?;
    cam.until(cx, "the posts structure", |workspace, _, cx| workspace.read(cx).structure_tab().is_some()).await?;
    cam.caption(cx, Some("Change a table without writing ALTER"));
    cam.hold(cx, 600).await?;
    let content = cam.robot.structure_cell(cx, CONTENT_ROW, 0)?;
    cam.press(cx, content, MouseButton::Left, 2).await?;
    cam.hold(cx, 200).await?;
    cam.keys(cx, "mod-a").await?;
    cam.type_text(cx, "body").await?;
    cam.keys(cx, "enter").await?;
    cam.hold(cx, 500).await?;
    let locked = cam.robot.structure_cell(cx, IS_LOCKED_ROW, 0)?;
    cam.click(cx, locked).await?;
    cam.click_on(cx, "structure-drop").await?;
    cam.hold(cx, 500).await?;
    cam.click_on(cx, "structure-add").await?;
    cam.hold(cx, 300).await?;
    cam.type_text(cx, "slug").await?;
    cam.keys(cx, "tab").await?;
    cam.type_text(cx, "text").await?;
    cam.keys(cx, "enter").await?;
    cam.until(cx, "the changes", |workspace, _, cx| {
        workspace.read(cx).structure_tab().is_some_and(|tab| tab.read(cx).has_changes())
    })
    .await?;
    cam.hold(cx, 2000).await
}

async fn data(cam: &Camera<'_>, cx: &mut AsyncApp) -> Res<()> {
    cam.caption(cx, None);
    let (users, import) = (cam.robot.tree_row(cx, "t:public.users")?, label(cx, "sidebar.importIntoTable"));
    cam.context_pick(cx, users, &import).await?;
    cam.until(cx, "the import dialog", |_, _, cx| importer(cx).is_some()).await?;
    cam.hold(cx, 500).await?;
    // Browse opens a system dialog the video can't film, so the file is handed over directly.
    let browse = cam.robot.center(cx, "import-browse")?;
    cam.mimic(cx, browse).await?;
    cam.robot.act(cx, |_, window, cx| {
        let dialog = importer(cx)?;
        dialog.update(cx, |dialog, cx| dialog.choose(IMPORT_CSV.into(), window, cx));
        Some(())
    })?;
    cam.robot
        .settle(cx, "the CSV preview", |_, _, cx| importer(cx).is_some_and(|dialog| dialog.read(cx).preview_loaded()))
        .await?;
    // After the preview grows the dialog, so the caption starts beside it.
    cam.caption(cx, Some("Import a CSV into any table"));
    cam.hold(cx, 2200).await?;
    cam.caption(cx, None);
    cam.click_on(cx, "import-dismiss").await?;
    cam.until(cx, "the dialog to close", |_, window, cx| !window.has_active_dialog(cx)).await?;
    // The posts table's tab.
    cam.click_on(cx, ("editor-tab-title", 1usize)).await?;
    cam.until(cx, "the posts table", |workspace, _, cx| workspace.read(cx).table_tab().is_some()).await?;
    cam.hold(cx, 400).await?;
    cam.click_on(cx, "export-results").await?;
    cam.until(cx, "the export dialog", |_, window, cx| window.has_active_dialog(cx)).await?;
    cam.caption(cx, Some("Export to CSV, JSON, Markdown or SQL"));
    cam.hold(cx, 2400).await?;
    cam.caption(cx, None);
    cam.click_on(cx, "export-dismiss").await?;
    cam.until(cx, "the dialog to close", |_, window, cx| !window.has_active_dialog(cx)).await
}

// Adds the MySQL connection on camera, with the others saved off camera, then a query tab for each engine.
async fn engines(cam: &Camera<'_>, cx: &mut AsyncApp) -> Res<()> {
    let (shop, others): (Vec<Sample>, Vec<Sample>) =
        samples().into_iter().partition(|sample| sample.config.driver == DriverType::MySql);
    let shop = shop.into_iter().next().ok_or("no MySQL sample")?;
    let mut saved = Vec::new();
    for sample in &others {
        saved.push(save(cx, sample.config.clone(), Some(sample.seed)).await?);
    }
    cam.robot.act(cx, |workspace, _, cx| {
        workspace.read(cx).sidebar().update(cx, |sidebar, cx| sidebar.reload_connections(cx))
    })?;
    add_shop(cam, cx).await?;
    // Off camera, the tour's tabs make way for one per engine.
    cam.robot.workspace(cx, |workspace, window, cx| {
        loop {
            if let Some(tab) = workspace.structure_tab() {
                tab.update(cx, |tab, cx| tab.revert(window, cx));
            }
            let open = workspace.query_tab().is_some()
                || workspace.table_tab().is_some()
                || workspace.diagram_tab().is_some()
                || workspace.structure_tab().is_some();
            if !open {
                break;
            }
            workspace.close_active(window, cx);
        }
        query_tab::set_results_share(RESULTS_SHARE, cx);
    })?;
    let found = cam.robot.act(cx, |workspace, _, cx| {
        let sidebar = workspace.read(cx).sidebar();
        sidebar.update(cx, |sidebar, cx| sidebar.close_switcher(cx));
        let connections = sidebar.read(cx).connections();
        let named = |name: &str| connections.iter().find(|c| c.name == name).cloned();
        Some((named("Forum")?, named("Shop")?))
    })?;
    let (forum, shop_connection) = found.ok_or("no Forum or Shop connection")?;
    // The orders table, which the dialog's connection can't make.
    let bar = cx.update(|cx| state::bar(cx));
    let id = shop_connection.id.clone();
    let seeded =
        cx.update(|cx| state::spawn(cx, async move { bar.execute_statement(&id, "demo-video-seed", shop.seed).await }));
    match seeded.await {
        Some(Ok(_)) => {}
        Some(Err(error)) => return Err(format!("Shop: {}", error.message)),
        None => return Err("Shop: the setup stopped".into()),
    }
    let top_posts = "SELECT title, score, views FROM posts ORDER BY score DESC LIMIT 50;";
    let mut tabs = vec![(forum, "Top posts · PostgreSQL", top_posts)];
    let mut others = others.iter().zip(saved);
    let (sqlite, sqlite_saved) = others.next().ok_or("no SQLite sample")?;
    tabs.push((sqlite_saved, sqlite.title, sqlite.sql));
    tabs.push((shop_connection, shop.title, shop.sql));
    tabs.extend(others.map(|(sample, saved)| (saved, sample.title, sample.sql)));
    let first = tabs[0].1;
    let mut opened: Vec<Entity<QueryTab>> = Vec::new();
    for (connection, title, sql) in tabs {
        let tab = cam.robot.workspace(cx, |workspace, window, cx| {
            workspace.open_query(connection, title, sql, window, cx);
            let tab = workspace.query_tab()?;
            tab.update(cx, |tab, cx| tab.run_all(window, cx));
            Some(tab)
        })?;
        opened.push(tab.ok_or_else(|| format!("no {title} tab"))?);
    }
    let count = opened.len();
    cam.robot
        .settle(cx, "every engine's rows", move |_, _, cx| {
            opened.iter().all(|tab| {
                let tab = tab.read(cx);
                let grid = tab.results().read(cx).active_grid();
                !tab.is_running() && grid.is_some_and(|grid| grid.read(cx).rows_shown() > 0)
            })
        })
        .await?;
    cam.robot.activate(cx, first).await?;
    cam.crossfade(cx).await?;
    cam.caption_over_results(cx, "Six engines. One app.");
    cam.hold(cx, 1000).await?;
    for ix in 1..count {
        cam.click_on(cx, ("editor-tab-title", ix)).await?;
        cam.hold(cx, 800).await?;
    }
    Ok(())
}

async fn add_shop(cam: &Camera<'_>, cx: &mut AsyncApp) -> Res<()> {
    cam.click_on(cx, "connection-switcher").await?;
    cam.hold(cx, 700).await?;
    cam.click_on(cx, "new-connection").await?;
    cam.until(cx, "the connection dialog", |_, window, cx| window.has_active_dialog(cx)).await?;
    cam.hold(cx, 300).await?;
    cam.type_text(cx, "Shop").await?;
    cam.click_on(cx, "conn-driver").await?;
    cam.hold(cx, 600).await?;
    let mysql = label(cx, "connection.mysql");
    cam.pick(cx, &mysql).await?;
    // After the pick, as the taller server form moves the caption beside the dialog.
    cam.caption(cx, Some("Add a connection in seconds"));
    cam.hold(cx, 600).await?;
    // Typed, as `localhost` would send MySQL's client to a socket.
    cam.click_field(cx, "conn-host").await?;
    cam.keys(cx, "mod-a").await?;
    cam.type_text(cx, "127.0.0.1").await?;
    cam.click_field(cx, "conn-port").await?;
    cam.keys(cx, "mod-a").await?;
    cam.type_text(cx, "33306").await?;
    // The form starts with MySQL's root user.
    cam.click_field(cx, "conn-password").await?;
    cam.type_text(cx, "root").await?;
    cam.caption(cx, Some("Pick the database from the server"));
    cam.click_on(cx, "conn-database-list").await?;
    cam.until(cx, "the database list", |_, _, cx| form(cx).is_some_and(|form| form.read(cx).databases().is_some()))
        .await?;
    cam.hold(cx, 500).await?;
    cam.pick(cx, "barsql_test").await?;
    cam.until(cx, "barsql_test to be picked", |_, _, cx| {
        form(cx).is_some_and(|form| form.read(cx).database(cx) == "barsql_test")
    })
    .await?;
    cam.hold(cx, 600).await?;
    cam.wheel_to_end(cx, "conn-mcp").await?;
    cam.caption(cx, Some("Let AI assistants query this database"));
    cam.hold(cx, 300).await?;
    cam.click_on(cx, "conn-mcp").await?;
    cam.hold(cx, 400).await?;
    // Down to what the tick adds.
    cam.wheel_to_end(cx, "conn-mcp").await?;
    cam.hold(cx, 1800).await?;
    cam.caption(cx, None);
    cam.click_on(cx, "connection-save").await?;
    cam.until(cx, "the dialog to close", |_, window, cx| !window.has_active_dialog(cx)).await?;
    cam.hold(cx, 600).await
}

struct Sample {
    config: ConnectionConfig,
    seed: &'static str,
    title: &'static str,
    sql: &'static str,
}

// The stack's other engines, as docker-compose.yml runs them, each with a table to show.
fn samples() -> Vec<Sample> {
    let server = |name: &str, driver: DriverType, port: i64, user: &str, password: &str, database: &str| {
        // SQL Server encrypts with the certificate it made itself.
        let mssql = driver == DriverType::SqlServer;
        ConnectionConfig {
            name: name.into(),
            driver,
            color: "#3b82f6".into(),
            host: "127.0.0.1".into(),
            port,
            username: user.into(),
            password: password.into(),
            database: database.into(),
            schema: if mssql { "dbo".into() } else { database.into() },
            ssl_mode: if mssql { "require".into() } else { "disable".into() },
            ..Default::default()
        }
    };
    let sqlite = ConnectionConfig {
        name: "Tasks".into(),
        driver: DriverType::Sqlite,
        color: "#0ea5e9".into(),
        file_path: barsql_core::paths::data_dir().join("tasks.db").to_string_lossy().into_owned(),
        ..Default::default()
    };
    vec![
        Sample {
            config: sqlite, seed: SQLITE_SEED, title: "Tasks · SQLite", sql: "SELECT * FROM tasks ORDER BY due;"
        },
        Sample {
            config: ConnectionConfig {
                color: "#f59e0b".into(),
                ..server("Shop", DriverType::MySql, 33306, "root", "root", "barsql_test")
            },
            seed: MYSQL_SEED,
            title: "Orders · MySQL",
            sql: "SELECT * FROM orders ORDER BY placed_at DESC;",
        },
        Sample {
            config: ConnectionConfig {
                color: "#ef4444".into(),
                ..server("Billing", DriverType::SqlServer, 31433, "sa", "BarSQL-e2e-Passw0rd", "master")
            },
            seed: MSSQL_SEED,
            title: "Invoices · SQL Server",
            sql: "SELECT * FROM dbo.invoices ORDER BY issued DESC;",
        },
        Sample {
            config: ConnectionConfig {
                color: "#eab308".into(),
                ..server("Analytics", DriverType::ClickHouse, 38123, "default", "clickhouse", "barsql_test")
            },
            seed: CLICKHOUSE_SEED,
            title: "Page views · ClickHouse",
            sql: "SELECT path, count() AS views, round(avg(ms)) AS avg_ms, quantile(0.95)(ms) AS p95_ms
FROM page_views
GROUP BY path
ORDER BY views DESC;",
        },
        Sample {
            config: ConnectionConfig {
                name: "Reading list".into(),
                driver: DriverType::Turso,
                color: "#14b8a6".into(),
                url: "http://127.0.0.1:38080".into(),
                ..Default::default()
            },
            seed: TURSO_SEED,
            title: "Bookmarks · Turso",
            sql: "SELECT * FROM bookmarks ORDER BY added DESC;",
        },
    ]
}

const SQLITE_SEED: &str = "DROP TABLE IF EXISTS tasks;
CREATE TABLE tasks (id INTEGER PRIMARY KEY, title TEXT NOT NULL, project TEXT NOT NULL, done INTEGER NOT NULL, due TEXT);
INSERT INTO tasks (title, project, done, due) VALUES
  ('Record the demo video', 'Launch', 1, '2026-11-06'),
  ('Answer the open issues', 'Community', 1, '2026-11-07'),
  ('Write the Show HN post', 'Launch', 0, '2026-11-10'),
  ('Post on r/rust', 'Launch', 0, '2026-11-11'),
  ('Add a French translation', 'Community', 0, '2026-11-20'),
  ('Sign the Windows build', 'Release', 0, '2026-12-01'),
  ('Publish v1.3.0', 'Release', 0, '2026-12-05');";

const MYSQL_SEED: &str = "DROP TABLE IF EXISTS orders;
CREATE TABLE orders (id INT AUTO_INCREMENT PRIMARY KEY, customer VARCHAR(60) NOT NULL, total DECIMAL(10,2) NOT NULL,
  status VARCHAR(20) NOT NULL, placed_at DATETIME NOT NULL);
INSERT INTO orders (customer, total, status, placed_at) VALUES
  ('Ana Lopez', 129.90, 'shipped', '2026-11-01 09:14:00'),
  ('Ben Carter', 54.00, 'delivered', '2026-10-30 17:42:00'),
  ('Chloe Martin', 310.25, 'processing', '2026-11-03 11:05:00'),
  ('Daniel Kim', 18.50, 'delivered', '2026-10-28 08:30:00'),
  ('Elena Petrova', 89.99, 'shipped', '2026-11-02 14:20:00'),
  ('Farid Haddad', 245.00, 'cancelled', '2026-10-31 19:55:00'),
  ('Grace Liu', 72.40, 'processing', '2026-11-04 10:12:00'),
  ('Hugo Silva', 159.00, 'delivered', '2026-10-27 13:47:00');";

const MSSQL_SEED: &str = "DROP TABLE IF EXISTS dbo.invoices;
CREATE TABLE dbo.invoices (id INT IDENTITY PRIMARY KEY, customer NVARCHAR(60) NOT NULL, amount DECIMAL(10,2) NOT NULL,
  issued DATE NOT NULL, due DATE NOT NULL, paid BIT NOT NULL);
INSERT INTO dbo.invoices (customer, amount, issued, due, paid) VALUES
  (N'Northwind Traders', 4200.00, '2026-10-01', '2026-10-31', 1),
  (N'Contoso Ltd', 1875.50, '2026-10-05', '2026-11-04', 1),
  (N'Fabrikam', 960.00, '2026-10-12', '2026-11-11', 0),
  (N'Adventure Works', 12500.00, '2026-10-15', '2026-11-14', 0),
  (N'Tailspin Toys', 340.75, '2026-10-20', '2026-11-19', 1),
  (N'Wide World Importers', 5120.00, '2026-10-28', '2026-11-27', 0);";

const CLICKHOUSE_SEED: &str = "DROP TABLE IF EXISTS page_views;
CREATE TABLE page_views (ts DateTime, path String, ms UInt32) ENGINE = MergeTree ORDER BY ts;
INSERT INTO page_views
SELECT toDateTime('2026-11-01 00:00:00') + number * 17,
       ['/', '/docs', '/download', '/guide', '/blog', '/privacy'][number % 6 + 1],
       20 + (number * 7919) % 480
FROM numbers(200000);";

const TURSO_SEED: &str = "DROP TABLE IF EXISTS bookmarks;
CREATE TABLE bookmarks (id INTEGER PRIMARY KEY, title TEXT NOT NULL, url TEXT NOT NULL, added TEXT NOT NULL);
INSERT INTO bookmarks (title, url, added) VALUES
  ('GPUI', 'https://www.gpui.rs', '2026-11-02'),
  ('GPUI Kit', 'https://github.com/longbridge/gpui-component', '2026-11-01'),
  ('libSQL', 'https://github.com/tursodatabase/libsql', '2026-10-29'),
  ('The Rust Book', 'https://doc.rust-lang.org/book/', '2026-10-25'),
  ('Use the Index, Luke', 'https://use-the-index-luke.com', '2026-10-20');";

async fn end(cam: &Camera<'_>, cx: &mut AsyncApp) -> Res<()> {
    cam.caption_over_results(cx, "Switch between dark and light themes");
    cam.hold(cx, 500).await?;
    let (view, theme, light) = (label(cx, "menu.view"), label(cx, "viewSections.theme"), label(cx, "theme.light"));
    cam.open_menu(cx, &view).await?;
    cam.hold(cx, 300).await?;
    // Across into the submenu before down, or the pointer would open Language's instead.
    let theme = cam.menu_item(cx, &theme)?;
    cam.glide(cx, theme).await?;
    cam.hold(cx, 400).await?;
    let light = cam.menu_item(cx, &light)?;
    cam.glide(cx, point(light.x, theme.y)).await?;
    cam.push(cx, light, MouseButton::Left, 1, Modifiers::none()).await?;
    cam.crossfade(cx).await?;
    cam.hold(cx, 1500).await?;
    cam.robot.act(cx, |_, window, cx| theme::set_choice(theme::ThemeChoice::Dark, window, cx))?;
    cam.stage(cx, |stage| {
        stage.caption = None;
        stage.leaving = None;
        stage.end_card = true;
    });
    cam.pointer.set(None);
    cam.crossfade(cx).await?;
    cam.hold(cx, 3500).await
}

fn form(cx: &App) -> Option<Entity<ConnectionForm>> {
    cx.try_global::<connection_dialog::Opened>().and_then(|opened| opened.0.upgrade())
}

fn diff_view(cx: &App) -> Option<Entity<SchemaDiffView>> {
    cx.try_global::<schema_diff_view::Opened>().and_then(|opened| opened.0.upgrade())
}

fn listed(workspace: &Entity<Workspace>, cx: &App, id: &str) -> bool {
    let tree = workspace.read(cx).sidebar().read(cx).panels().0;
    tree.read(cx).listed().iter().any(|(row, ..)| row == id)
}

// Sine in and out, peaking at 1.6× the average speed, so long glides don't jump.
fn ease(t: f32) -> f32 {
    0.5 - 0.5 * (std::f32::consts::PI * t).cos()
}

fn blend(from: &RgbaImage, to: &RgbaImage, amount: f32) -> RgbaImage {
    let weight = (amount * 256.).round() as u32;
    let mut out = to.clone();
    for (channel, &under) in out.iter_mut().zip(from.iter()) {
        *channel = ((u32::from(under) * (256 - weight) + u32::from(*channel) * weight) >> 8) as u8;
    }
    out
}

// Loading states the camera doesn't film.
fn loading(workspace: &Entity<Workspace>, _: &mut Window, cx: &mut App) -> bool {
    let workspace = workspace.read(cx);
    workspace.query_tab().is_some_and(|tab| tab.read(cx).is_running())
        || workspace.table_tab().is_some_and(|tab| !tab.read(cx).idle())
        || workspace.diagram_tab().is_some_and(|diagram| !diagram.read(cx).fitted())
        || workspace.structure_tab().is_some_and(|tab| !tab.read(cx).editable())
        || workspace.sidebar().read(cx).panels().0.read(cx).loading()
}

fn active_plan(workspace: &Entity<Workspace>, cx: &App) -> Option<Entity<PlanView>> {
    workspace.read(cx).query_tab()?.read(cx).results().read(cx).active_plan()
}

fn label(cx: &mut AsyncApp, key: &str) -> SharedString {
    cx.update(|cx| t(cx, key))
}

fn suggesting(workspace: &Entity<Workspace>, _: &mut Window, cx: &mut App) -> bool {
    workspace.read(cx).query_tab().is_some_and(|tab| tab.read(cx).completion().read(cx).is_open(cx))
}

fn filtering(workspace: &Entity<Workspace>, _: &mut Window, cx: &mut App) -> bool {
    workspace.read(cx).table_tab().is_some_and(|tab| tab.read(cx).completion().read(cx).is_open(cx))
}

fn hovering(workspace: &Entity<Workspace>, _: &mut Window, cx: &mut App) -> bool {
    workspace.read(cx).query_tab().is_some_and(|tab| tab.read(cx).hover().read(cx).showing())
}

fn importer(cx: &App) -> Option<Entity<ImportDialog>> {
    cx.try_global::<import_dialog::Opened>().and_then(|opened| opened.0.upgrade())
}

// The robot at a viewer's pace, recording each frame with how long it shows.
struct Camera<'a> {
    robot: &'a Robot,
    recorder: RefCell<Recorder>,
    // None once put away for the end card.
    pointer: Cell<Option<Point<Pixels>>>,
    arrow: RgbaImage,
    keystrokes: Cell<u32>,
}

impl<'a> Camera<'a> {
    fn new(robot: &'a Robot, out: &Path, arrow: RgbaImage) -> Res<Self> {
        Ok(Self {
            robot,
            recorder: RefCell::new(Recorder::new(out)?),
            pointer: Cell::new(None),
            arrow,
            keystrokes: Cell::new(0),
        })
    }

    fn stage(&self, cx: &mut AsyncApp, f: impl FnOnce(&mut Stage)) {
        cx.update(|cx| f(cx.default_global::<Stage>()));
    }

    fn caption(&self, cx: &mut AsyncApp, text: Option<&'static str>) {
        self.set_caption(cx, text, None);
    }

    // A caption over the empty end of the editor, clear of the results.
    fn caption_over_results(&self, cx: &mut AsyncApp, text: &'static str) {
        self.set_caption(cx, Some(text), Some("results-pane"));
    }

    // The caption showing fades out, then `text` fades in.
    fn set_caption(&self, cx: &mut AsyncApp, text: Option<&'static str>, above: Option<&'static str>) {
        let now = self.recorder.borrow().elapsed;
        self.stage(cx, |stage| {
            let showing = stage.caption.as_ref().map(|caption| (caption.text.as_ref(), caption.above));
            if showing == text.map(|text| (text, above)) {
                return;
            }
            let leaving = stage.caption.take().map(|caption| Caption { fade_from: now, ..caption });
            let fade_from = if leaving.is_some() { now + FADE_OUT } else { now };
            stage.leaving = leaving;
            stage.caption = text.map(|text| Caption { text: text.into(), above, fade_from });
        });
    }

    // Without a glide. GPUI's mouse moves too, so hovers follow.
    fn place(&self, cx: &mut AsyncApp, at: Point<Pixels>) {
        self.pointer.set(Some(at));
        let _ = self.robot.window.update(cx, |_, window, cx| {
            let moved = MouseMoveEvent { position: at, pressed_button: None, modifiers: Modifiers::none() };
            window.dispatch_event(PlatformInput::MouseMove(moved), cx);
        });
    }

    // Waits out the rest of `shown`, keeping the app in real time, and always a little so its tasks run.
    async fn shot(&self, cx: &mut AsyncApp, shown: Duration) -> Res<()> {
        self.loaded(cx).await?;
        let started = Instant::now();
        let image = self.render(cx)?;
        self.recorder.borrow_mut().add(image, shown)?;
        let rest = shown.saturating_sub(started.elapsed()).max(Duration::from_millis(1));
        cx.background_executor().timer(rest).await;
        Ok(())
    }

    fn render(&self, cx: &mut AsyncApp) -> Res<RgbaImage> {
        let now = self.recorder.borrow().elapsed;
        self.robot
            .window
            .update(cx, |_, window, cx| {
                cx.default_global::<Stage>().now = now;
                window.refresh();
                window.draw(cx).clear(cx);
                window.simulate_next_frame(cx);
                let mut image = window.render_to_image().map_err(|error| error.to_string())?;
                // Over GPUI's tooltips too, as a system pointer is.
                if let Some(at) = self.pointer.get() {
                    let corner = |at: Pixels, tip: f32| ((f32::from(at) - tip) * window.scale_factor()).round() as i64;
                    imageops::overlay(
                        &mut image,
                        &self.arrow,
                        corner(at.x, POINTER_TIP.0),
                        corner(at.y, POINTER_TIP.1),
                    );
                }
                Ok::<_, String>(image)
            })
            .map_err(|error| error.to_string())?
    }

    // Waits out loads unfilmed. Still draws, which some views wait on.
    async fn loaded(&self, cx: &mut AsyncApp) -> Res<()> {
        let deadline = Instant::now() + WAIT;
        while self.robot.act(cx, loading)? {
            if Instant::now() > deadline {
                return Err("timed out waiting for a load".into());
            }
            let _ = self.robot.window.update(cx, |_, window, cx| {
                window.refresh();
                window.draw(cx).clear(cx);
                window.simulate_next_frame(cx);
            });
            cx.background_executor().timer(Duration::from_millis(20)).await;
        }
        Ok(())
    }

    // From the last recorded frame to the window now, over a change made off camera.
    async fn crossfade(&self, cx: &mut AsyncApp) -> Res<()> {
        self.loaded(cx).await?;
        let from = self.recorder.borrow().last.clone().ok_or("no frame to fade from")?;
        let to = self.render(cx)?;
        for frame in 1..CROSSFADE_FRAMES {
            let mixed = blend(&from, &to, ease(frame as f32 / CROSSFADE_FRAMES as f32));
            self.recorder.borrow_mut().add(mixed, FRAME)?;
            cx.background_executor().timer(Duration::from_millis(1)).await;
        }
        self.recorder.borrow_mut().add(to, FRAME)
    }

    async fn hold(&self, cx: &mut AsyncApp, ms: u64) -> Res<()> {
        let frames = (Duration::from_millis(ms).as_secs_f64() / FRAME.as_secs_f64()).round().max(1.) as usize;
        for _ in 0..frames {
            self.shot(cx, FRAME).await?;
        }
        Ok(())
    }

    async fn until(
        &self,
        cx: &mut AsyncApp,
        what: &str,
        check: impl Fn(&Entity<Workspace>, &mut Window, &mut App) -> bool,
    ) -> Res<()> {
        let deadline = Instant::now() + WAIT;
        loop {
            self.shot(cx, FRAME).await?;
            if self.robot.act(cx, &check)? {
                return Ok(());
            }
            if Instant::now() > deadline {
                return Err(format!("timed out waiting for {what}"));
            }
        }
    }

    async fn glide(&self, cx: &mut AsyncApp, to: Point<Pixels>) -> Res<()> {
        let from = self.pointer.get().unwrap_or(to);
        let distance = f32::from(from.x - to.x).hypot(f32::from(from.y - to.y));
        if distance < 1. {
            return Ok(());
        }
        let ms = (280. + distance * 0.45).clamp(300., 700.);
        let steps = (ms / FRAME.as_secs_f32() / 1000.).round().max(1.) as usize;
        for step in 1..=steps {
            self.place(cx, from + (to - from) * ease(step as f32 / steps as f32));
            self.shot(cx, FRAME).await?;
        }
        Ok(())
    }

    async fn click(&self, cx: &mut AsyncApp, at: Point<Pixels>) -> Res<()> {
        self.press(cx, at, MouseButton::Left, 1).await
    }

    // Where `word` first appears in the query tab's SQL.
    fn word(&self, cx: &mut AsyncApp, word: &str) -> Res<Point<Pixels>> {
        let found = self.robot.act(cx, |workspace, window, cx| {
            let editor = workspace.read(cx).query_tab()?.read(cx).editor();
            editor.update(cx, |state, cx| {
                // The SQL is ASCII, so its byte offsets count UTF-16 units too.
                let start = state.value().find(word)?;
                let laid_out = state.text_bounds()?;
                state.bounds_for_range(start..start + word.len(), laid_out, window, cx)
            })
        })?;
        found.map(|bounds| bounds.center()).ok_or_else(|| format!("{word} is not in the editor"))
    }

    async fn click_on(&self, cx: &mut AsyncApp, id: impl Into<ElementId>) -> Res<()> {
        let at = self.robot.center(cx, id)?;
        self.click(cx, at).await
    }

    async fn click_field(&self, cx: &mut AsyncApp, scope: &'static str) -> Res<()> {
        let at = self.robot.field(cx, scope)?;
        self.click(cx, at).await
    }

    fn menu_item(&self, cx: &mut AsyncApp, label: &str) -> Res<Point<Pixels>> {
        let item = self.robot.act(cx, |_, window, _| {
            let items = window.find_all_by_role(Role::MenuItem);
            items.into_iter().find(|item| item.label() == Some(label)).map(|item| item.bounds().center())
        })?;
        item.ok_or_else(|| format!("no menu item {label}"))
    }

    async fn pick(&self, cx: &mut AsyncApp, label: &str) -> Res<()> {
        let item = self.menu_item(cx, label)?;
        self.click(cx, item).await
    }

    async fn open_menu(&self, cx: &mut AsyncApp, label: &str) -> Res<()> {
        let menu = self.robot.act(cx, |_, window, _| {
            let menus = window.find_all("menu");
            menus.into_iter().find(|menu| menu.label() == Some(label)).map(|menu| menu.bounds().center())
        })?;
        self.click(cx, menu.ok_or_else(|| format!("no {label} menu"))?).await
    }

    // A click shown but not sent, for buttons that open system windows.
    async fn mimic(&self, cx: &mut AsyncApp, at: Point<Pixels>) -> Res<()> {
        self.glide(cx, at).await?;
        self.hold(cx, 120).await?;
        self.ring(cx, at).await
    }

    // The distance is measured off camera by how far `inside` moves, so the ease runs its whole course.
    async fn wheel_to_end(&self, cx: &mut AsyncApp, inside: &'static str) -> Res<()> {
        let at = self.pointer.get().ok_or("no pointer to scroll under")?;
        let start = self.robot.bounds(cx, inside)?.top();
        self.scroll(cx, at, px(-100_000.))?;
        let distance = start - self.robot.bounds(cx, inside)?.top();
        self.scroll(cx, at, distance)?;
        let frames = 18;
        let mut turned = px(0.);
        for frame in 1..=frames {
            let to = distance * ease(frame as f32 / frames as f32);
            self.scroll(cx, at, turned - to)?;
            turned = to;
            self.shot(cx, FRAME).await?;
        }
        Ok(())
    }

    // One wheel event, laid out at once so the bounds read next are current.
    fn scroll(&self, cx: &mut AsyncApp, at: Point<Pixels>, dy: Pixels) -> Res<()> {
        let delta = ScrollDelta::Pixels(point(px(0.), dy));
        let scrolled = ScrollEvent { position: at, delta, ..Default::default() };
        self.robot
            .window
            .update(cx, |_, window, cx| {
                window.dispatch_event(PlatformInput::Scroll(scrolled), cx);
                window.refresh();
                window.draw(cx).clear(cx);
            })
            .map_err(|error| error.to_string())
    }

    async fn context_pick(&self, cx: &mut AsyncApp, at: Point<Pixels>, label: &str) -> Res<()> {
        self.press(cx, at, MouseButton::Right, 1).await?;
        self.hold(cx, 500).await?;
        self.pick(cx, label).await
    }

    // Replaces the active tab's SQL in one go, as a paste does.
    fn paste(&self, cx: &mut AsyncApp, sql: &str) -> Res<()> {
        self.caret_moved(cx);
        self.robot.act(cx, |workspace, window, cx| {
            let tab = workspace.read(cx).query_tab()?;
            tab.update(cx, |tab, cx| tab.set_sql(sql.into(), window, cx));
            Some(())
        })?;
        Ok(())
    }

    fn caret_moved(&self, cx: &mut AsyncApp) {
        let now = self.recorder.borrow().elapsed;
        self.stage(cx, |stage| stage.caret_moved = Some(now));
    }

    async fn press(&self, cx: &mut AsyncApp, at: Point<Pixels>, button: MouseButton, count: usize) -> Res<()> {
        self.push(cx, at, button, count, Modifiers::none()).await?;
        self.ring(cx, at).await
    }

    async fn press_with(&self, cx: &mut AsyncApp, at: Point<Pixels>, modifiers: Modifiers) -> Res<()> {
        self.push(cx, at, MouseButton::Left, 1, modifiers).await?;
        self.ring(cx, at).await
    }

    // Glides to `at` and clicks there, before any frame shows what the click did.
    async fn push(
        &self,
        cx: &mut AsyncApp,
        at: Point<Pixels>,
        button: MouseButton,
        count: usize,
        modifiers: Modifiers,
    ) -> Res<()> {
        self.glide(cx, at).await?;
        self.hold(cx, 120).await?;
        let _ = self.robot.window.update(cx, |_, window, cx| {
            for click_count in 1..=count {
                let down = MouseDownEvent { button, position: at, modifiers, click_count, first_mouse: false };
                window.dispatch_event(PlatformInput::MouseDown(down), cx);
                let up = MouseUpEvent { button, position: at, modifiers, click_count };
                window.dispatch_event(PlatformInput::MouseUp(up), cx);
            }
        });
        self.caret_moved(cx);
        Ok(())
    }

    async fn ring(&self, cx: &mut AsyncApp, at: Point<Pixels>) -> Res<()> {
        for frame in 0..RING_FRAMES {
            self.stage(cx, |stage| stage.ring = Some((at, frame as f32 / RING_FRAMES as f32)));
            self.shot(cx, FRAME).await?;
        }
        self.stage(cx, |stage| stage.ring = None);
        Ok(())
    }

    // Space-separated keystrokes, with `mod` as the platform's command key, a beat apart.
    async fn keys(&self, cx: &mut AsyncApp, keys: &str) -> Res<()> {
        let os = cx.update(|cx| cx.platform_info().os);
        for key in keys.split_whitespace() {
            let keystroke =
                Keystroke::parse_with_os(&key.replace("mod-", "secondary-"), os).map_err(|error| error.to_string())?;
            self.caret_moved(cx);
            let _ = self.robot.window.update(cx, |_, window, cx| window.dispatch_keystroke(keystroke, cx));
            self.hold(cx, 180).await?;
        }
        Ok(())
    }

    async fn close_suggestions(&self, cx: &mut AsyncApp) -> Res<()> {
        if self.robot.act(cx, suggesting)? {
            self.keys(cx, "escape").await?;
        }
        Ok(())
    }

    // A newline first closes the suggestions, which would take the Enter.
    async fn type_text(&self, cx: &mut AsyncApp, text: &str) -> Res<()> {
        for ch in text.chars() {
            if ch == '\n' {
                self.close_suggestions(cx).await?;
                self.keys(cx, "enter").await?;
                continue;
            }
            let key = if ch == ' ' { "space".to_string() } else { ch.to_lowercase().to_string() };
            let keystroke = Keystroke { modifiers: Modifiers::none(), key, key_char: Some(ch.to_string()) };
            self.caret_moved(cx);
            let _ = self.robot.window.update(cx, |_, window, cx| window.dispatch_keystroke(keystroke, cx));
            self.shot(cx, self.beat(ch)).await?;
        }
        Ok(())
    }

    // 55 to 105 ms a key, unevenly, and a little longer after a word.
    fn beat(&self, ch: char) -> Duration {
        let n = self.keystrokes.get().wrapping_mul(1_103_515_245).wrapping_add(12_345);
        self.keystrokes.set(n);
        let pause = if ch == ' ' { 40 } else { 0 };
        Duration::from_millis(55 + u64::from((n >> 16) % 50) + pause)
    }
}

// PNGs on worker threads; an unchanged frame lengthens the last one.
struct Recorder {
    dir: PathBuf,
    shown: Vec<Duration>,
    elapsed: Duration,
    last: Option<Arc<RgbaImage>>,
    jobs: Option<SyncSender<(PathBuf, Arc<RgbaImage>)>>,
    writers: Vec<JoinHandle<Res<()>>>,
}

impl Recorder {
    fn new(dir: &Path) -> Res<Self> {
        std::fs::create_dir_all(dir).map_err(|error| format!("{}: {error}", dir.display()))?;
        let (jobs, queue) = sync_channel(8);
        let queue = Arc::new(Mutex::new(queue));
        let writers = (0..4)
            .map(|_| {
                let queue = queue.clone();
                std::thread::spawn(move || write_frames(&queue))
            })
            .collect();
        let dir = dir.to_path_buf();
        Ok(Self { dir, shown: Vec::new(), elapsed: Duration::ZERO, last: None, jobs: Some(jobs), writers })
    }

    fn add(&mut self, image: RgbaImage, shown: Duration) -> Res<()> {
        self.elapsed += shown;
        if self.last.as_ref().is_some_and(|last| last.as_raw() == image.as_raw()) {
            if let Some(last) = self.shown.last_mut() {
                *last += shown;
            }
            return Ok(());
        }
        let image = Arc::new(image);
        let path = self.dir.join(frame_name(self.shown.len() + 1));
        let sent = self.jobs.as_ref().ok_or("the recorder is closed")?.send((path, image.clone()));
        if sent.is_err() {
            // Each writer stops at its first error, so the reason is in one of them.
            return Err(self.stop().err().unwrap_or_else(|| "every frame writer stopped".into()));
        }
        self.last = Some(image);
        self.shown.push(shown);
        Ok(())
    }

    fn stop(&mut self) -> Res<()> {
        drop(self.jobs.take());
        for writer in self.writers.drain(..) {
            writer.join().map_err(|_| "a frame writer panicked")??;
        }
        Ok(())
    }

    // ffmpeg reads PNGs at 25 fps unless told, and plays the last entry as long as the one before it.
    fn finish(mut self) -> Res<Duration> {
        self.stop()?;
        let file = |n: usize| format!("file {}\noption framerate 1000000\n", frame_name(n));
        let mut list = String::from("ffconcat version 1.0\n");
        let count = self.shown.len();
        for (ix, shown) in self.shown.iter().enumerate() {
            let shown = if ix + 1 == count { *shown / 2 } else { *shown };
            list.push_str(&file(ix + 1));
            list.push_str(&format!("duration {:.6}\n", shown.as_secs_f64()));
        }
        list.push_str(&file(count));
        let path = self.dir.join("frames.ffconcat");
        std::fs::write(&path, list).map_err(|error| format!("{}: {error}", path.display()))?;
        Ok(self.elapsed)
    }
}

fn frame_name(n: usize) -> String {
    format!("{n:06}.png")
}

// Fast compression: these are only ffmpeg's input.
fn write_frames(queue: &Mutex<Receiver<(PathBuf, Arc<RgbaImage>)>>) -> Res<()> {
    loop {
        let job = queue.lock().map_err(|_| "the frame queue broke")?.recv();
        let Ok((path, image)) = job else { return Ok(()) };
        let failed = |error: &dyn std::fmt::Display| format!("{}: {error}", path.display());
        let mut file = BufWriter::new(File::create(&path).map_err(|error| failed(&error))?);
        PngEncoder::new_with_quality(&mut file, CompressionType::Fast, FilterType::Sub)
            .write_image(image.as_raw(), image.width(), image.height(), ExtendedColorType::Rgba8)
            .map_err(|error| failed(&error))?;
        // Dropping the writer flushes too, but loses the error.
        file.flush().map_err(|error| failed(&error))?;
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use image::{Rgba, RgbaImage};

    use super::{Recorder, balanced};

    #[test]
    fn a_caption_breaks_into_even_lines() {
        assert_eq!(balanced("Add a connection in seconds"), "Add a connection\nin seconds");
        assert_eq!(balanced("Pick the database from the server"), "Pick the database\nfrom the server");
        assert_eq!(balanced("Let AI assistants query this database"), "Let AI assistants\nquery this database");
    }

    #[test]
    fn an_unchanged_frame_lengthens_the_last_one() {
        let dir = tempfile::tempdir().unwrap();
        let mut recorder = Recorder::new(dir.path()).unwrap();
        let frame = |shade| RgbaImage::from_pixel(4, 2, Rgba([shade, shade, shade, 255]));
        recorder.add(frame(10), Duration::from_millis(40)).unwrap();
        recorder.add(frame(10), Duration::from_millis(60)).unwrap();
        recorder.add(frame(200), Duration::from_millis(30)).unwrap();
        assert_eq!(recorder.finish().unwrap(), Duration::from_millis(130));
        let list = std::fs::read_to_string(dir.path().join("frames.ffconcat")).unwrap();
        let expected = "ffconcat version 1.0
file 000001.png
option framerate 1000000
duration 0.100000
file 000002.png
option framerate 1000000
duration 0.015000
file 000002.png
option framerate 1000000
";
        assert_eq!(list, expected);
        assert!(dir.path().join("000002.png").exists() && !dir.path().join("000003.png").exists());
    }

    #[test]
    fn the_writers_own_error_stops_the_recording() {
        let dir = tempfile::tempdir().unwrap();
        let frames = dir.path().join("frames");
        let mut recorder = Recorder::new(&frames).unwrap();
        std::fs::remove_dir(&frames).unwrap();
        // Each writer stops at the frame it can't save, and the queue closes once all four have.
        let error = (0..=255u8).find_map(|shade| {
            let frame = RgbaImage::from_pixel(4, 2, Rgba([shade, shade, shade, 255]));
            recorder.add(frame, Duration::from_millis(40)).err()
        });
        let error = error.expect("adding frames went on after every writer stopped");
        assert!(error.contains(".png:"), "{error}");
    }
}
