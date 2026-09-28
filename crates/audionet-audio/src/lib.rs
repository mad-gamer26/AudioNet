//! Platform-neutral AudioNet audio abstractions.
//!
//! Platform backends (`audionet-wasapi`, and later Core Audio, PipeWire,
//! mobile and browser backends) implement the traits here, and higher layers
//! (CLI, routing, sessions) depend only on this crate.
//!
//! Real-time building blocks live here too: the bounded capture ring
//! ([`ring`]), sample conversion ([`pcm`]) and callback timing
//! ([`timing`]). Anything called from an audio callback is documented as
//! real-time safe: no allocation, no locks, no waiting, no I/O.
//!
//! The playout buffer, drift estimator and depth controller are not written
//! yet: AGENTS.md §33 requires researching established implementations
//! first. See `docs/architecture.md`.

#![forbid(unsafe_code)]

pub mod backend;
pub mod capture;
pub mod clock;
pub mod gain;
pub mod inventory;
pub mod levels;
pub mod pcm;
pub mod render;
pub mod resolve;
pub mod ring;
pub mod threads;
pub mod timing;

pub use backend::{BackendError, EndpointEnumerator, EnumerationOptions};
pub use inventory::{EndpointInventory, EnumerationWarning};
pub use resolve::{EndpointResolution, SavedEndpointRef, resolve_saved_endpoint};
