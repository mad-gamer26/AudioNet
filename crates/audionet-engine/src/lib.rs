//! The AudioNet media engine.
//!
//! Sans-I/O cores (driven with explicit timestamps, so they run the same in
//! real threads and in the deterministic simulator):
//!
//! * [`sender::SenderCore`]: 48 kHz stereo frames → Opus → RTP → encrypted datagram.
//! * [`receiver::PacketStage`]: datagram → authenticate → sequence/reorder →
//!   Opus decode with FEC/PLC → playout ring. Runs on the network thread.
//! * [`playout::Playout`]: playout ring → drift/depth-controlled resampler
//!   → device frames. Runs in the render callback.
//! * [`controller::DriftDepthController`]: the clock-drift and depth loops.
//! * [`adaptive::AdaptiveTarget`]: the adaptive playout target.

#![forbid(unsafe_code)]

pub mod adapt;
pub mod adaptive;
pub mod controller;
pub mod playout;
pub mod receiver;
pub mod runtime;
pub mod sender;
pub mod stats;
