# Vendored dependencies

## librqbit 9.1.0

Source of [librqbit](https://github.com/ikatson/rqbit) 9.1.0 (Apache-2.0,
Copyright Igor Katson), vendored so Rustorrent can extend the BitTorrent
engine. It is used via `[patch.crates-io]` in the top-level `Cargo.toml`.

Rustorrent's changes are marked with `// rustorrent:` comments:

- rarest-first piece selection (per-piece availability tracking)
- MSE/PE protocol encryption (`src/mse.rs`)
- web seeds / BEP 19 (`src/webseed.rs`)
