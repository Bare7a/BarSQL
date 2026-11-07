use std::path::PathBuf;
use std::time::Duration;

use gpui_kit::{AnyWindowHandle, App, AsyncApp, Modifiers, MouseMoveEvent, PlatformInput, Window, point, px};

use crate::actions::RunAll;

pub type AfterRun = Box<dyn FnOnce(&mut Window, &mut App)>;

// BARSQL_SNAPSHOT saves a PNG of the settled window and quits, to check the UI unattended. `run` runs the
// active tab's SQL first (BARSQL_SNAPSHOT_RUN), then `after` acts on the results, e.g. opens a dialog.
pub fn schedule(window: AnyWindowHandle, path: PathBuf, run: bool, after: Option<AfterRun>, cx: &mut AsyncApp) {
    cx.spawn(async move |cx| {
        cx.background_executor().timer(Duration::from_millis(800)).await;
        let _ = window.update(cx, |_, window, cx| {
            // The window's mouse starts wherever the real one is, so it would hover whatever lies under it.
            let away = MouseMoveEvent {
                position: point(px(-1.), px(-1.)),
                pressed_button: None,
                modifiers: Modifiers::none(),
            };
            window.dispatch_event(PlatformInput::MouseMove(away), cx);
            // Actions go to what the last drawn frame focused.
            draw(window, cx);
        });
        if run {
            let _ = window.update(cx, |_, window, cx| window.dispatch_action(Box::new(RunAll), cx));
            cx.background_executor().timer(Duration::from_millis(2000)).await;
        }
        if let Some(after) = after {
            // Actions go to what the last drawn frame focused, so draw the results first.
            let _ = window.update(cx, |_, window, cx| {
                draw(window, cx);
                after(window, cx);
            });
            cx.background_executor().timer(Duration::from_millis(500)).await;
        }
        // A window behind others gets no frames, so draw the one a layout change asked for.
        let _ = window.update(cx, |_, window, cx| {
            draw(window, cx);
            window.refresh();
            draw(window, cx);
            save(window, &path);
            cx.quit();
        });
    })
    .detach();
}

// Windows sends a hidden window no frames, so its next-frame callbacks wait on these draws.
fn draw(window: &mut Window, cx: &mut App) {
    window.draw(cx).clear(cx);
    #[cfg(feature = "snapshot")]
    window.simulate_next_frame(cx);
}

#[cfg(feature = "snapshot")]
fn save(window: &mut gpui_kit::Window, path: &std::path::Path) {
    match window.render_to_image() {
        Ok(image) => match image.save(path) {
            Ok(()) => println!("snapshot saved to {}", path.display()),
            Err(err) => eprintln!("snapshot save failed: {err}"),
        },
        Err(err) => eprintln!("snapshot render failed: {err}"),
    }
}

#[cfg(not(feature = "snapshot"))]
fn save(_: &mut gpui_kit::Window, _: &std::path::Path) {
    eprintln!("BARSQL_SNAPSHOT needs a build with --features snapshot");
}
