//! The audio interface a platform provides to the node agent.
//!
//! The agent is platform-neutral; Windows implements this with WASAPI (see
//! the `audionet` CLI), and other platforms will implement it with their
//! own backends.

use audionet_audio::render::RenderSource;
use audionet_audio::ring::RingConsumer;
use audionet_protocol::signal::{DestinationInfo, SourceInfo};

/// How a [`StreamGuard::failure`] starts when the device is still there
/// but its format changed (an iPhone switching to a Bluetooth microphone
/// at 16 kHz): the stream is reopened at once, not after the retry delay
/// for a device that went away.
pub const FORMAT_CHANGED: &str = "the audio format changed";

/// Keeps an open capture or render stream alive and reports failures.
pub trait StreamGuard: Send {
    /// If the stream has stopped (device removed, format changed...),
    /// returns why, in words. Non-blocking.
    fn failure(&mut self) -> Option<String>;

    /// A capture stream's running totals, where the platform counts them:
    /// what the device delivered, silence filled in for it, and its
    /// glitches. Read from a non-real-time thread (atomics).
    fn capture_totals(&self) -> Option<CaptureTotals> {
        None
    }
}

/// Running totals of a capture stream, in frames at its sample rate.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CaptureTotals {
    /// Frames the device delivered.
    pub captured: u64,
    /// Frames of silence written while a loopback endpoint delivered
    /// nothing (nothing playing).
    pub filled: u64,
    /// Gaps the device reported in its stream.
    pub glitches: u64,
}

/// An open capture stream.
pub struct OpenCapture {
    pub consumer: RingConsumer,
    pub sample_rate: u32,
    pub guard: Box<dyn StreamGuard>,
    /// Human-readable description, e.g. "loopback of Speakers (Realtek)".
    pub description: String,
}

impl std::fmt::Debug for OpenCapture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenCapture")
            .field("sample_rate", &self.sample_rate)
            .field("description", &self.description)
            .finish_non_exhaustive()
    }
}

pub trait NodeAudio: Send + Sync + 'static {
    /// Current sources and destinations. Control path; may take a while.
    fn endpoints(&self) -> Result<(Vec<SourceInfo>, Vec<DestinationInfo>), String>;

    /// Starts capturing from a source id previously returned by `endpoints`.
    fn open_source(&self, source_id: &str) -> Result<OpenCapture, String>;

    /// Starts playing `source` on a destination id. Returns the guard and a
    /// description of the device.
    fn open_destination(
        &self,
        destination_id: &str,
        source: Box<dyn RenderSource>,
    ) -> Result<(Box<dyn StreamGuard>, String), String>;
}

/// How many audio streams this device has open, and how many of them record
/// a microphone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioInUse {
    pub streams: u32,
    pub microphones: u32,
}

/// Told about [`AudioInUse`] before a stream opens (with that stream
/// counted) and after one closes. The iPhone app sets its audio session
/// from it: play-and-record only while a microphone is recorded, other
/// audio lowered only while streaming, so listening alone never involves
/// the microphone. The hook may wait briefly for the system (a category
/// change must settle before the new stream opens); it is called from the
/// thread opening or closing a stream, never from an audio callback.
pub type AudioUseHook = Box<dyn Fn(AudioInUse) + Send + Sync>;

static AUDIO_USE: std::sync::Mutex<(Option<AudioUseHook>, AudioInUse)> = std::sync::Mutex::new((
    None,
    AudioInUse {
        streams: 0,
        microphones: 0,
    },
));

/// Sets (or removes) the [`AudioUseHook`].
pub fn set_audio_use_hook(hook: Option<AudioUseHook>) {
    if let Ok(mut m) = AUDIO_USE.lock() {
        m.0 = hook;
    }
}

/// One open stream, counted from before it opens until dropped (after it
/// has closed).
#[derive(Debug)]
pub struct StreamUse {
    microphone: bool,
}

impl StreamUse {
    pub fn begin(microphone: bool) -> Self {
        change(1, microphone);
        StreamUse { microphone }
    }
}

impl Drop for StreamUse {
    fn drop(&mut self) {
        change(-1, self.microphone);
    }
}

fn change(by: i32, microphone: bool) {
    if let Ok(mut m) = AUDIO_USE.lock() {
        m.1.streams = m.1.streams.saturating_add_signed(by);
        if microphone {
            m.1.microphones = m.1.microphones.saturating_add_signed(by);
        }
        let now = m.1;
        if let Some(hook) = &m.0 {
            hook(now);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::Mutex;

    #[test]
    fn the_hook_sees_every_stream_and_microphone_count() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s = Arc::clone(&seen);
        set_audio_use_hook(Some(Box::new(move |u: AudioInUse| {
            s.lock().unwrap().push((u.streams, u.microphones))
        })));
        let listen = StreamUse::begin(false);
        let mic = StreamUse::begin(true);
        drop(mic);
        drop(listen);
        set_audio_use_hook(None);
        assert_eq!(*seen.lock().unwrap(), vec![(1, 0), (2, 1), (1, 0), (0, 0)]);
    }
}
