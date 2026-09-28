//! A process-wide monotonic clock in nanoseconds.
//!
//! Network receive threads and audio render threads timestamp events on
//! this one timeline, so the playout stage can relate "when was the newest
//! frame written" to "when is the device asking for audio". It is backed by
//! `std::time::Instant` (QueryPerformanceCounter on Windows), which is
//! cheap, does not allocate, and is safe to call from real-time threads.

use std::sync::OnceLock;
use std::time::Instant;

fn origin() -> Instant {
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    *ORIGIN.get_or_init(Instant::now)
}

/// Nanoseconds since the first call in this process.
pub fn now_ns() -> u64 {
    origin().elapsed().as_nanos() as u64
}

/// Converts an `Instant` to this clock.
pub fn to_ns(instant: Instant) -> u64 {
    instant.saturating_duration_since(origin()).as_nanos() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monotonic() {
        let a = now_ns();
        let b = now_ns();
        assert!(b >= a);
        assert!(to_ns(Instant::now()) >= b);
    }
}
