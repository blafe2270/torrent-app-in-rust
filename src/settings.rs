//! Persisted user settings, stored as JSON in the OS config directory.
//!
//! Defaults are tuned for maximum download speed: unlimited bandwidth, a high
//! per-torrent peer limit, TCP + uTP, UPnP port forwarding and a set of extra
//! public trackers for non-private torrents.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::upload_policy::{self, WhileDownloading};

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub download_dir: PathBuf,
    /// Global download limit in KB/s (1000 bytes). 0 = unlimited.
    pub download_limit_kib: u32,
    /// Global upload limit in KB/s (1000 bytes). 0 = unlimited.
    pub upload_limit_kib: u32,
    /// The connection's upload capacity in Mbit/s (0 = not set). While
    /// downloading, upload is kept to 80% of it so the link never saturates.
    pub upload_capacity_mbps: f32,
    /// Maximum connected peers per torrent.
    pub max_peers_per_torrent: usize,
    /// TCP/uTP port for incoming peer connections.
    pub listen_port: u16,
    /// Use uTP in addition to TCP (more reachable peers).
    pub enable_utp: bool,
    /// Ask the router to forward the listen port (UPnP).
    pub enable_upnp: bool,
    /// Add well-known public trackers to every non-private torrent.
    pub extra_trackers: bool,
    /// Show torrents as an icon grid (true) or a detailed list (false).
    pub grid_view: bool,
    /// Torrents downloading at once; more wait in the queue (0 = unlimited).
    /// Fewer simultaneous downloads finish each one sooner.
    pub max_active_downloads: usize,
    /// Stop seeding at this upload/size ratio (0 = never).
    pub seed_ratio_limit: f32,
    /// Stop seeding after this many minutes (0 = never).
    pub seed_time_limit_min: u32,
    /// Desktop notification when a download finishes.
    pub notify_on_complete: bool,
    /// MSE/PE protocol encryption.
    pub encryption: EncryptionMode,
    /// Keep running in the menu bar / notification area when the window is
    /// closed (macOS and Windows).
    pub close_to_tray: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, Debug)]
#[serde(rename_all = "lowercase")]
pub enum EncryptionMode {
    /// Encrypt when the peer supports it (reaches peers that require it and
    /// avoids ISP throttling); fall back to plaintext otherwise.
    #[default]
    Prefer,
    /// Only talk to peers over encrypted connections.
    Require,
    /// Never encrypt.
    Off,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            download_dir: default_download_dir(),
            download_limit_kib: 0,
            upload_limit_kib: 0,
            upload_capacity_mbps: 0.0,
            max_peers_per_torrent: 250,
            listen_port: 4240,
            enable_utp: true,
            enable_upnp: true,
            extra_trackers: true,
            grid_view: true,
            max_active_downloads: 3,
            seed_ratio_limit: 0.0,
            seed_time_limit_min: 0,
            notify_on_complete: true,
            encryption: EncryptionMode::Prefer,
            close_to_tray: true,
        }
    }
}

impl Settings {
    fn file() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("rustorrent").join("settings.json"))
    }

    pub fn load() -> Self {
        Self::file()
            .and_then(|f| std::fs::read_to_string(f).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let Some(f) = Self::file() else { return };
        if let Some(parent) = f.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match serde_json::to_string_pretty(self) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&f, json) {
                    tracing::warn!("could not save settings to {}: {e}", f.display());
                }
            }
            Err(e) => tracing::warn!("could not serialize settings: {e}"),
        }
    }

    pub fn upload_policy(&self) -> WhileDownloading {
        if self.upload_capacity_mbps > 0.0 {
            let bytes_per_sec = f64::from(self.upload_capacity_mbps) * 1_000_000.0 / 8.0;
            WhileDownloading::Fixed((bytes_per_sec * upload_policy::CAPACITY_SHARE) as u64)
        } else {
            WhileDownloading::Off
        }
    }

    /// Settings that only take effect after the session is restarted.
    pub fn needs_restart_compared_to(&self, running: &Settings) -> bool {
        self.listen_port != running.listen_port
            || self.enable_utp != running.enable_utp
            || self.enable_upnp != running.enable_upnp
            || self.extra_trackers != running.extra_trackers
            || self.encryption != running.encryption
    }
}

pub fn default_download_dir() -> PathBuf {
    dirs::download_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join("Downloads")))
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("rustorrent")
}

/// Popular, long-lived public trackers. More trackers = more peers found,
/// especially for magnet links that ship with few or dead trackers.
/// librqbit never adds these to private torrents.
pub const PUBLIC_TRACKERS: &[&str] = &[
    "udp://tracker.opentrackr.org:1337/announce",
    "udp://open.demonii.com:1337/announce",
    "udp://open.stealth.si:80/announce",
    "udp://tracker.torrent.eu.org:451/announce",
    "udp://exodus.desync.com:6969/announce",
    "udp://explodie.org:6969/announce",
    "udp://tracker.openbittorrent.com:6969/announce",
    "udp://opentracker.io:6969/announce",
    "udp://tracker.dler.org:6969/announce",
    "udp://tracker.tiny-vps.com:6969/announce",
    "udp://tracker.theoks.net:6969/announce",
    "udp://tracker1.bt.moack.co.kr:80/announce",
    "http://tracker.opentrackr.org:1337/announce",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trackers_parse() {
        for t in PUBLIC_TRACKERS {
            url::Url::parse(t).unwrap();
        }
    }

    #[test]
    fn partial_json_uses_defaults() {
        let s: Settings = serde_json::from_str(r#"{"download_limit_kib": 500}"#).unwrap();
        assert_eq!(s.download_limit_kib, 500);
        assert_eq!(
            s.max_peers_per_torrent,
            Settings::default().max_peers_per_torrent
        );
    }
}
