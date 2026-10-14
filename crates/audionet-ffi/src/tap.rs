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
//! [`AppSource`] is the other way round: a source the app writes samples
//! into (a game's own sound, a microphone it records itself), for sending
//! to a device. Sending "from a source" is sending with the source's id
//! ([`AppSource::source_id`]). Writes never wait: what does not fit the
//! stream's buffer (half a second) is dropped and counted, like any
//! capture's overflow, and nothing is kept while no stream sends it.
//!
//! [`AppAudio`] is the [`NodeAudio`] the engine uses: taps for `app:` ids,
//! app sources for `app-source:` ids, the platform's devices for everything
//! else.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex, Weak};

use audionet_audio::clock;
use audionet_audio::render::RenderSource;
use audionet_audio::ring::{RingProducer, WriteOutcome, audio_ring};
use audionet_node::audio::{NodeAudio, OpenCapture, StreamGuard};
use audionet_protocol::signal::{DestinationInfo, SourceInfo};

use crate::PlatformAudio;

const PREFIX: &str = "app:";
const SOURCE_PREFIX: &str = "app-source:";

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

impl AudioTap {
    /// Fills `out` (interleaved, a whole number of frames) with the next
    /// frames as floats, without allocating. Silence while no stream plays
    /// here. Returns the frames written. Never waits.
    pub fn read_f32(&self, out: &mut [f32]) -> usize {
        let channels = self.shared.channels as usize;
        let frames = (out.len() / channels).min(MAX_READ as usize);
        let out = &mut out[..frames * channels];
        let mut slot = lock(&self.shared.slot);
        match slot.source.as_mut() {
            Some(source) => source.render(out, clock::now_ns()),
            None => out.fill(0.0),
        }
        frames
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

// MARK: Sources the app writes into

/// The stream sending from a source: where the app's samples go.
#[derive(Default)]
struct SourceSlot {
    producer: Option<RingProducer>,
    stream: u64,
    /// Blocks dropped because they did not fit.
    dropped: u64,
}

struct SourceShared {
    name: String,
    sample_rate: u32,
    channels: u32,
    slot: Mutex<SourceSlot>,
}

/// App sources that exist, by source id.
static SOURCES: std::sync::LazyLock<Mutex<HashMap<String, Weak<SourceShared>>>> =
    std::sync::LazyLock::new(Default::default);

/// A source the app writes audio into (see the module documentation).
#[derive(uniffi::Object)]
pub struct AppSource {
    id: String,
    shared: Arc<SourceShared>,
}

impl std::fmt::Debug for AppSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppSource")
            .field("id", &self.id)
            .field("name", &self.shared.name)
            .finish_non_exhaustive()
    }
}

#[uniffi::export]
impl AppSource {
    /// A source taking `sample_rate` Hz with `channels` channels (1 or 2).
    /// `name` says what it is, in words ("Game sound"); the stream's status
    /// uses it.
    #[uniffi::constructor]
    pub fn new(name: String, sample_rate: u32, channels: u32) -> Arc<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let id = format!("{SOURCE_PREFIX}{}", NEXT.fetch_add(1, Relaxed));
        let shared = Arc::new(SourceShared {
            name,
            sample_rate: sample_rate.clamp(8_000, 192_000),
            channels: channels.clamp(1, 2),
            slot: Mutex::new(SourceSlot::default()),
        });
        let mut sources = lock(&SOURCES);
        sources.retain(|_, s| s.strong_count() > 0);
        sources.insert(id.clone(), Arc::downgrade(&shared));
        Arc::new(Self { id, shared })
    }

    /// The source id to send with (`Client::send`).
    pub fn source_id(&self) -> String {
        self.id.clone()
    }

    pub fn sample_rate(&self) -> u32 {
        self.shared.sample_rate
    }

    pub fn channels(&self) -> u32 {
        self.shared.channels
    }

    /// Whether a stream is sending this source.
    pub fn is_sending(&self) -> bool {
        lock(&self.shared.slot).producer.is_some()
    }

    /// Blocks dropped so far because the stream's buffer was full (the app
    /// wrote faster than real time, or the stream stalled).
    pub fn dropped_blocks(&self) -> u64 {
        lock(&self.shared.slot).dropped
    }

    /// Writes interleaved samples (a whole number of frames). Returns the
    /// frames taken: 0 while no stream sends this source, or when the block
    /// does not fit (it is dropped). Never waits.
    pub fn write(&self, samples: Vec<f32>) -> u32 {
        self.write_f32(&samples) as u32
    }
}

impl AppSource {
    /// [`AppSource::write`] without the copy, for C callers.
    pub fn write_f32(&self, samples: &[f32]) -> usize {
        let channels = self.shared.channels as usize;
        let whole = samples.len() - samples.len() % channels;
        let mut slot = lock(&self.shared.slot);
        let SourceSlot {
            producer, dropped, ..
        } = &mut *slot;
        let Some(producer) = producer.as_mut() else {
            return 0;
        };
        match producer.write(&samples[..whole]) {
            WriteOutcome::Written => whole / channels,
            WriteOutcome::DroppedOverflow => {
                *dropped += 1;
                0
            }
        }
    }
}

impl Drop for AppSource {
    fn drop(&mut self) {
        lock(&SOURCES).remove(&self.id);
    }
}

/// Lets go of its stream's buffer when the stream closes.
struct SourceGuard {
    shared: Weak<SourceShared>,
    stream: u64,
}

impl StreamGuard for SourceGuard {
    fn failure(&mut self) -> Option<String> {
        None
    }
}

impl Drop for SourceGuard {
    fn drop(&mut self) {
        if let Some(shared) = self.shared.upgrade() {
            let mut slot = lock(&shared.slot);
            if slot.stream == self.stream {
                slot.producer = None;
            }
        }
    }
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
        if !source_id.starts_with(SOURCE_PREFIX) {
            return PlatformAudio.open_source(source_id);
        }
        static STREAMS: AtomicU64 = AtomicU64::new(1);
        let shared = lock(&SOURCES)
            .get(source_id)
            .and_then(Weak::upgrade)
            .ok_or_else(|| "the app stopped giving this sound".to_owned())?;
        // Control path: half a second of buffer, allocated here.
        let (producer, consumer) =
            audio_ring(shared.sample_rate as usize / 2, shared.channels as usize);
        let stream = STREAMS.fetch_add(1, Relaxed);
        {
            let mut slot = lock(&shared.slot);
            // A newer stream replaces an older one.
            slot.producer = Some(producer);
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
        Ok(OpenCapture {
            consumer,
            sample_rate: shared.sample_rate,
            guard: Box::new(SourceGuard {
                shared: Arc::downgrade(&shared),
                stream,
            }),
            description,
        })
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
    fn an_app_source_feeds_its_stream_and_drops_what_does_not_fit() {
        let source = AppSource::new("Game sound".into(), 48_000, 2);
        assert!(!source.is_sending());
        // Nothing kept while no stream sends it.
        assert_eq!(source.write(vec![0.5; 960]), 0);

        let mut open = AppAudio.open_source(&source.source_id()).unwrap();
        assert_eq!(open.description, "Game sound (stereo, 48000 Hz)");
        assert_eq!(open.sample_rate, 48_000);
        assert!(source.is_sending());
        assert_eq!(source.write(vec![0.25; 960]), 480);
        let mut out = vec![0.0; 960];
        assert_eq!(open.consumer.read(&mut out), 480);
        assert!(out.iter().all(|&s| s == 0.25));

        // Half a second fits; a block beyond that is dropped, not waited on.
        assert_eq!(source.write(vec![0.0; 48_000]), 24_000);
        assert_eq!(source.write(vec![0.0; 960]), 0);
        assert_eq!(source.dropped_blocks(), 1);

        drop(open.guard);
        assert!(!source.is_sending());
    }

    #[test]
    fn a_dropped_source_cannot_be_sent() {
        let source = AppSource::new("Game sound".into(), 48_000, 1);
        let id = source.source_id();
        drop(source);
        let err = AppAudio.open_source(&id).err().unwrap();
        assert_eq!(err, "the app stopped giving this sound");
    }

    #[test]
    fn a_tap_reads_floats_without_allocating() {
        let tap = AudioTap::new("Game".into(), 48_000, 2);
        let mut out = vec![1.0; 9];
        // Whole frames only; silence without a stream.
        assert_eq!(tap.read_f32(&mut out), 4);
        assert!(out[..8].iter().all(|&s| s == 0.0));
        assert_eq!(out[8], 1.0);
    }

    #[test]
    fn samples_are_clipped_not_wrapped() {
        assert_eq!(to_i16(2.0), i16::MAX);
        assert_eq!(to_i16(-2.0), i16::MIN);
        assert_eq!(to_i16(f32::NAN), 0);
    }
}
