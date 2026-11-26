//! `rustorrent --headless <torrent|magnet> [--out DIR]`
//!
//! Downloads one torrent with exactly the same engine and settings as the GUI,
//! printing progress to stdout, then exits. Handy for scripting and for
//! benchmarking the engine.

use std::path::PathBuf;
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

use eframe::egui;

use crate::engine::{Engine, Event, TorrentSource};
use crate::format;
use crate::settings::{self, Settings};

pub fn run(args: &[String]) -> anyhow::Result<()> {
    let mut source = None;
    let mut out = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--out" => out = it.next().map(PathBuf::from),
            _ => source = Some(a.clone()),
        }
    }
    let source = source.ok_or_else(|| {
        anyhow::anyhow!("usage: rustorrent --headless <file.torrent|magnet> [--out DIR]")
    })?;

    let mut settings = Settings::load();
    if let Some(dir) = out {
        settings.download_dir = dir;
    }
    let (tx, rx) = channel();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let engine = Engine::new(
        rt,
        &settings,
        settings::data_dir().join("headless"),
        tx,
        egui::Context::default(),
    )?;

    engine.fetch_metadata(TorrentSource::from_arg(&source));
    let started = Instant::now();
    let mut id = None;
    loop {
        while let Ok(ev) = rx.try_recv() {
            match ev {
                Event::Info(s) => println!("{s}"),
                Event::Error(s) => anyhow::bail!(s),
                Event::MetadataReady(p) => {
                    println!(
                        "Downloading \"{}\" to {}",
                        p.name,
                        settings.download_dir.display()
                    );
                    engine.start_download(p, settings.download_dir.clone());
                }
            }
        }
        let rows = engine.torrents();
        if id.is_none() {
            id = rows.last().map(|r| r.id);
        }
        if let Some(row) = id.and_then(|id| rows.into_iter().find(|r| r.id == id)) {
            let peers = row
                .stats
                .live
                .as_ref()
                .map_or(0, |l| l.snapshot.peer_stats.live);
            println!(
                "{:5.1}%  {:>10}  down {:>11}  up {:>11}  peers {peers}",
                row.progress() * 100.0,
                format::bytes(row.stats.progress_bytes),
                format::speed_bps(row.rates.down_bps),
                format::speed_bps(row.rates.up_bps),
            );
            if row.stats.finished {
                let secs = started.elapsed().as_secs_f64();
                println!(
                    "Done: {} in {secs:.1}s (average {}/s)",
                    format::bytes(row.stats.total_bytes),
                    format::bytes((row.stats.total_bytes as f64 / secs) as u64)
                );
                break;
            }
            if let Some(err) = &row.stats.error {
                anyhow::bail!("torrent error: {err}");
            }
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    engine.shutdown();
    Ok(())
}
