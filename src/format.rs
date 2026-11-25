//! Human-readable formatting helpers.

pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut unit = 0;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[unit])
    }
}

/// librqbit reports speeds in MiB/s.
pub fn speed_mibps(mibps: f64) -> String {
    let bps = (mibps * 1024.0 * 1024.0).max(0.0) as u64;
    if bps < 1024 {
        "–".into()
    } else {
        format!("{}/s", bytes(bps))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_bytes() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(1023), "1023 B");
        assert_eq!(bytes(1024), "1.0 KiB");
        assert_eq!(bytes(1536 * 1024), "1.5 MiB");
    }

    #[test]
    fn formats_speed() {
        assert_eq!(speed_mibps(0.0), "–");
        assert_eq!(speed_mibps(1.0), "1.0 MiB/s");
    }
}
