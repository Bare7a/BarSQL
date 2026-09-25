use std::path::PathBuf;
use std::time::Duration;

use gpui_kit::{AnyWindowHandle, App, AsyncApp, Window};

use crate::actions::RunAll;

pub type AfterRun = Box<dyn FnOnce(&mut Window, &mut App)>;

// BARSQL_SNAPSHOT saves a PNG of the settled window and quits, to check the UI unattended. `run` runs the
// active tab's SQL first (BARSQL_SNAPSHOT_RUN), then `after` acts on the results, e.g. opens a dialog.
pub fn schedule(window: AnyWindowHandle, path: PathBuf, run: bool, after: Option<AfterRun>, cx: &mut AsyncApp) {
    cx.spawn(async move |cx| {
        cx.background_executor().timer(Duration::from_millis(800)).await;
        if run {
            let _ = window.update(cx, |_, window, cx| window.dispatch_action(Box::new(RunAll), cx));
            cx.background_executor().timer(Duration::from_millis(2000)).await;
        }
        if let Some(after) = after {
            // Actions go to what the last drawn frame focused, so draw the results first.
            let _ = window.update(cx, |_, window, cx| {
                window.draw(cx).clear(cx);
                after(window, cx);
            });
            cx.background_executor().timer(Duration::from_millis(500)).await;
        }
        // A window behind others gets no frames, so draw the one a layout change asked for.
        let _ = window.update(cx, |_, window, cx| {
            window.draw(cx).clear(cx);
            window.refresh();
            window.draw(cx).clear(cx);
            save(window, &path);
            cx.quit();
        });
    })
    .detach();
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
