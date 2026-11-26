// Hide the console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod engine;
mod format;
mod headless;
mod platform;
mod settings;
mod speed;
mod theme;
mod upload_policy;

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

    if startup_args.first().map(String::as_str) == Some("--headless") {
        if let Err(e) = headless::run(&startup_args[1..]) {
            eprintln!("error: {e:#}");
            std::process::exit(1);
        }
        return Ok(());
    }

    // Already running? Hand the torrents/links to that window and exit.
    if platform::instance::forward_or_listen(&startup_args) {
        return Ok(());
    }
    // macOS delivers clicked links / opened files as Apple events.
    #[cfg(target_os = "macos")]
    platform::macos::install_open_url_handler();

    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon-256.png"))
        .expect("embedded icon is a valid PNG");
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Rustorrent")
            .with_inner_size([1080.0, 680.0])
            .with_min_inner_size([760.0, 440.0])
            .with_drag_and_drop(true)
            .with_icon(icon)
            // macOS: unified title bar, content runs under the traffic lights.
            .with_fullsize_content_view(true)
            .with_title_shown(false)
            .with_titlebar_shown(false),
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
