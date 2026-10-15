// The Dock's Quit, logging out and shutting down send terminate: past the Quit action, and GPUI Kit's delegate has no
// applicationShouldTerminate:, so this adds one that cancels the quit and asks while a tab holds a transaction.
// A cancelled logout or shutdown stays cancelled, and OK quits BarSQL alone.
use std::cell::{Cell, OnceCell};

use barsql_app::BarApp;
use gpui_kit::{App, AsyncApp};
use objc2::ffi::{NSUInteger, class_addMethod};
use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
use objc2::sel;

use crate::workspace;

// NSApplicationTerminateReply's NSTerminateCancel and NSTerminateNow.
const TERMINATE_CANCEL: NSUInteger = 0;
const TERMINATE_NOW: NSUInteger = 1;

type ShouldTerminate = extern "C" fn(*mut AnyObject, Sel, *mut AnyObject) -> NSUInteger;

thread_local! {
    // AppKit asks on the main thread, from outside GPUI.
    static HOOK: OnceCell<(BarApp, AsyncApp)> = const { OnceCell::new() };
    // Set once Quit has asked, or had nothing to ask about.
    static ALLOWED: Cell<bool> = const { Cell::new(false) };
}

#[derive(Debug, PartialEq, Eq)]
pub enum Reply {
    Now,
    Cancel,
}

// Before GPUI sets its delegate, in case AppKit looks for the method then.
pub fn install() {
    let selector = sel!(applicationShouldTerminate:);
    let class = AnyClass::get(c"GPUIApplicationDelegate").filter(|class| !class.responds_to(selector));
    debug_assert!(class.is_some(), "GPUI Kit's app delegate changed, so system_quit needs a look");
    let Some(class) = class else { return };
    let method: ShouldTerminate = should_terminate_objc;
    // SAFETY: the class is registered and lacks the method. The function has the method's signature, which "Q@:@"
    // encodes: an NSApplicationTerminateReply (NSUInteger) from self, _cmd and the sender.
    unsafe {
        let imp = std::mem::transmute::<ShouldTerminate, Imp>(method);
        class_addMethod(std::ptr::from_ref(class).cast_mut(), selector, imp, c"Q@:@".as_ptr());
    }
}

pub fn init(bar: BarApp, cx: &App) {
    HOOK.with(|hook| {
        hook.get_or_init(|| (bar, cx.to_async()));
    });
}

pub fn allow_terminate() {
    ALLOWED.set(true);
}

// AppKit asks from its event loop, which can run nested inside a GPUI callback that holds the App. So this reads
// BarApp alone and asks from a task.
pub fn should_terminate(bar: &BarApp, cx: &AsyncApp) -> Reply {
    if ALLOWED.get() || !bar.any_transaction_open() {
        return Reply::Now;
    }
    cx.spawn(async |cx| cx.update(ask)).detach();
    Reply::Cancel
}

// The app may be in the background, hidden or minimized, and the confirmation shows in its window.
fn ask(cx: &mut App) {
    cx.activate(true);
    for window in cx.windows() {
        let _ = window.update(cx, |_, window, _| window.activate_window());
    }
    workspace::quit(cx);
}

extern "C" fn should_terminate_objc(_: *mut AnyObject, _: Sel, _: *mut AnyObject) -> NSUInteger {
    let reply = HOOK.try_with(|hook| hook.get().map(|(bar, cx)| should_terminate(bar, cx))).ok().flatten();
    match reply {
        Some(Reply::Cancel) => TERMINATE_CANCEL,
        Some(Reply::Now) | None => TERMINATE_NOW,
    }
}
