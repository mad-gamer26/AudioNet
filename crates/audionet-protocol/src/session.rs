//! Stream session metadata.
//!
//! A *stream session* is the runtime instance of media flowing for a route.
//! It carries its own audio clock: when a source restarts, it gets a new
//! [`SessionId`], and receivers must discard drift and timing state from the
//! old session rather than carry it over (AGENTS.md §38).

use core::fmt;

use serde::{Deserialize, Serialize};

use crate::ids::{NodeId, RouteId, SessionId};
use crate::version::ProtocolVersion;

/// Media codec for a stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Codec {
    Opus,
}

/// Duration of one codec frame. Serialized as an integer number of
/// microseconds (`10000` for 10 ms); only Opus-legal durations are accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "u32", into = "u32")]
pub enum FrameDuration {
    Ms2_5,
    Ms5,
    Ms10,
    Ms20,
    Ms40,
    Ms60,
}

impl FrameDuration {
    pub const fn micros(self) -> u32 {
        match self {
            FrameDuration::Ms2_5 => 2_500,
            FrameDuration::Ms5 => 5_000,
            FrameDuration::Ms10 => 10_000,
            FrameDuration::Ms20 => 20_000,
            FrameDuration::Ms40 => 40_000,
            FrameDuration::Ms60 => 60_000,
        }
    }

    /// Samples per channel in one frame at `sample_rate_hz`, or `None` if the
    /// duration is not a whole number of samples at that rate.
    pub const fn samples_per_frame(self, sample_rate_hz: u32) -> Option<u32> {
        let product = sample_rate_hz as u64 * self.micros() as u64;
        if product % 1_000_000 == 0 {
            Some((product / 1_000_000) as u32)
        } else {
            None
        }
    }
}

impl TryFrom<u32> for FrameDuration {
    type Error = StreamFormatError;
    fn try_from(micros: u32) -> Result<Self, StreamFormatError> {
        Ok(match micros {
            2_500 => FrameDuration::Ms2_5,
            5_000 => FrameDuration::Ms5,
            10_000 => FrameDuration::Ms10,
            20_000 => FrameDuration::Ms20,
            40_000 => FrameDuration::Ms40,
            60_000 => FrameDuration::Ms60,
            _ => return Err(StreamFormatError::UnsupportedFrameDuration { micros }),
        })
    }
}

impl From<FrameDuration> for u32 {
    fn from(d: FrameDuration) -> u32 {
        d.micros()
    }
}

/// Why a stream format is not usable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StreamFormatError {
    UnsupportedFrameDuration { micros: u32 },
    UnsupportedSampleRate { sample_rate_hz: u32 },
    UnsupportedChannelCount { channels: u16 },
}

impl fmt::Display for StreamFormatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StreamFormatError::UnsupportedFrameDuration { micros } => {
                write!(f, "unsupported frame duration of {micros} microseconds")
            }
            StreamFormatError::UnsupportedSampleRate { sample_rate_hz } => {
                write!(f, "unsupported sample rate of {sample_rate_hz} Hz")
            }
            StreamFormatError::UnsupportedChannelCount { channels } => {
                write!(f, "unsupported channel count of {channels}")
            }
        }
    }
}

impl std::error::Error for StreamFormatError {}

/// The media format of a stream session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StreamFormat {
    pub codec: Codec,
    pub sample_rate_hz: u32,
    pub channels: u16,
    /// Serialized as `frame_duration_us` so the unit is explicit on the wire.
    #[serde(rename = "frame_duration_us")]
    pub frame_duration: FrameDuration,
}

impl StreamFormat {
    /// The first-milestone baseline from AGENTS.md §42: Opus, 48 kHz,
    /// stereo, 10 ms frames. Not a claim that these values are optimal.
    pub const BASELINE: StreamFormat = StreamFormat {
        codec: Codec::Opus,
        sample_rate_hz: 48_000,
        channels: 2,
        frame_duration: FrameDuration::Ms10,
    };

    pub fn validate(&self) -> Result<(), StreamFormatError> {
        match self.codec {
            Codec::Opus => {
                // AudioNet always runs Opus at its native 48 kHz; the other
                // Opus rates exist for narrowband telephony.
                if self.sample_rate_hz != 48_000 {
                    return Err(StreamFormatError::UnsupportedSampleRate {
                        sample_rate_hz: self.sample_rate_hz,
                    });
                }
                // More than two channels needs the Opus multistream API,
                // which is out of scope for now.
                if !(1..=2).contains(&self.channels) {
                    return Err(StreamFormatError::UnsupportedChannelCount {
                        channels: self.channels,
                    });
                }
            }
        }
        Ok(())
    }

    /// Samples per channel in one frame.
    pub fn samples_per_frame(&self) -> Option<u32> {
        self.frame_duration.samples_per_frame(self.sample_rate_hz)
    }
}

/// Describes one live stream session as negotiated between nodes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamSessionDescriptor {
    pub session_id: SessionId,
    pub route_id: RouteId,
    /// The node whose clock drives this session's audio.
    pub source_node: NodeId,
    pub format: StreamFormat,
    pub protocol_version: ProtocolVersion,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baseline_is_valid_and_480_samples() {
        assert_eq!(StreamFormat::BASELINE.validate(), Ok(()));
        assert_eq!(StreamFormat::BASELINE.samples_per_frame(), Some(480));
    }

    #[test]
    fn frame_sizes_at_48k() {
        let expected = [
            (FrameDuration::Ms2_5, 120),
            (FrameDuration::Ms5, 240),
            (FrameDuration::Ms10, 480),
            (FrameDuration::Ms20, 960),
            (FrameDuration::Ms40, 1920),
            (FrameDuration::Ms60, 2880),
        ];
        for (d, n) in expected {
            assert_eq!(d.samples_per_frame(48_000), Some(n), "{d:?}");
        }
        // 2.5 ms at 44.1 kHz is 110.25 samples: not a whole frame.
        assert_eq!(FrameDuration::Ms2_5.samples_per_frame(44_100), None);
    }

    #[test]
    fn frame_duration_serializes_as_micros() {
        assert_eq!(
            serde_json::to_string(&FrameDuration::Ms10).unwrap(),
            "10000"
        );
        assert_eq!(
            serde_json::from_str::<FrameDuration>("2500").unwrap(),
            FrameDuration::Ms2_5
        );
        assert!(serde_json::from_str::<FrameDuration>("7000").is_err());
    }

    #[test]
    fn rejects_unsupported_formats() {
        let mut f = StreamFormat::BASELINE;
        f.sample_rate_hz = 44_100;
        assert!(matches!(
            f.validate(),
            Err(StreamFormatError::UnsupportedSampleRate { .. })
        ));
        let mut f = StreamFormat::BASELINE;
        f.channels = 6;
        assert!(matches!(
            f.validate(),
            Err(StreamFormatError::UnsupportedChannelCount { .. })
        ));
    }

    #[test]
    fn format_json_shape() {
        assert_eq!(
            serde_json::to_value(StreamFormat::BASELINE).unwrap(),
            serde_json::json!({
                "codec": "opus",
                "sample_rate_hz": 48000,
                "channels": 2,
                "frame_duration_us": 10000
            })
        );
    }
}
