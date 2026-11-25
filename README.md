# Rustorrent

A fast, simple BitTorrent client written in Rust, with a native GUI for
**Windows, macOS and Linux**.

- Add torrents from `.torrent` files, magnet links or URLs — or drag & drop files onto the window
- Pick which files to download and where to save them
- Pause / resume / remove (optionally deleting files)
- Live progress, download/upload speed, peers and ETA
- Torrents and progress survive restarts (fast resume)
- **Tuned for speed by default**, with optional download/upload limits

Built on [librqbit](https://github.com/ikatson/rqbit) (BitTorrent engine) and
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

**macOS note:** the app is not notarized, so the first time right-click
`Rustorrent.app` → **Open** (or run `xattr -dr com.apple.quarantine Rustorrent.app`).

**Windows note:** SmartScreen may warn about an unrecognized app; click
**More info → Run anyway**.

## Speed

Out of the box Rustorrent is configured to download as fast as your
connection allows:

- **No bandwidth limits**
- **Up to 250 peers per torrent** (adjustable up to 2000)
- **TCP + uTP** connections, so more peers are reachable
- **UPnP port forwarding**, so peers can connect *to* you (the single biggest
  speed factor behind a home router)
- **Extra public trackers** added to every non-private torrent, so magnet links
  with few/dead trackers still find plenty of peers (never added to private
  torrents)
- DHT and local peer discovery enabled
- Short connect timeouts so dead peers are skipped quickly, and a deep
  per-peer request pipeline for high-bandwidth links
- The OS open-file limit is raised automatically to allow many connections

Open **⚙ Settings** to set a download and/or upload limit (applies
immediately), change the max peers, the listening port, or turn any of the
above off.

Tips if downloads are still slow: make sure UPnP is enabled on your router (or
forward the listening port, default `4240`, TCP+UDP, manually) and allow
Rustorrent through your firewall when asked.

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
