//! Sequence tracking for one RTP stream (one SSRC).
//!
//! Extends 16-bit RTP sequence numbers to 64 bits across wraparound, and
//! classifies each arrival as in order, after a gap, reordered, or
//! duplicate. Thresholds follow RFC 3550 Appendix A.1: a jump of more than
//! [`MAX_DROPOUT`] ahead or [`MAX_MISORDER`] behind is treated as a
//! sequence reset (a restarted sender), not as loss.
//!
//! Loss here is *network* loss: sequence numbers never received. Packets
//! that arrive too late for playout are counted by the playout stage.

use serde::Serialize;

pub const MAX_DROPOUT: u64 = 3000;
pub const MAX_MISORDER: u64 = 100;
/// Duplicate-detection window, in packets.
const WINDOW: u64 = 1024;

/// How a packet relates to those before it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arrival {
    /// The next expected packet (or the first packet).
    InOrder,
    /// Ahead of the next expected packet; `missing` packets are not (yet) here.
    AfterGap { missing: u64 },
    /// Older than the highest seen, but not seen before.
    Reordered,
    /// Already received.
    Duplicate,
    /// Too far from the current position: the sender probably restarted.
    /// Tracking restarts from this packet.
    Reset,
}

/// Counters for one stream.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct SequenceStats {
    /// Unique packets received.
    pub received: u64,
    /// Packets expected from the first to the highest sequence number.
    pub expected: u64,
    pub reordered: u64,
    pub duplicates: u64,
    pub resets: u64,
}

impl SequenceStats {
    /// Packets never received (expected minus received, not below zero).
    pub fn lost(&self) -> u64 {
        self.expected.saturating_sub(self.received)
    }

    pub fn loss_percent(&self) -> f64 {
        if self.expected == 0 {
            0.0
        } else {
            self.lost() as f64 * 100.0 / self.expected as f64
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct SequenceTracker {
    first: Option<u64>,
    highest: u64,
    /// Bit i set: packet `highest - i` received.
    seen: [u64; (WINDOW / 64) as usize],
    stats: SequenceStats,
}

impl SequenceTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn stats(&self) -> SequenceStats {
        self.stats
    }

    /// Highest extended sequence number seen.
    pub fn highest(&self) -> Option<u64> {
        self.first.map(|_| self.highest)
    }

    /// Extends `seq` to the 64-bit value closest to the highest seen.
    pub fn extend(&self, seq: u16) -> u64 {
        if self.first.is_none() {
            return u64::from(seq) + (1 << 16); // headroom for early reordering
        }
        let base = self.highest & !0xFFFF;
        let candidates = [
            base.wrapping_sub(1 << 16) | u64::from(seq),
            base | u64::from(seq),
            (base + (1 << 16)) | u64::from(seq),
        ];
        *candidates
            .iter()
            .min_by_key(|c| c.abs_diff(self.highest))
            .expect("three candidates")
    }

    /// Records a packet. Returns its extended sequence number and how it arrived.
    pub fn on_packet(&mut self, seq: u16) -> (u64, Arrival) {
        let ext = self.extend(seq);
        let Some(first) = self.first else {
            self.start(ext);
            return (ext, Arrival::InOrder);
        };
        if ext > self.highest {
            let ahead = ext - self.highest;
            if ahead > MAX_DROPOUT {
                self.stats.resets += 1;
                self.restart(ext);
                return (ext, Arrival::Reset);
            }
            self.shift(ahead);
            self.highest = ext;
            self.set_seen(0);
            self.stats.received += 1;
            self.stats.expected = ext - first + 1;
            let arrival = if ahead == 1 {
                Arrival::InOrder
            } else {
                Arrival::AfterGap { missing: ahead - 1 }
            };
            return (ext, arrival);
        }
        let behind = self.highest - ext;
        if behind > MAX_MISORDER && ext + MAX_DROPOUT < self.highest {
            self.stats.resets += 1;
            self.restart(ext);
            return (ext, Arrival::Reset);
        }
        if behind >= WINDOW || ext < first {
            // Too old to tell; treat as a duplicate so it is not played.
            self.stats.duplicates += 1;
            return (ext, Arrival::Duplicate);
        }
        if self.is_seen(behind) {
            self.stats.duplicates += 1;
            (ext, Arrival::Duplicate)
        } else {
            self.set_seen(behind);
            self.stats.received += 1;
            self.stats.reordered += 1;
            (ext, Arrival::Reordered)
        }
    }

    fn start(&mut self, ext: u64) {
        self.first = Some(ext);
        self.highest = ext;
        self.seen = Default::default();
        self.set_seen(0);
        self.stats.received += 1;
        self.stats.expected += 1;
    }

    fn restart(&mut self, ext: u64) {
        // Keep cumulative counters; restart the position.
        let expected_so_far = self.stats.expected;
        let received_so_far = self.stats.received;
        self.first = Some(ext);
        self.highest = ext;
        self.seen = Default::default();
        self.set_seen(0);
        self.stats.received = received_so_far + 1;
        self.stats.expected = expected_so_far + 1;
        // `expected` is recomputed relative to the new first packet from
        // here on; fold the old total in by offsetting `first`.
        self.first = Some(ext - (expected_so_far));
    }

    fn shift(&mut self, by: u64) {
        if by >= WINDOW {
            self.seen = Default::default();
            return;
        }
        let words = (by / 64) as usize;
        let bits = (by % 64) as u32;
        let n = self.seen.len();
        for i in (0..n).rev() {
            let src = i.checked_sub(words);
            let mut v = src.map_or(0, |s| self.seen[s] << bits);
            if bits > 0 {
                if let Some(s) = src.and_then(|s| s.checked_sub(1)) {
                    v |= self.seen[s] >> (64 - bits);
                }
            }
            self.seen[i] = v;
        }
    }

    fn set_seen(&mut self, behind: u64) {
        self.seen[(behind / 64) as usize] |= 1 << (behind % 64);
    }

    fn is_seen(&self, behind: u64) -> bool {
        self.seen[(behind / 64) as usize] & (1 << (behind % 64)) != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(t: &mut SequenceTracker, seqs: &[u16]) -> Vec<Arrival> {
        seqs.iter().map(|&s| t.on_packet(s).1).collect()
    }

    #[test]
    fn in_order_across_wraparound() {
        let mut t = SequenceTracker::new();
        let seqs: Vec<u16> = (0..10).map(|i| 65530u16.wrapping_add(i)).collect();
        let arrivals = feed(&mut t, &seqs);
        assert!(arrivals.iter().all(|a| *a == Arrival::InOrder));
        let s = t.stats();
        assert_eq!((s.received, s.expected, s.lost()), (10, 10, 0));
        // Extended numbers keep increasing through the wrap.
        let a = t.extend(65535);
        let b = t.extend(3);
        assert_eq!(b, a + 4);
    }

    #[test]
    fn loss_reorder_duplicate() {
        let mut t = SequenceTracker::new();
        let arrivals = feed(&mut t, &[1, 2, 5, 3, 3, 6, 2]);
        assert_eq!(
            arrivals,
            [
                Arrival::InOrder,
                Arrival::InOrder,
                Arrival::AfterGap { missing: 2 },
                Arrival::Reordered,
                Arrival::Duplicate,
                Arrival::InOrder,
                Arrival::Duplicate,
            ]
        );
        let s = t.stats();
        assert_eq!(s.expected, 6);
        assert_eq!(s.received, 5);
        assert_eq!(s.lost(), 1, "packet 4 never arrived");
        assert_eq!((s.reordered, s.duplicates), (1, 2));
    }

    #[test]
    fn duplicate_detection_survives_window_shifts() {
        let mut t = SequenceTracker::new();
        for s in 0..300u16 {
            t.on_packet(s);
        }
        assert_eq!(t.on_packet(10).1, Arrival::Duplicate);
        assert_eq!(t.on_packet(250).1, Arrival::Duplicate);
        assert_eq!(t.stats().lost(), 0);
    }

    #[test]
    fn large_jump_is_a_reset_not_loss() {
        let mut t = SequenceTracker::new();
        feed(&mut t, &[100, 101, 102]);
        assert_eq!(t.on_packet(40_000).1, Arrival::Reset);
        assert_eq!(t.on_packet(40_001).1, Arrival::InOrder);
        let s = t.stats();
        assert_eq!(s.resets, 1);
        assert_eq!(
            s.lost(),
            0,
            "a restart is not counted as tens of thousands lost"
        );
        assert_eq!(s.received, 5);
    }

    #[test]
    fn loss_percent() {
        let mut t = SequenceTracker::new();
        for s in (0..100u16).filter(|s| s % 10 != 5) {
            t.on_packet(s);
        }
        let s = t.stats();
        assert_eq!(s.expected, 100);
        assert!((s.loss_percent() - 10.0).abs() < 1e-9);
    }
}
