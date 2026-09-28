//! Platform hooks for audio-related threads.
//!
//! Engine threads (sender/encoder, network receive, WebRTC sessions) are
//! platform-neutral, but on some platforms they should join a scheduling
//! class for audio work (on Windows, MMCSS "Audio"). A [`ThreadSetup`] runs
//! at the start of such a thread and returns a guard that is kept until the
//! thread ends.

use std::sync::Arc;

/// Called at the start of an audio-related thread; the returned guard lives
/// as long as the thread.
pub type ThreadSetup = Arc<dyn Fn() -> Box<dyn std::any::Any> + Send + Sync>;

/// Runs `setup` if present, returning the guard to hold.
pub fn enter(setup: &Option<ThreadSetup>) -> Option<Box<dyn std::any::Any>> {
    setup.as_ref().map(|f| f())
}
