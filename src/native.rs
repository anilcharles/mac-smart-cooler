//! The few window features GPUI does not expose, done through AppKit directly.
#![allow(unexpected_cfgs)] // emitted by the objc msg_send! macro

use objc::runtime::Object;
use objc::{class, msg_send, sel, sel_impl};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

#[repr(C)]
#[derive(Clone, Copy)]
struct NSSize {
    width: f64,
    height: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct NSRect {
    x: f64,
    y: f64,
    size: NSSize,
}

/// The NSWindow behind a GPUI window.
pub fn ns_window(window: &gpui::Window) -> Option<*mut Object> {
    let handle = HasWindowHandle::window_handle(window).ok()?;
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else { return None };
    let ns_window: *mut Object = unsafe { msg_send![appkit.ns_view.as_ptr() as *mut Object, window] };
    (!ns_window.is_null()).then_some(ns_window)
}

/// Changes the smallest size the user can drag the window to (GPUI only sets it at creation).
pub fn set_min_content_size(window: &gpui::Window, width: f32, height: f32) {
    if let Some(ns_window) = ns_window(window) {
        let size = NSSize { width: width as f64, height: height as f64 };
        let _: () = unsafe { msg_send![ns_window, setContentMinSize: size] };
    }
}

/// Makes this the active app and the window its key window. Only then does macOS draw the
/// title-bar buttons in colour, so the README screenshots use this to look the same in every shot.
#[cfg(debug_assertions)]
pub fn activate(window: &gpui::Window) {
    if let Some(ns_window) = ns_window(window) {
        unsafe {
            let app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
            let _: () = msg_send![app, activateIgnoringOtherApps: true];
            let nil: *mut Object = std::ptr::null_mut();
            let _: () = msg_send![ns_window, makeKeyAndOrderFront: nil];
        }
    }
}

/// The part of the main screen that windows can use (without the menu bar and the Dock), in points.
pub fn screen_visible_size() -> Option<(f32, f32)> {
    unsafe {
        let screen: *mut Object = msg_send![class!(NSScreen), mainScreen];
        if screen.is_null() {
            return None;
        }
        let frame: NSRect = msg_send![screen, visibleFrame];
        Some((frame.size.width as f32, frame.size.height as f32))
    }
}
