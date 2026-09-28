//! Small lock-free helpers for diagnostics shared across threads.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// An `f64` stored in an `AtomicU64` (relaxed; for diagnostics only).
#[derive(Debug, Default)]
pub struct AtomicF64(AtomicU64);

impl AtomicF64 {
    pub fn store(&self, v: f64) {
        self.0.store(v.to_bits(), Relaxed);
    }

    pub fn load(&self) -> f64 {
        f64::from_bits(self.0.load(Relaxed))
    }
}

/// Lifetime and per-window maximum of a nanosecond duration, plus a sum and
/// count for the average. Written by one thread, read by a diagnostics thread.
#[derive(Debug, Default)]
pub struct DurationStat {
    count: AtomicU64,
    sum_ns: AtomicU64,
    max_ns: AtomicU64,
    window_max_ns: AtomicU64,
}

/// Snapshot of a [`DurationStat`]; `None` until something is recorded.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct DurationSnapshot {
    pub count: u64,
    pub avg_ns: Option<u64>,
    pub max_ns: Option<u64>,
    pub window_max_ns: Option<u64>,
}

impl DurationStat {
    pub fn record(&self, ns: u64) {
        self.count.fetch_add(1, Relaxed);
        self.sum_ns.fetch_add(ns, Relaxed);
        self.max_ns.fetch_max(ns, Relaxed);
        self.window_max_ns.fetch_max(ns, Relaxed);
    }

    /// Resets the window maximum; use one reader.
    pub fn snapshot(&self) -> DurationSnapshot {
        let count = self.count.load(Relaxed);
        let window = self.window_max_ns.swap(0, Relaxed);
        DurationSnapshot {
            count,
            avg_ns: (count > 0).then(|| self.sum_ns.load(Relaxed) / count),
            max_ns: (count > 0).then(|| self.max_ns.load(Relaxed)),
            window_max_ns: (window > 0).then_some(window),
        }
    }
}

/// Current value plus lifetime and per-window minimum and maximum.
#[derive(Debug)]
pub struct GaugeStat {
    current: AtomicU64,
    min: AtomicU64,
    max: AtomicU64,
    window_min: AtomicU64,
    window_max: AtomicU64,
}

impl Default for GaugeStat {
    fn default() -> Self {
        Self {
            current: AtomicU64::new(0),
            min: AtomicU64::new(u64::MAX),
            max: AtomicU64::new(0),
            window_min: AtomicU64::new(u64::MAX),
            window_max: AtomicU64::new(0),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct GaugeSnapshot {
    pub current: u64,
    pub min: Option<u64>,
    pub max: Option<u64>,
    pub window_min: Option<u64>,
    pub window_max: Option<u64>,
}

impl GaugeStat {
    pub fn record(&self, v: u64) {
        self.current.store(v, Relaxed);
        self.min.fetch_min(v, Relaxed);
        self.max.fetch_max(v, Relaxed);
        self.window_min.fetch_min(v, Relaxed);
        self.window_max.fetch_max(v, Relaxed);
    }

    /// Resets the window extremes; use one reader.
    pub fn snapshot(&self) -> GaugeSnapshot {
        let min = self.min.load(Relaxed);
        let wmin = self.window_min.swap(u64::MAX, Relaxed);
        let wmax = self.window_max.swap(0, Relaxed);
        GaugeSnapshot {
            current: self.current.load(Relaxed),
            min: (min != u64::MAX).then_some(min),
            max: (min != u64::MAX).then(|| self.max.load(Relaxed)),
            window_min: (wmin != u64::MAX).then_some(wmin),
            window_max: (wmin != u64::MAX).then_some(wmax),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_and_gauge() {
        let d = DurationStat::default();
        assert_eq!(d.snapshot().avg_ns, None);
        d.record(10);
        d.record(30);
        let s = d.snapshot();
        assert_eq!(
            (s.count, s.avg_ns, s.max_ns, s.window_max_ns),
            (2, Some(20), Some(30), Some(30))
        );
        assert_eq!(d.snapshot().window_max_ns, None);

        let g = GaugeStat::default();
        assert_eq!(g.snapshot().min, None);
        g.record(5);
        g.record(2);
        g.record(9);
        let s = g.snapshot();
        assert_eq!((s.current, s.min, s.max), (9, Some(2), Some(9)));
        assert_eq!((s.window_min, s.window_max), (Some(2), Some(9)));
        g.record(4);
        let s = g.snapshot();
        assert_eq!(
            (s.window_min, s.window_max, s.min),
            (Some(4), Some(4), Some(2))
        );

        let f = AtomicF64::default();
        f.store(-1.5);
        assert_eq!(f.load(), -1.5);
    }
}
