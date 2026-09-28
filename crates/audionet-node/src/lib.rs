//! The AudioNet node agent.
//!
//! A *node* is a device that owns audio sources and destinations. The agent
//! signs the device in to an account on a coordination server (any server:
//! the URL is configuration), keeps a signaling connection open, advertises
//! the device's endpoints, and answers WebRTC sessions:
//!
//! * [`config`]: saved server URL and device credential.
//! * [`account`]: sign in with the account password to add this device.
//! * [`agent`]: signaling loop with reconnection.
//! * [`session`]: one WebRTC (str0m) media session per thread.
//! * [`audio`]: the interface each platform implements.
//! * [`stun`]: a minimal STUN Binding client for server-reflexive candidates.

#![forbid(unsafe_code)]

pub mod account;
pub mod agent;
pub mod audio;
pub mod config;
#[cfg(not(windows))]
pub mod cpal_audio;
pub mod mdns;
pub mod relay;
pub mod session;
pub mod stun;
#[cfg(windows)]
pub mod windows_audio;
