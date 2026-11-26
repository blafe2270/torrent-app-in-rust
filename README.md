# Rustorrent

A fast, simple BitTorrent client written in Rust, with a native GUI for
**Windows, macOS and Linux**.

- macOS-style interface: floating sidebar, icon grid or list view, capsule
  toolbar, light & dark appearance (follows the system)
- Add torrents from `.torrent` files, magnet links or URLs — or drag & drop files onto the window
- Pick which files to download and where to save them; change it later in the inspector
- Inspector panel: info, piece map, per-file progress and selection, peers, trackers
- Download queue (3 at once by default), stop-seeding limits (ratio / time)
- Desktop notification when a download finishes
- Opens magnet links and .torrent files from your browser / file manager
  (Settings → Integration), and forwards them to the already-open window
- Menu-bar (macOS) / notification-area (Windows) icon; closing the window
  keeps downloads running
- Torrents and progress survive restarts (fast resume)
- **Tuned for speed by default**, with optional download/upload limits
- Command-line mode: `rustorrent --headless file.torrent [--out DIR] [--seed]`

Built on [librqbit](https://github.com/ikatson/rqbit) (BitTorrent engine,
vendored in `vendor/librqbit` with Rustorrent's extensions: protocol
encryption, rarest-first piece selection and web seeds) and
[egui/eframe](https://github.com/emilk/egui) (GUI).

## Download

Prebuilt binaries are produced by GitHub Actions for every push to `main`
(see the **Actions** tab → latest *Build* run → *Artifacts*) and attached to
every tagged release on the **Releases** page:

| Platform | File |
|---|---|
| Windows 10/11 (x64) | `rustorrent-windows-x86_64.zip` → run `rustorrent.exe` |
| macOS 11+ (Apple Silicon & Intel) | `rustorrent-macos-universal.zip` → `Rustorrent.app` |
| Linux (x64, glibc 2.35+) | `rustorrent-linux-x86_64.tar.gz` → run `./rustorrent` |

**macOS note:** unless the release was signed (see *Signing the Mac app*
below), the first time right-click `Rustorrent.app` → **Open** (or run
`xattr -dr com.apple.quarantine Rustorrent.app`). Move it to Applications
before using Settings → Integration, so macOS remembers it as the handler.

**Windows note:** SmartScreen may warn about an unrecognized app; click
**More info → Run anyway**.

## Speed

Out of the box Rustorrent is configured to download as fast as your
connection allows:

- **No download or upload limits.** Uploading is how a BitTorrent client
  earns download speed: other peers send the most to whoever sends them the
  most (tit-for-tat). In benchmarks, capping upload made downloads ~20%
  slower. If uploading ever slows your internet, enter your plan's upload
  speed in Settings and upload is kept to 80% of it while downloading.
- **Up to 250 peers per torrent** (adjustable up to 2000)
- **TCP + uTP** connections, so more peers are reachable
- **UPnP port forwarding**, so peers can connect *to* you (the single biggest
  speed factor behind a home router)
- **Extra public trackers** added to every non-private torrent, so magnet links
  with few/dead trackers still find plenty of peers (never added to private
  torrents)
- DHT and local peer discovery enabled
- A per-peer request pipeline of 64 blocks: deep enough for ~10 MB/s from a
  single peer, without queueing hundreds of requests at slow peers (which
  ties pieces up behind them)
- The OS open-file limit is raised automatically to allow many connections
- Disk writes and piece hash checks run on up to 64 parallel slots (scaled to
  your CPU) instead of librqbit's default 8, so slow disks or antivirus
  scanning don't stall incoming data
- Peer exchange (PEX) and DHT keep discovering new peers while downloading
- **Protocol encryption (MSE/PE)**, like qBittorrent: connections are
  encrypted when the peer supports it, which gets around ISP throttling of
  BitTorrent traffic and reaches peers that only accept encrypted
  connections. Falls back to plaintext for peers without it (Settings →
  Encryption: Prefer / Require / Off). Tested against libtorrent in both
  directions, over TCP and uTP.
- **Rarest-first piece selection**: pieces few peers have are fetched first,
  so downloads don't stall near the end and other peers value us more
- **Web seeds** (BEP 19): torrents that list HTTP mirrors (Linux ISOs,
  archive.org, …) download from the mirror in parallel with peers
- **Download queue**: only 3 torrents download at once by default, so each
  one gets the full bandwidth and finishes sooner

The speed shown in the app is averaged over 5 seconds. BitTorrent data
arrives in bursts, so an instantaneous reading jumps around even when the real
throughput is steady.

### Benchmarks

Measured locally against **libtorrent 2.1** (the engine inside qBittorrent and
Deluge), with every peer limited to 1 MB/s of upload:

| Swarm | libtorrent | Rustorrent |
|---|---|---|
| 16 full seeders (100 ms latency) | 25 s | **18 s** |
| 1 seeder + 12 downloaders trading pieces (tit-for-tat) | 107–118 s | 119–134 s |

With plenty of seeders Rustorrent saturates the link; in swarms of
downloaders it is within ~10% of libtorrent. On one 4-core machine the engine
sustains about **340 MB/s** locally, so the client itself is never the
bottleneck on a home connection — real-world speed is set by how many peers
you reach and how fast they upload.

You can benchmark your own torrents with the headless mode:

```bash
rustorrent --headless file.torrent --out ~/Downloads
```

Open **⚙ Settings** to set a download and/or upload limit (applies
immediately), change the max peers, the listening port, or turn any of the
above off.

Tips if downloads are still slow: make sure UPnP is enabled on your router (or
forward the listening port, default `4240`, TCP+UDP, manually) and allow
Rustorrent through your firewall when asked.

## Signing the Mac app

The build signs the app with a Developer ID and notarizes it automatically
when these repository secrets exist (otherwise it ships ad-hoc signed):

| Secret | Value |
|---|---|
| `MACOS_CERTIFICATE` | your *Developer ID Application* certificate exported as `.p12`, base64-encoded |
| `MACOS_CERTIFICATE_PASSWORD` | the `.p12` password |
| `APPLE_ID` | your Apple ID email |
| `APPLE_TEAM_ID` | your 10-character team ID |
| `APPLE_APP_PASSWORD` | an [app-specific password](https://support.apple.com/HT204397) |

## Publishing a release

Actions → **Build** → **Run workflow**, enter a tag like `v0.3.0` (it must
match the version in `Cargo.toml`). All three platforms are built and attached
to a new GitHub release.

## Build from source

Requires Rust 1.95 or newer.

```bash
cargo run --release
```

You can also pass `.torrent` paths or magnet links on the command line:

```bash
cargo run --release -- "magnet:?xt=urn:btih:..."
```

## Where things are stored

| | Location |
|---|---|
| Settings | `<config dir>/rustorrent/settings.json` |
| Session / resume data | `<data dir>/rustorrent/session/` |
| Downloads | your *Downloads* folder by default (changeable) |

`<config dir>` / `<data dir>` are `%APPDATA%` on Windows,
`~/Library/Application Support` on macOS and `~/.config` / `~/.local/share` on Linux.

## License

MIT
