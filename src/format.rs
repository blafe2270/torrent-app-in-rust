//! Human-readable formatting helpers.

/// Decimal units, like macOS Finder ("1.5 MB").
pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut unit = 0;
    while v >= 1000.0 && unit < UNITS.len() - 1 {
        v /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[unit])
    }
}

/// Bytes per second → "1.5 MB/s".
pub fn speed_bps(bps: f64) -> String {
    let bps = bps.max(0.0) as u64;
    if bps < 1000 {
        "–".into()
    } else {
        format!("{}/s", bytes(bps))
    }
}

/// "2h 05m", "4m 09s", "12s".
pub fn duration(d: std::time::Duration) -> String {
    let s = d.as_secs();
    let (h, m, s) = (s / 3600, (s / 60) % 60, s % 60);
    if h >= 100 {
        "∞".into()
    } else if h > 0 {
        format!("{h}h {m:02}m")
    } else if m > 0 {
        format!("{m}m {s:02}s")
    } else {
        format!("{s}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_bytes() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(999), "999 B");
        assert_eq!(bytes(1000), "1.0 KB");
        assert_eq!(bytes(1_500_000), "1.5 MB");
    }

    #[test]
    fn formats_speed() {
        assert_eq!(speed_bps(0.0), "–");
        assert_eq!(speed_bps(1_000_000.0), "1.0 MB/s");
        assert_eq!(duration(std::time::Duration::from_secs(3725)), "1h 02m");
        assert_eq!(duration(std::time::Duration::from_secs(65)), "1m 05s");
    }
}
