// Hide the console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod engine;
mod format;
mod settings;
mod speed;

use eframe::egui;

fn main() -> eframe::Result {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "warn,librqbit=warn".into()),
        )
        .init();

    // Torrents passed on the command line (e.g. from a file association or
    // a magnet: link handler) are queued and added once the window is up.
    let startup_args: Vec<String> = std::env::args().skip(1).collect();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Rustorrent")
            .with_inner_size([960.0, 600.0])
            .with_min_inner_size([640.0, 360.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };

    let result = eframe::run_native(
        "Rustorrent",
        options,
        Box::new(move |cc| Ok(Box::new(app::TorrentApp::new(cc, startup_args)?))),
    );
    if let Err(e) = &result {
        // Release builds on Windows have no console, so show a dialog.
        rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Error)
            .set_title("Rustorrent could not start")
            .set_description(e.to_string())
            .show();
    }
    result
}
