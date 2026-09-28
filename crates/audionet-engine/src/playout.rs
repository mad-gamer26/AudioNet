//! Render-side playout: the per-stream playout ring → drift/depth-corrected
//! resampling → device frames.
//!
//! [`Playout::render`] runs inside the audio render callback. It always
//! fills the whole output buffer, never waits, and does not allocate: every
//! buffer is sized in [`Playout::new`] on the control path.
//!
//! States:
//!
//! * **Priming**: output silence until the ring holds the target depth.
//!   Used at start, after an underrun (sender stall, outage) and after a
//!   stream reset, so playback always resumes with a full jitter margin.
//! * **Playing**: pull input through a sinc resampler whose ratio is
//!   `device_rate / 48000` scaled by the controller's correction.
//!
//! Discontinuities are declicked: entering silence holds the last output
//! value and ramps it to zero over [`FADE_FRAMES`]; leaving silence (or a
//! catastrophic trim) crossfades from the last output value into the new
//! audio over the same span.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

use audioadapter_buffers::direct::InterleavedSlice;
use audionet_audio::ring::RingConsumer;
use rubato::{
    Adjustable, Async, FixedAsync, Resampler, SincInterpolationParameters, WindowFunction,
};
use serde::Serialize;

use crate::adaptive::{AdaptiveConfig, AdaptiveTarget, ChangeReason, TargetChange};
use crate::controller::{ControllerConfig, ControllerState, DriftDepthController};
use crate::stats::{AtomicF64, GaugeSnapshot, GaugeStat};

/// The stream (network) sample rate. Opus always runs at 48 kHz.
pub const STREAM_RATE: u32 = 48_000;
/// Resampler output chunk, in device frames.
const CHUNK: usize = 64;
/// Declick span.
pub const FADE_FRAMES: usize = 96;
/// Headroom for the correction ratio (1 %: far beyond the 3000 ppm clamp).
const MAX_RELATIVE_RATIO: f64 = 1.01;

/// Playout settings.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct PlayoutConfig {
    pub controller: ControllerConfig,
    /// Depth above target at which stale audio is trimmed at once. A safety
    /// net only; normal recovery is the depth controller.
    pub catastrophic_excess_ms: f64,
    /// Sustained excess (filtered depth above target) beyond which the
    /// excess is drained by a controlled, crossfaded trim instead of waiting
    /// for the clamped rate correction (2000 ppm drains only 2 ms per
    /// second). AGENTS.md §26: controlled trimming for large excess.
    pub drain_excess_ms: f64,
    /// How long the excess must persist before draining.
    pub drain_after_s: f64,
    /// Depth left above target after a drain.
    pub drain_keep_excess_ms: f64,
    /// Adapt the target to the network (`controller.target_ms` is then the
    /// starting value); `None` keeps it fixed.
    pub adaptive: Option<AdaptiveConfig>,
}

impl PlayoutConfig {
    /// A fixed target.
    pub fn with_target(target_ms: f64) -> Self {
        Self {
            controller: ControllerConfig::with_target(target_ms),
            catastrophic_excess_ms: 200.0,
            drain_excess_ms: 80.0,
            drain_after_s: 1.0,
            drain_keep_excess_ms: 10.0,
            adaptive: None,
        }
    }

    /// An adaptive target starting at `start_ms`.
    pub fn adaptive(start_ms: f64, adaptive: AdaptiveConfig) -> Self {
        Self {
            adaptive: Some(adaptive),
            ..Self::with_target(start_ms)
        }
    }

    pub fn target_ms(&self) -> f64 {
        self.controller.target_ms
    }
}

impl Default for PlayoutConfig {
    fn default() -> Self {
        Self::with_target(40.0)
    }
}

/// Signals from the network side to the render side of one stream.
///
/// * `epoch` is bumped when a new stream (new clock) starts; the render side
///   then flushes and forgets its drift estimate.
/// * `last_write_ns` is when the network side last received new audio (a
///   packet beyond the highest so far). The render side uses it to measure
///   depth *continuously*: packets arrive in 10 ms steps, and render
///   callbacks sample the buffer at a nearly fixed phase relative to those
///   steps, so a raw frame count aliases slow drift into long flat
///   stretches followed by 10 ms jumps. Treating the newest frame as
///   arriving continuously removes that sawtooth (see [`Playout::render`]).
/// * `pending_frames` is audio received but held by the network side behind
///   a missing packet (not yet in the playout ring). It counts toward depth.
#[derive(Debug, Default)]
pub struct StreamControl {
    epoch: AtomicU64,
    last_write_ns: AtomicU64,
    last_write_frames: AtomicU64,
    pending_frames: AtomicU64,
    late_packets: AtomicU64,
}

impl StreamControl {
    pub fn new_stream(&self) {
        self.epoch.fetch_add(1, Relaxed);
    }

    pub fn epoch(&self) -> u64 {
        self.epoch.load(Relaxed)
    }

    /// Records that `frames` stream frames of new audio arrived at `now_ns`.
    pub fn frame_written(&self, now_ns: u64, frames: usize) {
        self.last_write_ns.store(now_ns, Relaxed);
        self.last_write_frames.store(frames as u64, Relaxed);
    }

    /// Size of the most recent write (the arrival step), in stream frames.
    pub fn last_write_frames(&self) -> u64 {
        self.last_write_frames.load(Relaxed)
    }

    pub fn last_write_ns(&self) -> u64 {
        self.last_write_ns.load(Relaxed)
    }

    /// Sets the audio held by the network side behind a gap, in stream frames.
    pub fn set_pending_frames(&self, frames: usize) {
        self.pending_frames.store(frames as u64, Relaxed);
    }

    pub fn pending_frames(&self) -> u64 {
        self.pending_frames.load(Relaxed)
    }

    /// Records a packet that arrived after its audio was concealed: the
    /// playout target was too small for this network.
    pub fn note_late_packet(&self) {
        self.late_packets.fetch_add(1, Relaxed);
    }

    pub fn late_packets(&self) -> u64 {
        self.late_packets.load(Relaxed)
    }
}

/// Arrival step assumed before any write is recorded (10 ms at 48 kHz).
const DEFAULT_ARRIVAL_STEP_FRAMES: f64 = 480.0;

/// Render-side counters.
#[derive(Debug, Default)]
pub struct PlayoutStats {
    /// Playout depth in microseconds.
    pub depth_us: GaugeStat,
    pub underruns: AtomicU64,
    /// Device frames filled with silence (priming and underruns).
    pub silence_frames: AtomicU64,
    pub primings: AtomicU64,
    pub catastrophic_trims: AtomicU64,
    pub catastrophic_trim_frames: AtomicU64,
    /// Controlled trims of large sustained excess (see `drain_excess_ms`).
    pub latency_drains: AtomicU64,
    pub latency_drain_frames: AtomicU64,
    pub stream_resets: AtomicU64,
    pub frames_out: AtomicU64,
    pub playing: AtomicBool,
    pub drift_ppm: AtomicF64,
    pub depth_bias_ppm: AtomicF64,
    pub depth_error_ms: AtomicF64,
    pub correction_ppm: AtomicF64,
    pub resample_ratio: AtomicF64,
    pub drift_locked: AtomicBool,
    pub drift_unlocks: AtomicU64,
    /// Current playout target in ms (moves when adaptive).
    pub target_ms: AtomicF64,
    pub adaptive: AtomicBool,
    pub target_changes: AtomicU64,
    /// The most recent target change (reason code 0: none yet).
    pub last_change_from_ms: AtomicF64,
    pub last_change_to_ms: AtomicF64,
    pub last_change_reason: AtomicU64,
    pub last_change_evidence: AtomicF64,
}

/// Point-in-time copy of [`PlayoutStats`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct PlayoutSnapshot {
    pub target_ms: f64,
    pub depth_us: GaugeSnapshot,
    pub underruns: u64,
    pub silence_frames: u64,
    pub primings: u64,
    pub catastrophic_trims: u64,
    pub catastrophic_trim_frames: u64,
    pub latency_drains: u64,
    pub latency_drain_frames: u64,
    pub stream_resets: u64,
    pub frames_out: u64,
    pub playing: bool,
    pub drift_ppm: f64,
    pub depth_bias_ppm: f64,
    pub depth_error_ms: f64,
    pub correction_ppm: f64,
    pub resample_ratio: f64,
    pub drift_locked: bool,
    pub drift_unlocks: u64,
    pub adaptive: bool,
    pub target_changes: u64,
    pub last_target_change: Option<TargetChange>,
}

impl PlayoutStats {
    pub fn snapshot(&self) -> PlayoutSnapshot {
        let last_target_change = ChangeReason::from_code(self.last_change_reason.load(Relaxed))
            .map(|reason| TargetChange {
                from_ms: self.last_change_from_ms.load(),
                to_ms: self.last_change_to_ms.load(),
                reason,
                evidence: self.last_change_evidence.load(),
            });
        PlayoutSnapshot {
            target_ms: self.target_ms.load(),
            adaptive: self.adaptive.load(Relaxed),
            target_changes: self.target_changes.load(Relaxed),
            last_target_change,
            depth_us: self.depth_us.snapshot(),
            underruns: self.underruns.load(Relaxed),
            silence_frames: self.silence_frames.load(Relaxed),
            primings: self.primings.load(Relaxed),
            catastrophic_trims: self.catastrophic_trims.load(Relaxed),
            catastrophic_trim_frames: self.catastrophic_trim_frames.load(Relaxed),
            latency_drains: self.latency_drains.load(Relaxed),
            latency_drain_frames: self.latency_drain_frames.load(Relaxed),
            stream_resets: self.stream_resets.load(Relaxed),
            frames_out: self.frames_out.load(Relaxed),
            playing: self.playing.load(Relaxed),
            drift_ppm: self.drift_ppm.load(),
            depth_bias_ppm: self.depth_bias_ppm.load(),
            depth_error_ms: self.depth_error_ms.load(),
            correction_ppm: self.correction_ppm.load(),
            resample_ratio: self.resample_ratio.load(),
            drift_locked: self.drift_locked.load(Relaxed),
            drift_unlocks: self.drift_unlocks.load(Relaxed),
        }
    }
}

pub struct Playout {
    consumer: RingConsumer,
    stream_channels: usize,
    device_rate: u32,
    device_channels: usize,
    base_ratio: f64,
    resampler: Async<f32>,
    in_buf: Vec<f32>,
    out_buf: Vec<f32>,
    fifo_pos: usize,
    fifo_len: usize,
    controller: DriftDepthController,
    config: PlayoutConfig,
    playing: bool,
    /// Depth measured at the start of the latest callback, in stream frames.
    measured_depth: f64,
    /// How long the filtered excess has stayed above `drain_excess_ms`.
    excess_for_s: f64,
    last_out: Vec<f32>,
    declick_from: Vec<f32>,
    declick_left: usize,
    stats: Arc<PlayoutStats>,
    control: Arc<StreamControl>,
    seen_epoch: u64,
    /// Current target (fixed, or moved by `adaptive`).
    target_ms: f64,
    adaptive: Option<AdaptiveTarget>,
    seen_late: u64,
    seen_arrival_ns: u64,
}

impl std::fmt::Debug for Playout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Playout")
            .field("device_rate", &self.device_rate)
            .field("device_channels", &self.device_channels)
            .field("playing", &self.playing)
            .finish_non_exhaustive()
    }
}

impl Playout {
    /// Builds the playout for a device. Control path: allocates all buffers.
    pub fn new(
        consumer: RingConsumer,
        control: Arc<StreamControl>,
        config: PlayoutConfig,
        device_rate: u32,
        device_channels: usize,
        stats: Arc<PlayoutStats>,
    ) -> Result<Self, String> {
        let stream_channels = consumer.channels();
        let base_ratio = f64::from(device_rate) / f64::from(STREAM_RATE);
        let params = SincInterpolationParameters::new(128, WindowFunction::BlackmanHarris2);
        let resampler = Async::<f32>::new_sinc(
            base_ratio,
            MAX_RELATIVE_RATIO,
            &params,
            CHUNK,
            stream_channels,
            FixedAsync::Output,
        )
        .map_err(|e| format!("could not create the playout resampler: {e}"))?;
        let in_buf = vec![0.0; resampler.input_frames_max() * stream_channels];
        let out_buf = vec![0.0; resampler.output_frames_max() * stream_channels];
        let seen_epoch = control.epoch();
        let target_ms = config.target_ms();
        let adaptive = config.adaptive.map(|a| AdaptiveTarget::new(a, target_ms));
        let target_ms = adaptive
            .as_ref()
            .map_or(target_ms, AdaptiveTarget::target_ms);
        stats.target_ms.store(target_ms);
        stats.adaptive.store(adaptive.is_some(), Relaxed);
        let mut controller_config = config.controller;
        controller_config.target_ms = target_ms;
        let seen_late = control.late_packets();
        Ok(Self {
            consumer,
            stream_channels,
            device_rate,
            device_channels: device_channels.max(1),
            base_ratio,
            resampler,
            in_buf,
            out_buf,
            fifo_pos: 0,
            fifo_len: 0,
            controller: DriftDepthController::new(controller_config),
            config,
            playing: false,
            measured_depth: 0.0,
            excess_for_s: 0.0,
            last_out: vec![0.0; device_channels.max(1)],
            declick_from: vec![0.0; device_channels.max(1)],
            declick_left: 0,
            stats,
            control,
            seen_epoch,
            target_ms,
            adaptive,
            seen_late,
            seen_arrival_ns: 0,
        })
    }

    /// The current playout target.
    pub fn target_ms(&self) -> f64 {
        self.target_ms
    }

    /// Feeds the adaptive target its inputs for this callback and applies a
    /// change. Real-time safe.
    fn adapt(&mut self, dt: f64) {
        let Some(adaptive) = &mut self.adaptive else {
            return;
        };
        let mut change = None;
        let late = self.control.late_packets();
        if late != self.seen_late {
            // Late packets while priming are start-up or recovery reordering.
            if self.playing {
                change = change.or(adaptive.late_packets(late.wrapping_sub(self.seen_late)));
            }
            self.seen_late = late;
        }
        let arrival = self.control.last_write_ns();
        if arrival != self.seen_arrival_ns {
            self.seen_arrival_ns = arrival;
            change = change.or(adaptive.arrival(arrival));
        }
        let depth_ms = self.measured_depth * 1000.0 / f64::from(STREAM_RATE);
        change = change.or(adaptive.tick(dt, self.playing.then_some(depth_ms)));
        if let Some(c) = change {
            self.target_ms = c.to_ms;
            self.controller.set_target(c.to_ms);
            let s = &self.stats;
            s.target_ms.store(c.to_ms);
            s.target_changes.fetch_add(1, Relaxed);
            s.last_change_from_ms.store(c.from_ms);
            s.last_change_to_ms.store(c.to_ms);
            s.last_change_evidence.store(c.evidence);
            s.last_change_reason.store(c.reason.code(), Relaxed);
        }
    }

    pub fn stats(&self) -> &Arc<PlayoutStats> {
        &self.stats
    }

    pub fn config(&self) -> &PlayoutConfig {
        &self.config
    }

    pub fn controller_state(&self) -> ControllerState {
        self.controller.state()
    }

    /// Buffered audio in stream frames: audio held by the network side
    /// behind a gap, the ring, and resampled output not yet delivered
    /// (converted back to stream frames).
    fn depth_frames(&self) -> f64 {
        let fifo = (self.fifo_len - self.fifo_pos) as f64 / self.base_ratio;
        self.control.pending_frames() as f64 + self.consumer.available_frames() as f64 + fifo
    }

    /// Depth with the arrival sawtooth removed: the newest frame counts only
    /// in proportion to the time since it was written, as if input arrived
    /// continuously.
    fn continuous_depth_frames(&self, now_ns: u64) -> f64 {
        let raw = self.depth_frames();
        let elapsed_ns = now_ns.saturating_sub(self.control.last_write_ns()) as f64;
        let step = match self.control.last_write_frames() {
            0 => DEFAULT_ARRIVAL_STEP_FRAMES,
            n => n as f64,
        };
        let elapsed_frames = (elapsed_ns * f64::from(STREAM_RATE) / 1e9).min(step);
        (raw - (step - elapsed_frames)).max(0.0)
    }

    fn ms_to_frames(ms: f64) -> f64 {
        ms * f64::from(STREAM_RATE) / 1000.0
    }

    /// Fills `out` (interleaved, `device_channels` wide). Real-time safe.
    /// `now_ns` is the current time on the clock the network side uses for
    /// [`StreamControl::frame_written`].
    pub fn render(&mut self, out: &mut [f32], now_ns: u64) {
        let dc = self.device_channels;
        let frames = out.len() / dc;
        let dt = frames as f64 / f64::from(self.device_rate);

        let epoch = self.control.epoch();
        if epoch != self.seen_epoch {
            self.seen_epoch = epoch;
            self.consumer.trim_stale(0, 0);
            self.resampler.reset();
            self.fifo_pos = 0;
            self.fifo_len = 0;
            self.controller.reset();
            self.stats.stream_resets.fetch_add(1, Relaxed);
            self.enter_silence();
        }

        let target = Self::ms_to_frames(self.target_ms);
        let ceiling = Self::ms_to_frames(self.target_ms + self.config.catastrophic_excess_ms);
        if self.depth_frames() > ceiling {
            let trimmed = self.consumer.trim_stale(0, target as usize);
            self.stats.catastrophic_trims.fetch_add(1, Relaxed);
            self.stats
                .catastrophic_trim_frames
                .fetch_add(trimmed as u64, Relaxed);
            self.controller.resync_depth();
            self.start_crossfade();
        }

        self.measured_depth = self.continuous_depth_frames(now_ns);
        self.adapt(dt);
        if !self.playing {
            if self.depth_frames() >= target {
                self.playing = true;
                self.controller.resync_depth();
                self.start_crossfade();
            } else {
                self.write_silence(out, 0);
                self.publish();
                return;
            }
        }

        let depth_ms = self.measured_depth * 1000.0 / f64::from(STREAM_RATE);
        let mut correction = self.controller.update(depth_ms, dt);
        if self.controller.state().depth_error_ms > self.config.drain_excess_ms {
            self.excess_for_s += dt;
        } else {
            self.excess_for_s = 0.0;
        }
        if self.excess_for_s >= self.config.drain_after_s {
            self.excess_for_s = 0.0;
            let keep = Self::ms_to_frames(self.target_ms + self.config.drain_keep_excess_ms);
            let trimmed = self.consumer.trim_stale(0, keep as usize);
            self.stats.latency_drains.fetch_add(1, Relaxed);
            self.stats
                .latency_drain_frames
                .fetch_add(trimmed as u64, Relaxed);
            self.controller.resync_depth();
            self.start_crossfade();
            self.measured_depth = self.continuous_depth_frames(now_ns);
            correction = self
                .controller
                .update(self.measured_depth * 1000.0 / f64::from(STREAM_RATE), dt);
        }
        let relative = 1.0 / (1.0 + correction * 1e-6);
        let _ = self.resampler.set_resample_ratio_relative(relative, true);

        let sc = self.stream_channels;
        for i in 0..frames {
            if self.fifo_pos == self.fifo_len && !self.refill() {
                self.stats.underruns.fetch_add(1, Relaxed);
                self.controller.disturbance();
                if let Some(a) = &mut self.adaptive {
                    a.underrun(now_ns);
                }
                self.enter_silence();
                self.write_silence(out, i);
                self.publish();
                return;
            }
            let src = &self.out_buf[self.fifo_pos * sc..(self.fifo_pos + 1) * sc];
            let dst = &mut out[i * dc..(i + 1) * dc];
            map_channels(src, dst);
            if self.declick_left > 0 {
                let g = 1.0 - self.declick_left as f32 / FADE_FRAMES as f32;
                for (d, from) in dst.iter_mut().zip(&self.declick_from) {
                    *d = *d * g + from * (1.0 - g);
                }
                self.declick_left -= 1;
            }
            self.last_out.copy_from_slice(dst);
            self.fifo_pos += 1;
        }
        self.stats.frames_out.fetch_add(frames as u64, Relaxed);
        self.publish();
    }

    /// Resamples one more chunk. Returns false if the ring cannot supply it.
    fn refill(&mut self) -> bool {
        let sc = self.stream_channels;
        let need = self.resampler.input_frames_next();
        if self.consumer.available_frames() < need {
            return false;
        }
        let got = self.consumer.read(&mut self.in_buf[..need * sc]);
        debug_assert_eq!(got, need);
        let input = InterleavedSlice::new(&self.in_buf[..], sc, need).expect("sized in new");
        let out_frames = self.resampler.output_frames_next();
        let mut output =
            InterleavedSlice::new_mut(&mut self.out_buf[..], sc, out_frames).expect("sized in new");
        match self
            .resampler
            .process_into_buffer(&input, &mut output, None)
        {
            Ok((_, produced)) => {
                self.fifo_pos = 0;
                self.fifo_len = produced;
                produced > 0
            }
            Err(_) => false,
        }
    }

    fn enter_silence(&mut self) {
        if self.playing {
            self.stats.primings.fetch_add(1, Relaxed);
        }
        self.playing = false;
        self.fifo_pos = 0;
        self.fifo_len = 0;
    }

    fn start_crossfade(&mut self) {
        self.declick_from.copy_from_slice(&self.last_out);
        self.declick_left = FADE_FRAMES;
    }

    /// Writes silence from frame `start`, first ramping the held last output
    /// value down to zero so the transition does not click.
    fn write_silence(&mut self, out: &mut [f32], start: usize) {
        let dc = self.device_channels;
        let frames = out.len() / dc;
        let mut fade_left = if self.last_out.iter().any(|v| *v != 0.0) {
            FADE_FRAMES
        } else {
            0
        };
        for i in start..frames {
            let g = fade_left as f32 / FADE_FRAMES as f32;
            for (c, d) in out[i * dc..(i + 1) * dc].iter_mut().enumerate() {
                *d = self.last_out[c] * g;
            }
            fade_left = fade_left.saturating_sub(1);
        }
        if frames > start {
            self.last_out
                .copy_from_slice(&out[(frames - 1) * dc..frames * dc]);
        }
        self.declick_left = 0;
        self.stats
            .silence_frames
            .fetch_add((frames - start) as u64, Relaxed);
        self.stats.frames_out.fetch_add(frames as u64, Relaxed);
    }

    fn publish(&self) {
        let s = &self.stats;
        // The same start-of-callback measurement the controller regulates.
        let depth_us = self.measured_depth * 1e6 / f64::from(STREAM_RATE);
        s.depth_us.record(depth_us as u64);
        s.playing.store(self.playing, Relaxed);
        let c = self.controller.state();
        s.drift_ppm.store(c.drift_ppm);
        s.depth_bias_ppm.store(c.depth_bias_ppm);
        s.depth_error_ms.store(c.depth_error_ms);
        s.correction_ppm.store(c.correction_ppm);
        s.resample_ratio
            .store(self.base_ratio / (1.0 + c.correction_ppm * 1e-6));
        s.drift_locked.store(c.locked, Relaxed);
        s.drift_unlocks
            .store(self.controller.unlock_events(), Relaxed);
    }
}

/// Maps one stream frame (mono or stereo) onto a device frame.
fn map_channels(src: &[f32], dst: &mut [f32]) {
    match (src.len(), dst.len()) {
        (_, 0) => {}
        (1, _) => {
            let n = dst.len().min(2);
            dst[..n].fill(src[0]);
            dst[n..].fill(0.0);
        }
        (_, 1) => dst[0] = 0.5 * (src[0] + src[1]),
        _ => {
            dst[0] = src[0];
            dst[1] = src[1];
            dst[2..].fill(0.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use audionet_audio::ring::audio_ring;

    fn setup(
        device_rate: u32,
        device_channels: usize,
    ) -> (
        audionet_audio::ring::RingProducer,
        Playout,
        Arc<StreamControl>,
    ) {
        let (p, c) = audio_ring(48_000, 2);
        let control = Arc::new(StreamControl::default());
        let playout = Playout::new(
            c,
            Arc::clone(&control),
            PlayoutConfig::default(),
            device_rate,
            device_channels,
            Arc::default(),
        )
        .unwrap();
        (p, playout, control)
    }

    fn tone(frames: usize, start: usize) -> Vec<f32> {
        (0..frames)
            .flat_map(|i| {
                let v = 0.5
                    * (2.0 * std::f32::consts::PI * 1000.0 * (start + i) as f32 / 48_000.0).sin();
                [v, v]
            })
            .collect()
    }

    #[test]
    fn primes_then_plays_without_gaps() {
        let (mut p, mut playout, _) = setup(48_000, 2);
        let mut out = vec![0.0; 480 * 2];
        // Below target (40 ms = 1920 frames): silence.
        p.write(&tone(960, 0));
        playout.render(&mut out, 0);
        assert!(out.iter().all(|v| *v == 0.0));
        assert!(!playout.stats().playing.load(Relaxed));
        // Reach target: plays.
        p.write(&tone(1920, 960));
        playout.render(&mut out, 0);
        assert!(playout.stats().playing.load(Relaxed));
        assert!(out.iter().any(|v| v.abs() > 0.1));
        let snap = playout.stats().snapshot();
        assert_eq!(snap.underruns, 0);
        assert_eq!(snap.frames_out, 960);
    }

    #[test]
    fn underrun_fills_the_whole_buffer_and_reprimes() {
        let (mut p, mut playout, _) = setup(48_000, 2);
        p.write(&tone(2000, 0));
        let mut out = vec![1.0; 4800 * 2]; // 100 ms request: more than buffered
        playout.render(&mut out, 0);
        let snap = playout.stats().snapshot();
        assert_eq!(snap.underruns, 1);
        assert!(!snap.playing);
        // The tail is silence, reached by a ramp (no values beyond the signal range).
        assert!(out[out.len() - 2..].iter().all(|v| *v == 0.0));
        assert!(out.iter().all(|v| v.abs() <= 0.51));
        // It stays silent until the target is back.
        p.write(&tone(500, 2000));
        playout.render(&mut out[..960], 0);
        assert!(out[..960].iter().all(|v| *v == 0.0));
    }

    #[test]
    fn declick_limits_the_step_into_silence() {
        let (mut p, mut playout, _) = setup(48_000, 2);
        p.write(&tone(2000, 0));
        let mut out = vec![0.0; 4800 * 2];
        playout.render(&mut out, 0);
        // Largest sample-to-sample step stays near a 1 kHz tone's own slope
        // (0.5 × 2π × 1000 / 48000 ≈ 0.065), i.e. no click at the underrun.
        let max_step = out
            .chunks(2)
            .map(|f| f[0])
            .collect::<Vec<_>>()
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0f32, f32::max);
        assert!(max_step < 0.08, "step {max_step}");
    }

    #[test]
    fn catastrophic_trim_returns_to_target() {
        let (mut p, mut playout, _) = setup(48_000, 2);
        p.write(&tone(48_000 / 2, 0)); // 500 ms: far above 40 + 200 ms
        let mut out = vec![0.0; 960];
        playout.render(&mut out, 0);
        let snap = playout.stats().snapshot();
        assert_eq!(snap.catastrophic_trims, 1);
        let depth_ms = snap.depth_us.current as f64 / 1000.0;
        assert!(
            (depth_ms - 30.0).abs() < 15.0,
            "depth after trim {depth_ms}"
        );
    }

    #[test]
    fn new_stream_flushes_and_resets() {
        let (mut p, mut playout, control) = setup(48_000, 2);
        p.write(&tone(4000, 0));
        let mut out = vec![0.0; 960];
        playout.render(&mut out, 0);
        control.new_stream();
        playout.render(&mut out, 0);
        let snap = playout.stats().snapshot();
        assert_eq!(snap.stream_resets, 1);
        assert!(!snap.playing);
        assert_eq!(snap.drift_ppm, 0.0);
    }

    #[test]
    fn resamples_to_other_device_rates_and_channel_counts() {
        for (rate, ch) in [(44_100u32, 2usize), (96_000, 8), (48_000, 1)] {
            let (mut p, mut playout, _) = setup(rate, ch);
            let mut written = 0;
            let mut produced = 0;
            let callback = (rate / 100) as usize;
            let mut out = vec![0.0; callback * ch];
            for _ in 0..100 {
                if p.write(&tone(480, written)) == audionet_audio::ring::WriteOutcome::Written {
                    written += 480;
                }
                playout.render(&mut out, 0);
                produced += callback;
            }
            let snap = playout.stats().snapshot();
            assert_eq!(snap.frames_out as usize, produced);
            assert!(snap.playing, "{rate}/{ch}");
            assert_eq!(snap.underruns, 0, "{rate}/{ch}");
            if ch > 2 {
                assert!(out.chunks(ch).all(|f| f[2..].iter().all(|v| *v == 0.0)));
            }
        }
    }
}
