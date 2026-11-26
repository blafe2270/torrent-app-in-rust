//! OS integration: single instance, file/link associations, menu-bar/tray
//! icon, and macOS "open URL" events.

pub mod instance;
#[cfg(target_os = "macos")]
pub mod macos;
pub mod register;
pub mod tray;

use std::sync::OnceLock;

/// Where incoming torrents (from a second launch, a clicked magnet link or a
/// double-clicked .torrent file) are delivered. An empty string means
/// "bring the window to the front".
static SINK: OnceLock<Box<dyn Fn(String) + Send + Sync>> = OnceLock::new();
static PENDING: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// Deliver a torrent/link to the app (or queue it until the app is ready).
pub fn deliver(arg: String) {
    match SINK.get() {
        Some(sink) => sink(arg),
        None => PENDING.lock().unwrap_or_else(|e| e.into_inner()).push(arg),
    }
}

/// Install the app's receiver and flush anything that arrived early.
pub fn set_sink(sink: impl Fn(String) + Send + Sync + 'static) {
    let _ = SINK.set(Box::new(sink));
    let pending = std::mem::take(&mut *PENDING.lock().unwrap_or_else(|e| e.into_inner()));
    for arg in pending {
        deliver(arg);
    }
}
