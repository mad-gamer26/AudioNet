//! Sender core: 48 kHz stream frames → Opus → RTP → encrypted datagram.
//!
//! Runs on the dedicated encoder thread, never in a capture callback. Input
//! arrives in arbitrary block sizes and is accumulated into codec frames in
//! a preallocated buffer; each finished frame is encoded, framed, sealed and
//! handed to `emit` (normally a non-blocking UDP send).

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::Instant;

use audionet_codec::{CodecError, EncoderConfig, MAX_OPUS_PACKET, OpusEncoder};
use audionet_transport::MAX_DATAGRAM;
use audionet_transport::crypto::MediaCipher;
use audionet_transport::rtp::{PT_OPUS, RtpHeader};
use serde::Serialize;

use crate::stats::{DurationSnapshot, DurationStat};

#[derive(Debug, Default)]
pub struct SenderStats {
    pub frames_encoded: AtomicU64,
    pub packets: AtomicU64,
    pub bytes: AtomicU64,
    pub encode_errors: AtomicU64,
    pub send_errors: AtomicU64,
    pub send_would_block: AtomicU64,
    pub encode: DurationStat,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct SenderSnapshot {
    pub frames_encoded: u64,
    pub packets: u64,
    pub bytes: u64,
    pub encode_errors: u64,
    pub send_errors: u64,
    pub send_would_block: u64,
    pub encode_ns: DurationSnapshot,
}

impl SenderStats {
    pub fn snapshot(&self) -> SenderSnapshot {
        SenderSnapshot {
            frames_encoded: self.frames_encoded.load(Relaxed),
            packets: self.packets.load(Relaxed),
            bytes: self.bytes.load(Relaxed),
            encode_errors: self.encode_errors.load(Relaxed),
            send_errors: self.send_errors.load(Relaxed),
            send_would_block: self.send_would_block.load(Relaxed),
            encode_ns: self.encode.snapshot(),
        }
    }
}

pub struct SenderCore {
    config: EncoderConfig,
    encoder: OpusEncoder,
    cipher: MediaCipher,
    ssrc: u32,
    sequence: u16,
    timestamp: u32,
    frame: Vec<f32>,
    fill: usize,
    opus: Vec<u8>,
    packet: Vec<u8>,
    stats: Arc<SenderStats>,
}

impl std::fmt::Debug for SenderCore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SenderCore")
            .field("ssrc", &self.ssrc)
            .field("sequence", &self.sequence)
            .finish_non_exhaustive()
    }
}

impl SenderCore {
    /// Starts a new stream with a random SSRC, sequence number and
    /// timestamp (RFC 3550 §5.1). Control path: allocates.
    pub fn new(config: EncoderConfig, cipher: MediaCipher) -> Result<Self, CodecError> {
        let encoder = OpusEncoder::new(&config)?;
        let frame_len = encoder.frame_samples() * usize::from(config.format.channels);
        let mut r = [0u8; 10];
        rand::fill(&mut r);
        Ok(Self {
            config,
            encoder,
            cipher,
            ssrc: u32::from_le_bytes([r[0], r[1], r[2], r[3]]),
            sequence: u16::from_le_bytes([r[4], r[5]]),
            timestamp: u32::from_le_bytes([r[6], r[7], r[8], r[9]]),
            frame: vec![0.0; frame_len],
            fill: 0,
            opus: vec![0; MAX_OPUS_PACKET],
            packet: vec![0; MAX_DATAGRAM],
            stats: Arc::new(SenderStats::default()),
        })
    }

    pub fn ssrc(&self) -> u32 {
        self.ssrc
    }

    pub fn config(&self) -> &EncoderConfig {
        &self.config
    }

    pub fn stats(&self) -> &Arc<SenderStats> {
        &self.stats
    }

    /// Adds interleaved stream-format samples; emits one datagram per
    /// completed frame.
    pub fn push(&mut self, mut samples: &[f32], mut emit: impl FnMut(&[u8])) {
        while !samples.is_empty() {
            let take = (self.frame.len() - self.fill).min(samples.len());
            self.frame[self.fill..self.fill + take].copy_from_slice(&samples[..take]);
            self.fill += take;
            samples = &samples[take..];
            if self.fill == self.frame.len() {
                self.fill = 0;
                if let Some(len) = self.encode_frame() {
                    emit(&self.packet[..len]);
                }
            }
        }
    }

    fn encode_frame(&mut self) -> Option<usize> {
        let start = Instant::now();
        let encoded = self.encoder.encode(&self.frame, &mut self.opus);
        self.stats.encode.record(start.elapsed().as_nanos() as u64);
        let frame_samples = self.encoder.frame_samples() as u32;
        let header = RtpHeader {
            marker: self.stats.frames_encoded.load(Relaxed) == 0,
            payload_type: PT_OPUS,
            sequence: self.sequence,
            timestamp: self.timestamp,
            ssrc: self.ssrc,
        };
        // Sequence and timestamp advance even if encoding fails, so the
        // receiver sees the gap as loss rather than a time jump.
        self.sequence = self.sequence.wrapping_add(1);
        self.timestamp = self.timestamp.wrapping_add(frame_samples);
        self.stats.frames_encoded.fetch_add(1, Relaxed);
        let len = match encoded {
            Ok(len) => len,
            Err(_) => {
                self.stats.encode_errors.fetch_add(1, Relaxed);
                return None;
            }
        };
        let sealed = self
            .cipher
            .seal(&header, &self.opus[..len], &mut self.packet)
            .ok()?;
        self.stats.packets.fetch_add(1, Relaxed);
        self.stats.bytes.fetch_add(sealed as u64, Relaxed);
        Some(sealed)
    }
}
