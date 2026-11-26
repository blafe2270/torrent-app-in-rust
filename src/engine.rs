//! Thin wrapper around the librqbit `Session` that the GUI talks to.
//!
//! All network / disk work happens on a Tokio runtime. Results that the UI
//! needs to know about are sent back over an `std::sync::mpsc` channel and
//! the egui context is poked so the window repaints.

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::Sender;

use anyhow::Context as _;
use std::collections::HashMap;
use std::num::NonZeroU32;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use librqbit::limits::LimitsConfig;
use librqbit::{
    AddTorrent, AddTorrentOptions, AddTorrentResponse, ConnectionOptions, ListenerMode,
    ListenerOptions, PeerConnectionOptions, Session, SessionOptions, SessionPersistenceConfig,
    TorrentStats, TorrentStatsState,
};

use crate::settings::{EncryptionMode, PUBLIC_TRACKERS, Settings};
use crate::speed::{self, Rates, SpeedTracker};
use crate::upload_policy::{self, WhileDownloading};
use librqbit::mse::EncryptionPolicy;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}
use eframe::egui;
use tokio::runtime::Runtime;

/// Messages sent from background tasks to the UI thread.
pub enum Event {
    Info(String),
    Error(String),
    /// Metadata for a new torrent was resolved; the UI should let the user
    /// pick files / a folder and then call [`Engine::start_download`].
    MetadataReady(PendingTorrent),
}

/// A torrent whose metadata is known but which has not been added yet.
pub struct PendingTorrent {
    pub name: String,
    pub files: Vec<PendingFile>,
    pub torrent_bytes: bytes::Bytes,
    pub seen_peers: Vec<SocketAddr>,
    /// Sub-folder used for multi-file torrents (mirrors librqbit's default).
    pub sub_folder: Option<PathBuf>,
}

pub struct PendingFile {
    pub path: String,
    pub size: u64,
    pub selected: bool,
}

/// A snapshot of one torrent, cheap enough to rebuild every frame.
pub struct TorrentRow {
    pub id: usize,
    pub name: String,
    pub output_folder: PathBuf,
    pub stats: TorrentStats,
    /// Download/upload speed averaged over a few seconds (bytes/s).
    pub rates: Rates,
    /// Time left at the averaged download speed.
    pub eta: Option<Duration>,
    /// Waiting for a free download slot (see the download queue).
    pub queued: bool,
}

impl TorrentRow {
    pub fn is_paused(&self) -> bool {
        matches!(
            self.stats.state,
            TorrentStatsState::Paused | TorrentStatsState::Initializing { paused: true }
        )
    }

    pub fn status_text(&self) -> String {
        if let Some(err) = &self.stats.error {
            return format!("Error: {err}");
        }
        if self.queued {
            return "Queued".into();
        }
        match self.stats.state {
            TorrentStatsState::Initializing { .. } => "Checking…".into(),
            TorrentStatsState::Paused => "Paused".into(),
            TorrentStatsState::Error => "Error".into(),
            TorrentStatsState::Live if self.stats.finished => "Seeding".into(),
            TorrentStatsState::Live => "Downloading".into(),
        }
    }

    pub fn progress(&self) -> f32 {
        if self.stats.total_bytes == 0 {
            return 0.0;
        }
        (self.stats.progress_bytes as f64 / self.stats.total_bytes as f64) as f32
    }
}

/// Everything the inspector panel shows for one torrent.
pub struct TorrentDetails {
    pub info_hash: String,
    pub save_path: PathBuf,
    pub piece_length: u32,
    pub pieces: u32,
    pub files: Vec<FileDetail>,
    pub peers: Vec<PeerDetail>,
    pub trackers: Vec<String>,
    /// Fraction downloaded per range of the torrent (empty when not running).
    pub piece_map: Vec<f32>,
    pub web_seeds: usize,
}

pub struct FileDetail {
    /// Index in the torrent's file list (for `set_file_included`).
    pub index: usize,
    pub path: String,
    pub size: u64,
    pub done: u64,
    pub included: bool,
}

pub struct PeerDetail {
    pub addr: SocketAddr,
    pub client: String,
    pub utp: bool,
    pub down_bps: f64,
    pub up_bps: f64,
    pub have: f32,
}

pub struct Engine {
    rt: Runtime,
    session: Arc<Session>,
    max_peers_per_torrent: usize,
    events: Sender<Event>,
    repaint: egui::Context,
    speeds: Mutex<HashMap<usize, SpeedTracker>>,
    upload: Arc<Mutex<UploadState>>,
    gov: Arc<Mutex<Governor>>,
    peer_speeds: Mutex<HashMap<(usize, SocketAddr), SpeedTracker>>,
}

/// Queue, seeding limits and completion tracking (see `spawn_governor`).
#[derive(Default)]
struct Governor {
    /// Max torrents downloading at once (0 = unlimited).
    max_active: usize,
    /// Pause seeding at this upload/size ratio (0 = never).
    ratio_limit: f32,
    /// Pause seeding after this many minutes (0 = never).
    seed_minutes: u32,
    notify: bool,
    /// Torrents waiting for a slot, by id (in add order).
    queued: std::collections::BTreeSet<usize>,
    queue_file: PathBuf,
    /// When we first saw each torrent complete (this session).
    finished_at: HashMap<usize, Instant>,
    /// Seen downloading this session (so we notify on completion).
    was_downloading: std::collections::HashSet<usize>,
    /// Paused by a seeding limit; not re-paused if the user resumes it.
    seed_limited: std::collections::HashSet<usize>,
}

impl Governor {
    fn save_queue(&self, session: &Session) {
        let hashes: Vec<String> = session.with_torrents(|it| {
            it.filter(|(id, _)| self.queued.contains(id))
                .map(|(_, t)| t.info_hash().as_string())
                .collect()
        });
        if let Ok(json) = serde_json::to_string(&hashes) {
            let _ = std::fs::write(&self.queue_file, json);
        }
    }

    fn load_queue(&mut self, session: &Session) {
        let hashes: Vec<String> = std::fs::read_to_string(&self.queue_file)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        self.queued = session.with_torrents(|it| {
            it.filter(|(_, t)| hashes.contains(&t.info_hash().as_string()))
                .map(|(id, _)| id)
                .collect()
        });
    }
}

fn is_paused_state(s: &TorrentStats) -> bool {
    matches!(
        s.state,
        TorrentStatsState::Paused | TorrentStatsState::Initializing { paused: true }
    )
}

/// Torrents currently using a download slot.
fn count_active(session: &Session) -> usize {
    session.with_torrents(|it| {
        it.filter(|(_, t)| {
            let s = t.stats();
            !is_paused_state(&s) && !s.finished && s.error.is_none()
        })
        .count()
    })
}

/// Inputs and current output of the upload policy (see `upload_policy`).
struct UploadState {
    /// User's own upload limit, bytes/s.
    user_limit: Option<u64>,
    mode: WhileDownloading,
    /// Limit currently applied to the session.
    applied: Option<u64>,
    /// Whether the cap is currently active because something is downloading.
    throttling: bool,
    /// Force re-applying on the next tick (settings changed).
    dirty: bool,
}

impl Engine {
    pub fn new(
        rt: Runtime,
        settings: &Settings,
        data_dir: PathBuf,
        events: Sender<Event>,
        repaint: egui::Context,
    ) -> anyhow::Result<Self> {
        // Lots of peers means lots of sockets; lift the per-process limit.
        if let Err(e) = librqbit::try_increase_nofile_limit() {
            tracing::debug!("could not raise open-file limit: {e:#}");
        }

        // Try progressively more conservative network setups so the app
        // still starts on machines without IPv6, with the port taken, etc.
        let port = settings.listen_port;
        let attempts = [
            Some(Listen {
                ipv4_only: false,
                port,
            }),
            Some(Listen {
                ipv4_only: true,
                port,
            }),
            Some(Listen {
                ipv4_only: true,
                port: 0,
            }),
            None,
        ];
        let session = rt.block_on(async {
            let mut last_err = None;
            for listen in attempts {
                match create_session(settings, data_dir.clone(), listen).await {
                    Ok(s) => return Ok(s),
                    Err(e) => {
                        tracing::warn!("session start with {listen:?} failed: {e:#}");
                        last_err = Some(e);
                    }
                }
            }
            Err(last_err.expect("at least one attempt"))
        })?;

        let engine = Self {
            rt,
            session,
            max_peers_per_torrent: settings.max_peers_per_torrent,
            events,
            repaint,
            speeds: Mutex::new(HashMap::new()),
            upload: Arc::new(Mutex::new(UploadState {
                user_limit: None,
                mode: WhileDownloading::Off,
                applied: None,
                throttling: false,
                dirty: true,
            })),
            peer_speeds: Mutex::new(HashMap::new()),
            gov: Arc::new(Mutex::new(Governor {
                queue_file: data_dir.join("queue.json"),
                ..Default::default()
            })),
        };
        lock(&engine.gov).load_queue(&engine.session);
        engine.apply_live_settings(settings);
        engine.spawn_upload_governor();
        engine.spawn_governor();
        Ok(engine)
    }

    /// Apply settings that can change while running (speed limits and the
    /// peer limit used for newly-added torrents).
    pub fn apply_live_settings(&self, settings: &Settings) {
        self.session
            .ratelimits
            .set_download_bps(kib_to_bps(settings.download_limit_kib));
        let mut up = lock(&self.upload);
        up.user_limit =
            (settings.upload_limit_kib > 0).then(|| u64::from(settings.upload_limit_kib) * 1000);
        up.mode = settings.upload_policy();
        up.dirty = true;
        drop(up);
        let mut g = lock(&self.gov);
        g.max_active = settings.max_active_downloads;
        g.ratio_limit = settings.seed_ratio_limit;
        g.seed_minutes = settings.seed_time_limit_min;
        g.notify = settings.notify_on_complete;
    }

    /// Once a second: start queued torrents when slots free up, apply
    /// seeding limits and announce finished downloads.
    fn spawn_governor(&self) {
        let session = Arc::downgrade(&self.session);
        let gov = self.gov.clone();
        let events = self.events.clone();
        let repaint = self.repaint.clone();
        self.rt.spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(1));
            loop {
                tick.tick().await;
                let Some(session) = session.upgrade() else {
                    return;
                };
                let handles: Vec<(usize, Arc<librqbit::ManagedTorrent>)> =
                    session.with_torrents(|it| it.map(|(id, t)| (id, t.clone())).collect());
                let now = Instant::now();
                let mut to_pause = Vec::new();
                let mut to_start = Vec::new();
                let mut finished_names = Vec::new();
                let notify;
                {
                    let mut g = lock(&gov);
                    notify = g.notify;
                    let before = g.queued.len();
                    g.queued.retain(|id| handles.iter().any(|(h, _)| h == id));
                    let mut active = 0usize;
                    for (id, t) in &handles {
                        let s = t.stats();
                        let paused = is_paused_state(&s);
                        if !paused && !s.finished && s.error.is_none() {
                            active += 1;
                            g.was_downloading.insert(*id);
                        }
                        if s.finished && s.live.is_some() {
                            if g.was_downloading.remove(id) {
                                finished_names
                                    .push(t.name().unwrap_or_else(|| t.info_hash().as_string()));
                            }
                            let since = *g.finished_at.entry(*id).or_insert(now);
                            if !g.seed_limited.contains(id) {
                                let ratio = s.uploaded_bytes as f64 / s.total_bytes.max(1) as f64;
                                let ratio_hit =
                                    g.ratio_limit > 0.0 && ratio >= f64::from(g.ratio_limit);
                                let time_hit = g.seed_minutes > 0
                                    && since.elapsed()
                                        >= Duration::from_secs(u64::from(g.seed_minutes) * 60);
                                if ratio_hit || time_hit {
                                    g.seed_limited.insert(*id);
                                    to_pause.push(t.clone());
                                }
                            }
                        }
                    }
                    // Start queued torrents while there are free slots.
                    while (g.max_active == 0 || active < g.max_active)
                        && let Some(id) = g.queued.pop_first()
                    {
                        if let Some((_, t)) = handles.iter().find(|(h, _)| *h == id) {
                            to_start.push(t.clone());
                            active += 1;
                        }
                    }
                    if g.queued.len() != before || !to_start.is_empty() {
                        g.save_queue(&session);
                    }
                }
                for t in to_pause {
                    let _ = session.pause(&t).await;
                }
                for t in to_start {
                    let _ = session.unpause(&t).await;
                }
                for name in finished_names {
                    let _ = events.send(Event::Info(format!("Finished \"{name}\"")));
                    repaint.request_repaint();
                    if notify {
                        tokio::task::spawn_blocking(move || {
                            let _ = notify_rust::Notification::new()
                                .summary("Download complete")
                                .body(&name)
                                .appname("Rustorrent")
                                .show();
                        });
                    }
                }
            }
        });
    }

    /// The upload cap currently in force because something is downloading,
    /// if any (bytes/s).
    pub fn upload_throttle(&self) -> Option<u64> {
        let up = lock(&self.upload);
        if up.throttling { up.applied } else { None }
    }

    /// Once a second: check whether anything is downloading and set the
    /// session's upload limit according to the upload policy.
    fn spawn_upload_governor(&self) {
        let session = Arc::downgrade(&self.session);
        let state = self.upload.clone();
        self.rt.spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(1));
            loop {
                tick.tick().await;
                let Some(session) = session.upgrade() else {
                    return;
                };
                let handles: Vec<Arc<librqbit::ManagedTorrent>> =
                    session.with_torrents(|iter| iter.map(|(_, t)| t.clone()).collect());
                let downloading = handles.iter().any(|t| {
                    let stats = t.stats();
                    stats.live.is_some() && !stats.finished
                });

                let mut st = lock(&state);
                let target = upload_policy::target_limit(st.user_limit, st.mode, downloading);
                if st.dirty || upload_policy::should_apply(st.applied, target) {
                    session.ratelimits.set_upload_bps(
                        target.and_then(|b| NonZeroU32::new(b.min(u64::from(u32::MAX)) as u32)),
                    );
                    st.applied = target;
                    st.dirty = false;
                }
                st.throttling =
                    downloading && st.mode != WhileDownloading::Off && st.applied != st.user_limit;
            }
        });
    }

    pub fn set_max_peers(&mut self, n: usize) {
        self.max_peers_per_torrent = n;
    }

    pub fn listen_port(&self) -> Option<u16> {
        self.session.announce_port()
    }

    fn send(&self, ev: Event) {
        let _ = self.events.send(ev);
        self.repaint.request_repaint();
    }

    /// Resolve metadata for a magnet link, URL or `.torrent` file without
    /// starting the download yet.
    pub fn fetch_metadata(&self, source: TorrentSource) {
        let session = self.session.clone();
        let events = self.events.clone();
        let repaint = self.repaint.clone();
        self.rt.spawn(async move {
            let label = source.label();
            let _ = events.send(Event::Info(format!("Fetching metadata for {label}…")));
            repaint.request_repaint();

            let result = async {
                let add = source.into_add_torrent()?;
                let opts = AddTorrentOptions {
                    list_only: true,
                    ..Default::default()
                };
                match session.add_torrent(add, Some(opts)).await? {
                    AddTorrentResponse::ListOnly(r) => {
                        let files: Vec<PendingFile> = r
                            .info
                            .iter_file_details()
                            .map(|fd| PendingFile {
                                path: fd.filename.to_pathbuf().display().to_string(),
                                size: fd.len,
                                selected: true,
                            })
                            .collect();
                        let name = r
                            .info
                            .name()
                            .map(|n| n.to_string())
                            .unwrap_or_else(|| r.info_hash.as_string());
                        let sub_folder = if files.len() >= 2 {
                            let p = PathBuf::from(&name);
                            let safe = !name.is_empty()
                                && p.components().all(|c| matches!(c, Component::Normal(_)));
                            Some(if safe {
                                p
                            } else {
                                PathBuf::from(r.info_hash.as_string())
                            })
                        } else {
                            None
                        };
                        Ok(PendingTorrent {
                            name,
                            files,
                            torrent_bytes: r.torrent_bytes,
                            seen_peers: r.seen_peers,
                            sub_folder,
                        })
                    }
                    AddTorrentResponse::AlreadyManaged(_, h) => Err(anyhow::anyhow!(
                        "\"{}\" is already in the list",
                        h.name().unwrap_or_default()
                    )),
                    AddTorrentResponse::Added(..) => {
                        Err(anyhow::anyhow!("unexpected response from engine"))
                    }
                }
            }
            .await;

            let ev = match result {
                Ok(p) => Event::MetadataReady(p),
                Err(e) => Event::Error(format!("Could not add {label}: {e:#}")),
            };
            let _ = events.send(ev);
            repaint.request_repaint();
        });
    }

    /// Actually start downloading a torrent previously resolved with
    /// [`Self::fetch_metadata`].
    pub fn start_download(&self, pending: PendingTorrent, folder: PathBuf) {
        let max_peers = self.max_peers_per_torrent;
        let gov = self.gov.clone();
        let session = self.session.clone();
        let events = self.events.clone();
        let repaint = self.repaint.clone();
        self.rt.spawn(async move {
            let output = match &pending.sub_folder {
                Some(sub) => folder.join(sub),
                None => folder,
            };
            let all_selected = pending.files.iter().all(|f| f.selected);
            let only_files = (!all_selected).then(|| {
                pending
                    .files
                    .iter()
                    .enumerate()
                    .filter(|(_, f)| f.selected)
                    .map(|(i, _)| i)
                    .collect::<Vec<_>>()
            });
            let max_active = lock(&gov).max_active;
            let queue_it = max_active > 0 && count_active(&session) >= max_active;
            let opts = AddTorrentOptions {
                paused: queue_it,
                peer_limit: Some(max_peers),
                output_folder: Some(output.display().to_string()),
                only_files,
                overwrite: true,
                initial_peers: (!pending.seen_peers.is_empty()).then_some(pending.seen_peers),
                ..Default::default()
            };
            let ev = match session
                .add_torrent(AddTorrent::from_bytes(pending.torrent_bytes), Some(opts))
                .await
            {
                Ok(AddTorrentResponse::Added(id, _)) if queue_it => {
                    let mut g = lock(&gov);
                    g.queued.insert(id);
                    g.save_queue(&session);
                    Event::Info(format!("Queued \"{}\"", pending.name))
                }
                Ok(AddTorrentResponse::Added(..)) => {
                    Event::Info(format!("Started \"{}\"", pending.name))
                }
                Ok(AddTorrentResponse::AlreadyManaged(..)) => {
                    Event::Info(format!("\"{}\" is already in the list", pending.name))
                }
                Ok(AddTorrentResponse::ListOnly(_)) => {
                    Event::Error("unexpected list-only response".into())
                }
                Err(e) => Event::Error(format!("Could not start \"{}\": {e:#}", pending.name)),
            };
            let _ = events.send(ev);
            repaint.request_repaint();
        });
    }

    pub fn torrents(&self) -> Vec<TorrentRow> {
        let now = Instant::now();
        // Grab cheap handles first so the session lock is held only briefly.
        let handles: Vec<(usize, Arc<librqbit::ManagedTorrent>)> = self
            .session
            .with_torrents(|iter| iter.map(|(id, t)| (id, t.clone())).collect());

        let queued = lock(&self.gov).queued.clone();
        let mut speeds = self.speeds.lock().unwrap_or_else(|e| e.into_inner());
        let mut rows: Vec<TorrentRow> = handles
            .into_iter()
            .map(|(id, t)| {
                let stats = t.stats();
                let rates = match &stats.live {
                    Some(live) => speeds.entry(id).or_default().update(
                        now,
                        live.snapshot.fetched_bytes,
                        live.snapshot.uploaded_bytes,
                    ),
                    None => {
                        speeds.remove(&id);
                        Rates::default()
                    }
                };
                let mut rates = rates;
                let eta = if stats.finished {
                    // Nothing left to download; don't show the tail of the
                    // averaging window as a download speed while seeding.
                    rates.down_bps = 0.0;
                    None
                } else {
                    let remaining = stats.total_bytes.saturating_sub(stats.progress_bytes);
                    speed::eta(remaining, rates.down_bps)
                };
                TorrentRow {
                    id,
                    name: t.name().unwrap_or_else(|| t.info_hash().as_string()),
                    output_folder: t.output_folder().to_path_buf(),
                    stats,
                    rates,
                    eta,
                    queued: queued.contains(&id),
                }
            })
            .collect();
        // Forget trackers of removed torrents.
        speeds.retain(|id, _| rows.iter().any(|r| r.id == *id));
        rows.sort_by_key(|r| r.id);
        rows
    }

    /// Detailed info for the inspector panel.
    pub fn details(&self, id: usize) -> Option<TorrentDetails> {
        let t = self.session.get(id.into())?;
        let stats = t.stats();
        let included: Option<std::collections::HashSet<usize>> =
            t.only_files().map(|v| v.into_iter().collect());
        let (files, piece_length, pieces, web_seeds) = t
            .with_metadata(|m| {
                let files = m
                    .file_infos
                    .iter()
                    .enumerate()
                    .filter(|(_, f)| !f.attrs.padding)
                    .map(|(i, f)| FileDetail {
                        index: i,
                        path: f.relative_filename.display().to_string(),
                        size: f.len,
                        done: stats.file_progress.get(i).copied().unwrap_or(0),
                        included: included.as_ref().is_none_or(|s| s.contains(&i)),
                    })
                    .collect::<Vec<_>>();
                let l = m.lengths();
                let web_seeds = librqbit::web_seed_urls(&m.torrent_bytes).len();
                (files, l.default_piece_length(), l.total_pieces(), web_seeds)
            })
            .ok()?;
        let now = Instant::now();
        let (peers, piece_map) = match t.live() {
            Some(live) => {
                let mut speeds = lock(&self.peer_speeds);
                let peers: Vec<PeerDetail> = live
                    .ui_peers()
                    .into_iter()
                    .map(|p| {
                        let r = speeds.entry((id, p.addr)).or_default().update(
                            now,
                            p.fetched_bytes,
                            p.uploaded_bytes,
                        );
                        PeerDetail {
                            addr: p.addr,
                            client: p.client.unwrap_or_default(),
                            utp: p.utp,
                            down_bps: r.down_bps,
                            up_bps: r.up_bps,
                            have: p.have,
                        }
                    })
                    .collect();
                speeds.retain(|(tid, addr), _| *tid != id || peers.iter().any(|p| p.addr == *addr));
                (peers, live.ui_piece_map(160))
            }
            None => (Vec::new(), Vec::new()),
        };
        let mut trackers: Vec<String> = t.shared().trackers.iter().map(|u| u.to_string()).collect();
        trackers.sort();
        Some(TorrentDetails {
            info_hash: t.info_hash().as_string(),
            save_path: t.output_folder().to_path_buf(),
            piece_length,
            pieces,
            files,
            peers,
            trackers,
            piece_map,
            web_seeds,
        })
    }

    /// Include or skip one file of a torrent.
    pub fn set_file_included(&self, id: usize, file: usize, include: bool) {
        let Some(handle) = self.session.get(id.into()) else {
            return;
        };
        let session = self.session.clone();
        let events = self.events.clone();
        let repaint = self.repaint.clone();
        self.rt.spawn(async move {
            let total = handle.with_metadata(|m| m.file_infos.len()).unwrap_or(0);
            let mut set: std::collections::HashSet<usize> = handle
                .only_files()
                .map(|v| v.into_iter().collect())
                .unwrap_or_else(|| (0..total).collect());
            if include {
                set.insert(file);
            } else {
                set.remove(&file);
            }
            if set.is_empty() {
                let _ = events.send(Event::Error("At least one file must stay selected".into()));
                repaint.request_repaint();
                return;
            }
            if let Err(e) = session.update_only_files(&handle, &set).await {
                let _ = events.send(Event::Error(format!("Could not change files: {e:#}")));
                repaint.request_repaint();
            }
        });
    }

    pub fn pause(&self, id: usize) {
        let mut g = lock(&self.gov);
        if g.queued.remove(&id) {
            g.save_queue(&self.session);
        }
        drop(g);
        self.with_handle(
            id,
            "pause",
            |session, h| async move { session.pause(&h).await },
        );
    }

    /// Resume (or force-start a queued torrent).
    pub fn resume(&self, id: usize) {
        let mut g = lock(&self.gov);
        if g.queued.remove(&id) {
            g.save_queue(&self.session);
        }
        drop(g);
        self.with_handle(id, "resume", |session, h| async move {
            session.unpause(&h).await
        });
    }

    pub fn remove(&self, id: usize, delete_files: bool) {
        let session = self.session.clone();
        let events = self.events.clone();
        let repaint = self.repaint.clone();
        self.rt.spawn(async move {
            let ev = match session.delete(id.into(), delete_files).await {
                Ok(()) if delete_files => {
                    Event::Info("Removed torrent and deleted its files".into())
                }
                Ok(()) => Event::Info("Removed torrent".into()),
                Err(e) => Event::Error(format!("Could not remove torrent: {e:#}")),
            };
            let _ = events.send(ev);
            repaint.request_repaint();
        });
    }

    fn with_handle<F, Fut>(&self, id: usize, what: &'static str, f: F)
    where
        F: FnOnce(Arc<Session>, Arc<librqbit::ManagedTorrent>) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        let Some(handle) = self.session.get(id.into()) else {
            self.send(Event::Error(format!("Torrent {id} not found")));
            return;
        };
        let session = self.session.clone();
        let events = self.events.clone();
        let repaint = self.repaint.clone();
        self.rt.spawn(async move {
            if let Err(e) = f(session, handle).await {
                let _ = events.send(Event::Error(format!("Could not {what} torrent: {e:#}")));
                repaint.request_repaint();
            }
        });
    }

    pub fn shutdown(&self) {
        let session = self.session.clone();
        self.rt.block_on(async move {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(3), session.stop()).await;
        });
    }
}

fn disk_io_slots() -> usize {
    let cores = std::thread::available_parallelism().map_or(4, |n| n.get());
    (cores * 4).clamp(16, 64)
}

fn kib_to_bps(kb: u32) -> Option<NonZeroU32> {
    NonZeroU32::new(kb.saturating_mul(1000))
}

#[derive(Debug, Clone, Copy)]
struct Listen {
    ipv4_only: bool,
    port: u16,
}

async fn create_session(
    settings: &Settings,
    data_dir: PathBuf,
    listen: Option<Listen>,
) -> anyhow::Result<Arc<Session>> {
    let trackers = if settings.extra_trackers {
        PUBLIC_TRACKERS
            .iter()
            .filter_map(|t| url::Url::parse(t).ok())
            .collect()
    } else {
        Default::default()
    };

    let opts = SessionOptions {
        fastresume: true,
        persistence: Some(SessionPersistenceConfig::Json {
            folder: Some(data_dir.join("session")),
        }),
        ipv4_only: listen.is_none_or(|l| l.ipv4_only),
        listen: listen.map(|l| ListenerOptions {
            mode: if settings.enable_utp {
                ListenerMode::TcpAndUtp
            } else {
                ListenerMode::TcpOnly
            },
            listen_addr: if l.ipv4_only {
                (Ipv4Addr::UNSPECIFIED, l.port).into()
            } else {
                (Ipv6Addr::UNSPECIFIED, l.port).into()
            },
            ipv4_only: l.ipv4_only,
            enable_upnp_port_forwarding: settings.enable_upnp,
            ..Default::default()
        }),
        connect: Some(ConnectionOptions {
            encryption: match settings.encryption {
                EncryptionMode::Prefer => EncryptionPolicy::Enabled,
                EncryptionMode::Require => EncryptionPolicy::Required,
                EncryptionMode::Off => EncryptionPolicy::Disabled,
            },
            peer_opts: Some(PeerConnectionOptions {
                // Benchmarked against libtorrent in a tit-for-tat swarm:
                // queueing hundreds of requests at slow peers ties pieces up
                // behind them; 16-64 finished ~7% faster than 256 while
                // still allowing ~10 MiB/s from a single peer at 100 ms RTT.
                max_request_window: Some(64),
                ..Default::default()
            }),
            ..Default::default()
        }),
        peer_limit: Some(settings.max_peers_per_torrent),
        // librqbit funnels every received-chunk disk write and every piece
        // hash check through this many concurrent slots (default 8, shared by
        // all torrents). With hundreds of peers that queue backs up, peers'
        // data sits unread and throughput turns bursty. Writes land in the OS
        // page cache and SHA-1 is CPU-bound, so scale with the core count.
        runtime_worker_threads: Some(disk_io_slots()),
        trackers,
        ratelimits: LimitsConfig {
            download_bps: kib_to_bps(settings.download_limit_kib),
            upload_bps: kib_to_bps(settings.upload_limit_kib),
        },
        ..Default::default()
    };
    Session::new_with_opts(settings.download_dir.clone(), opts)
        .await
        .context("failed to start torrent session")
}

/// Where a new torrent comes from.
pub enum TorrentSource {
    /// Magnet link, info-hash or http(s) URL to a `.torrent` file.
    Url(String),
    /// Local `.torrent` file.
    File(PathBuf),
}

impl TorrentSource {
    /// Interpret a command-line argument / dropped item.
    pub fn from_arg(arg: &str) -> Self {
        let lower = arg.to_ascii_lowercase();
        if lower.starts_with("magnet:")
            || lower.starts_with("http://")
            || lower.starts_with("https://")
            || (arg.len() == 40 && arg.chars().all(|c| c.is_ascii_hexdigit()))
        {
            TorrentSource::Url(arg.trim().to_string())
        } else {
            TorrentSource::File(PathBuf::from(arg))
        }
    }

    fn label(&self) -> String {
        match self {
            TorrentSource::Url(u) if u.len() > 60 => {
                format!("{}…", &u[..u.floor_char_boundary(60)])
            }
            TorrentSource::Url(u) => u.clone(),
            TorrentSource::File(p) => p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.display().to_string()),
        }
    }

    fn into_add_torrent(self) -> anyhow::Result<AddTorrent<'static>> {
        Ok(match self {
            TorrentSource::Url(u) => AddTorrent::from_url(u),
            TorrentSource::File(p) => AddTorrent::from_bytes(read_file(&p)?),
        })
    }
}

fn read_file(p: &Path) -> anyhow::Result<Vec<u8>> {
    std::fs::read(p).with_context(|| format!("reading {}", p.display()))
}
