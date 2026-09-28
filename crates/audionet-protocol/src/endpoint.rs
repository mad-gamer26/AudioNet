//! Local OS audio endpoints.
//!
//! An *endpoint* is an OS-level audio input or output device on one node,
//! such as a WASAPI render endpoint or a PipeWire sink. It is not the same
//! thing as an AudioNet node (a whole computer or phone), nor as a source or
//! destination (roles an endpoint can play within a route).

use serde::{Deserialize, Serialize};

use crate::ids::EndpointId;

/// Whether an endpoint produces or consumes audio from the OS's point of view.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// A playback device: speakers, headphones, a virtual cable's input side.
    Output,
    /// A recording device: microphone, line in, a virtual cable's output side.
    Input,
}

impl Direction {
    pub fn label(self) -> &'static str {
        match self {
            Direction::Output => "Output",
            Direction::Input => "Input",
        }
    }
}

/// OS-reported availability of an endpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndpointState {
    /// Present and enabled; streams can be opened.
    Active,
    /// Present but disabled by the user or policy.
    Disabled,
    /// The adapter is present but nothing is plugged into the jack.
    Unplugged,
    /// The OS remembers the endpoint but its hardware is not present.
    NotPresent,
}

impl EndpointState {
    pub fn label(self) -> &'static str {
        match self {
            EndpointState::Active => "Active",
            EndpointState::Disabled => "Disabled",
            EndpointState::Unplugged => "Unplugged",
            EndpointState::NotPresent => "Not present",
        }
    }
}

/// OS default-device roles an endpoint currently holds.
///
/// These mirror WASAPI's `ERole`; other backends map what they can and
/// usually report only `Console`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DefaultRole {
    Console,
    Multimedia,
    Communications,
}

impl DefaultRole {
    pub fn label(self) -> &'static str {
        match self {
            DefaultRole::Console => "console",
            DefaultRole::Multimedia => "multimedia",
            DefaultRole::Communications => "communications",
        }
    }
}

/// How samples are encoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SampleEncoding {
    Float,
    /// Signed integer PCM (unsigned for 8-bit, per the WAVE convention).
    Integer,
    /// A compressed or otherwise unrecognized encoding.
    Other,
}

/// Sample encoding plus bit depth.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SampleFormat {
    pub encoding: SampleEncoding,
    /// Bits each sample occupies in memory.
    pub container_bits: u16,
    /// Bits of real precision within the container, e.g. 24 in a 32-bit slot.
    pub valid_bits: u16,
}

impl SampleFormat {
    pub const F32: SampleFormat = SampleFormat {
        encoding: SampleEncoding::Float,
        container_bits: 32,
        valid_bits: 32,
    };

    /// A short human-readable description, e.g. "32-bit float" or
    /// "24-bit integer in 32-bit container".
    pub fn describe(&self) -> String {
        let kind = match self.encoding {
            SampleEncoding::Float => "float",
            SampleEncoding::Integer => "integer",
            SampleEncoding::Other => "unrecognized encoding",
        };
        if self.valid_bits == self.container_bits || self.valid_bits == 0 {
            format!("{}-bit {kind}", self.container_bits)
        } else {
            format!(
                "{}-bit {kind} in {}-bit container",
                self.valid_bits, self.container_bits
            )
        }
    }
}

/// The format an endpoint's shared-mode audio engine uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DeviceFormat {
    pub sample_rate_hz: u32,
    pub channels: u16,
    pub sample_format: SampleFormat,
    /// Speaker-position bitmask (WAVE `dwChannelMask`), if the backend reports one.
    pub channel_mask: Option<u32>,
}

/// What is known about capturing an endpoint's playback ("loopback").
///
/// These are deliberately separate facts, in increasing strength of
/// evidence. Routine enumeration only ever reports the first three; the
/// stronger values are produced by explicitly opening a loopback stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoopbackSupport {
    /// Loopback only applies to output endpoints.
    NotApplicable,
    /// An output endpoint that is not active cannot be captured right now.
    EndpointNotActive,
    /// The backend documents loopback support for this kind of endpoint,
    /// but nothing has tried it.
    Expected,
    /// A loopback client was created and initialized successfully.
    ClientInitialized,
    /// Loopback capture actually delivered audio buffers.
    CaptureVerified,
    /// An attempt to initialize or run loopback capture failed.
    Failed,
}

impl LoopbackSupport {
    pub fn label(self) -> &'static str {
        match self {
            LoopbackSupport::NotApplicable => "Not applicable to input endpoints",
            LoopbackSupport::EndpointNotActive => "Unavailable, endpoint is not active",
            LoopbackSupport::Expected => "Expected, not yet verified",
            LoopbackSupport::ClientInitialized => "Client initialized, capture not yet verified",
            LoopbackSupport::CaptureVerified => "Verified by capture",
            LoopbackSupport::Failed => "Failed",
        }
    }
}

/// A description of one local OS audio endpoint.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointDescriptor {
    pub id: EndpointId,
    pub direction: Direction,
    /// Full display name, e.g. "Speakers (Realtek Audio)".
    pub name: String,
    /// Endpoint description without the adapter, e.g. "Speakers".
    pub description: Option<String>,
    /// The adapter or driver name, e.g. "Realtek Audio".
    pub adapter: Option<String>,
    pub state: EndpointState,
    /// Default roles held by this endpoint, in `DefaultRole` order. Empty if none.
    pub default_roles: Vec<DefaultRole>,
    /// Shared-mode engine format, if it could be read.
    pub format: Option<DeviceFormat>,
    pub loopback: LoopbackSupport,
}

impl EndpointDescriptor {
    pub fn is_default(&self) -> bool {
        !self.default_roles.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::AudioBackend;

    fn sample() -> EndpointDescriptor {
        EndpointDescriptor {
            id: EndpointId::new(AudioBackend::Wasapi, "{0.0.0.00000000}.{a}").unwrap(),
            direction: Direction::Output,
            name: "Speakers (Example Audio)".into(),
            description: None,
            adapter: Some("Example Audio".into()),
            state: EndpointState::Active,
            default_roles: vec![],
            format: None,
            loopback: LoopbackSupport::Expected,
        }
    }

    #[test]
    fn absent_values_serialize_as_null_not_omitted() {
        let value = serde_json::to_value(sample()).unwrap();
        let obj = value.as_object().unwrap();
        for key in [
            "id",
            "direction",
            "name",
            "description",
            "adapter",
            "state",
            "default_roles",
            "format",
            "loopback",
        ] {
            assert!(obj.contains_key(key), "missing key {key}");
        }
        assert!(obj["description"].is_null());
        assert!(obj["format"].is_null());
        assert_eq!(obj["default_roles"], serde_json::json!([]));
        assert_eq!(obj["loopback"], "expected");
        assert_eq!(obj["state"], "active");
    }

    #[test]
    fn round_trips() {
        let mut e = sample();
        e.format = Some(DeviceFormat {
            sample_rate_hz: 48_000,
            channels: 2,
            sample_format: SampleFormat::F32,
            channel_mask: Some(0x3),
        });
        e.default_roles = vec![DefaultRole::Console, DefaultRole::Multimedia];
        let json = serde_json::to_string(&e).unwrap();
        assert_eq!(
            serde_json::from_str::<EndpointDescriptor>(&json).unwrap(),
            e
        );
    }

    #[test]
    fn describes_sample_formats() {
        assert_eq!(SampleFormat::F32.describe(), "32-bit float");
        let s24 = SampleFormat {
            encoding: SampleEncoding::Integer,
            container_bits: 32,
            valid_bits: 24,
        };
        assert_eq!(s24.describe(), "24-bit integer in 32-bit container");
    }
}
