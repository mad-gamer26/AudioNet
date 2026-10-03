//! macOS Core Audio pieces that cpal does not offer. Like
//! `audionet-wasapi`, a platform backend crate: the only place (with it)
//! where AudioNet itself writes `unsafe`. Empty on other platforms.
//!
//! [`OutputMute`]: while AudioNet streams what an output device plays
//! ("system audio"), keep that sound off the device itself. AudioNet does
//! not touch the device's volume or mute (the ones in Control Center and
//! System Settings): it creates its own private Core Audio process tap on
//! the device with the "muted" behaviour, which stops every other app's
//! sound from reaching the hardware while the tap exists. Microphones and
//! other inputs are never involved. AudioNet's own sound (another device
//! it plays here) is left out of the tap, so it still plays. The tap
//! belongs to AudioNet's process: if AudioNet quits or crashes, macOS
//! removes it and the sound comes back.
//!
//! Threads: Core Audio's tap calls are made from whichever thread holds an
//! [`OutputMute`] (the thread that owns the capture stream), never from an
//! audio callback. No COM-like apartment rules apply on macOS.

#[cfg(target_os = "macos")]
mod mute;

#[cfg(target_os = "macos")]
pub use mute::OutputMute;
