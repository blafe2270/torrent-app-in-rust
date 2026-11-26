//! macOS delivers clicked magnet links and double-clicked .torrent files as
//! `application:openURLs:` calls on the app delegate. winit's delegate doesn't
//! implement it, so we add the method to NSObject (which every delegate
//! inherits) before the app finishes launching. That also catches the URL
//! that launched the app.

use std::ffi::c_char;

use objc2::ffi;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Sel};
use objc2::sel;
use objc2_foundation::{NSArray, NSString, NSURL};

unsafe extern "C-unwind" fn open_urls(
    _this: *mut AnyObject,
    _cmd: Sel,
    _app: *mut AnyObject,
    urls: *mut NSArray<NSURL>,
) {
    let Some(urls) = (unsafe { urls.as_ref() }) else {
        return;
    };
    for url in urls.iter() {
        let arg = if url.isFileURL() {
            url.path().map(|p| p.to_string())
        } else {
            url.absoluteString()
                .map(|s: Retained<NSString>| s.to_string())
        };
        if let Some(arg) = arg {
            super::deliver(arg);
        }
    }
}

pub fn install_open_url_handler() {
    let cls: &AnyClass = objc2::class!(NSObject);
    let sel: Sel = sel!(application:openURLs:);
    // v = void, @ = self, : = _cmd, @ = NSApplication, @ = NSArray
    let types = c"v@:@@";
    unsafe {
        let imp = std::mem::transmute::<
            unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject, *mut NSArray<NSURL>),
            unsafe extern "C-unwind" fn(),
        >(open_urls);
        let added = ffi::class_addMethod(
            cls as *const AnyClass as *mut AnyClass,
            sel,
            imp,
            types.as_ptr() as *const c_char,
        );
        tracing::debug!(added = added.as_bool(), "installed application:openURLs:");
    }
}
