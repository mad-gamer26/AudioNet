//! Backend traits and errors.

use core::fmt;

use audionet_protocol::AudioBackend;

use crate::inventory::EndpointInventory;

/// Options for endpoint enumeration.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EnumerationOptions {
    /// Include endpoints the OS remembers but whose hardware is absent.
    /// These are usually noise, so they are excluded by default.
    pub include_not_present: bool,
}

/// Lists a platform's audio endpoints.
///
/// Enumeration is a control-path operation: it may block, allocate and call
/// into the OS. It must never be called from an audio callback.
pub trait EndpointEnumerator {
    fn backend(&self) -> AudioBackend;

    /// Returns the current endpoints. A failure affecting only one endpoint
    /// should become an [`crate::EnumerationWarning`], not an error.
    fn enumerate(&self, options: EnumerationOptions) -> Result<EndpointInventory, BackendError>;
}

/// A failure of a whole backend operation.
///
/// `Display` names the failing subsystem and operation so a screen-reader
/// user hears what failed, not only an error code (AGENTS.md §23).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackendError {
    pub backend: AudioBackend,
    /// The operation that failed, e.g. "creating the device enumerator".
    pub operation: String,
    /// Backend-specific detail, e.g. an HRESULT and its message.
    pub detail: String,
}

impl fmt::Display for BackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} audio backend failed while {}: {}",
            self.backend.display_name(),
            self.operation,
            self.detail
        )
    }
}

impl std::error::Error for BackendError {}
