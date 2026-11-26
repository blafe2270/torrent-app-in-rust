//! Smoothed transfer-speed measurement.
//!
//! librqbit's built-in estimator samples every 100 ms but only keeps 5
//! samples, so it effectively averages over ~0.5 s. BitTorrent data arrives in
//! bursts, which makes that number swing wildly (e.g. 300 KB/s ↔ 7 MB/s) even
//! when the real throughput is steady. This tracker averages over a proper
//! multi-second window instead.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// How far back the average looks.
pub const WINDOW: Duration = Duration::from_secs(5);
/// Minimum spacing between stored samples (UI may call us every frame).
const MIN_SAMPLE_SPACING: Duration = Duration::from_millis(200);

#[derive(Clone, Copy)]
struct Sample {
    at: Instant,
    down: u64,
    up: u64,
}

#[derive(Default)]
pub struct SpeedTracker {
    samples: VecDeque<Sample>,
}

/// Bytes per second.
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct Rates {
    pub down_bps: f64,
    pub up_bps: f64,
}

impl SpeedTracker {
    /// Record cumulative byte counters and return the averaged rates.
    pub fn update(&mut self, now: Instant, down_total: u64, up_total: u64) -> Rates {
        // Counters restart when a torrent is paused/resumed: start over.
        if let Some(last) = self.samples.back()
            && (down_total < last.down || up_total < last.up)
        {
            self.samples.clear();
        }

        let should_push = self
            .samples
            .back()
            .is_none_or(|last| now.duration_since(last.at) >= MIN_SAMPLE_SPACING);
        if should_push {
            self.samples.push_back(Sample {
                at: now,
                down: down_total,
                up: up_total,
            });
        }

        // Keep exactly one sample at or beyond the window edge so the average
        // always spans the full window once we have enough history.
        while self.samples.len() > 2 && now.duration_since(self.samples[1].at) >= WINDOW {
            self.samples.pop_front();
        }

        let (Some(first), Some(last)) = (self.samples.front(), self.samples.back()) else {
            return Rates::default();
        };
        let elapsed = last.at.duration_since(first.at).as_secs_f64();
        if elapsed < 0.5 {
            return Rates::default();
        }
        Rates {
            down_bps: (last.down - first.down) as f64 / elapsed,
            up_bps: (last.up - first.up) as f64 / elapsed,
        }
    }
}

/// Estimated time to download `remaining` bytes at `bps`.
pub fn eta(remaining: u64, bps: f64) -> Option<Duration> {
    if remaining == 0 || bps < 1.0 {
        return None;
    }
    Some(Duration::from_secs_f64(remaining as f64 / bps))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bursty_input_gives_steady_average() {
        let mut t = SpeedTracker::default();
        let start = Instant::now();
        let mut down = 0u64;
        let mut last = Rates::default();
        // Every 100 ms, alternate a 600 KB burst with nothing: 3 MB/s average.
        for i in 0..100u64 {
            if i % 2 == 0 {
                down += 600_000;
            }
            last = t.update(start + Duration::from_millis(i * 100), down, 0);
        }
        assert!(
            (last.down_bps - 3_000_000.0).abs() < 200_000.0,
            "got {}",
            last.down_bps
        );
    }

    #[test]
    fn counter_reset_starts_over() {
        let mut t = SpeedTracker::default();
        let start = Instant::now();
        t.update(start, 10_000_000, 0);
        t.update(start + Duration::from_secs(2), 20_000_000, 0);
        let r = t.update(start + Duration::from_secs(3), 0, 0);
        assert_eq!(r, Rates::default());
    }

    #[test]
    fn eta_math() {
        assert_eq!(eta(10_000, 1_000.0), Some(Duration::from_secs(10)));
        assert_eq!(eta(0, 1_000.0), None);
        assert_eq!(eta(10, 0.0), None);
    }
}
