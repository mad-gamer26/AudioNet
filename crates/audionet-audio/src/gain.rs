//! Per-stream volume and mute.
//!
//! [`StreamGain`] is set from a control thread (an app's slider or mute
//! switch); [`GainRamp`] applies it to audio blocks, on the real-time render
//! thread or a capture pump. Real-time rules: the audio side reads one
//! atomic, never blocks or allocates, and moves to a new gain over at most
//! [`RAMP`] so a change never clicks.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering::Relaxed};
use std::time::Duration;

use crate::render::RenderSource;

/// How long a change of gain takes (from full to silent at most).
pub const RAMP: Duration = Duration::from_millis(10);

/// The volume and mute of one stream, shared between the control side and
/// the audio side.
#[derive(Debug)]
pub struct StreamGain {
    /// The amplitude factor to move to, as `f32` bits.
    target: AtomicU32,
}

impl Default for StreamGain {
    fn default() -> Self {
        Self {
            target: AtomicU32::new(1.0f32.to_bits()),
        }
    }
}

impl StreamGain {
    /// `volume` is the slider position, 0.0 to 1.0 (clamped). It is heard
    /// on a square curve: half-way is about -12 dB, which sounds like half
    /// as loud, where a straight line (-6 dB) would sound barely quieter.
    /// Muted is silence whatever the volume.
    pub fn set(&self, volume: f32, muted: bool) {
        let v = if volume.is_finite() {
            volume.clamp(0.0, 1.0)
        } else {
            1.0
        };
        let amplitude = if muted { 0.0 } else { v * v };
        self.target.store(amplitude.to_bits(), Relaxed);
    }

    /// The amplitude factor to move to.
    pub fn target(&self) -> f32 {
        f32::from_bits(self.target.load(Relaxed))
    }
}

/// Applies a [`StreamGain`] to interleaved blocks, moving towards its target
/// by at most one full swing per [`RAMP`].
#[derive(Debug, Clone)]
pub struct GainRamp {
    current: f32,
    /// The largest change per frame.
    step: f32,
}

impl GainRamp {
    pub fn new(sample_rate: u32) -> Self {
        let frames = (sample_rate as f32 * RAMP.as_secs_f32()).max(1.0);
        Self {
            current: 1.0,
            step: 1.0 / frames,
        }
    }

    /// The gain now applied (for tests and diagnostics).
    pub fn current(&self) -> f32 {
        self.current
    }

    /// Scales `samples` (interleaved, `channels` per frame) towards
    /// `target`. At unity gain the block is left untouched.
    pub fn apply(&mut self, samples: &mut [f32], channels: usize, target: f32) {
        let channels = channels.max(1);
        if self.current == target {
            if target != 1.0 {
                for s in samples.iter_mut() {
                    *s *= target;
                }
            }
            return;
        }
        for frame in samples.chunks_mut(channels) {
            let delta = target - self.current;
            self.current = if delta.abs() <= self.step {
                target
            } else {
                self.current + self.step.copysign(delta)
            };
            for s in frame.iter_mut() {
                *s *= self.current;
            }
        }
    }
}

/// A [`RenderSource`] played at a stream's volume.
pub struct GainedSource {
    inner: Box<dyn RenderSource>,
    gain: Arc<StreamGain>,
    ramp: GainRamp,
    channels: usize,
}

impl std::fmt::Debug for GainedSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GainedSource")
            .field("gain", &self.gain)
            .field("ramp", &self.ramp)
            .finish()
    }
}

impl GainedSource {
    pub fn new(inner: Box<dyn RenderSource>, gain: Arc<StreamGain>) -> Self {
        Self {
            inner,
            gain,
            ramp: GainRamp::new(48_000),
            channels: 2,
        }
    }
}

impl RenderSource for GainedSource {
    fn prepare(
        &mut self,
        device_rate: u32,
        channels: usize,
        max_frames: usize,
    ) -> Result<(), String> {
        self.ramp = GainRamp::new(device_rate);
        self.channels = channels;
        self.inner.prepare(device_rate, channels, max_frames)
    }

    fn render(&mut self, out: &mut [f32], now_ns: u64) {
        self.inner.render(out, now_ns);
        self.ramp.apply(out, self.channels, self.gain.target());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_slider_is_heard_on_a_square_curve_and_mute_is_silence() {
        let g = StreamGain::default();
        assert_eq!(g.target(), 1.0);
        g.set(0.5, false);
        assert_eq!(g.target(), 0.25);
        g.set(0.5, true);
        assert_eq!(g.target(), 0.0);
        g.set(7.0, false);
        assert_eq!(g.target(), 1.0);
        g.set(f32::NAN, false);
        assert_eq!(g.target(), 1.0);
    }

    #[test]
    fn full_volume_leaves_the_audio_untouched() {
        let mut r = GainRamp::new(48_000);
        let mut block = [0.3f32; 960];
        r.apply(&mut block, 2, 1.0);
        assert!(block.iter().all(|&s| s == 0.3));
    }

    #[test]
    fn a_change_ramps_over_ten_milliseconds_without_a_jump() {
        let mut r = GainRamp::new(48_000);
        // 20 ms of a constant signal, stereo; mute at once.
        let mut block = vec![1.0f32; 960 * 2];
        r.apply(&mut block, 2, 0.0);
        let left: Vec<f32> = block.iter().step_by(2).copied().collect();
        let largest_jump = left
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max);
        assert!(largest_jump <= 1.0 / 480.0 + 1e-6, "jump {largest_jump}");
        // Silent after 10 ms (480 frames), and both channels alike.
        assert_eq!(left[480], 0.0);
        assert!(block.chunks(2).all(|f| f[0] == f[1]));
        assert_eq!(r.current(), 0.0);
        // And back up.
        let mut block = vec![1.0f32; 960 * 2];
        r.apply(&mut block, 2, 1.0);
        assert_eq!(*block.last().unwrap(), 1.0);
    }

    struct Ones;
    impl RenderSource for Ones {
        fn prepare(&mut self, _: u32, _: usize, _: usize) -> Result<(), String> {
            Ok(())
        }
        fn render(&mut self, out: &mut [f32], _: u64) {
            out.fill(1.0);
        }
    }

    #[test]
    fn a_gained_source_plays_at_the_stream_volume() {
        let gain = Arc::new(StreamGain::default());
        let mut s = GainedSource::new(Box::new(Ones), Arc::clone(&gain));
        s.prepare(48_000, 2, 960).unwrap();
        gain.set(0.5, false);
        let mut out = vec![0.0f32; 960 * 2];
        s.render(&mut out, 0);
        s.render(&mut out, 0);
        assert!(out.iter().all(|&x| (x - 0.25).abs() < 1e-6));
    }
}
