//! Bounded, preallocated single-producer/single-consumer audio ring.
//!
//! Built on `rtrb`, a lock-free SPSC ring buffer: the producer (a capture
//! thread) and consumer (an encoder or test consumer) never lock or wait on
//! each other. Storage is allocated once, when the ring is created.
//!
//! Samples are interleaved `f32` and always moved in whole frames, so
//! channels cannot get out of step.
//!
//! # Overflow policy
//!
//! Each side only discards audio it owns:
//!
//! * **Producer (real-time):** if the whole incoming block does not fit, the
//!   block is dropped. The producer never blocks and never touches the read
//!   position. Counted as *ring overflow*, and a discontinuity is flagged for
//!   the consumer.
//! * **Consumer (non-real-time):** [`RingConsumer::trim_stale`] discards the
//!   *oldest* audio when depth exceeds a threshold, returning to the live
//!   edge. Counted as *stale trim*.
//!
//! Together these implement AGENTS.md §6 ("prefer dropping the oldest stale
//! audio") without a lock or a shared read index: in normal operation the
//! consumer trims long before the ring is full, and producer drops only
//! happen when the consumer has stalled completely.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

use serde::Serialize;

/// Counters shared by both ends of a ring. Written with relaxed atomics;
/// read by a diagnostics thread through [`RingStats::snapshot`].
#[derive(Debug)]
pub struct RingStats {
    channels: u64,
    capacity_frames: u64,
    frames_written: AtomicU64,
    frames_read: AtomicU64,
    overflow_events: AtomicU64,
    overflow_frames: AtomicU64,
    stale_trim_events: AtomicU64,
    stale_trim_frames: AtomicU64,
    max_depth_frames: AtomicU64,
    window_max_depth_frames: AtomicU64,
}

/// A point-in-time copy of [`RingStats`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct RingSnapshot {
    pub capacity_frames: u64,
    pub depth_frames: u64,
    pub max_depth_frames: u64,
    /// Maximum depth since the previous snapshot.
    pub window_max_depth_frames: u64,
    pub frames_written: u64,
    pub frames_read: u64,
    pub overflow_events: u64,
    pub overflow_frames: u64,
    pub stale_trim_events: u64,
    pub stale_trim_frames: u64,
}

impl RingStats {
    /// Takes a snapshot. Resets the per-window maximum, so there should be
    /// one diagnostics reader.
    pub fn snapshot(&self) -> RingSnapshot {
        let written = self.frames_written.load(Relaxed);
        let read = self.frames_read.load(Relaxed);
        let trimmed = self.stale_trim_frames.load(Relaxed);
        RingSnapshot {
            capacity_frames: self.capacity_frames,
            // Relaxed loads can be momentarily inconsistent; saturate.
            depth_frames: written.saturating_sub(read + trimmed),
            max_depth_frames: self.max_depth_frames.load(Relaxed),
            window_max_depth_frames: self.window_max_depth_frames.swap(0, Relaxed),
            frames_written: written,
            frames_read: read,
            overflow_events: self.overflow_events.load(Relaxed),
            overflow_frames: self.overflow_frames.load(Relaxed),
            stale_trim_events: self.stale_trim_events.load(Relaxed),
            stale_trim_frames: trimmed,
        }
    }

    pub fn channels(&self) -> usize {
        self.channels as usize
    }
}

/// Result of a producer write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteOutcome {
    Written,
    /// The block did not fit and was discarded.
    DroppedOverflow,
}

/// The real-time end of the ring.
#[derive(Debug)]
pub struct RingProducer {
    inner: rtrb::Producer<f32>,
    channels: usize,
    stats: Arc<RingStats>,
    discontinuity: Arc<AtomicBool>,
}

/// The non-real-time end of the ring.
#[derive(Debug)]
pub struct RingConsumer {
    inner: rtrb::Consumer<f32>,
    channels: usize,
    stats: Arc<RingStats>,
    discontinuity: Arc<AtomicBool>,
}

/// Creates a ring holding `capacity_frames` frames of `channels` channels.
///
/// Allocates; call on the control path, never from an audio callback.
///
/// # Panics
///
/// If `channels` or `capacity_frames` is zero.
pub fn audio_ring(capacity_frames: usize, channels: usize) -> (RingProducer, RingConsumer) {
    assert!(channels > 0 && capacity_frames > 0, "empty audio ring");
    let (producer, consumer) = rtrb::RingBuffer::new(capacity_frames * channels);
    let stats = Arc::new(RingStats {
        channels: channels as u64,
        capacity_frames: capacity_frames as u64,
        frames_written: AtomicU64::new(0),
        frames_read: AtomicU64::new(0),
        overflow_events: AtomicU64::new(0),
        overflow_frames: AtomicU64::new(0),
        stale_trim_events: AtomicU64::new(0),
        stale_trim_frames: AtomicU64::new(0),
        max_depth_frames: AtomicU64::new(0),
        window_max_depth_frames: AtomicU64::new(0),
    });
    let discontinuity = Arc::new(AtomicBool::new(false));
    (
        RingProducer {
            inner: producer,
            channels,
            stats: Arc::clone(&stats),
            discontinuity: Arc::clone(&discontinuity),
        },
        RingConsumer {
            inner: consumer,
            channels,
            stats,
            discontinuity,
        },
    )
}

impl RingProducer {
    /// Writes a block of whole interleaved frames, or drops all of it if it
    /// does not fit. Real-time safe: no allocation, no locks, no waiting.
    pub fn write(&mut self, samples: &[f32]) -> WriteOutcome {
        debug_assert_eq!(samples.len() % self.channels, 0, "partial frame");
        let frames = (samples.len() / self.channels) as u64;
        if frames == 0 {
            return WriteOutcome::Written;
        }
        match self.inner.push_entire_slice(samples) {
            Ok(()) => {
                self.stats.frames_written.fetch_add(frames, Relaxed);
                let capacity = self.inner.buffer().capacity();
                let depth = ((capacity - self.inner.slots()) / self.channels) as u64;
                self.stats.max_depth_frames.fetch_max(depth, Relaxed);
                self.stats.window_max_depth_frames.fetch_max(depth, Relaxed);
                WriteOutcome::Written
            }
            Err(_) => {
                self.stats.overflow_events.fetch_add(1, Relaxed);
                self.stats.overflow_frames.fetch_add(frames, Relaxed);
                self.discontinuity.store(true, Relaxed);
                WriteOutcome::DroppedOverflow
            }
        }
    }

    pub fn stats(&self) -> &Arc<RingStats> {
        &self.stats
    }

    /// Frames currently queued for the consumer. Real-time safe.
    pub fn depth_frames(&self) -> usize {
        (self.inner.buffer().capacity() - self.inner.slots()) / self.channels
    }
}

impl RingConsumer {
    pub fn channels(&self) -> usize {
        self.channels
    }

    pub fn available_frames(&self) -> usize {
        self.inner.slots() / self.channels
    }

    /// Reads up to `out.len() / channels` whole frames into the start of
    /// `out`. Returns the number of frames read.
    pub fn read(&mut self, out: &mut [f32]) -> usize {
        let frames = self.available_frames().min(out.len() / self.channels);
        if frames == 0 {
            return 0;
        }
        let n = frames * self.channels;
        let (filled, _) = self.inner.pop_partial_slice(&mut out[..n]);
        debug_assert_eq!(filled.len(), n);
        self.stats.frames_read.fetch_add(frames as u64, Relaxed);
        frames
    }

    /// If more than `threshold_frames` are buffered, discards the oldest
    /// audio so that `keep_frames` remain. Returns frames discarded.
    pub fn trim_stale(&mut self, threshold_frames: usize, keep_frames: usize) -> usize {
        let available = self.available_frames();
        if available <= threshold_frames {
            return 0;
        }
        let drop = available - keep_frames.min(available);
        if drop == 0 {
            return 0;
        }
        match self.inner.read_chunk(drop * self.channels) {
            Ok(chunk) => chunk.commit_all(),
            Err(_) => return 0,
        }
        self.stats.stale_trim_events.fetch_add(1, Relaxed);
        self.stats.stale_trim_frames.fetch_add(drop as u64, Relaxed);
        self.discontinuity.store(true, Relaxed);
        drop
    }

    /// Returns whether audio was dropped (overflow or trim) since the last
    /// call, and clears the flag.
    pub fn take_discontinuity(&self) -> bool {
        self.discontinuity.swap(false, Relaxed)
    }

    /// Whether the producer has been dropped (its stream ended).
    pub fn is_producer_gone(&self) -> bool {
        self.inner.is_abandoned()
    }

    pub fn stats(&self) -> &Arc<RingStats> {
        &self.stats
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames(n: usize, channels: usize, start: f32) -> Vec<f32> {
        (0..n * channels).map(|i| start + i as f32).collect()
    }

    #[test]
    fn round_trips_whole_frames_in_order() {
        let (mut p, mut c) = audio_ring(8, 2);
        assert_eq!(p.write(&frames(3, 2, 0.0)), WriteOutcome::Written);
        let mut out = [0.0; 16];
        assert_eq!(c.read(&mut out), 3);
        assert_eq!(&out[..6], &[0.0, 1.0, 2.0, 3.0, 4.0, 5.0]);
        let s = c.stats().snapshot();
        assert_eq!((s.frames_written, s.frames_read, s.depth_frames), (3, 3, 0));
    }

    #[test]
    fn read_never_splits_a_frame() {
        let (mut p, mut c) = audio_ring(8, 2);
        p.write(&frames(4, 2, 0.0));
        let mut out = [0.0; 3]; // room for 1.5 frames
        assert_eq!(c.read(&mut out), 1);
        assert_eq!(c.available_frames(), 3);
    }

    #[test]
    fn overflow_drops_the_whole_incoming_block_and_counts_it() {
        let (mut p, mut c) = audio_ring(4, 2);
        assert_eq!(p.write(&frames(3, 2, 0.0)), WriteOutcome::Written);
        // 2 more frames do not fit in the 1 free frame: dropped entirely.
        assert_eq!(p.write(&frames(2, 2, 100.0)), WriteOutcome::DroppedOverflow);
        let s = p.stats().snapshot();
        assert_eq!((s.overflow_events, s.overflow_frames), (1, 2));
        assert_eq!(s.frames_written, 3);
        assert!(c.take_discontinuity());
        assert!(!c.take_discontinuity(), "flag clears after reading");
        // Existing audio is intact and in order.
        let mut out = [0.0; 8];
        assert_eq!(c.read(&mut out), 3);
        assert_eq!(out[0], 0.0);
        assert_eq!(out[5], 5.0);
    }

    #[test]
    fn exactly_full_is_not_overflow() {
        let (mut p, _c) = audio_ring(4, 2);
        assert_eq!(p.write(&frames(4, 2, 0.0)), WriteOutcome::Written);
        let s = p.stats().snapshot();
        assert_eq!(s.overflow_events, 0);
        assert_eq!(s.max_depth_frames, 4);
    }

    #[test]
    fn producer_recovers_after_consumer_drains() {
        let (mut p, mut c) = audio_ring(4, 1);
        p.write(&frames(4, 1, 0.0));
        assert_eq!(p.write(&[9.0]), WriteOutcome::DroppedOverflow);
        let mut out = [0.0; 4];
        c.read(&mut out);
        assert_eq!(p.write(&[9.0]), WriteOutcome::Written);
    }

    #[test]
    fn stale_trim_discards_oldest_and_keeps_newest() {
        let (mut p, mut c) = audio_ring(16, 1);
        p.write(&frames(10, 1, 0.0));
        assert_eq!(c.trim_stale(12, 2), 0, "below threshold: untouched");
        assert_eq!(c.trim_stale(8, 2), 8);
        let mut out = [0.0; 4];
        assert_eq!(c.read(&mut out), 2);
        assert_eq!(&out[..2], &[8.0, 9.0], "newest audio kept");
        let s = c.stats().snapshot();
        assert_eq!((s.stale_trim_events, s.stale_trim_frames), (1, 8));
        assert_eq!(s.overflow_events, 0, "trim is not counted as overflow");
        assert_eq!(s.depth_frames, 0);
        assert!(c.take_discontinuity());
    }

    #[test]
    fn window_max_depth_resets_per_snapshot() {
        let (mut p, mut c) = audio_ring(8, 1);
        p.write(&frames(6, 1, 0.0));
        let mut out = [0.0; 8];
        c.read(&mut out);
        let first = c.stats().snapshot();
        assert_eq!(first.window_max_depth_frames, 6);
        let second = c.stats().snapshot();
        assert_eq!(second.window_max_depth_frames, 0);
        assert_eq!(second.max_depth_frames, 6, "lifetime maximum is kept");
    }

    #[test]
    fn memory_stays_bounded_under_a_stalled_consumer() {
        let (mut p, c) = audio_ring(480, 2);
        let block = frames(480, 2, 0.0);
        for _ in 0..10_000 {
            p.write(&block[..96 * 2]);
        }
        assert!(c.available_frames() <= 480);
        let s = p.stats().snapshot();
        assert_eq!(s.frames_written + s.overflow_frames, 10_000 * 96);
    }

    #[test]
    fn detects_abandoned_producer() {
        let (p, c) = audio_ring(4, 1);
        assert!(!c.is_producer_gone());
        drop(p);
        assert!(c.is_producer_gone());
    }
}
