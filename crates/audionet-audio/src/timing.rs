//! Lightweight callback timing measurement.
//!
//! [`CallbackTimingRecorder`] lives on the real-time thread and records each
//! wakeup with a few relaxed atomic operations (no allocation, no locks).
//! [`CallbackTimingStats::snapshot`] is read from a diagnostics thread.
//!
//! Timestamps are plain nanosecond counts from any monotonic origin, so the
//! logic is deterministic in tests.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

use serde::Serialize;

/// Shared timing counters for one callback stream.
#[derive(Debug)]
pub struct CallbackTimingStats {
    nominal_period_ns: u64,
    callbacks: AtomicU64,
    idle_wakeups: AtomicU64,
    intervals: AtomicU64,
    interval_sum_ns: AtomicU64,
    interval_min_ns: AtomicU64,
    interval_max_ns: AtomicU64,
    window_interval_max_ns: AtomicU64,
    late_callbacks: AtomicU64,
    work_sum_ns: AtomicU64,
    work_max_ns: AtomicU64,
    window_work_max_ns: AtomicU64,
}

/// A point-in-time copy of [`CallbackTimingStats`]. Durations are in
/// nanoseconds; `None` where nothing has been measured yet.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct TimingSnapshot {
    pub nominal_period_ns: u64,
    /// Wakeups that processed audio.
    pub callbacks: u64,
    /// Wakeups (event or timeout) that found no audio.
    pub idle_wakeups: u64,
    pub interval_avg_ns: Option<u64>,
    pub interval_min_ns: Option<u64>,
    pub interval_max_ns: Option<u64>,
    /// Largest interval since the previous snapshot.
    pub window_interval_max_ns: Option<u64>,
    /// Intervals longer than twice the nominal period.
    pub late_callbacks: u64,
    pub work_avg_ns: Option<u64>,
    pub work_max_ns: Option<u64>,
    /// Largest work time since the previous snapshot.
    pub window_work_max_ns: Option<u64>,
}

impl CallbackTimingStats {
    /// Allocates the shared stats and the recorder for the real-time thread.
    /// Call on the control path.
    pub fn new(nominal_period_ns: u64) -> (Arc<Self>, CallbackTimingRecorder) {
        let stats = Arc::new(Self {
            nominal_period_ns,
            callbacks: AtomicU64::new(0),
            idle_wakeups: AtomicU64::new(0),
            intervals: AtomicU64::new(0),
            interval_sum_ns: AtomicU64::new(0),
            interval_min_ns: AtomicU64::new(u64::MAX),
            interval_max_ns: AtomicU64::new(0),
            window_interval_max_ns: AtomicU64::new(0),
            late_callbacks: AtomicU64::new(0),
            work_sum_ns: AtomicU64::new(0),
            work_max_ns: AtomicU64::new(0),
            window_work_max_ns: AtomicU64::new(0),
        });
        let recorder = CallbackTimingRecorder {
            stats: Arc::clone(&stats),
            last_callback_ns: None,
        };
        (stats, recorder)
    }

    /// Takes a snapshot. Resets per-window maxima; use one reader.
    pub fn snapshot(&self) -> TimingSnapshot {
        let callbacks = self.callbacks.load(Relaxed);
        let intervals = self.intervals.load(Relaxed);
        let nonzero = |v: u64| (v != 0).then_some(v);
        let window_interval = self.window_interval_max_ns.swap(0, Relaxed);
        let window_work = self.window_work_max_ns.swap(0, Relaxed);
        TimingSnapshot {
            nominal_period_ns: self.nominal_period_ns,
            callbacks,
            idle_wakeups: self.idle_wakeups.load(Relaxed),
            interval_avg_ns: (intervals > 0)
                .then(|| self.interval_sum_ns.load(Relaxed) / intervals),
            interval_min_ns: (intervals > 0).then(|| self.interval_min_ns.load(Relaxed)),
            interval_max_ns: (intervals > 0).then(|| self.interval_max_ns.load(Relaxed)),
            window_interval_max_ns: nonzero(window_interval),
            late_callbacks: self.late_callbacks.load(Relaxed),
            work_avg_ns: (callbacks > 0).then(|| self.work_sum_ns.load(Relaxed) / callbacks),
            work_max_ns: (callbacks > 0).then(|| self.work_max_ns.load(Relaxed)),
            window_work_max_ns: nonzero(window_work),
        }
    }
}

/// The real-time side of timing measurement. Not shared; owned by the
/// thread that runs the callback.
#[derive(Debug)]
pub struct CallbackTimingRecorder {
    stats: Arc<CallbackTimingStats>,
    last_callback_ns: Option<u64>,
}

impl CallbackTimingRecorder {
    /// Records a wakeup that processed audio. `wake_ns` is when the thread
    /// woke; `work_ns` is how long processing took.
    pub fn record_callback(&mut self, wake_ns: u64, work_ns: u64) {
        let s = &*self.stats;
        s.callbacks.fetch_add(1, Relaxed);
        s.work_sum_ns.fetch_add(work_ns, Relaxed);
        s.work_max_ns.fetch_max(work_ns, Relaxed);
        s.window_work_max_ns.fetch_max(work_ns, Relaxed);
        if let Some(last) = self.last_callback_ns {
            let interval = wake_ns.saturating_sub(last);
            s.intervals.fetch_add(1, Relaxed);
            s.interval_sum_ns.fetch_add(interval, Relaxed);
            s.interval_min_ns.fetch_min(interval, Relaxed);
            s.interval_max_ns.fetch_max(interval, Relaxed);
            s.window_interval_max_ns.fetch_max(interval, Relaxed);
            if s.nominal_period_ns > 0 && interval > 2 * s.nominal_period_ns {
                s.late_callbacks.fetch_add(1, Relaxed);
            }
        }
        self.last_callback_ns = Some(wake_ns);
    }

    /// Records a wakeup that found no audio (for loopback, normal while the
    /// endpoint plays nothing).
    pub fn record_idle(&mut self) {
        self.stats.idle_wakeups.fetch_add(1, Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: u64 = 1_000_000;

    #[test]
    fn steady_callbacks() {
        let (stats, mut rec) = CallbackTimingStats::new(10 * MS);
        for i in 0..100 {
            rec.record_callback(i * 10 * MS, MS / 10);
        }
        let s = stats.snapshot();
        assert_eq!(s.callbacks, 100);
        assert_eq!(s.interval_avg_ns, Some(10 * MS));
        assert_eq!(s.interval_min_ns, Some(10 * MS));
        assert_eq!(s.interval_max_ns, Some(10 * MS));
        assert_eq!(s.late_callbacks, 0);
        assert_eq!(s.work_avg_ns, Some(MS / 10));
    }

    #[test]
    fn detects_a_stall_as_a_late_gap() {
        let (stats, mut rec) = CallbackTimingStats::new(10 * MS);
        rec.record_callback(0, 1);
        rec.record_callback(10 * MS, 1);
        rec.record_callback(55 * MS, 3 * MS); // 45 ms stall
        rec.record_callback(65 * MS, 1);
        let s = stats.snapshot();
        assert_eq!(s.interval_max_ns, Some(45 * MS));
        assert_eq!(s.window_interval_max_ns, Some(45 * MS));
        assert_eq!(s.late_callbacks, 1);
        assert_eq!(s.work_max_ns, Some(3 * MS));
    }

    #[test]
    fn window_maxima_reset_but_lifetime_maxima_persist() {
        let (stats, mut rec) = CallbackTimingStats::new(10 * MS);
        rec.record_callback(0, 1);
        rec.record_callback(30 * MS, 5);
        stats.snapshot();
        rec.record_callback(40 * MS, 2);
        let s = stats.snapshot();
        assert_eq!(s.window_interval_max_ns, Some(10 * MS));
        assert_eq!(s.window_work_max_ns, Some(2));
        assert_eq!(s.interval_max_ns, Some(30 * MS));
        assert_eq!(s.work_max_ns, Some(5));
    }

    #[test]
    fn idle_wakeups_do_not_affect_intervals() {
        let (stats, mut rec) = CallbackTimingStats::new(10 * MS);
        rec.record_idle();
        rec.record_idle();
        let s = stats.snapshot();
        assert_eq!(s.idle_wakeups, 2);
        assert_eq!(s.callbacks, 0);
        assert_eq!(s.interval_avg_ns, None);
        assert_eq!(s.work_max_ns, None);
    }
}
