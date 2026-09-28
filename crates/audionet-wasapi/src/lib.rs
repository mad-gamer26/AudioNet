//! AudioNet's Windows audio backend, built on WASAPI and the MMDevice API.
//!
//! Current scope: endpoint enumeration and event-driven capture (input
//! endpoints and output loopback) into a bounded ring. Render comes next.
//! See `docs/windows-audio.md` for the design, COM threading assumptions,
//! and the loopback-probing policy.
//!
//! # Unsafe code
//!
//! All `unsafe` is confined to the `cfg(windows)` modules `capture`, `com`,
//! `enumerate`, `process` and `render`, and every block carries a `SAFETY:`
//! comment. Parsing of Windows data structures (`waveformat`) is safe Rust
//! over byte slices so it can be tested deterministically on any platform.

pub mod waveformat;

#[cfg(windows)]
pub mod capture;
#[cfg(windows)]
mod com;
#[cfg(windows)]
mod enumerate;
#[cfg(windows)]
pub mod process;
#[cfg(windows)]
pub mod render;

use audionet_audio::{BackendError, EndpointEnumerator, EndpointInventory, EnumerationOptions};
use audionet_protocol::AudioBackend;

/// Enumerates Windows audio endpoints through the MMDevice API.
///
/// Each call initializes COM on the calling thread for its own duration
/// (multithreaded apartment, or the thread's existing apartment if it already
/// has one), so it may be called from any non-real-time thread.
#[derive(Clone, Copy, Debug, Default)]
pub struct WasapiEnumerator;

impl EndpointEnumerator for WasapiEnumerator {
    fn backend(&self) -> AudioBackend {
        AudioBackend::Wasapi
    }

    fn enumerate(&self, options: EnumerationOptions) -> Result<EndpointInventory, BackendError> {
        #[cfg(windows)]
        {
            enumerate::enumerate(options)
        }
        #[cfg(not(windows))]
        {
            let _ = options;
            Err(BackendError {
                backend: AudioBackend::Wasapi,
                operation: "starting".into(),
                detail: "WASAPI is only available on Windows".into(),
            })
        }
    }
}
