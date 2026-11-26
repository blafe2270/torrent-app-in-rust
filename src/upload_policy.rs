//! Decides the upload rate limit while downloading.
//!
//! Uploading is how a BitTorrent client earns download speed: other peers
//! unchoke and favour whoever uploads to them (tit-for-tat). Benchmarks
//! against libtorrent showed that capping upload to a fraction of the
//! download speed made downloads ~20% slower. What does hurt is saturating
//! the *link*: a full upstream delays the ACKs and requests downloads depend
//! on. So the only cap applied is 80% of the connection's upload capacity,
//! and only when the user has told us that capacity.

/// Upload cap applied while at least one torrent is downloading.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WhileDownloading {
    /// Don't touch the upload rate.
    Off,
    /// A fixed cap in bytes/s.
    Fixed(u64),
}

/// Share of the connection's upload capacity used while downloading.
pub const CAPACITY_SHARE: f64 = 0.8;

/// Upload limit (bytes/s) to apply right now; `None` = unlimited.
pub fn target_limit(
    user_limit: Option<u64>,
    mode: WhileDownloading,
    downloading: bool,
) -> Option<u64> {
    if !downloading {
        return user_limit;
    }
    let cap = match mode {
        WhileDownloading::Off => return user_limit,
        WhileDownloading::Fixed(bps) => bps.max(16 * 1024),
    };
    Some(user_limit.map_or(cap, |u| u.min(cap)))
}

/// Whether moving from `current` to `target` is worth re-creating the rate
/// limiter (each new limiter starts with a full one-second burst, so we avoid
/// churning it for small changes).
pub fn should_apply(current: Option<u64>, target: Option<u64>) -> bool {
    match (current, target) {
        (None, None) => false,
        (Some(c), Some(t)) => {
            let (c, t) = (c as f64, t as f64);
            (t - c).abs() / c.max(1.0) > 0.25
        }
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeding_only_uses_user_limit() {
        assert_eq!(
            target_limit(None, WhileDownloading::Fixed(1000), false),
            None
        );
        assert_eq!(
            target_limit(Some(5000), WhileDownloading::Fixed(1000), false),
            Some(5000)
        );
    }

    #[test]
    fn caps_only_while_downloading() {
        assert_eq!(target_limit(None, WhileDownloading::Off, true), None);
        assert_eq!(
            target_limit(None, WhileDownloading::Fixed(1_000_000), true),
            Some(1_000_000)
        );
        assert_eq!(
            target_limit(Some(100_000), WhileDownloading::Fixed(1_000_000), true),
            Some(100_000)
        );
    }

    #[test]
    fn hysteresis() {
        assert!(!should_apply(Some(1000), Some(1100)));
        assert!(should_apply(Some(1000), Some(1500)));
        assert!(should_apply(None, Some(1)));
        assert!(should_apply(Some(1), None));
        assert!(!should_apply(None, None));
    }
}
