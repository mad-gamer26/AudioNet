//! Shared AudioNet protocol types.
//!
//! This crate defines the platform-neutral vocabulary that every AudioNet
//! component agrees on: identifiers, node and endpoint descriptions, the
//! logical source → route → destination model, and stream session metadata.
//!
//! It deliberately contains no I/O, no threads and no platform code, so it can
//! be used from any backend, from tests, and eventually from non-desktop
//! targets. Serialization conventions are documented in `docs/protocol.md`:
//!
//! * field and enum names are `snake_case`;
//! * every field is always serialized; an absent value is an explicit `null`,
//!   never an omitted key;
//! * collections are always present, possibly empty.
//!
//! The wire format for real-time media packets is intentionally not defined
//! yet; see the UDP/WebRTC transport discussion in `docs/architecture.md`.

#![forbid(unsafe_code)]

pub mod endpoint;
pub mod ids;
pub mod node;
pub mod route;
pub mod session;
pub mod signal;
pub mod version;

pub use endpoint::{
    DefaultRole, DeviceFormat, Direction, EndpointDescriptor, EndpointState, LoopbackSupport,
    SampleEncoding, SampleFormat,
};
pub use ids::{AudioBackend, EndpointId, IdError, NodeId, RouteId, SessionId};
pub use node::{NodeDescriptor, Platform};
pub use route::{AudioRoute, DestinationKind, DestinationRef, RouteError, SourceKind, SourceRef};
pub use session::{Codec, FrameDuration, StreamFormat, StreamFormatError, StreamSessionDescriptor};
pub use version::{PROTOCOL_VERSION, ProtocolVersion};
