//! Developer-only: saves PNG snapshots of this app's own window.
//! macOS lets an app capture its own windows without Screen Recording permission.
//! Enabled with MACSMARTCOOLER_SNAPSHOT_DIR (debug builds); see `ui::start_demo`.
#![allow(unexpected_cfgs)] // emitted by the objc msg_send! macro

use objc::{msg_send, sel, sel_impl};
use std::ffi::{c_void, CString};

type CFRef = *const c_void;

#[repr(C)]
struct CGRect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGWindowListCreateImage(rect: CGRect, list_option: u32, window_id: u32, image_option: u32) -> CFRef;
}

#[link(name = "ImageIO", kind = "framework")]
extern "C" {
    fn CGImageDestinationCreateWithURL(url: CFRef, kind: CFRef, count: usize, options: CFRef) -> CFRef;
    fn CGImageDestinationAddImage(dest: CFRef, image: CFRef, properties: CFRef);
    fn CGImageDestinationFinalize(dest: CFRef) -> bool;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFURLCreateFromFileSystemRepresentation(alloc: CFRef, buf: *const u8, len: isize, is_dir: bool) -> CFRef;
    fn CFStringCreateWithCString(alloc: CFRef, s: *const i8, encoding: u32) -> CFRef;
    fn CFRelease(obj: CFRef);
}

/// The CoreGraphics window number of a GPUI window.
pub fn window_number(window: &gpui::Window) -> Option<u32> {
    let ns_window = crate::native::ns_window(window)?;
    let number: isize = unsafe { msg_send![ns_window, windowNumber] };
    Some(number as u32)
}

/// Writes a PNG of the window (including its rounded frame) to `path`.
pub fn save(window_id: u32, path: &str) -> bool {
    const INCLUDING_WINDOW: u32 = 1 << 3;
    const BOUNDS_IGNORE_FRAMING: u32 = 1 << 0;
    const BEST_RESOLUTION: u32 = 1 << 3;
    const UTF8: u32 = 0x0800_0100;
    unsafe {
        let null_rect = CGRect { x: f64::INFINITY, y: f64::INFINITY, w: 0., h: 0. };
        let image = CGWindowListCreateImage(null_rect, INCLUDING_WINDOW, window_id, BOUNDS_IGNORE_FRAMING | BEST_RESOLUTION);
        if image.is_null() {
            return false;
        }
        let url = CFURLCreateFromFileSystemRepresentation(std::ptr::null(), path.as_ptr(), path.len() as isize, false);
        let png = CString::new("public.png").unwrap();
        let kind = CFStringCreateWithCString(std::ptr::null(), png.as_ptr(), UTF8);
        let dest = CGImageDestinationCreateWithURL(url, kind, 1, std::ptr::null());
        let ok = !dest.is_null() && {
            CGImageDestinationAddImage(dest, image, std::ptr::null());
            CGImageDestinationFinalize(dest)
        };
        for obj in [dest, kind, url, image] {
            if !obj.is_null() {
                CFRelease(obj);
            }
        }
        ok
    }
}
