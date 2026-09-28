//! Identifiers.
//!
//! Identifiers are opaque strings. They are validated on construction and on
//! deserialization so malformed input is rejected at the boundary instead of
//! deep inside routing or session code.

use core::fmt;

use serde::{Deserialize, Serialize};

/// Maximum length, in bytes, of a node/route/session identifier.
pub const MAX_ID_LEN: usize = 128;

/// Maximum length, in bytes, of a backend-native endpoint identifier.
///
/// WASAPI endpoint IDs are around 55 characters; the limit leaves room for
/// other backends while still bounding memory for untrusted input.
pub const MAX_NATIVE_ENDPOINT_ID_LEN: usize = 1024;

/// Why an identifier was rejected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IdError {
    Empty,
    TooLong { len: usize, max: usize },
    ControlCharacter,
}

impl fmt::Display for IdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IdError::Empty => f.write_str("identifier is empty"),
            IdError::TooLong { len, max } => {
                write!(f, "identifier is {len} bytes long; the maximum is {max}")
            }
            IdError::ControlCharacter => f.write_str("identifier contains a control character"),
        }
    }
}

impl std::error::Error for IdError {}

fn validate(value: &str, max: usize) -> Result<(), IdError> {
    if value.is_empty() {
        return Err(IdError::Empty);
    }
    if value.len() > max {
        return Err(IdError::TooLong {
            len: value.len(),
            max,
        });
    }
    if value.chars().any(char::is_control) {
        return Err(IdError::ControlCharacter);
    }
    Ok(())
}

macro_rules! string_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, IdError> {
                let value = value.into();
                validate(&value, MAX_ID_LEN)?;
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = IdError;
            fn try_from(value: String) -> Result<Self, IdError> {
                Self::new(value)
            }
        }

        impl From<$name> for String {
            fn from(id: $name) -> String {
                id.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

string_id!(
    /// Identifies an AudioNet node: one participating computer, phone,
    /// browser tab, or server instance.
    NodeId
);
string_id!(
    /// Identifies a logical audio route.
    RouteId
);
string_id!(
    /// Identifies one live stream session. A new session ID is issued
    /// whenever a stream restarts with a new, unrelated audio clock.
    SessionId
);

/// The platform audio API that produced an endpoint identifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioBackend {
    Wasapi,
    CoreAudio,
    PipeWire,
    AvAudioSession,
    AAudio,
    WebAudio,
}

impl AudioBackend {
    pub fn display_name(self) -> &'static str {
        match self {
            AudioBackend::Wasapi => "WASAPI",
            AudioBackend::CoreAudio => "Core Audio",
            AudioBackend::PipeWire => "PipeWire",
            AudioBackend::AvAudioSession => "AVAudioSession",
            AudioBackend::AAudio => "AAudio",
            AudioBackend::WebAudio => "Web Audio",
        }
    }
}

/// Identifies a local OS audio endpoint on one node.
///
/// `native_id` is the backend's own identifier (for WASAPI, the string from
/// `IMMDevice::GetId`). It may be persisted, but it is **not** guaranteed to
/// be permanent: driver updates and device reinstallation can change it.
/// Saved configuration must be resolved against the current inventory and
/// report an endpoint as missing rather than assume it still exists.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "RawEndpointId")]
pub struct EndpointId {
    backend: AudioBackend,
    native_id: String,
}

#[derive(Deserialize)]
struct RawEndpointId {
    backend: AudioBackend,
    native_id: String,
}

impl TryFrom<RawEndpointId> for EndpointId {
    type Error = IdError;
    fn try_from(raw: RawEndpointId) -> Result<Self, IdError> {
        EndpointId::new(raw.backend, raw.native_id)
    }
}

impl EndpointId {
    pub fn new(backend: AudioBackend, native_id: impl Into<String>) -> Result<Self, IdError> {
        let native_id = native_id.into();
        validate(&native_id, MAX_NATIVE_ENDPOINT_ID_LEN)?;
        Ok(Self { backend, native_id })
    }

    pub fn backend(&self) -> AudioBackend {
        self.backend
    }

    pub fn native_id(&self) -> &str {
        &self.native_id
    }
}

impl fmt::Display for EndpointId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.native_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_ids() {
        assert_eq!(NodeId::new(""), Err(IdError::Empty));
        assert_eq!(NodeId::new("a\nb"), Err(IdError::ControlCharacter));
        assert!(matches!(
            NodeId::new("x".repeat(MAX_ID_LEN + 1)),
            Err(IdError::TooLong { .. })
        ));
        assert!(NodeId::new("x".repeat(MAX_ID_LEN)).is_ok());
    }

    #[test]
    fn string_ids_serialize_as_plain_strings() {
        let id = RouteId::new("route-1").unwrap();
        assert_eq!(serde_json::to_string(&id).unwrap(), r#""route-1""#);
        let back: RouteId = serde_json::from_str(r#""route-1""#).unwrap();
        assert_eq!(back, id);
    }

    #[test]
    fn deserialization_validates() {
        assert!(serde_json::from_str::<SessionId>(r#""""#).is_err());
        assert!(
            serde_json::from_str::<EndpointId>(r#"{"backend":"wasapi","native_id":""}"#).is_err()
        );
    }

    #[test]
    fn endpoint_id_round_trips() {
        let id = EndpointId::new(
            AudioBackend::Wasapi,
            "{0.0.0.00000000}.{8f5a1c2e-0000-4000-8000-000000000001}",
        )
        .unwrap();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(
            json,
            r#"{"backend":"wasapi","native_id":"{0.0.0.00000000}.{8f5a1c2e-0000-4000-8000-000000000001}"}"#
        );
        assert_eq!(serde_json::from_str::<EndpointId>(&json).unwrap(), id);
    }
}
