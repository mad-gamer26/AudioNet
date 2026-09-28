//! Opus encoding and decoding for AudioNet, wrapping libopus (via the
//! `opus` crate, which builds libopus from source).
//!
//! Both sides work on fixed-size frames of interleaved `f32` at 48 kHz and
//! write into caller-provided buffers, so steady-state operation does not
//! allocate. Codec calls run on the encoder and receive threads, never in an
//! audio device callback.
//!
//! # Low delay versus in-band FEC
//!
//! AGENTS.md prefers `OPUS_APPLICATION_RESTRICTED_LOWDELAY` and asks for
//! in-band FEC where appropriate. These conflict: restricted low delay is
//! CELT-only, and Opus in-band FEC (LBRR) only exists in the SILK layer. So
//! [`EncoderMode::LowDelay`] relies on Opus packet loss concealment, and
//! [`EncoderMode::Resilient`] uses the normal audio application with in-band
//! FEC, at the cost of about 4 ms more codec lookahead. The decoder handles
//! both: [`OpusDecoder::decode_fec`] recovers a lost frame from the next
//! packet's redundancy when present, and falls back to PLC otherwise.

#![forbid(unsafe_code)]

use core::fmt;

use audionet_protocol::{Codec, StreamFormat};
use opus::{Application, Bitrate, Channels};
use serde::Serialize;

/// Maximum Opus packet size AudioNet produces. Keeps an encrypted RTP
/// datagram well under the ~1200-byte MTU-safe target (AGENTS.md §8).
pub const MAX_OPUS_PACKET: usize = 1000;

/// Longest Opus frame: 120 ms at 48 kHz, in samples per channel.
pub const MAX_FRAME_SAMPLES: usize = 5760;

/// How the encoder trades delay against loss resilience.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EncoderMode {
    /// CELT-only restricted low delay; losses are concealed with PLC.
    LowDelay,
    /// Normal audio application with in-band FEC.
    Resilient,
}

/// Encoder settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct EncoderConfig {
    pub format: StreamFormat,
    pub mode: EncoderMode,
    pub bitrate_bps: i32,
    /// Expected loss percentage, used to size in-band FEC in resilient mode.
    pub expected_loss_percent: i32,
}

impl EncoderConfig {
    /// AGENTS.md baseline: 48 kHz stereo, 10 ms, low delay, 128 kbit/s.
    pub const BASELINE: EncoderConfig = EncoderConfig {
        format: StreamFormat::BASELINE,
        mode: EncoderMode::LowDelay,
        bitrate_bps: 128_000,
        expected_loss_percent: 0,
    };
}

/// A codec failure, naming the operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodecError {
    pub operation: &'static str,
    pub detail: String,
}

impl fmt::Display for CodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Opus codec failed while {}: {}",
            self.operation, self.detail
        )
    }
}

impl std::error::Error for CodecError {}

fn err(operation: &'static str) -> impl Fn(opus::Error) -> CodecError {
    move |e| CodecError {
        operation,
        detail: e.to_string(),
    }
}

fn channels(format: &StreamFormat) -> Result<Channels, CodecError> {
    format.validate().map_err(|e| CodecError {
        operation: "checking the stream format",
        detail: e.to_string(),
    })?;
    let Codec::Opus = format.codec;
    Ok(if format.channels == 1 {
        Channels::Mono
    } else {
        Channels::Stereo
    })
}

/// An Opus encoder for one stream.
pub struct OpusEncoder {
    inner: opus::Encoder,
    frame_samples: usize,
    channels: usize,
    lookahead_samples: u32,
}

impl fmt::Debug for OpusEncoder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpusEncoder")
            .field("frame_samples", &self.frame_samples)
            .field("channels", &self.channels)
            .finish_non_exhaustive()
    }
}

impl OpusEncoder {
    pub fn new(config: &EncoderConfig) -> Result<Self, CodecError> {
        let ch = channels(&config.format)?;
        let app = match config.mode {
            EncoderMode::LowDelay => Application::LowDelay,
            EncoderMode::Resilient => Application::Audio,
        };
        let mut inner = opus::Encoder::new(config.format.sample_rate_hz, ch, app)
            .map_err(err("creating the encoder"))?;
        inner
            .set_bitrate(Bitrate::Bits(config.bitrate_bps))
            .map_err(err("setting the bitrate"))?;
        if config.mode == EncoderMode::Resilient {
            inner
                .set_inband_fec(true)
                .map_err(err("enabling in-band FEC"))?;
            inner
                .set_packet_loss_perc(config.expected_loss_percent.clamp(0, 100))
                .map_err(err("setting the expected loss"))?;
        }
        let lookahead_samples = inner
            .get_lookahead()
            .map_err(err("reading the lookahead"))?
            .max(0) as u32;
        let frame_samples = config
            .format
            .samples_per_frame()
            .ok_or_else(|| CodecError {
                operation: "checking the frame size",
                detail: "frame duration is not a whole number of samples".into(),
            })? as usize;
        Ok(Self {
            inner,
            frame_samples,
            channels: usize::from(config.format.channels),
            lookahead_samples,
        })
    }

    /// Samples per channel in one frame (480 for 10 ms at 48 kHz).
    pub fn frame_samples(&self) -> usize {
        self.frame_samples
    }

    /// Codec algorithmic delay in samples per channel.
    pub fn lookahead_samples(&self) -> u32 {
        self.lookahead_samples
    }

    /// Encodes one frame of interleaved samples (`frame_samples * channels`
    /// long) into `out`. Returns the packet length.
    pub fn encode(&mut self, pcm: &[f32], out: &mut [u8]) -> Result<usize, CodecError> {
        debug_assert_eq!(pcm.len(), self.frame_samples * self.channels);
        let limit = out.len().min(MAX_OPUS_PACKET);
        self.inner
            .encode_float(pcm, &mut out[..limit])
            .map_err(err("encoding a frame"))
    }

    /// Updates the loss estimate (resilient mode sizes FEC from it).
    pub fn set_expected_loss(&mut self, percent: i32) -> Result<(), CodecError> {
        self.inner
            .set_packet_loss_perc(percent.clamp(0, 100))
            .map_err(err("setting the expected loss"))
    }
}

/// An Opus decoder for one stream. Output buffers must hold one frame.
pub struct OpusDecoder {
    inner: opus::Decoder,
    frame_samples: usize,
    channels: usize,
}

impl fmt::Debug for OpusDecoder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpusDecoder")
            .field("frame_samples", &self.frame_samples)
            .field("channels", &self.channels)
            .finish_non_exhaustive()
    }
}

impl OpusDecoder {
    pub fn new(format: &StreamFormat) -> Result<Self, CodecError> {
        let ch = channels(format)?;
        let frame_samples = format.samples_per_frame().ok_or_else(|| CodecError {
            operation: "checking the frame size",
            detail: "frame duration is not a whole number of samples".into(),
        })? as usize;
        Ok(Self {
            inner: opus::Decoder::new(format.sample_rate_hz, ch)
                .map_err(err("creating the decoder"))?,
            frame_samples,
            channels: usize::from(format.channels),
        })
    }

    pub fn frame_samples(&self) -> usize {
        self.frame_samples
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Interleaved samples in one frame.
    pub fn frame_len(&self) -> usize {
        self.frame_samples * self.channels
    }

    /// Decodes a packet into `out` (at least one frame long). Returns
    /// samples per channel written.
    pub fn decode(&mut self, packet: &[u8], out: &mut [f32]) -> Result<usize, CodecError> {
        if packet.is_empty() {
            return self.conceal(out);
        }
        self.inner
            .decode_float(packet, out, false)
            .map_err(err("decoding a packet"))
    }

    /// Produces one frame of packet-loss concealment.
    pub fn conceal(&mut self, out: &mut [f32]) -> Result<usize, CodecError> {
        self.conceal_samples(out, self.frame_samples)
    }

    /// Conceals `samples` per channel: the lost packet's duration, which can
    /// differ from the configured frame size (browsers send 20 ms frames).
    pub fn conceal_samples(
        &mut self,
        out: &mut [f32],
        samples: usize,
    ) -> Result<usize, CodecError> {
        let n = samples.min(MAX_FRAME_SAMPLES) * self.channels;
        self.inner
            .decode_float(&[], &mut out[..n], false)
            .map_err(err("concealing a lost packet"))
    }

    /// Recovers the frame *before* `next_packet` from its in-band FEC data,
    /// or conceals it if the packet carries none.
    pub fn decode_fec(&mut self, next_packet: &[u8], out: &mut [f32]) -> Result<usize, CodecError> {
        self.decode_fec_samples(next_packet, out, self.frame_samples)
    }

    /// Like [`decode_fec`](Self::decode_fec) for a lost packet of `samples` per channel.
    pub fn decode_fec_samples(
        &mut self,
        next_packet: &[u8],
        out: &mut [f32],
        samples: usize,
    ) -> Result<usize, CodecError> {
        let n = samples.min(MAX_FRAME_SAMPLES) * self.channels;
        self.inner
            .decode_float(next_packet, &mut out[..n], true)
            .map_err(err("recovering a lost packet from FEC"))
    }

    /// Resets decoder state, e.g. for a new stream session.
    pub fn reset(&mut self) -> Result<(), CodecError> {
        self.inner
            .reset_state()
            .map_err(err("resetting the decoder"))
    }
}

/// The libopus version string.
pub fn libopus_version() -> &'static str {
    opus::version()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(frames: usize, channels: usize, start: usize, freq: f32, amp: f32) -> Vec<f32> {
        (0..frames)
            .flat_map(|i| {
                let v = amp
                    * (2.0 * core::f32::consts::PI * freq * (start + i) as f32 / 48_000.0).sin();
                core::iter::repeat_n(v, channels)
            })
            .collect()
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
    }

    #[test]
    fn round_trip_preserves_a_tone() {
        for mode in [EncoderMode::LowDelay, EncoderMode::Resilient] {
            let config = EncoderConfig {
                mode,
                ..EncoderConfig::BASELINE
            };
            let mut enc = OpusEncoder::new(&config).unwrap();
            let mut dec = OpusDecoder::new(&config.format).unwrap();
            assert_eq!(enc.frame_samples(), 480);
            let mut packet = [0u8; MAX_OPUS_PACKET];
            let mut out = vec![0.0f32; dec.frame_len()];
            let mut decoded = Vec::new();
            for f in 0..50 {
                let pcm = sine(480, 2, f * 480, 997.0, 0.25);
                let n = enc.encode(&pcm, &mut packet).unwrap();
                assert!(n > 0 && n < 400, "packet size {n}");
                let got = dec.decode(&packet[..n], &mut out).unwrap();
                assert_eq!(got, 480);
                decoded.extend_from_slice(&out);
            }
            // After codec startup the level matches the input (0.25 peak sine ≈ 0.177 RMS).
            let level = rms(&decoded[20 * 960..]);
            assert!((level - 0.177).abs() < 0.02, "{mode:?}: rms {level}");
        }
    }

    #[test]
    fn concealment_produces_a_full_frame_that_decays() {
        let mut enc = OpusEncoder::new(&EncoderConfig::BASELINE).unwrap();
        let mut dec = OpusDecoder::new(&StreamFormat::BASELINE).unwrap();
        let mut packet = [0u8; MAX_OPUS_PACKET];
        let mut out = vec![0.0f32; 960];
        for f in 0..20 {
            let n = enc
                .encode(&sine(480, 2, f * 480, 440.0, 0.3), &mut packet)
                .unwrap();
            dec.decode(&packet[..n], &mut out).unwrap();
        }
        let mut levels = Vec::new();
        for _ in 0..30 {
            assert_eq!(dec.conceal(&mut out).unwrap(), 480);
            assert!(out.iter().all(|v| v.is_finite()));
            levels.push(rms(&out));
        }
        assert!(
            levels[0] > 0.05,
            "first concealed frame continues the signal"
        );
        assert!(levels[29] < levels[0] * 0.5, "long concealment fades");
    }

    #[test]
    fn fec_recovers_in_resilient_mode() {
        let config = EncoderConfig {
            mode: EncoderMode::Resilient,
            expected_loss_percent: 20,
            ..EncoderConfig::BASELINE
        };
        let mut enc = OpusEncoder::new(&config).unwrap();
        let mut dec = OpusDecoder::new(&config.format).unwrap();
        let mut packets = Vec::new();
        let mut buf = [0u8; MAX_OPUS_PACKET];
        for f in 0..40 {
            let n = enc
                .encode(&sine(480, 2, f * 480, 300.0, 0.3), &mut buf)
                .unwrap();
            packets.push(buf[..n].to_vec());
        }
        let mut out = vec![0.0f32; 960];
        for p in &packets[..30] {
            dec.decode(p, &mut out).unwrap();
        }
        // Packet 30 lost: recover it from packet 31's FEC.
        assert_eq!(dec.decode_fec(&packets[31], &mut out).unwrap(), 480);
        assert!(rms(&out) > 0.05);
        dec.decode(&packets[31], &mut out).unwrap();
    }

    #[test]
    fn rejects_non_opus_formats() {
        let mut f = StreamFormat::BASELINE;
        f.sample_rate_hz = 44_100;
        assert!(OpusDecoder::new(&f).is_err());
        assert!(!libopus_version().is_empty());
    }
}
