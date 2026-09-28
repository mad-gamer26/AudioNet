//! Protocol versioning.

use core::fmt;

use serde::{Deserialize, Serialize};

/// The protocol version implemented by this build.
///
/// While `major` is 0 the protocol is unstable: peers must match both `major`
/// and `minor` exactly. From 1.0 onward, peers with the same `major` are
/// compatible and `minor` only adds optional behavior.
pub const PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion { major: 0, minor: 1 };

/// A `major.minor` protocol version.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ProtocolVersion {
    pub major: u16,
    pub minor: u16,
}

impl ProtocolVersion {
    /// Whether a peer speaking `other` can interoperate with `self`.
    pub fn is_compatible_with(self, other: ProtocolVersion) -> bool {
        if self.major != other.major {
            return false;
        }
        // Pre-1.0: every minor revision may break the format.
        self.major != 0 || self.minor == other.minor
    }
}

impl fmt::Display for ProtocolVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn v(major: u16, minor: u16) -> ProtocolVersion {
        ProtocolVersion { major, minor }
    }

    #[test]
    fn pre_1_0_requires_exact_match() {
        assert!(v(0, 1).is_compatible_with(v(0, 1)));
        assert!(!v(0, 1).is_compatible_with(v(0, 2)));
    }

    #[test]
    fn post_1_0_matches_on_major() {
        assert!(v(1, 0).is_compatible_with(v(1, 7)));
        assert!(!v(1, 0).is_compatible_with(v(2, 0)));
    }

    #[test]
    fn serializes_as_object() {
        let json = serde_json::to_string(&v(0, 1)).unwrap();
        assert_eq!(json, r#"{"major":0,"minor":1}"#);
        assert_eq!(PROTOCOL_VERSION.to_string(), "0.1");
    }
}
