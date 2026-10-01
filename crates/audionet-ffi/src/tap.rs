//! Audio handed to the app instead of a speaker: an [`AudioTap`] is an
//! output the app reads 16-bit samples from, for apps that pass what they
//! listen to on to something else (TeamTalk NG sends it into a TeamTalk
//! channel as its voice).
//!
//! Listening "to a tap" is listening with the tap's destination id
//! ([`AudioTap::destination_id`]) as the output. The stream's playout then
//! runs on the app's reads instead of a device callback: the reads set the
//! pace (the app's own clock), and the playout's drift and depth control
//! follow it like any device's. A read with nothing to play returns
//! silence at once, never waiting.
//!
//! [`AppAudio`] is the [`NodeAudio`] the engine uses: taps for `app:` ids,
//! the platform's devices for everything else.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex, Weak};

use audionet_audio::clock;
use audionet_audio::render::RenderSource;
use audionet_node::audio::{NodeAudio, OpenCapture, StreamGuard};
use audionet_protocol::signal::{DestinationInfo, SourceInfo};

use crate::PlatformAudio;

const PREFIX: &str = "app:";

/// The largest read, in frames per channel (one second at 48 kHz).
const MAX_READ: u32 = 48_000;

/// What a tap plays: the stream listening to it (if any), and a buffer for
/// rendering into.
#[derive(Default)]
struct Slot {
    source: Option<Box<dyn RenderSource>>,
    /// Which stream `source` belongs to: a closing stream removes only its
    /// own source.
    stream: u64,
    buffer: Vec<f32>,
}

struct Shared {
    name: String,
    sample_rate: u32,
    channels: u32,
    slot: Mutex<Slot>,
}

/// Taps that exist, by destination id.
static TAPS: std::sync::LazyLock<Mutex<HashMap<String, Weak<Shared>>>> =
    std::sync::LazyLock::new(Default::default);

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// An output the app reads audio from (see the module documentation).
#[derive(uniffi::Object)]
pub struct AudioTap {
    id: String,
    shared: Arc<Shared>,
}

impl std::fmt::Debug for AudioTap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioTap")
            .field("id", &self.id)
            .field("name", &self.shared.name)
            .finish_non_exhaustive()
    }
}

#[uniffi::export]
impl AudioTap {
    /// A tap delivering `sample_rate` Hz with `channels` channels (1 or 2).
    /// `name` says where the sound goes, in words ("TeamTalk channel"); the
    /// stream's status uses it.
    #[uniffi::constructor]
    pub fn new(name: String, sample_rate: u32, channels: u32) -> Arc<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let id = format!("{PREFIX}{}", NEXT.fetch_add(1, Relaxed));
        let shared = Arc::new(Shared {
            name,
            sample_rate: sample_rate.clamp(8_000, 192_000),
            channels: channels.clamp(1, 2),
            slot: Mutex::new(Slot::default()),
        });
        let mut taps = lock(&TAPS);
        taps.retain(|_, t| t.strong_count() > 0);
        taps.insert(id.clone(), Arc::downgrade(&shared));
        Arc::new(Self { id, shared })
    }

    /// The output id to listen with (`Client::listen`).
    pub fn destination_id(&self) -> String {
        self.id.clone()
    }

    pub fn sample_rate(&self) -> u32 {
        self.shared.sample_rate
    }

    pub fn channels(&self) -> u32 {
        self.shared.channels
    }

    /// Whether a stream is playing into this tap.
    pub fn is_playing(&self) -> bool {
        lock(&self.shared.slot).source.is_some()
    }

    /// The next `frames` frames, interleaved 16-bit samples (`frames` times
    /// the channel count). Silence while no stream plays here. Never waits:
    /// call it from the app's own sending thread at its own pace, not from
    /// an audio callback (the result is a new array).
    pub fn read(&self, frames: u32) -> Vec<i16> {
        let channels = self.shared.channels as usize;
        let n = frames.min(MAX_READ) as usize * channels;
        let mut slot = lock(&self.shared.slot);
        let Slot { source, buffer, .. } = &mut *slot;
        let Some(source) = source.as_mut() else {
            return vec![0; n];
        };
        buffer.resize(n, 0.0);
        source.render(&mut buffer[..n], clock::now_ns());
        buffer[..n].iter().map(|&s| to_i16(s)).collect()
    }
}

impl Drop for AudioTap {
    fn drop(&mut self) {
        lock(&TAPS).remove(&self.id);
    }
}

fn to_i16(s: f32) -> i16 {
    // `as` saturates; NaN becomes 0.
    (s * 32_767.0).round() as i16
}

/// Removes its stream from the tap when the stream closes.
struct TapGuard {
    shared: Weak<Shared>,
    stream: u64,
}

impl StreamGuard for TapGuard {
    fn failure(&mut self) -> Option<String> {
        None
    }
}

impl Drop for TapGuard {
    fn drop(&mut self) {
        if let Some(shared) = self.shared.upgrade() {
            let mut slot = lock(&shared.slot);
            if slot.stream == self.stream {
                slot.source = None;
            }
        }
    }
}

/// The engine's audio: taps for `app:` ids, the platform's devices
/// otherwise.
#[derive(Debug, Default)]
pub struct AppAudio;

impl NodeAudio for AppAudio {
    fn endpoints(&self) -> Result<(Vec<SourceInfo>, Vec<DestinationInfo>), String> {
        PlatformAudio.endpoints()
    }

    fn open_source(&self, source_id: &str) -> Result<OpenCapture, String> {
        PlatformAudio.open_source(source_id)
    }

    fn open_destination(
        &self,
        destination_id: &str,
        mut source: Box<dyn RenderSource>,
    ) -> Result<(Box<dyn StreamGuard>, String), String> {
        if !destination_id.starts_with(PREFIX) {
            return PlatformAudio.open_destination(destination_id, source);
        }
        static STREAMS: AtomicU64 = AtomicU64::new(1);
        let shared = lock(&TAPS)
            .get(destination_id)
            .and_then(Weak::upgrade)
            .ok_or_else(|| "the app stopped taking this sound".to_owned())?;
        // Control path: the playout allocates its buffers here.
        source.prepare(
            shared.sample_rate,
            shared.channels as usize,
            MAX_READ as usize,
        )?;
        let stream = STREAMS.fetch_add(1, Relaxed);
        {
            let mut slot = lock(&shared.slot);
            slot.buffer
                .reserve(MAX_READ as usize * shared.channels as usize);
            // A newer stream replaces an older one (the old one's guard then
            // leaves the slot alone).
            slot.source = Some(source);
            slot.stream = stream;
        }
        let description = format!(
            "{} ({}, {} Hz)",
            shared.name,
            if shared.channels == 1 {
                "mono"
            } else {
                "stereo"
            },
            shared.sample_rate
        );
        Ok((
            Box::new(TapGuard {
                shared: Arc::downgrade(&shared),
                stream,
            }),
            description,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Plays a constant level, counting what it was asked for.
    struct Level(f32, Arc<Mutex<(u32, usize, usize)>>);

    impl RenderSource for Level {
        fn prepare(&mut self, rate: u32, channels: usize, max: usize) -> Result<(), String> {
            *self.1.lock().unwrap() = (rate, channels, max);
            Ok(())
        }
        fn render(&mut self, out: &mut [f32], _now_ns: u64) {
            out.fill(self.0);
        }
    }

    #[test]
    fn a_tap_plays_its_stream_and_silence_without_one() {
        let tap = AudioTap::new("TeamTalk channel".into(), 48_000, 2);
        assert!(!tap.is_playing());
        assert_eq!(tap.read(480), vec![0; 960]);

        let seen = Arc::new(Mutex::new((0, 0, 0)));
        let (guard, description) = AppAudio
            .open_destination(
                &tap.destination_id(),
                Box::new(Level(0.5, Arc::clone(&seen))),
            )
            .unwrap();
        assert_eq!(description, "TeamTalk channel (stereo, 48000 Hz)");
        assert_eq!(*seen.lock().unwrap(), (48_000, 2, 48_000));
        assert!(tap.is_playing());
        let block = tap.read(480);
        assert_eq!(block.len(), 960);
        assert!(block.iter().all(|&s| s == 16_384));

        drop(guard);
        assert!(!tap.is_playing());
        assert_eq!(tap.read(10), vec![0; 20]);
    }

    #[test]
    fn a_newer_stream_replaces_an_older_one() {
        let tap = AudioTap::new("TeamTalk channel".into(), 16_000, 1);
        let seen = Arc::new(Mutex::new((0, 0, 0)));
        let open = |level| {
            AppAudio
                .open_destination(
                    &tap.destination_id(),
                    Box::new(Level(level, Arc::clone(&seen))),
                )
                .unwrap()
                .0
        };
        let old = open(0.25);
        let new = open(-1.0);
        // The old stream closing leaves the new one playing.
        drop(old);
        assert_eq!(tap.read(4), vec![-32_767; 4]);
        drop(new);
        assert!(!tap.is_playing());
    }

    #[test]
    fn a_dropped_tap_cannot_be_listened_to() {
        let tap = AudioTap::new("TeamTalk channel".into(), 48_000, 1);
        let id = tap.destination_id();
        drop(tap);
        let err = AppAudio
            .open_destination(&id, Box::new(Level(0.0, Default::default())))
            .err()
            .unwrap();
        assert_eq!(err, "the app stopped taking this sound");
    }

    #[test]
    fn samples_are_clipped_not_wrapped() {
        assert_eq!(to_i16(2.0), i16::MAX);
        assert_eq!(to_i16(-2.0), i16::MIN);
        assert_eq!(to_i16(f32::NAN), 0);
    }
}
