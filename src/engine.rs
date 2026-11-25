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
use std::num::NonZeroU32;
use std::time::Duration;

use librqbit::limits::LimitsConfig;
use librqbit::{
    AddTorrent, AddTorrentOptions, AddTorrentResponse, ConnectionOptions, ListenerMode,
    ListenerOptions, PeerConnectionOptions, Session, SessionOptions, SessionPersistenceConfig,
    TorrentStats, TorrentStatsState,
};

use crate::settings::{PUBLIC_TRACKERS, Settings};
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

pub struct Engine {
    rt: Runtime,
    session: Arc<Session>,
    max_peers_per_torrent: usize,
    events: Sender<Event>,
    repaint: egui::Context,
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
        };
        engine.apply_live_settings(settings);
        Ok(engine)
    }

    /// Apply settings that can change while running (speed limits and the
    /// peer limit used for newly-added torrents).
    pub fn apply_live_settings(&self, settings: &Settings) {
        self.session
            .ratelimits
            .set_download_bps(kib_to_bps(settings.download_limit_kib));
        self.session
            .ratelimits
            .set_upload_bps(kib_to_bps(settings.upload_limit_kib));
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
            let opts = AddTorrentOptions {
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
        let mut rows = self.session.with_torrents(|iter| {
            iter.map(|(id, t)| TorrentRow {
                id,
                name: t.name().unwrap_or_else(|| t.info_hash().as_string()),
                output_folder: t.output_folder().to_path_buf(),
                stats: t.stats(),
            })
            .collect::<Vec<_>>()
        });
        rows.sort_by_key(|r| r.id);
        rows
    }

    pub fn pause(&self, id: usize) {
        self.with_handle(
            id,
            "pause",
            |session, h| async move { session.pause(&h).await },
        );
    }

    pub fn resume(&self, id: usize) {
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

fn kib_to_bps(kib: u32) -> Option<NonZeroU32> {
    NonZeroU32::new(kib.saturating_mul(1024))
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
            peer_opts: Some(PeerConnectionOptions {
                // Give up on unreachable peers quickly and move on to others.
                connect_timeout: Some(Duration::from_secs(5)),
                // Keep more block requests in flight per peer: better
                // throughput on fast / high-latency links.
                max_request_window: Some(256),
                ..Default::default()
            }),
            ..Default::default()
        }),
        peer_limit: Some(settings.max_peers_per_torrent),
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
