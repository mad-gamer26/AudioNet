//! AudioNet media transport building blocks.
//!
//! * [`rtp`]: RTP (RFC 3550) header serialization and parsing. AudioNet
//!   media packets are RTP packets carrying Opus (RFC 7587), so the framing
//!   stays compatible with a later move to SRTP/WebRTC.
//! * [`seq`]: extended sequence numbers and loss, reorder, duplicate and
//!   late-packet accounting.
//! * [`crypto`]: per-packet authenticated encryption for the pre-shared-key
//!   LAN mode.
//!
//! All of this is sans-I/O: no sockets, threads or clocks, so it is fully
//! deterministic in tests. Socket handling lives in `audionet-engine`.

#![forbid(unsafe_code)]

pub mod crypto;
pub mod rtp;
pub mod seq;

/// Upper bound on an AudioNet media datagram (AGENTS.md §8: stay below
/// ~1200 bytes to avoid IP fragmentation).
pub const MAX_DATAGRAM: usize = 1200;
