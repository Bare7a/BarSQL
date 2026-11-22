use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use barsql_app::update::{self, Asset, Release, Stage, Staged};
use gpui_kit::component::{Root, WindowExt};
use gpui_kit::{AppContext as _, Context, Entity, IntoElement, Render, TestAppContext, VisualTestContext, Window, div};

use super::{
    STARTUP_DELAY, Skipped, Transfer, UpdateChip, UpdateDialog, UpdateState, check_after, chip, chip_for, flow,
    open_in, remember, reopen,
};
use crate::test_support::{Env, settle};

type Routes = Arc<Mutex<HashMap<String, (u16, String)>>>;

// Fake GitHub API.
fn server() -> (String, Routes) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let routes: Routes = Arc::default();
    let shared = routes.clone();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request = String::new();
            let _ = reader.read_line(&mut request);
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap_or(0) > 2 {
                line.clear();
            }
            let path = request.split_whitespace().nth(1).unwrap_or("/").to_string();
            let (status, body) = shared.lock().unwrap().get(&path).cloned().unwrap_or((404, String::new()));
            let _ = write!(
                stream,
                "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    (base, routes)
}

fn release(base: &str, tag: &str) -> String {
    let asset = format!("BarSQL-{}-{}.zip", update::platform(), update::arch());
    format!(
        r#"{{"tag_name":"{tag}","name":"BarSQL {tag}","body":"- Faster","html_url":"{base}/r","assets":[{{"name":"{asset}","size":2048,"browser_download_url":"{base}/d"}}]}}"#
    )
}

struct Blank;

impl Render for Blank {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

fn window(cx: &mut TestAppContext) -> &mut VisualTestContext {
    let window = cx.add_window(|window, cx| {
        let view = cx.new(|_| Blank);
        Root::new(view, window, cx)
    });
    VisualTestContext::from_window(window.into(), cx).into_mut()
}

fn open(base: &str, cx: &mut VisualTestContext) -> Entity<UpdateDialog> {
    let base = base.to_string();
    let dialog = cx.update(|window, cx| open_in(base, None, window, cx));
    settle(cx, |cx| cx.update(|_, cx| dialog.read(cx).state != UpdateState::Checking));
    dialog
}

fn state(dialog: &Entity<UpdateDialog>, cx: &mut VisualTestContext) -> UpdateState {
    cx.update(|_, cx| dialog.read(cx).state.clone())
}

#[gpui_kit::test]
fn a_skipped_release_counts_as_up_to_date_for_the_session(cx: &mut TestAppContext) {
    let _env = Env::new(cx);
    let (base, routes) = server();
    routes.lock().unwrap().insert("/repos/Bare7a/BarSQL/releases/latest".into(), (200, release(&base, "v9.9.9")));
    let cx = window(cx);

    let dialog = open(&base, cx);
    match state(&dialog, cx) {
        UpdateState::Available(release) => {
            assert_eq!((release.version.as_str(), release.notes.as_str()), ("9.9.9", "- Faster"));
        }
        other => panic!("expected a release, got {other:?}"),
    }
    dialog.update_in(cx, |dialog, window, cx| dialog.skip(window, cx));
    assert_eq!(state(&open(&base, cx), cx), UpdateState::UpToDate);

    routes.lock().unwrap().insert("/repos/Bare7a/BarSQL/releases/latest".into(), (200, release(&base, "v9.9.10")));
    assert!(matches!(state(&open(&base, cx), cx), UpdateState::Available(_)), "a newer release is offered again");
}

// Enter presses Try again. Once the check is through, Close is the only button, so Enter closes.
#[gpui_kit::test]
fn a_failed_check_retries_on_enter(cx: &mut TestAppContext) {
    let _env = Env::new(cx);
    let (base, routes) = server();
    routes.lock().unwrap().insert("/repos/Bare7a/BarSQL/releases/latest".into(), (502, "bad gateway".into()));
    let cx = window(cx);

    let dialog = open(&base, cx);
    let failed =
        UpdateState::Failed { stage: Stage::Check, message: "github: api 502: bad gateway".into(), release: None };
    assert_eq!(state(&dialog, cx), failed);

    routes.lock().unwrap().insert("/repos/Bare7a/BarSQL/releases/latest".into(), (200, release(&base, "v0.0.1")));
    cx.simulate_keystrokes("enter");
    settle(cx, |cx| cx.update(|_, cx| dialog.read(cx).state != UpdateState::Checking));
    assert_eq!(state(&dialog, cx), UpdateState::UpToDate, "0.0.1 is older than this build");
    assert!(cx.update(|window, cx| window.has_active_dialog(cx)));
    cx.simulate_keystrokes("enter");
    assert!(!cx.update(|window, cx| window.has_active_dialog(cx)));
}

fn toasts(cx: &mut VisualTestContext) -> usize {
    cx.update(|_, cx| crate::toast::messages(cx).len())
}

#[gpui_kit::test]
fn the_launch_check_turns_a_release_into_a_sticky_toast(cx: &mut TestAppContext) {
    let _env = Env::new(cx);
    let (base, routes) = server();
    routes.lock().unwrap().insert("/repos/Bare7a/BarSQL/releases/latest".into(), (200, release(&base, "v9.9.9")));
    let cx = window(cx);

    let url = base.clone();
    cx.update(|_, cx| check_after(url, STARTUP_DELAY, cx));
    cx.run_until_parked();
    assert_eq!(toasts(cx), 0, "not before the delay");
    cx.executor().advance_clock(Duration::from_secs(5));
    settle(cx, |cx| toasts(cx) == 1);

    cx.update(|_, cx| cx.set_global(Skipped(Some("9.9.9".into()))));
    let url = base.clone();
    cx.update(|_, cx| check_after(url, Duration::ZERO, cx));
    cx.executor().advance_clock(Duration::from_millis(1));
    for _ in 0..40 {
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(toasts(cx), 1, "a skipped release stays quiet");
}

fn release_value(version: &str) -> Release {
    Release {
        version: version.into(),
        name: format!("BarSQL v{version}"),
        notes: String::new(),
        html_url: String::new(),
        asset: Asset { name: "BarSQL.zip".into(), size: 1, url: String::new() },
        digest: None,
    }
}

#[test]
fn the_chip_follows_the_update_flow() {
    let r = release_value("9.9.9");
    let failed = |stage, release| UpdateState::Failed { stage, message: "x".into(), release };
    assert_eq!(chip_for(&UpdateState::Checking, None), None);
    assert_eq!(chip_for(&UpdateState::UpToDate, None), None);
    assert_eq!(chip_for(&UpdateState::Available(r.clone()), None), Some(UpdateChip::Available("9.9.9".into())));
    assert_eq!(chip_for(&UpdateState::Available(r.clone()), Some("9.9.9")), None, "skipped");
    let half = Transfer { written: 42, total: 100, rate: 1 };
    assert_eq!(
        chip_for(&UpdateState::Downloading(r.clone(), Some(half)), None),
        Some(UpdateChip::Downloading(Some(42)))
    );
    for busy in [
        UpdateState::Downloading(r.clone(), None),
        UpdateState::Verifying(r.clone()),
        UpdateState::Installing(r.clone()),
    ] {
        assert_eq!(chip_for(&busy, None), Some(UpdateChip::Downloading(None)));
    }
    let staged = Staged { path: "BarSQL.app".into(), dir: std::env::temp_dir() };
    assert_eq!(chip_for(&UpdateState::Ready(r.clone(), staged), None), Some(UpdateChip::Ready));
    assert_eq!(chip_for(&failed(Stage::Check, None), None), None, "a failed check stays quiet");
    assert_eq!(chip_for(&failed(Stage::Download, Some(r)), None), Some(UpdateChip::Failed));
}

// The launch check leaves a chip behind its toast, and the chip reopens the dialog on that release without checking
// again.
#[gpui_kit::test]
fn the_launch_check_leaves_an_update_chip(cx: &mut TestAppContext) {
    let _env = Env::new(cx);
    let (base, routes) = server();
    routes.lock().unwrap().insert("/repos/Bare7a/BarSQL/releases/latest".into(), (200, release(&base, "v9.9.9")));
    let cx = window(cx);
    cx.update(|_, cx| check_after(base.clone(), Duration::ZERO, cx));
    cx.executor().advance_clock(Duration::from_millis(1));
    settle(cx, |cx| cx.update(|_, cx| chip(cx).is_some()));
    assert_eq!(cx.update(|_, cx| chip(cx)), Some(UpdateChip::Available("9.9.9".into())));
    routes.lock().unwrap().insert("/repos/Bare7a/BarSQL/releases/latest".into(), (502, "down".into()));
    let dialog = cx.update(reopen);
    assert!(matches!(state(&dialog, cx), UpdateState::Available(_)), "no new check");
}

// Check for Updates opened before the launch check answers stays the flow's dialog, so a download started in it keeps
// the chip in step and survives Close.
#[gpui_kit::test]
fn the_launch_check_joins_a_dialog_already_open(cx: &mut TestAppContext) {
    let _env = Env::new(cx);
    let (base, routes) = server();
    routes.lock().unwrap().insert("/repos/Bare7a/BarSQL/releases/latest".into(), (200, release(&base, "v9.9.9")));
    let cx = window(cx);
    let dialog = open(&base, cx);
    cx.update(|_, cx| check_after(base.clone(), Duration::ZERO, cx));
    cx.executor().advance_clock(Duration::from_millis(1));
    settle(cx, |cx| toasts(cx) == 1);
    assert_eq!(cx.update(|_, cx| flow(cx)), Some(dialog.clone()));
    assert!(matches!(state(&dialog, cx), UpdateState::Available(_)));
}

// The toast's Update opens the release the launch check found, without checking again.
#[gpui_kit::test]
fn the_toast_reopens_the_release_it_found(cx: &mut TestAppContext) {
    let _env = Env::new(cx);
    let (base, routes) = server();
    routes.lock().unwrap().insert("/repos/Bare7a/BarSQL/releases/latest".into(), (200, release(&base, "v9.9.9")));
    let cx = window(cx);
    cx.update(|_, cx| check_after(base.clone(), Duration::ZERO, cx));
    cx.executor().advance_clock(Duration::from_millis(1));
    settle(cx, |cx| toasts(cx) == 1);
    let update = cx.update(|_, cx| crate::toast::action(0, cx)).expect("the toast's Update button");
    cx.update(|window, cx| (update.on_click)(window, cx));
    let dialog = cx.update(|_, cx| flow(cx)).expect("the update flow");
    assert!(matches!(state(&dialog, cx), UpdateState::Available(_)), "no new check");
}

#[gpui_kit::test]
fn skipping_the_release_hides_the_chip(cx: &mut TestAppContext) {
    let _env = Env::new(cx);
    let (base, routes) = server();
    routes.lock().unwrap().insert("/repos/Bare7a/BarSQL/releases/latest".into(), (200, release(&base, "v9.9.9")));
    let cx = window(cx);
    cx.update(|_, cx| check_after(base.clone(), Duration::ZERO, cx));
    cx.executor().advance_clock(Duration::from_millis(1));
    settle(cx, |cx| cx.update(|_, cx| chip(cx).is_some()));
    let dialog = cx.update(reopen);
    dialog.update_in(cx, |dialog, window, cx| dialog.skip(window, cx));
    assert_eq!(cx.update(|_, cx| chip(cx)), None);
}

// A launch check that answers during an install, or with one staged, leaves it be.
#[gpui_kit::test]
fn the_launch_check_leaves_an_install_alone(cx: &mut TestAppContext) {
    let _env = Env::new(cx);
    let cx = window(cx);
    let staged = Staged { path: "BarSQL.app".into(), dir: std::env::temp_dir() };
    let ready = UpdateState::Ready(release_value("9.9.9"), staged);
    let dialog = cx.update(|window, cx| open_in(String::new(), Some(ready.clone()), window, cx));
    cx.update(|_, cx| remember(String::new(), release_value("9.9.10"), cx));
    assert_eq!(cx.update(|_, cx| flow(cx)), Some(dialog.clone()));
    assert_eq!(state(&dialog, cx), ready);
}

const BUILD: &str = "BarSQL 9.9.9 build";
const BUILD_SHA256: &str = "c7b5e9792619b44e2cf9cc0be2f5607892846430c81dd8194fa0dee73e148a34";

fn asset_name() -> String {
    format!("BarSQL-{}-{}", update::platform(), update::arch())
}

// Bare binary asset, listed in SHA256SUMS with `digest`.
fn installable(base: &str, routes: &Routes, digest: &str) {
    let asset = asset_name();
    let json = format!(
        r#"{{"tag_name":"v9.9.9","name":"BarSQL v9.9.9","body":"- Faster","html_url":"{base}/r","assets":[{{"name":"{asset}","size":{},"browser_download_url":"{base}/d"}},{{"name":"SHA256SUMS","size":100,"browser_download_url":"{base}/sums"}}]}}"#,
        BUILD.len()
    );
    let mut routes = routes.lock().unwrap();
    routes.insert("/repos/Bare7a/BarSQL/releases/latest".into(), (200, json));
    routes.insert("/d".into(), (200, BUILD.into()));
    routes.insert("/sums".into(), (200, format!("{digest}  {asset}\n")));
}

fn kind(state: &UpdateState) -> &'static str {
    match state {
        UpdateState::Checking => "checking",
        UpdateState::UpToDate => "up-to-date",
        UpdateState::Available(_) => "available",
        UpdateState::Downloading(_, None) => "starting",
        UpdateState::Downloading(_, Some(_)) => "downloading",
        UpdateState::Verifying(_) => "verifying",
        UpdateState::Installing(_) => "installing",
        UpdateState::Ready(..) => "ready",
        UpdateState::Failed { .. } => "failed",
    }
}

fn install(dialog: &Entity<UpdateDialog>, cx: &mut VisualTestContext) -> UpdateState {
    dialog.update(cx, |dialog, cx| dialog.install(cx));
    settle(cx, |cx| cx.update(|_, cx| matches!(kind(&dialog.read(cx).state), "ready" | "failed")));
    state(dialog, cx)
}

#[gpui_kit::test]
fn install_downloads_verifies_stages_and_hands_the_build_to_the_helper(cx: &mut TestAppContext) {
    let _env = Env::new(cx);
    let (base, routes) = server();
    installable(&base, &routes, BUILD_SHA256);
    let cx = window(cx);
    let dialog = open(&base, cx);
    let seen = Rc::new(RefCell::new(Vec::new()));
    let log = seen.clone();
    cx.update(|_, cx| {
        cx.observe(&dialog, move |dialog, cx| {
            let kind = kind(&dialog.read(cx).state);
            if log.borrow().last() != Some(&kind) {
                log.borrow_mut().push(kind);
            }
        })
        .detach()
    });

    let UpdateState::Ready(release, staged) = install(&dialog, cx) else { panic!("{:?}", state(&dialog, cx)) };
    assert_eq!(*seen.borrow(), ["starting", "downloading", "verifying", "installing", "ready"]);
    assert_eq!(release.version, "9.9.9");
    assert_eq!(staged.path, staged.dir.join(asset_name()));
    assert_eq!(std::fs::read_to_string(&staged.path).unwrap(), BUILD);

    cx.update(|window, cx| window.close_dialog(cx));
    let reopened = cx.update(|window, cx| open_in(base.clone(), None, window, cx));
    assert_eq!(reopened.entity_id(), dialog.entity_id(), "Check for Updates shows the staged update again");
    assert_eq!(kind(&state(&reopened, cx)), "ready");

    let applied: Rc<RefCell<Vec<(String, PathBuf, String)>>> = Rc::default();
    let record = applied.clone();
    dialog.update(cx, |dialog, _| {
        dialog.apply = Rc::new(move |release, path, prompt| {
            record.borrow_mut().push((release.version.clone(), path.to_path_buf(), prompt.to_string()));
            Ok(())
        })
    });
    cx.update(|window, cx| dialog.update(cx, |dialog, cx| dialog.restart(window, cx)));
    let prompt = "BarSQL needs your password to install the update.".to_string();
    assert_eq!(*applied.borrow(), [("9.9.9".to_string(), staged.path.clone(), prompt)]);

    dialog.update(cx, |dialog, _| dialog.apply = Rc::new(|_, _, _| Err(std::io::Error::other("denied"))));
    cx.update(|window, cx| dialog.update(cx, |dialog, cx| dialog.restart(window, cx)));
    let failed =
        UpdateState::Failed { stage: Stage::Install, message: "spawn helper: denied".into(), release: Some(release) };
    assert_eq!(state(&dialog, cx), failed);
    let _ = std::fs::remove_dir_all(staged.dir);
}

#[gpui_kit::test]
fn a_bad_checksum_stops_at_verify_and_try_again_resumes_a_failed_download(cx: &mut TestAppContext) {
    let _env = Env::new(cx);
    let (base, routes) = server();
    let wrong = "0".repeat(64);
    installable(&base, &routes, &wrong);
    let cx = window(cx);
    let dialog = open(&base, cx);
    let UpdateState::Failed { stage, message, .. } = install(&dialog, cx) else { panic!("{:?}", state(&dialog, cx)) };
    assert_eq!((stage, message), (Stage::Verify, format!("checksum mismatch: expected {wrong}, got {BUILD_SHA256}")));

    installable(&base, &routes, BUILD_SHA256);
    routes.lock().unwrap().insert("/d".into(), (404, String::new()));
    let reopened = cx.update(|window, cx| {
        window.close_dialog(cx);
        open_in(base.clone(), None, window, cx)
    });
    assert_ne!(reopened.entity_id(), dialog.entity_id(), "after a failure the dialog checks afresh");
    settle(cx, |cx| cx.update(|_, cx| reopened.read(cx).state != UpdateState::Checking));
    let failed = install(&reopened, cx);
    let UpdateState::Failed { stage, message, release: Some(_) } = failed else { panic!("{failed:?}") };
    assert_eq!((stage, message.as_str()), (Stage::Download, "download: http status: 404"));

    routes.lock().unwrap().insert("/d".into(), (200, BUILD.into()));
    reopened.update(cx, |dialog, cx| dialog.retry(cx));
    settle(cx, |cx| cx.update(|_, cx| matches!(kind(&reopened.read(cx).state), "ready" | "failed")));
    let UpdateState::Ready(_, staged) = state(&reopened, cx) else { panic!("{:?}", state(&reopened, cx)) };
    assert_eq!(std::fs::read_to_string(&staged.path).unwrap(), BUILD);
    let _ = std::fs::remove_dir_all(staged.dir);
}
