//! Decides the upload rate limit so uploading never starves downloads.
//!
//! librqbit unchokes every peer and serves every request, so with many peers
//! it will happily saturate the upstream link. A saturated upstream delays
//! the TCP ACKs and block requests that downloads depend on, so download
//! speed collapses (e.g. 10 MiB/s up, 2.8 MiB/s down). While anything is
//! downloading we therefore cap upload; once everything is complete, upload
//! goes back to the user's own limit (unlimited by default) for seeding.

/// Upload cap applied while at least one torrent is downloading.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WhileDownloading {
    /// Don't touch the upload rate.
    Off,
    /// A share of the current download speed, with a floor.
    Auto,
    /// A fixed cap in bytes/s.
    Fixed(u64),
}

/// Share of the current download speed we keep uploading in `Auto` mode,
/// enough to keep tit-for-tat peers trading with us.
pub const AUTO_SHARE: f64 = 0.25;
/// Never throttle upload below this in `Auto` mode.
pub const AUTO_FLOOR_BPS: u64 = 256 * 1024;

/// Upload limit (bytes/s) to apply right now; `None` = unlimited.
pub fn target_limit(
    user_limit: Option<u64>,
    mode: WhileDownloading,
    downloading: bool,
    down_bps: f64,
) -> Option<u64> {
    if !downloading {
        return user_limit;
    }
    let cap = match mode {
        WhileDownloading::Off => return user_limit,
        WhileDownloading::Auto => ((down_bps * AUTO_SHARE) as u64).max(AUTO_FLOOR_BPS),
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

    const MIB: f64 = 1024.0 * 1024.0;

    #[test]
    fn seeding_only_uses_user_limit() {
        assert_eq!(target_limit(None, WhileDownloading::Auto, false, 0.0), None);
        assert_eq!(
            target_limit(Some(5000), WhileDownloading::Auto, false, 0.0),
            Some(5000)
        );
    }

    #[test]
    fn auto_caps_to_share_of_download() {
        // 2.8 MiB/s down -> 0.7 MiB/s up
        let t = target_limit(None, WhileDownloading::Auto, true, 2.8 * MIB).unwrap();
        assert_eq!(t, (2.8 * MIB * AUTO_SHARE) as u64);
    }

    #[test]
    fn auto_has_floor() {
        assert_eq!(
            target_limit(None, WhileDownloading::Auto, true, 0.0),
            Some(AUTO_FLOOR_BPS)
        );
    }

    #[test]
    fn user_limit_still_wins_when_lower() {
        assert_eq!(
            target_limit(Some(100_000), WhileDownloading::Auto, true, 10.0 * MIB),
            Some(100_000)
        );
    }

    #[test]
    fn off_and_fixed() {
        assert_eq!(target_limit(None, WhileDownloading::Off, true, MIB), None);
        assert_eq!(
            target_limit(None, WhileDownloading::Fixed(1_000_000), true, MIB),
            Some(1_000_000)
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
