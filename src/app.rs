//! The egui front-end.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, RichText};

use crate::engine::{Engine, Event, PendingTorrent, TorrentRow, TorrentSource};
use crate::format;
use crate::settings::{self, Settings};

const MAX_LOG_LINES: usize = 200;
const ROWS_REFRESH_INTERVAL: Duration = Duration::from_millis(250);

struct LogLine {
    at: Instant,
    text: String,
    is_error: bool,
}

/// State of the "Add torrent" dialog shown once metadata is known.
struct AddDialog {
    pending: PendingTorrent,
    folder: PathBuf,
}

/// A destructive action waiting for confirmation.
struct ConfirmRemove {
    id: usize,
    name: String,
    delete_files: bool,
}

pub struct TorrentApp {
    engine: Engine,
    events: Receiver<Event>,
    settings: Settings,
    magnet_input: String,
    add_queue: Vec<AddDialog>,
    confirm_remove: Option<ConfirmRemove>,
    log: Vec<LogLine>,
    show_log: bool,
    /// Settings the engine was started with (to detect restart-only changes).
    running_settings: Settings,
    /// Editable copy while the settings window is open.
    settings_draft: Option<Settings>,
    /// Last torrent snapshot; refreshed a few times per second rather than
    /// on every repaint so the UI never competes with the download engine.
    rows: Vec<TorrentRow>,
    rows_refreshed: Option<Instant>,
}

impl TorrentApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        startup_args: Vec<String>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let settings = Settings::load();
        let (tx, rx) = channel();

        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("rustorrent-worker")
            .build()?;

        let engine = Engine::new(rt, &settings, settings::data_dir(), tx, cc.egui_ctx.clone())
            .map_err(|e| format!("{e:#}"))?;

        let mut app = Self {
            engine,
            events: rx,
            running_settings: settings.clone(),
            settings_draft: None,
            rows: Vec::new(),
            rows_refreshed: None,
            settings,
            magnet_input: String::new(),
            add_queue: Vec::new(),
            confirm_remove: None,
            log: Vec::new(),
            show_log: false,
        };
        if let Some(port) = app.engine.listen_port() {
            app.push_log(format!("Listening for peers on port {port}"), false);
        }
        for arg in startup_args {
            app.engine.fetch_metadata(TorrentSource::from_arg(&arg));
        }
        Ok(app)
    }

    fn push_log(&mut self, text: String, is_error: bool) {
        if is_error {
            tracing::error!("{text}");
        }
        self.log.push(LogLine {
            at: Instant::now(),
            text,
            is_error,
        });
        if self.log.len() > MAX_LOG_LINES {
            let excess = self.log.len() - MAX_LOG_LINES;
            self.log.drain(..excess);
        }
    }

    fn drain_events(&mut self) {
        while let Ok(ev) = self.events.try_recv() {
            match ev {
                Event::Info(s) => self.push_log(s, false),
                Event::Error(s) => self.push_log(s, true),
                Event::MetadataReady(pending) => self.add_queue.push(AddDialog {
                    pending,
                    folder: self.settings.download_dir.clone(),
                }),
            }
        }
    }

    fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        for f in dropped {
            let path = f.path();
            if !path.as_os_str().is_empty() {
                self.engine
                    .fetch_metadata(TorrentSource::File(path.to_path_buf()));
            }
        }
    }

    fn submit_magnet(&mut self) {
        let text = self.magnet_input.trim().to_string();
        if text.is_empty() {
            return;
        }
        self.engine.fetch_metadata(TorrentSource::from_arg(&text));
        self.magnet_input.clear();
    }

    // ------------------------------------------------------------------ UI

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui
                .button("➕ Open .torrent…")
                .on_hover_text("Add one or more .torrent files")
                .clicked()
                && let Some(files) = rfd::FileDialog::new()
                    .add_filter("Torrent files", &["torrent"])
                    .pick_files()
            {
                for f in files {
                    self.engine.fetch_metadata(TorrentSource::File(f));
                }
            }

            if ui
                .button("⚙ Settings")
                .on_hover_text("Speed limits, peers and network options")
                .clicked()
            {
                self.settings_draft = Some(self.settings.clone());
            }

            ui.separator();

            let add_width = 70.0;
            let input = egui::TextEdit::singleline(&mut self.magnet_input)
                .hint_text("Paste a magnet link or .torrent URL and press Enter")
                .desired_width(ui.available_width() - add_width);
            let resp = ui.add(input);
            if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                self.submit_magnet();
            }
            if ui
                .add_enabled(
                    !self.magnet_input.trim().is_empty(),
                    egui::Button::new("Add"),
                )
                .clicked()
            {
                self.submit_magnet();
            }
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Default save folder:").weak());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("Open").clicked() {
                    open_in_file_manager(&self.settings.download_dir);
                }
                if ui.small_button("Change…").clicked()
                    && let Some(dir) = rfd::FileDialog::new()
                        .set_directory(&self.settings.download_dir)
                        .pick_folder()
                {
                    self.settings.download_dir = dir;
                    self.settings.save();
                }
                path_label(ui, &self.settings.download_dir);
            });
        });
        ui.add_space(6.0);
    }

    fn status_bar(&mut self, ui: &mut egui::Ui, rows: &[TorrentRow]) {
        let (down, up) = rows.iter().fold((0.0, 0.0), |(d, u), r| {
            (d + r.rates.down_bps, u + r.rates.up_bps)
        });
        ui.horizontal(|ui| {
            ui.label(format!("⬇ {}", format::speed_bps(down)));
            if self.settings.download_limit_kib > 0 {
                ui.label(
                    RichText::new(format!(
                        "(limit {}/s)",
                        format::bytes(u64::from(self.settings.download_limit_kib) * 1024)
                    ))
                    .weak(),
                );
            }
            ui.label(format!("⬆ {}", format::speed_bps(up)));
            if self.settings.upload_limit_kib > 0 {
                ui.label(
                    RichText::new(format!(
                        "(limit {}/s)",
                        format::bytes(u64::from(self.settings.upload_limit_kib) * 1024)
                    ))
                    .weak(),
                );
            }
            ui.separator();
            ui.label(format!("{} torrent(s)", rows.len()));
            ui.separator();
            if let Some(last) = self.log.last() {
                let age = last.at.elapsed();
                if age < Duration::from_secs(15) {
                    let color = if last.is_error {
                        ui.visuals().error_fg_color
                    } else {
                        ui.visuals().text_color()
                    };
                    ui.label(RichText::new(&last.text).color(color));
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.toggle_value(&mut self.show_log, "Log");
            });
        });
    }

    fn log_panel(&self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical()
            .stick_to_bottom(true)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for line in &self.log {
                    let color = if line.is_error {
                        ui.visuals().error_fg_color
                    } else {
                        ui.visuals().weak_text_color()
                    };
                    ui.label(RichText::new(&line.text).color(color).monospace());
                }
            });
    }

    fn torrent_list(&mut self, ui: &mut egui::Ui, rows: &[TorrentRow]) {
        if rows.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.label(
                    RichText::new(
                        "No torrents yet.\n\nOpen a .torrent file, paste a magnet link above,\nor drag & drop .torrent files onto this window.",
                    )
                    .size(16.0)
                    .weak(),
                );
            });
            return;
        }

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for row in rows {
                    self.torrent_card(ui, row);
                    ui.add_space(6.0);
                }
            });
    }

    fn torrent_card(&mut self, ui: &mut egui::Ui, row: &TorrentRow) {
        egui::Frame::group(ui.style())
            .inner_margin(egui::Margin::same(10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());

                // Title line + action buttons.
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&row.name).strong().size(15.0))
                        .on_hover_text(&row.name);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .button("Remove")
                            .on_hover_text("Remove torrent…")
                            .clicked()
                        {
                            self.confirm_remove = Some(ConfirmRemove {
                                id: row.id,
                                name: row.name.clone(),
                                delete_files: false,
                            });
                        }
                        if ui
                            .button("Open folder")
                            .on_hover_text("Show in file manager")
                            .clicked()
                        {
                            open_in_file_manager(&row.output_folder);
                        }
                        if row.is_paused() {
                            if ui.button("▶ Resume").clicked() {
                                self.engine.resume(row.id);
                            }
                        } else if ui.button("⏸ Pause").clicked() {
                            self.engine.pause(row.id);
                        }
                    });
                });

                // Progress bar.
                let progress = row.progress();
                let bar_color = if row.stats.error.is_some() {
                    Color32::from_rgb(200, 70, 70)
                } else if row.stats.finished {
                    Color32::from_rgb(70, 160, 90)
                } else if row.is_paused() {
                    Color32::GRAY
                } else {
                    ui.visuals().selection.bg_fill
                };
                ui.add(
                    egui::ProgressBar::new(progress)
                        .fill(bar_color)
                        .text(format!("{:.1}%", progress * 100.0)),
                );

                // Details line.
                ui.horizontal_wrapped(|ui| {
                    let status = row.status_text();
                    if row.stats.error.is_some() {
                        ui.colored_label(ui.visuals().error_fg_color, status);
                    } else {
                        ui.label(RichText::new(status).strong());
                    }
                    ui.separator();
                    ui.label(format!(
                        "{} / {}",
                        format::bytes(row.stats.progress_bytes),
                        format::bytes(row.stats.total_bytes)
                    ));
                    if let Some(live) = &row.stats.live {
                        ui.separator();
                        ui.label(format!("⬇ {}", format::speed_bps(row.rates.down_bps)));
                        ui.label(format!("⬆ {}", format::speed_bps(row.rates.up_bps)));
                        ui.separator();
                        let peers = &live.snapshot.peer_stats;
                        ui.label(format!("Peers: {} ({} seen)", peers.live, peers.seen));
                        if let Some(eta) = row.eta {
                            ui.separator();
                            ui.label(format!("ETA: {}", format::duration(eta)));
                        }
                    }
                    ui.separator();
                    ui.label(format!(
                        "Uploaded: {}",
                        format::bytes(row.stats.uploaded_bytes)
                    ));
                });
            });
    }

    fn add_dialog(&mut self, ctx: &egui::Context) {
        let Some(dialog) = self.add_queue.first_mut() else {
            return;
        };
        let mut start = false;
        let mut cancel = false;

        let modal = egui::Modal::new(egui::Id::unique("add_torrent_modal")).show(ctx, |ui| {
            ui.set_width(560.0_f32.min(ctx.content_rect().width() - 40.0));
            ui.heading("Add torrent");
            ui.add_space(4.0);
            ui.label(RichText::new(&dialog.pending.name).strong());
            ui.add_space(8.0);

            ui.horizontal(|ui| {
                ui.label("Save to:");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("Change…").clicked()
                        && let Some(dir) = rfd::FileDialog::new()
                            .set_directory(&dialog.folder)
                            .pick_folder()
                    {
                        dialog.folder = dir;
                    }
                    path_label(ui, &dialog.folder);
                });
            });
            if let Some(sub) = &dialog.pending.sub_folder {
                ui.label(
                    RichText::new(format!(
                        "Files will be placed in a sub-folder \"{}\"",
                        sub.display()
                    ))
                    .weak()
                    .small(),
                );
            }
            ui.add_space(8.0);

            let files = &mut dialog.pending.files;
            let selected_size: u64 = files.iter().filter(|f| f.selected).map(|f| f.size).sum();
            let total_size: u64 = files.iter().map(|f| f.size).sum();
            ui.horizontal(|ui| {
                ui.label(format!(
                    "{} file(s) — {} of {} selected",
                    files.len(),
                    format::bytes(selected_size),
                    format::bytes(total_size)
                ));
                if files.len() > 1 {
                    if ui.small_button("All").clicked() {
                        files.iter_mut().for_each(|f| f.selected = true);
                    }
                    if ui.small_button("None").clicked() {
                        files.iter_mut().for_each(|f| f.selected = false);
                    }
                }
            });

            egui::ScrollArea::vertical()
                .max_height(260.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for f in files.iter_mut() {
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut f.selected, &f.path);
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| ui.label(RichText::new(format::bytes(f.size)).weak()),
                            );
                        });
                    }
                });

            ui.add_space(10.0);
            ui.horizontal(|ui| {
                let any_selected = files.iter().any(|f| f.selected);
                if ui
                    .add_enabled(any_selected, egui::Button::new("⬇ Start download"))
                    .clicked()
                {
                    start = true;
                }
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
            });
        });

        if modal.should_close() {
            cancel = true;
        }
        if start {
            let d = self.add_queue.remove(0);
            self.engine.start_download(d.pending, d.folder);
        } else if cancel {
            let d = self.add_queue.remove(0);
            self.push_log(format!("Cancelled \"{}\"", d.pending.name), false);
        }
    }

    fn settings_window(&mut self, ctx: &egui::Context) {
        let Some(draft) = &mut self.settings_draft else {
            return;
        };
        let mut open = true;
        let mut save = false;
        let mut cancel = false;
        let running = &self.running_settings;

        egui::Window::new("Settings")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.heading("Speed");
                ui.label(
                    RichText::new("By default there are no limits — Rustorrent downloads as fast as your connection allows.")
                        .weak(),
                );
                ui.add_space(6.0);
                egui::Grid::new(egui::Id::unique("speed_grid"))
                    .num_columns(2)
                    .spacing([16.0, 8.0])
                    .show(ui, |ui| {
                        limit_row(ui, "Download limit", &mut draft.download_limit_kib);
                        ui.end_row();
                        limit_row(ui, "Upload limit", &mut draft.upload_limit_kib);
                        ui.end_row();
                        ui.label("Max peers per torrent");
                        ui.add(egui::DragValue::new(&mut draft.max_peers_per_torrent).range(10..=2000))
                            .on_hover_text("More peers usually means faster downloads. Applies to newly added torrents; existing ones after restart.");
                        ui.end_row();
                    });

                ui.add_space(10.0);
                ui.heading("Network");
                ui.label(RichText::new("Changes here apply after restarting the app.").weak());
                ui.add_space(6.0);
                egui::Grid::new(egui::Id::unique("net_grid"))
                    .num_columns(2)
                    .spacing([16.0, 8.0])
                    .show(ui, |ui| {
                        ui.label("Listening port");
                        ui.add(egui::DragValue::new(&mut draft.listen_port).range(1024..=65535));
                        ui.end_row();
                        ui.label("");
                        ui.checkbox(&mut draft.enable_upnp, "UPnP port forwarding (lets more peers connect to you)");
                        ui.end_row();
                        ui.label("");
                        ui.checkbox(&mut draft.enable_utp, "Use uTP in addition to TCP (reach more peers)");
                        ui.end_row();
                        ui.label("");
                        ui.checkbox(&mut draft.extra_trackers, "Add public trackers to non-private torrents (find more peers)");
                        ui.end_row();
                    });

                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() {
                        save = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                    if ui.button("Reset to fastest defaults").clicked() {
                        let dir = draft.download_dir.clone();
                        *draft = Settings {
                            download_dir: dir,
                            ..Settings::default()
                        };
                    }
                    if draft.needs_restart_compared_to(running) {
                        ui.label(RichText::new("Restart required").color(ui.visuals().warn_fg_color));
                    }
                });
            });

        if save {
            let mut new = self.settings_draft.take().expect("checked above");
            // The save folder is edited from the toolbar, keep whatever is current.
            new.download_dir = self.settings.download_dir.clone();
            self.engine.apply_live_settings(&new);
            self.engine.set_max_peers(new.max_peers_per_torrent);
            let restart = new.needs_restart_compared_to(&self.running_settings);
            self.settings = new;
            self.settings.save();
            self.push_log(
                if restart {
                    "Settings saved. Network changes apply after restarting Rustorrent.".into()
                } else {
                    "Settings saved.".into()
                },
                false,
            );
        } else if cancel || !open {
            self.settings_draft = None;
        }
    }

    fn remove_dialog(&mut self, ctx: &egui::Context) {
        let Some(confirm) = &mut self.confirm_remove else {
            return;
        };
        let mut done: Option<bool> = None; // Some(true) = remove, Some(false) = cancel

        let modal = egui::Modal::new(egui::Id::unique("remove_torrent_modal")).show(ctx, |ui| {
            ui.set_width(420.0);
            ui.heading("Remove torrent?");
            ui.add_space(4.0);
            ui.label(RichText::new(&confirm.name).strong());
            ui.add_space(8.0);
            ui.checkbox(
                &mut confirm.delete_files,
                "Also delete downloaded files from disk",
            );
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                let label = if confirm.delete_files {
                    RichText::new("Remove and delete files").color(ui.visuals().error_fg_color)
                } else {
                    RichText::new("Remove")
                };
                if ui.button(label).clicked() {
                    done = Some(true);
                }
                if ui.button("Cancel").clicked() {
                    done = Some(false);
                }
            });
        });
        if modal.should_close() && done.is_none() {
            done = Some(false);
        }

        if let Some(remove) = done {
            let c = self.confirm_remove.take().expect("checked above");
            if remove {
                self.engine.remove(c.id, c.delete_files);
            }
        }
    }
}

impl eframe::App for TorrentApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.drain_events();
        self.handle_dropped_files(&ctx);

        if self
            .rows_refreshed
            .is_none_or(|t| t.elapsed() >= ROWS_REFRESH_INTERVAL)
        {
            self.rows = self.engine.torrents();
            self.rows_refreshed = Some(Instant::now());
        }
        let rows = std::mem::take(&mut self.rows);

        egui::Panel::top("toolbar").show(ui, |ui| self.toolbar(ui));
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui, &rows));
        if self.show_log {
            egui::Panel::bottom("log")
                .resizable(true)
                .default_size(140.0)
                .show(ui, |ui| self.log_panel(ui));
        }
        egui::CentralPanel::default().show(ui, |ui| self.torrent_list(ui, &rows));

        // Drag-and-drop hint overlay.
        if ctx.input(|i| !i.raw.hovered_files.is_empty()) {
            let painter = ctx.layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::unique("drop_overlay"),
            ));
            let rect = ctx.content_rect();
            painter.rect_filled(rect, 0.0, Color32::from_black_alpha(160));
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "Drop .torrent files to add them",
                egui::FontId::proportional(22.0),
                Color32::WHITE,
            );
        }

        self.settings_window(&ctx);
        self.add_dialog(&ctx);
        self.remove_dialog(&ctx);
        self.rows = rows;

        // Keep stats fresh even without user input.
        ctx.request_repaint_after(Duration::from_millis(1000));
    }

    fn on_exit(&mut self) {
        self.engine.shutdown();
    }
}

/// A monospace path that truncates (with full path on hover) instead of
/// pushing the rest of the row off-screen.
fn path_label(ui: &mut egui::Ui, path: &std::path::Path) {
    let text = path.display().to_string();
    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.add(egui::Label::new(RichText::new(&text).monospace()).truncate())
            .on_hover_text(&text);
    });
}

/// A "Limit [x]  [ 500 ] KiB/s" row. 0 means unlimited.
fn limit_row(ui: &mut egui::Ui, label: &str, kib: &mut u32) {
    ui.label(label);
    ui.horizontal(|ui| {
        let mut limited = *kib > 0;
        if ui.checkbox(&mut limited, "").changed() {
            *kib = if limited { 1024 } else { 0 };
        }
        if limited {
            ui.add(
                egui::DragValue::new(kib)
                    .range(1..=4_000_000)
                    .speed(16.0)
                    .suffix(" KiB/s"),
            );
            ui.label(
                RichText::new(format!("≈ {}/s", format::bytes(u64::from(*kib) * 1024))).weak(),
            );
        } else {
            ui.label(RichText::new("Unlimited").strong());
        }
    });
}

fn open_in_file_manager(path: &std::path::Path) {
    let target = if path.exists() {
        path.to_path_buf()
    } else {
        // Folder for a not-yet-started torrent may not exist; open its parent.
        path.parent().map(|p| p.to_path_buf()).unwrap_or_default()
    };

    #[cfg(target_os = "windows")]
    let cmd = "explorer";
    #[cfg(target_os = "macos")]
    let cmd = "open";
    #[cfg(all(unix, not(target_os = "macos")))]
    let cmd = "xdg-open";

    if let Err(e) = std::process::Command::new(cmd).arg(&target).spawn() {
        tracing::warn!("could not open {}: {e}", target.display());
    }
}
