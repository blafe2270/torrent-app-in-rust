// rustorrent: web seeds (BEP 19, "GetRight style").
//
// A .torrent may list HTTP(S) mirrors in its top-level "url-list". Each mirror
// is served by a few workers that behave like virtual peers: they reserve a
// piece through the normal piece picker (so rarest-first, streaming priority
// and stealing all apply), download its bytes with HTTP range requests, and
// hand them to the same write + hash-check path used for peer data.

use std::{
    net::{Ipv4Addr, SocketAddr},
    sync::{Arc, Weak, atomic::Ordering},
    time::Duration,
};

use anyhow::{Context, bail};
use librqbit_core::lengths::ValidPieceIndex;
use peer_binary_protocol::Piece;
use tracing::{debug, debug_span, warn};

use super::{
    AtomicPeerCounters, TorrentStateLive, TorrentStateLocked, write_received_chunk,
};
use crate::{
    Error,
    piece_tracker::{AcquireRequest, AcquireResult},
};

/// Concurrent piece downloads per mirror.
const WORKERS_PER_SEED: u16 = 4;
/// Give up on a mirror after this many failures in a row.
const MAX_CONSECUTIVE_FAILURES: u32 = 8;

/// Extract the BEP 19 "url-list" from raw .torrent bytes.
pub(crate) fn url_list(torrent: &[u8]) -> Vec<String> {
    fn skip(b: &[u8], mut i: usize) -> Option<usize> {
        match *b.get(i)? {
            b'i' => Some(i + b[i..].iter().position(|&c| c == b'e')? + 1),
            b'l' | b'd' => {
                i += 1;
                while *b.get(i)? != b'e' {
                    i = skip(b, i)?;
                }
                Some(i + 1)
            }
            b'0'..=b'9' => {
                let colon = i + b[i..].iter().position(|&c| c == b':')?;
                let len: usize = std::str::from_utf8(&b[i..colon]).ok()?.parse().ok()?;
                let end = colon + 1 + len;
                (end <= b.len()).then_some(end)
            }
            _ => None,
        }
    }
    fn string(b: &[u8], i: usize) -> Option<(&[u8], usize)> {
        let colon = i + b[i..].iter().position(|&c| c == b':')?;
        let len: usize = std::str::from_utf8(&b[i..colon]).ok()?.parse().ok()?;
        let s = b.get(colon + 1..colon + 1 + len)?;
        Some((s, colon + 1 + len))
    }

    let mut out = Vec::new();
    let b = torrent;
    if b.first() != Some(&b'd') {
        return out;
    }
    let mut i = 1;
    while let Some(&c) = b.get(i) {
        if c == b'e' {
            break;
        }
        let Some((key, vstart)) = string(b, i) else {
            break;
        };
        let Some(vend) = skip(b, vstart) else { break };
        if key == b"url-list" {
            let push = |s: &[u8], out: &mut Vec<String>| {
                if let Ok(s) = std::str::from_utf8(s)
                    && (s.starts_with("http://") || s.starts_with("https://"))
                {
                    out.push(s.to_string());
                }
            };
            match b[vstart] {
                b'l' => {
                    let mut j = vstart + 1;
                    while j < vend - 1 {
                        match string(b, j) {
                            Some((s, next)) => {
                                push(s, &mut out);
                                j = next;
                            }
                            None => match skip(b, j) {
                                Some(next) => j = next,
                                None => break,
                            },
                        }
                    }
                }
                b'0'..=b'9' => {
                    if let Some((s, _)) = string(b, vstart) {
                        push(s, &mut out);
                    }
                }
                _ => {}
            }
        }
        i = vend;
    }
    out.dedup();
    out
}

/// Start workers for every web seed in the torrent.
pub(crate) fn spawn_all(state: &Arc<TorrentStateLive>, client: reqwest::Client) {
    let urls = url_list(&state.metadata.torrent_bytes);
    for (seed_idx, url) in urls.into_iter().enumerate() {
        debug!(%url, "starting web seed");
        for w in 0..WORKERS_PER_SEED {
            // Virtual peer address: 0.0.0.0 is never a real peer.
            let port = 1 + (seed_idx as u16).saturating_mul(WORKERS_PER_SEED) + w;
            let addr = SocketAddr::from((Ipv4Addr::UNSPECIFIED, port));
            let weak = Arc::downgrade(state);
            let url = url.clone();
            let client = client.clone();
            state.spawn(
                debug_span!(parent: state.shared.span.clone(), "webseed", %url, worker = w),
                format!("[{}]webseed", state.shared.id),
                async move { worker(weak, url, client, addr).await },
            );
        }
    }
}

async fn worker(
    state: Weak<TorrentStateLive>,
    url: String,
    client: reqwest::Client,
    addr: SocketAddr,
) -> crate::Result<()> {
    let counters = Arc::new(AtomicPeerCounters::default());
    let mut failures = 0u32;
    loop {
        let Some(st) = state.upgrade() else {
            return Ok(());
        };
        if st.is_finished_and_no_active_streams() {
            drop(st);
            tokio::time::sleep(Duration::from_secs(15)).await;
            continue;
        }
        let piece = match acquire(&st, addr, &counters)? {
            Some(p) => p,
            None => {
                // Everything is in flight or done; check again shortly.
                drop(st);
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        };
        match download_piece(&st, &client, &url, addr, &counters, piece).await {
            Ok(()) => failures = 0,
            Err(e) => {
                failures += 1;
                debug!(%url, piece = piece.get(), "web seed error: {e:#}");
                {
                    let mut g = st.lock_write("webseed_release");
                    if let Ok(pieces) = g.get_pieces_mut() {
                        pieces.release_pieces_owned_by(addr);
                    }
                }
                st.new_pieces_notify.notify_waiters();
                if failures >= MAX_CONSECUTIVE_FAILURES {
                    warn!(%url, "web seed keeps failing, giving up on it: {e:#}");
                    return Ok(());
                }
                drop(st);
                let backoff = Duration::from_secs((1u64 << failures.min(6)).min(60));
                tokio::time::sleep(backoff).await;
            }
        }
    }
}

fn acquire(
    state: &TorrentStateLive,
    addr: SocketAddr,
    counters: &AtomicPeerCounters,
) -> crate::Result<Option<ValidPieceIndex>> {
    let mut g = state.lock_write("webseed_acquire");
    let TorrentStateLocked {
        pieces,
        file_priorities,
        ..
    } = &mut **g;
    let pieces = pieces.as_mut().ok_or(Error::ChunkTrackerEmpty)?;
    let result = pieces.acquire_piece(AcquireRequest {
        peer: addr,
        peer_avg_time: counters.average_piece_download_time(),
        priority_pieces: state.streams.iter_next_pieces(&state.lengths),
        file_priorities,
        file_infos: &state.metadata.file_infos,
        peer_has_piece: |_| true,
        can_steal: |p| state.per_piece_locks[p.get_usize()].try_write().is_some(),
        availability: |p| state.peers.availability_of(p.get_usize()),
    });
    Ok(match result {
        AcquireResult::Reserved(p) | AcquireResult::Stolen { piece: p, .. } => Some(p),
        AcquireResult::NoneAvailable => None,
    })
}

/// URL of one file on the mirror, per BEP 19.
fn file_url(base: &str, name: &str, relative: &std::path::Path, multi_file: bool) -> anyhow::Result<url::Url> {
    let mut url = url::Url::parse(base).context("bad web seed URL")?;
    if multi_file || base.ends_with('/') {
        let mut segs = url
            .path_segments_mut()
            .map_err(|_| anyhow::anyhow!("web seed URL can't have a path"))?;
        segs.pop_if_empty();
        segs.push(name);
        if multi_file {
            for c in relative.components() {
                segs.push(&c.as_os_str().to_string_lossy());
            }
        }
    }
    Ok(url)
}

async fn download_piece(
    state: &Arc<TorrentStateLive>,
    client: &reqwest::Client,
    base: &str,
    addr: SocketAddr,
    counters: &Arc<AtomicPeerCounters>,
    piece: ValidPieceIndex,
) -> anyhow::Result<()> {
    let start = state.lengths.piece_offset(piece);
    let len = state.lengths.piece_length(piece) as u64;
    let end = start + len;
    let name = state
        .metadata
        .info
        .name()
        .map(|n| n.to_string())
        .unwrap_or_default();
    let multi_file = state.metadata.file_infos.len() > 1;

    let mut data = Vec::with_capacity(len as usize);
    for fi in state.metadata.file_infos.iter() {
        let f_start = fi.offset_in_torrent;
        let f_end = f_start + fi.len;
        if f_end <= start || f_start >= end || fi.len == 0 {
            continue;
        }
        let from = start.max(f_start) - f_start;
        let to = end.min(f_end) - f_start; // exclusive
        if fi.attrs.padding {
            data.resize(data.len() + (to - from) as usize, 0);
            continue;
        }
        let url = file_url(base, &name, &fi.relative_filename, multi_file)?;
        let resp = client
            .get(url.clone())
            .header(reqwest::header::RANGE, format!("bytes={}-{}", from, to - 1))
            .timeout(Duration::from_secs(60))
            .send()
            .await
            .with_context(|| format!("GET {url}"))?
            .error_for_status()?;
        let partial = resp.status() == reqwest::StatusCode::PARTIAL_CONTENT;
        if !partial && !(from == 0 && to == fi.len) {
            bail!("server ignored the Range request ({})", resp.status());
        }
        let bytes = resp.bytes().await?;
        if bytes.len() as u64 != to - from {
            bail!("expected {} bytes, got {}", to - from, bytes.len());
        }
        data.extend_from_slice(&bytes);
    }
    if data.len() as u64 != len {
        bail!("piece assembled to {} bytes, expected {len}", data.len());
    }

    // Count it as downloaded for speed statistics.
    for c in [
        &state.stats.fetched_bytes,
        &state.session_stats.counters.fetched_bytes,
        &counters.fetched_bytes,
    ] {
        c.fetch_add(len, Ordering::Relaxed);
    }

    let state2 = state.clone();
    let counters2 = counters.clone();
    state
        .shared
        .spawner
        .block_in_place_with_semaphore(move || -> anyhow::Result<()> {
            for chunk in state2.lengths.iter_chunk_infos(piece) {
                let off = chunk.offset as usize;
                let block = &data[off..off + chunk.size as usize];
                let p = Piece::from_data(piece.get(), chunk.offset, block);
                write_received_chunk(&state2, addr, &counters2, &p, &chunk)?;
            }
            Ok(())
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_url_list_forms() {
        let list = b"d8:announce3:foo8:url-listl19:http://a.example/x/6:ftp://19:https://b.example/ye4:infod4:name1:xee";
        assert_eq!(url_list(list), vec!["http://a.example/x/", "https://b.example/y"]);
        let single = b"d4:infod4:name1:xe8:url-list16:http://m.ex/a.ise";
        assert_eq!(url_list(single), vec!["http://m.ex/a.is"]);
        assert!(url_list(b"d4:infod4:name1:xee").is_empty());
    }

    #[test]
    fn builds_bep19_urls() {
        let p = std::path::Path::new("sub/f.bin");
        assert_eq!(
            file_url("http://h/dir/", "My Torrent", p, true).unwrap().as_str(),
            "http://h/dir/My%20Torrent/sub/f.bin"
        );
        assert_eq!(
            file_url("http://h/file.iso", "file.iso", std::path::Path::new("file.iso"), false)
                .unwrap()
                .as_str(),
            "http://h/file.iso"
        );
        assert_eq!(
            file_url("http://h/dir/", "file.iso", std::path::Path::new("file.iso"), false)
                .unwrap()
                .as_str(),
            "http://h/dir/file.iso"
        );
    }
}
