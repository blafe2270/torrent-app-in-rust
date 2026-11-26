//! The egui front-end, styled after native macOS apps (see `theme`).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontId, Layout, Margin, RichText, Sense, Stroke,
    vec2,
};

use crate::engine::{Engine, Event, PendingTorrent, TorrentRow, TorrentSource};
use crate::format;
use crate::settings::{self, Settings};
use crate::theme::{self, Icon, Palette};

const MAX_LOG_LINES: usize = 200;
const ROWS_REFRESH_INTERVAL: Duration = Duration::from_millis(250);
const SPEED_HISTORY: usize = 90;

struct LogLine {
    at: Instant,
    text: String,
    is_error: bool,
}

/// State of the "Add Torrent" sheet shown once metadata is known.
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

#[derive(Clone, Copy, PartialEq)]
enum Filter {
    All,
    Downloading,
    Seeding,
    Paused,
    Errors,
}

impl Filter {
    fn matches(self, row: &TorrentRow) -> bool {
        let ok = row.stats.error.is_none();
        match self {
            Filter::All => true,
            Filter::Downloading => {
                ok && (row.queued || (!row.is_paused() && !row.stats.finished))
            }
            Filter::Seeding => ok && !row.is_paused() && row.stats.finished,
            Filter::Paused => row.is_paused() && !row.queued,
            Filter::Errors => !ok,
        }
    }
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
    filter: Filter,
    /// Torrent selected in the grid/list (its actions appear in the toolbar).
    selected: Option<usize>,
    /// Settings the engine was started with (to detect restart-only changes).
    running_settings: Settings,
    /// Editable copy while the settings sheet is open.
    settings_draft: Option<Settings>,
    /// Last torrent snapshot; refreshed a few times per second rather than
    /// on every repaint so the UI never competes with the download engine.
    rows: Vec<TorrentRow>,
    rows_refreshed: Option<Instant>,
    down_history: VecDeque<f64>,
    history_sampled: Option<Instant>,
}

impl TorrentApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        startup_args: Vec<String>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        theme::install(&cc.egui_ctx);

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
            down_history: VecDeque::with_capacity(SPEED_HISTORY),
            history_sampled: None,
            filter: Filter::All,
            selected: None,
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

    fn open_torrent_files(&mut self) {
        if let Some(files) = rfd::FileDialog::new()
            .add_filter("Torrent files", &["torrent"])
            .pick_files()
        {
            for f in files {
                self.engine.fetch_metadata(TorrentSource::File(f));
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

    fn totals(rows: &[TorrentRow]) -> (f64, f64) {
        rows.iter().fold((0.0, 0.0), |(d, u), r| {
            (d + r.rates.down_bps, u + r.rates.up_bps)
        })
    }

    // ------------------------------------------------------------- toolbar

    fn filter_title(&self) -> &'static str {
        match self.filter {
            Filter::All => "All Torrents",
            Filter::Downloading => "Downloading",
            Filter::Seeding => "Seeding",
            Filter::Paused => "Paused",
            Filter::Errors => "Errors",
        }
    }

    /// Title on the left, capsule button groups on the right (Finder style).
    fn toolbar(&mut self, ui: &mut egui::Ui, rows: &[TorrentRow]) {
        let p = theme::palette(ui);
        ui.horizontal(|ui| {
            ui.set_min_height(36.0);
            ui.label(RichText::new(self.filter_title()).size(15.5).strong());

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                theme::capsule(ui, |ui| {
                    if theme::capsule_button(ui, Icon::Gear, "Settings", false).clicked() {
                        self.settings_draft = Some(self.settings.clone());
                    }
                });

                // Actions for the selected torrent.
                if let Some(row) = self
                    .selected
                    .and_then(|id| rows.iter().find(|r| r.id == id))
                {
                    let (id, paused) = (row.id, row.is_paused());
                    let (name, folder) = (row.name.clone(), row.output_folder.clone());
                    theme::capsule(ui, |ui| {
                        if theme::capsule_button(ui, Icon::Close, "Remove…", false).clicked() {
                            self.confirm_remove = Some(ConfirmRemove {
                                id,
                                name: name.clone(),
                                delete_files: false,
                            });
                        }
                        if theme::capsule_button(ui, Icon::Folder, "Show in folder", false)
                            .clicked()
                        {
                            open_in_file_manager(&folder);
                        }
                        if paused {
                            if theme::capsule_button(ui, Icon::Play, "Resume", false).clicked() {
                                self.engine.resume(id);
                            }
                        } else if theme::capsule_button(ui, Icon::Pause, "Pause", false).clicked() {
                            self.engine.pause(id);
                        }
                    });
                }

                theme::capsule(ui, |ui| {
                    let grid = self.settings.grid_view;
                    if theme::capsule_button(ui, Icon::List, "List", !grid).clicked() && grid {
                        self.settings.grid_view = false;
                        self.settings.save();
                    }
                    if theme::capsule_button(ui, Icon::Grid, "Icons", grid).clicked() && !grid {
                        self.settings.grid_view = true;
                        self.settings.save();
                    }
                });

                theme::capsule(ui, |ui| {
                    if theme::capsule_button(ui, Icon::Folder, "Open .torrent files…", false)
                        .clicked()
                    {
                        self.open_torrent_files();
                    }
                    let has_text = !self.magnet_input.trim().is_empty();
                    let input = egui::TextEdit::singleline(&mut self.magnet_input)
                        .hint_text("Paste magnet link…")
                        .desired_width(ui.available_width().clamp(120.0, 300.0))
                        .margin(Margin::symmetric(10, 4))
                        .align(Align2::LEFT_CENTER)
                        .frame(egui::Frame::NONE);
                    let resp = ui.add(input);
                    if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        self.submit_magnet();
                    }
                    if has_text && theme::capsule_button(ui, Icon::Down, "Add", false).clicked() {
                        self.submit_magnet();
                    }
                });
                let _ = p;
            });
        });
    }

    // ------------------------------------------------------------- sidebar

    fn sidebar(&mut self, ui: &mut egui::Ui, rows: &[TorrentRow]) {
        egui::Panel::bottom("sidebar_stats")
            .frame(egui::Frame::NONE)
            .show_separator_line(false)
            .resizable(false)
            .show(ui, |ui| self.sidebar_stats(ui, rows));
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| self.sidebar_filters(ui, rows));
    }

    fn sidebar_section(ui: &mut egui::Ui, p: &Palette, text: &str) {
        ui.add_space(6.0);
        ui.label(RichText::new(text).size(11.5).strong().color(p.tertiary));
        ui.add_space(2.0);
    }

    /// One Finder-style sidebar row. Returns true if clicked.
    fn sidebar_row(
        ui: &mut egui::Ui,
        p: &Palette,
        icon: Icon,
        label: &str,
        trailing: Option<String>,
        selected: bool,
    ) -> bool {
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::click());
        if selected {
            ui.painter()
                .rect_filled(rect, CornerRadius::same(7), p.select);
        } else if resp.hovered() {
            ui.painter()
                .rect_filled(rect, CornerRadius::same(7), p.select.gamma_multiply(0.5));
        }
        let icon_rect = egui::Rect::from_center_size(
            rect.left_center() + vec2(15.0, 0.0),
            egui::Vec2::splat(15.0),
        );
        theme::paint_icon(
            ui,
            icon_rect,
            icon,
            if selected { p.text } else { p.secondary },
        );
        ui.painter().text(
            rect.left_center() + vec2(32.0, 0.0),
            Align2::LEFT_CENTER,
            label,
            FontId::proportional(13.0),
            p.text,
        );
        if let Some(t) = trailing {
            ui.painter().text(
                rect.right_center() - vec2(8.0, 0.0),
                Align2::RIGHT_CENTER,
                t,
                FontId::proportional(12.0),
                p.secondary,
            );
        }
        resp.clicked()
    }

    fn sidebar_filters(&mut self, ui: &mut egui::Ui, rows: &[TorrentRow]) {
        let p = theme::palette(ui);
        // macOS draws the window buttons over the top of the sidebar.
        if cfg!(target_os = "macos") {
            ui.add_space(26.0);
        }
        Self::sidebar_section(ui, &p, "Library");
        let count = |f: Filter| rows.iter().filter(|r| f.matches(r)).count();
        let mut items = vec![
            (Filter::All, Icon::List, "All Torrents"),
            (Filter::Downloading, Icon::Down, "Downloading"),
            (Filter::Seeding, Icon::Up, "Seeding"),
            (Filter::Paused, Icon::Pause, "Paused"),
        ];
        if count(Filter::Errors) > 0 {
            items.push((Filter::Errors, Icon::Warning, "Errors"));
        }
        for (filter, icon, label) in items {
            let n = count(filter);
            let trailing = (n > 0).then(|| n.to_string());
            if Self::sidebar_row(ui, &p, icon, label, trailing, self.filter == filter) {
                self.filter = filter;
            }
        }
    }

    /// Live throughput at the bottom of the sidebar.
    fn sidebar_stats(&mut self, ui: &mut egui::Ui, rows: &[TorrentRow]) {
        let p = theme::palette(ui);
        let (down, up) = Self::totals(rows);
        Self::sidebar_section(ui, &p, "Transfers");
        Self::sidebar_row(
            ui,
            &p,
            Icon::Down,
            "Download",
            Some(format::speed_bps(down)),
            false,
        );
        let hist: Vec<f64> = self.down_history.iter().copied().collect();
        ui.add_space(2.0);
        theme::sparkline(ui, &hist, p.accent, 30.0);
        ui.add_space(4.0);
        Self::sidebar_row(
            ui,
            &p,
            Icon::Up,
            "Upload",
            Some(format::speed_bps(up)),
            false,
        );

        let mut notes = Vec::new();
        if self.settings.download_limit_kib > 0 {
            notes.push(format!(
                "Download limited to {}/s",
                format::bytes(u64::from(self.settings.download_limit_kib) * 1000)
            ));
        }
        if let Some(c) = self.engine.upload_throttle() {
            notes.push(format!("Upload kept to {}/s", format::bytes(c)));
        } else if self.settings.upload_limit_kib > 0 {
            notes.push(format!(
                "Upload limited to {}/s",
                format::bytes(u64::from(self.settings.upload_limit_kib) * 1000)
            ));
        }
        for n in notes {
            ui.label(RichText::new(n).size(11.0).color(p.tertiary));
        }
        ui.add_space(6.0);
        if Self::sidebar_row(ui, &p, Icon::List, "Activity", None, self.show_log) {
            self.show_log = !self.show_log;
        }
    }

    // ---------------------------------------------------------------- list

    fn torrent_list(&mut self, ui: &mut egui::Ui, rows: &[TorrentRow]) {
        let p = theme::palette(ui);
        if rows.is_empty() {
            self.empty_state(ui);
            return;
        }
        let visible: Vec<&TorrentRow> = rows.iter().filter(|r| self.filter.matches(r)).collect();
        if visible.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.label(RichText::new("Nothing here").size(15.0).color(p.secondary));
            });
            return;
        }
        let grid = self.settings.grid_view;
        let resp = egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add_space(6.0);
                if grid {
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = vec2(6.0, 10.0);
                        for row in &visible {
                            self.torrent_tile(ui, row);
                        }
                    });
                } else {
                    for row in &visible {
                        self.torrent_card(ui, row);
                        ui.add_space(8.0);
                    }
                }
                // Click on empty space clears the selection.
                let rest = ui.allocate_response(ui.available_size(), Sense::click());
                if rest.clicked() {
                    self.selected = None;
                }
            });
        let _ = resp;
    }

    fn state_style(p: &Palette, row: &TorrentRow) -> (Color32, Icon) {
        if row.stats.error.is_some() {
            (p.red, Icon::Warning)
        } else if row.queued {
            (Color32::from_rgb(94, 92, 230), Icon::Clock)
        } else if row.is_paused() {
            (Color32::from_rgb(142, 142, 147), Icon::Pause)
        } else if row.stats.finished {
            (p.green, Icon::Check)
        } else {
            (p.accent, Icon::Down)
        }
    }

    /// Finder-style icon tile: app-icon square, name, progress and speed.
    fn torrent_tile(&mut self, ui: &mut egui::Ui, row: &TorrentRow) {
        let p = theme::palette(ui);
        let size = vec2(132.0, 150.0);
        let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
        let selected = self.selected == Some(row.id);
        if selected {
            ui.painter()
                .rect_filled(rect, CornerRadius::same(12), p.select);
        } else if resp.hovered() {
            ui.painter()
                .rect_filled(rect, CornerRadius::same(12), p.select.gamma_multiply(0.45));
        }
        let (color, glyph) = Self::state_style(&p, row);
        let icon_rect = egui::Rect::from_center_size(
            rect.center_top() + vec2(0.0, 40.0),
            egui::Vec2::splat(56.0),
        );
        theme::app_icon(ui, icon_rect, color, glyph);

        // Name, up to two lines, centred.
        let mut job = egui::text::LayoutJob::simple(
            row.name.clone(),
            FontId::proportional(12.5),
            p.text,
            rect.width() - 16.0,
        );
        job.wrap.max_rows = 2;
        job.wrap.break_anywhere = false;
        job.halign = Align::Center;
        let galley = ui.painter().layout_job(job);
        let name_top = icon_rect.bottom() + 8.0;
        ui.painter().galley(
            egui::pos2(rect.center().x, name_top),
            galley.clone(),
            p.text,
        );

        // Progress bar and status line.
        let bar_y = name_top + galley.size().y + 6.0;
        let bar = egui::Rect::from_min_size(
            egui::pos2(rect.left() + 22.0, bar_y),
            vec2(rect.width() - 44.0, 4.0),
        );
        if !row.stats.finished {
            ui.painter()
                .rect_filled(bar, CornerRadius::same(2), p.track);
            let mut fill = bar;
            fill.set_width(bar.width() * row.progress().clamp(0.0, 1.0));
            if fill.width() > 0.5 {
                ui.painter().rect_filled(fill, CornerRadius::same(2), color);
            }
        }
        let status = if row.stats.error.is_some() {
            "Error".to_string()
        } else if row.queued {
            format!("Queued · {:.0}%", row.progress() * 100.0)
        } else if row.is_paused() {
            format!("Paused · {:.0}%", row.progress() * 100.0)
        } else if row.stats.finished {
            match row.rates.up_bps {
                b if b >= 1000.0 => format!("↑ {}", format::speed_bps(b)),
                _ => format::bytes(row.stats.total_bytes),
            }
        } else {
            format!(
                "{:.0}% · {}",
                row.progress() * 100.0,
                format::speed_bps(row.rates.down_bps)
            )
        };
        ui.painter().text(
            egui::pos2(rect.center().x, bar_y + 8.0),
            Align2::CENTER_TOP,
            status,
            FontId::proportional(11.0),
            p.secondary,
        );

        let resp = resp.on_hover_text(&row.name);
        if resp.clicked() {
            self.selected = Some(row.id);
        }
        if resp.double_clicked() {
            open_in_file_manager(&row.output_folder);
        }
        let (id, paused, name, folder) = (
            row.id,
            row.is_paused(),
            row.name.clone(),
            row.output_folder.clone(),
        );
        resp.context_menu(|ui| {
            self.selected = Some(id);
            if paused {
                if ui.button("Resume").clicked() {
                    self.engine.resume(id);
                    ui.close();
                }
            } else if ui.button("Pause").clicked() {
                self.engine.pause(id);
                ui.close();
            }
            if ui.button("Show in Folder").clicked() {
                open_in_file_manager(&folder);
                ui.close();
            }
            ui.separator();
            if ui.button("Remove…").clicked() {
                self.confirm_remove = Some(ConfirmRemove {
                    id,
                    name: name.clone(),
                    delete_files: false,
                });
                ui.close();
            }
        });
    }

    fn empty_state(&mut self, ui: &mut egui::Ui) {
        let p = theme::palette(ui);
        ui.vertical_centered(|ui| {
            ui.add_space((ui.available_height() / 2.0 - 110.0).max(20.0));
            let (rect, _) = ui.allocate_exact_size(egui::Vec2::splat(72.0), Sense::hover());
            ui.painter()
                .circle_filled(rect.center(), 36.0, p.accent.gamma_multiply(0.14));
            theme::paint_icon(ui, rect.shrink(20.0), Icon::Down, p.accent);
            ui.add_space(14.0);
            ui.label(RichText::new("No Torrents").size(20.0).strong());
            ui.add_space(4.0);
            ui.label(
                RichText::new(
                    "Add a .torrent file, paste a magnet link above,\nor drop .torrent files anywhere in this window.",
                )
                .color(p.secondary),
            );
            ui.add_space(14.0);
            if theme::primary_button(ui, "Add Torrent…").clicked() {
                self.open_torrent_files();
            }
        });
    }

    fn torrent_card(&mut self, ui: &mut egui::Ui, row: &TorrentRow) {
        let p = theme::palette(ui);
        let paused = row.is_paused();
        let errored = row.stats.error.is_some();
        let finished = row.stats.finished;
        let state_color = if errored {
            p.red
        } else if paused {
            p.tertiary
        } else if finished {
            p.green
        } else {
            p.accent
        };

        let selected = self.selected == Some(row.id);
        let frame = theme::card(&p).fill(if selected { p.select } else { p.card });
        let card = frame.show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if theme::icon_button(ui, Icon::Close, "Remove…").clicked() {
                        self.confirm_remove = Some(ConfirmRemove {
                            id: row.id,
                            name: row.name.clone(),
                            delete_files: false,
                        });
                    }
                    if theme::icon_button(ui, Icon::Folder, "Show in folder").clicked() {
                        open_in_file_manager(&row.output_folder);
                    }
                    if paused {
                        if theme::icon_button(ui, Icon::Play, "Resume").clicked() {
                            self.engine.resume(row.id);
                        }
                    } else if theme::icon_button(ui, Icon::Pause, "Pause").clicked() {
                        self.engine.pause(row.id);
                    }
                    ui.add_space(6.0);
                    // Headline speed for the current state.
                    if !paused && !errored {
                        let (text, color) = if finished {
                            (
                                format!("↑ {}", format::speed_bps(row.rates.up_bps)),
                                p.green,
                            )
                        } else {
                            (
                                format!("↓ {}", format::speed_bps(row.rates.down_bps)),
                                p.accent,
                            )
                        };
                        ui.label(RichText::new(text).size(13.5).strong().color(color));
                    }
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        ui.add(
                            egui::Label::new(RichText::new(&row.name).size(14.5).strong())
                                .truncate(),
                        )
                        .on_hover_text(&row.name);
                    });
                });
            });
            ui.add_space(2.0);
            theme::progress_bar(ui, row.progress(), state_color);
            ui.add_space(2.0);

            let mut parts: Vec<String> = Vec::new();
            let status = match &row.stats.error {
                Some(err) => format!("Error: {err}"),
                None => row.status_text(),
            };
            if finished {
                parts.push(format!(
                    "{} · {}",
                    status,
                    format::bytes(row.stats.total_bytes)
                ));
            } else {
                parts.push(format!(
                    "{} · {} of {} ({:.0}%)",
                    status,
                    format::bytes(row.stats.progress_bytes),
                    format::bytes(row.stats.total_bytes),
                    row.progress() * 100.0
                ));
            }
            if let Some(live) = &row.stats.live {
                let peers = &live.snapshot.peer_stats;
                parts.push(format!("{} of {} peers", peers.live, peers.seen));
                if !finished && row.rates.up_bps >= 1000.0 {
                    parts.push(format!("↑ {}", format::speed_bps(row.rates.up_bps)));
                }
            }
            if let Some(eta) = row.eta {
                parts.push(format!("{} left", format::duration(eta)));
            }
            if row.stats.uploaded_bytes > 0 {
                parts.push(format!(
                    "{} uploaded",
                    format::bytes(row.stats.uploaded_bytes)
                ));
            }
            let color = if errored { p.red } else { p.secondary };
            ui.add(
                egui::Label::new(RichText::new(parts.join("  ·  ")).size(12.0).color(color))
                    .truncate(),
            );
        });
        if card.response.interact(Sense::click()).clicked() {
            self.selected = Some(row.id);
        }
    }

    fn log_panel(&self, ui: &mut egui::Ui) {
        let p = theme::palette(ui);
        egui::ScrollArea::vertical()
            .stick_to_bottom(true)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for line in &self.log {
                    let color = if line.is_error { p.red } else { p.secondary };
                    ui.label(
                        RichText::new(&line.text)
                            .color(color)
                            .size(12.0)
                            .monospace(),
                    );
                }
            });
    }

    /// Recent message as a small floating toast at the bottom of the window.
    fn toast(&self, ctx: &egui::Context) {
        let Some(last) = self.log.last() else { return };
        if last.at.elapsed() > Duration::from_secs(6) || self.show_log {
            return;
        }
        egui::Area::new(egui::Id::unique("toast"))
            .anchor(Align2::CENTER_BOTTOM, [112.0, -18.0])
            .interactable(false)
            .show(ctx, |ui| {
                let p = theme::palette(ui);
                egui::Frame::new()
                    .fill(if last.is_error { p.red } else { p.card })
                    .corner_radius(CornerRadius::same(18))
                    .inner_margin(Margin::symmetric(16, 8))
                    .stroke(Stroke::new(1.0, p.separator))
                    .shadow(ui.visuals().popup_shadow)
                    .show(ui, |ui| {
                        let color = if last.is_error {
                            Color32::WHITE
                        } else {
                            p.text
                        };
                        ui.label(RichText::new(&last.text).size(12.5).color(color));
                    });
            });
        ctx.request_repaint_after(Duration::from_millis(500));
    }

    // -------------------------------------------------------------- sheets

    fn sheet_header(ui: &mut egui::Ui, title: &str, subtitle: Option<&str>) {
        let p = theme::palette(ui);
        ui.label(RichText::new(title).size(17.0).strong());
        if let Some(s) = subtitle {
            ui.add(egui::Label::new(RichText::new(s).color(p.secondary)).truncate());
        }
        ui.add_space(10.0);
    }

    fn add_dialog(&mut self, ctx: &egui::Context) {
        let Some(dialog) = self.add_queue.first_mut() else {
            return;
        };
        let mut start = false;
        let mut cancel = false;

        let modal = egui::Modal::new(egui::Id::unique("add_torrent_modal")).show(ctx, |ui| {
            let p = theme::palette(ui);
            ui.set_width(540.0_f32.min(ctx.content_rect().width() - 60.0));
            Self::sheet_header(ui, "Add Torrent", Some(&dialog.pending.name));

            settings_group(ui, &p, |ui| {
                setting_row(ui, "Save to", |ui| {
                    if ui.button("Change…").clicked()
                        && let Some(dir) = rfd::FileDialog::new()
                            .set_directory(&dialog.folder)
                            .pick_folder()
                    {
                        dialog.folder = dir;
                    }
                    path_label(ui, &dialog.folder);
                });
                if let Some(sub) = &dialog.pending.sub_folder {
                    hint(ui, &p, &format!("in a folder named “{}”", sub.display()));
                }
            });
            ui.add_space(10.0);

            let files = &mut dialog.pending.files;
            let selected_size: u64 = files.iter().filter(|f| f.selected).map(|f| f.size).sum();
            let total_size: u64 = files.iter().map(|f| f.size).sum();
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!(
                        "{} {} · {} of {} selected",
                        files.len(),
                        if files.len() == 1 { "file" } else { "files" },
                        format::bytes(selected_size),
                        format::bytes(total_size)
                    ))
                    .color(p.secondary),
                );
                if files.len() > 1 {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.small_button("None").clicked() {
                            files.iter_mut().for_each(|f| f.selected = false);
                        }
                        if ui.small_button("All").clicked() {
                            files.iter_mut().for_each(|f| f.selected = true);
                        }
                    });
                }
            });
            ui.add_space(4.0);
            settings_group(ui, &p, |ui| {
                egui::ScrollArea::vertical()
                    .max_height(240.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for f in files.iter_mut() {
                            ui.horizontal(|ui| {
                                ui.checkbox(&mut f.selected, "");
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    ui.label(
                                        RichText::new(format::bytes(f.size)).color(p.secondary),
                                    );
                                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                        ui.add(egui::Label::new(&f.path).truncate());
                                    });
                                });
                            });
                        }
                    });
            });

            ui.add_space(14.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let any = files.iter().any(|f| f.selected);
                ui.add_enabled_ui(any, |ui| {
                    if theme::primary_button(ui, "Download").clicked() {
                        start = true;
                    }
                });
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
            self.add_queue.remove(0);
        }
    }

    fn remove_dialog(&mut self, ctx: &egui::Context) {
        let Some(confirm) = &mut self.confirm_remove else {
            return;
        };
        let mut done: Option<bool> = None; // Some(true) = remove, Some(false) = cancel

        let modal = egui::Modal::new(egui::Id::unique("remove_torrent_modal")).show(ctx, |ui| {
            let p = theme::palette(ui);
            ui.set_width(420.0);
            Self::sheet_header(ui, "Remove Torrent?", Some(&confirm.name));
            settings_group(ui, &p, |ui| {
                setting_row(ui, "Also delete downloaded files", |ui| {
                    theme::toggle(ui, &mut confirm.delete_files);
                });
            });
            ui.add_space(14.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let clicked = if confirm.delete_files {
                    theme::danger_button(ui, "Remove & Delete Files")
                } else {
                    theme::primary_button(ui, "Remove")
                };
                if clicked.clicked() {
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

    fn settings_window(&mut self, ctx: &egui::Context) {
        let Some(draft) = &mut self.settings_draft else {
            return;
        };
        let mut save = false;
        let mut cancel = false;
        let running = &self.running_settings;

        let modal = egui::Modal::new(egui::Id::unique("settings_modal")).show(ctx, |ui| {
            let p = theme::palette(ui);
            ui.set_width(500.0_f32.min(ctx.content_rect().width() - 60.0));
            Self::sheet_header(ui, "Settings", None);

            egui::ScrollArea::vertical()
                .max_height((ctx.content_rect().height() - 180.0).max(200.0))
                .show(ui, |ui| {
                    group_title(ui, &p, "SPEED");
                    settings_group(ui, &p, |ui| {
                        limit_row(ui, &p, "Download limit", &mut draft.download_limit_kib);
                        separator(ui, &p);
                        limit_row(ui, &p, "Upload limit", &mut draft.upload_limit_kib);
                        separator(ui, &p);
                        setting_row(ui, "Your upload speed", |ui| {
                            ui.add(
                                egui::DragValue::new(&mut draft.upload_capacity_mbps)
                                    .range(0.0..=10_000.0)
                                    .speed(1.0)
                                    .suffix(" Mbit/s"),
                            );
                        });
                        hint(
                            ui,
                            &p,
                            if draft.upload_capacity_mbps > 0.0 {
                                "While downloading, upload is kept to 80% of this so it never clogs your connection."
                            } else {
                                "Optional (0 = not set). Uploading freely earns faster downloads from other peers. Set your plan's upload speed only if uploading slows your internet."
                            },
                        );
                        separator(ui, &p);
                        setting_row(ui, "Max peers per torrent", |ui| {
                            ui.add(
                                egui::DragValue::new(&mut draft.max_peers_per_torrent)
                                    .range(10..=2000),
                            );
                        });
                    });

                    ui.add_space(14.0);
                    group_title(ui, &p, "NETWORK");
                    settings_group(ui, &p, |ui| {
                        setting_row(ui, "Listening port", |ui| {
                            ui.add(
                                egui::DragValue::new(&mut draft.listen_port).range(1024..=65535),
                            );
                        });
                        separator(ui, &p);
                        setting_row(ui, "UPnP port forwarding", |ui| {
                            theme::toggle(ui, &mut draft.enable_upnp);
                        });
                        separator(ui, &p);
                        setting_row(ui, "uTP connections", |ui| {
                            theme::toggle(ui, &mut draft.enable_utp);
                        });
                        separator(ui, &p);
                        setting_row(ui, "Extra public trackers", |ui| {
                            theme::toggle(ui, &mut draft.extra_trackers);
                        });
                        separator(ui, &p);
                        setting_row(ui, "Encryption", |ui| {
                            use crate::settings::EncryptionMode as E;
                            theme::capsule(ui, |ui| {
                                for (mode, label) in
                                    [(E::Off, "Off"), (E::Require, "Require"), (E::Prefer, "Prefer")]
                                {
                                    let on = draft.encryption == mode;
                                    if ui
                                        .add(egui::Button::selectable(on, label).corner_radius(13))
                                        .clicked()
                                    {
                                        draft.encryption = mode;
                                    }
                                }
                            });
                        });
                        hint(
                            ui,
                            &p,
                            match draft.encryption {
                                crate::settings::EncryptionMode::Prefer => "Encrypts traffic when the peer supports it: avoids ISP throttling and reaches peers that require encryption.",
                                crate::settings::EncryptionMode::Require => "Only encrypted peers: most private, but skips peers without encryption.",
                                crate::settings::EncryptionMode::Off => "Plain BitTorrent only: can't reach peers that require encryption.",
                            },
                        );
                    });
                    ui.add_space(4.0);
                    hint(ui, &p, "Network changes take effect after restarting Rustorrent.");

                    ui.add_space(14.0);
                    group_title(ui, &p, "QUEUE & SEEDING");
                    settings_group(ui, &p, |ui| {
                        setting_row(ui, "Downloads at once", |ui| {
                            let mut unlimited = draft.max_active_downloads == 0;
                            if !unlimited {
                                ui.add(
                                    egui::DragValue::new(&mut draft.max_active_downloads)
                                        .range(1..=50),
                                );
                            } else {
                                ui.label(RichText::new("Unlimited").color(p.secondary));
                            }
                            let mut limited = !unlimited;
                            if theme::toggle(ui, &mut limited).changed() {
                                unlimited = !limited;
                                draft.max_active_downloads = if unlimited { 0 } else { 3 };
                            }
                        });
                        hint(ui, &p, "Extra torrents wait in a queue. Fewer at once means each finishes sooner.");
                        separator(ui, &p);
                        setting_row(ui, "Stop seeding at ratio", |ui| {
                            let mut on = draft.seed_ratio_limit > 0.0;
                            if on {
                                ui.add(
                                    egui::DragValue::new(&mut draft.seed_ratio_limit)
                                        .range(0.1..=100.0)
                                        .speed(0.05)
                                        .max_decimals(2),
                                );
                            }
                            if theme::toggle(ui, &mut on).changed() {
                                draft.seed_ratio_limit = if on { 2.0 } else { 0.0 };
                            }
                        });
                        separator(ui, &p);
                        setting_row(ui, "Stop seeding after", |ui| {
                            let mut on = draft.seed_time_limit_min > 0;
                            if on {
                                ui.add(
                                    egui::DragValue::new(&mut draft.seed_time_limit_min)
                                        .range(1..=100_000)
                                        .suffix(" min"),
                                );
                            }
                            if theme::toggle(ui, &mut on).changed() {
                                draft.seed_time_limit_min = if on { 60 } else { 0 };
                            }
                        });
                        separator(ui, &p);
                        setting_row(ui, "Notify when downloads finish", |ui| {
                            theme::toggle(ui, &mut draft.notify_on_complete);
                        });
                    });
                });

            ui.add_space(14.0);
            ui.horizontal(|ui| {
                if ui.button("Reset to Defaults").clicked() {
                    let dir = draft.download_dir.clone();
                    *draft = Settings {
                        download_dir: dir,
                        ..Settings::default()
                    };
                }
                if draft.needs_restart_compared_to(running) {
                    ui.label(RichText::new("Restart required").size(12.0).color(p.orange));
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if theme::primary_button(ui, "Save").clicked() {
                        save = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
        });

        if save {
            let mut new = self.settings_draft.take().expect("checked above");
            // The save folder is chosen per torrent; keep whatever is current.
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
                    "Settings saved".into()
                },
                false,
            );
        } else if cancel || modal.should_close() {
            self.settings_draft = None;
        }
    }

    fn drop_overlay(ctx: &egui::Context) {
        if ctx.input(|i| i.raw.hovered_files.is_empty()) {
            return;
        }
        let p = if ctx.global_style().visuals.dark_mode {
            theme::DARK
        } else {
            theme::LIGHT
        };
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::unique("drop_overlay"),
        ));
        let rect = ctx.content_rect().shrink(16.0);
        painter.rect_filled(rect, CornerRadius::same(16), p.bg.gamma_multiply(0.92));
        painter.rect_stroke(
            rect,
            CornerRadius::same(16),
            Stroke::new(2.0, p.accent),
            egui::StrokeKind::Inside,
        );
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "Drop to add torrents",
            FontId::proportional(22.0),
            p.accent,
        );
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
        if self
            .history_sampled
            .is_none_or(|t| t.elapsed() >= Duration::from_secs(1))
        {
            let (down, _) = Self::totals(&self.rows);
            if self.down_history.len() == SPEED_HISTORY {
                self.down_history.pop_front();
            }
            self.down_history.push_back(down);
            self.history_sampled = Some(Instant::now());
        }
        let rows = std::mem::take(&mut self.rows);
        if self
            .selected
            .is_some_and(|id| !rows.iter().any(|r| r.id == id))
        {
            self.selected = None;
        }
        let p: Palette = theme::palette(ui);

        // Floating, rounded sidebar inset from the window edge (Finder style).
        egui::Panel::left("sidebar")
            .exact_size(224.0)
            .resizable(false)
            .show_separator_line(false)
            .frame(egui::Frame::new().fill(p.bg).inner_margin(Margin {
                left: 8,
                right: 0,
                top: 8,
                bottom: 8,
            }))
            .show(ui, |ui| {
                egui::Frame::new()
                    .fill(p.sidebar)
                    .corner_radius(CornerRadius::same(14))
                    .stroke(Stroke::new(1.0, p.separator.gamma_multiply(0.7)))
                    .shadow(egui::Shadow {
                        offset: [0, 2],
                        blur: 12,
                        spread: 0,
                        color: p.shadow.gamma_multiply(0.7),
                    })
                    .inner_margin(Margin::symmetric(10, 10))
                    .show(ui, |ui| {
                        ui.set_min_size(ui.available_size());
                        self.sidebar(ui, &rows);
                    });
            });

        if self.show_log {
            egui::Panel::bottom("log")
                .resizable(true)
                .default_size(150.0)
                .frame(
                    egui::Frame::new()
                        .fill(p.card)
                        .inner_margin(Margin::symmetric(16, 10))
                        .stroke(Stroke::new(1.0, p.separator)),
                )
                .show(ui, |ui| self.log_panel(ui));
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(p.bg).inner_margin(Margin {
                left: 18,
                right: 14,
                top: 10,
                bottom: 8,
            }))
            .show(ui, |ui| {
                self.toolbar(ui, &rows);
                ui.add_space(6.0);
                self.torrent_list(ui, &rows);
            });

        Self::drop_overlay(&ctx);
        self.toast(&ctx);
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

// ---------------------------------------------------------------- helpers

fn group_title(ui: &mut egui::Ui, p: &Palette, text: &str) {
    ui.label(RichText::new(text).size(11.0).strong().color(p.tertiary));
    ui.add_space(2.0);
}

/// A rounded "inset grouped" box like macOS System Settings.
fn settings_group(ui: &mut egui::Ui, p: &Palette, add: impl FnOnce(&mut egui::Ui)) {
    let fill = if ui.visuals().dark_mode {
        theme::DARK.control.gamma_multiply(0.55)
    } else {
        theme::LIGHT.bg
    };
    egui::Frame::new()
        .fill(fill)
        .corner_radius(CornerRadius::same(10))
        .stroke(Stroke::new(1.0, p.separator))
        .inner_margin(Margin::symmetric(14, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui);
        });
}

/// Label on the left, controls right-aligned.
fn setting_row(ui: &mut egui::Ui, label: &str, add: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.set_min_height(28.0);
        ui.label(label);
        ui.with_layout(Layout::right_to_left(Align::Center), add);
    });
}

fn separator(ui: &mut egui::Ui, p: &Palette) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().hline(
        rect.x_range(),
        rect.center().y,
        Stroke::new(1.0, p.separator),
    );
}

fn hint(ui: &mut egui::Ui, p: &Palette, text: &str) {
    ui.label(RichText::new(text).size(11.5).color(p.secondary));
}

/// "Download limit   [toggle] [ 500 KB/s ]". 0 means unlimited.
fn limit_row(ui: &mut egui::Ui, p: &Palette, label: &str, kb: &mut u32) {
    setting_row(ui, label, |ui| {
        let mut limited = *kb > 0;
        if limited {
            ui.add(
                egui::DragValue::new(kb)
                    .range(1..=4_000_000)
                    .speed(16.0)
                    .suffix(" KB/s"),
            );
        } else {
            ui.label(RichText::new("Unlimited").color(p.secondary));
        }
        if theme::toggle(ui, &mut limited).changed() {
            *kb = if limited { 1000 } else { 0 };
        }
    });
}

/// A path that truncates (with the full path on hover).
fn path_label(ui: &mut egui::Ui, path: &Path) {
    let text = path.display().to_string();
    ui.add(egui::Label::new(RichText::new(&text).monospace().size(12.0)).truncate())
        .on_hover_text(&text);
}

fn open_in_file_manager(path: &Path) {
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
