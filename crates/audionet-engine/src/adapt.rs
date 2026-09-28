//! Converts captured audio (any rate, any channel count) to the 48 kHz
//! stereo stream format, on the sender thread.
//!
//! Channel mapping: mono is duplicated to both channels; two or more
//! channels take the first two (front left and right in WAVE order).
//! Rate conversion, when needed, is a fixed-ratio sinc resampler; the
//! capture device's clock drift is left in the stream on purpose, because
//! the receiver measures and corrects it.

use audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Async, FixedAsync, Resampler, SincInterpolationParameters, WindowFunction};

use crate::playout::STREAM_RATE;

/// Input frames per resampler call.
const CHUNK: usize = 480;

pub struct StreamAdapter {
    in_channels: usize,
    resampler: Option<Async<f32>>,
    /// Stereo frames waiting for a full resampler chunk.
    pending: Vec<f32>,
    pending_frames: usize,
    stereo: Vec<f32>,
    out: Vec<f32>,
}

impl std::fmt::Debug for StreamAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamAdapter")
            .field("in_channels", &self.in_channels)
            .field("resampling", &self.resampler.is_some())
            .finish_non_exhaustive()
    }
}

impl StreamAdapter {
    /// `max_block_frames` bounds the input block size passed to `process`.
    pub fn new(in_rate: u32, in_channels: usize, max_block_frames: usize) -> Result<Self, String> {
        let resampler = if in_rate == STREAM_RATE {
            None
        } else {
            let params = SincInterpolationParameters::new(128, WindowFunction::BlackmanHarris2);
            Some(
                Async::<f32>::new_sinc(
                    f64::from(STREAM_RATE) / f64::from(in_rate),
                    1.0,
                    &params,
                    CHUNK,
                    2,
                    FixedAsync::Input,
                )
                .map_err(|e| format!("could not create the capture resampler: {e}"))?,
            )
        };
        let out_max = resampler.as_ref().map_or(0, |r| r.output_frames_max());
        Ok(Self {
            in_channels: in_channels.max(1),
            resampler,
            pending: vec![0.0; CHUNK * 2],
            pending_frames: 0,
            stereo: vec![0.0; max_block_frames * 2],
            out: vec![0.0; out_max * 2],
        })
    }

    /// Converts a block of interleaved capture samples and passes 48 kHz
    /// stereo output to `emit` (possibly in several pieces).
    pub fn process(&mut self, input: &[f32], mut emit: impl FnMut(&[f32])) {
        let ch = self.in_channels;
        let frames = (input.len() / ch).min(self.stereo.len() / 2);
        for (i, frame) in input.chunks_exact(ch).take(frames).enumerate() {
            let (l, r) = if ch == 1 {
                (frame[0], frame[0])
            } else {
                (frame[0], frame[1])
            };
            self.stereo[2 * i] = l;
            self.stereo[2 * i + 1] = r;
        }
        let Some(resampler) = &mut self.resampler else {
            emit(&self.stereo[..frames * 2]);
            return;
        };
        let mut offset = 0;
        while offset < frames {
            let take = (CHUNK - self.pending_frames).min(frames - offset);
            let dst = self.pending_frames * 2;
            self.pending[dst..dst + take * 2]
                .copy_from_slice(&self.stereo[offset * 2..(offset + take) * 2]);
            self.pending_frames += take;
            offset += take;
            if self.pending_frames == CHUNK {
                self.pending_frames = 0;
                let input = InterleavedSlice::new(&self.pending[..], 2, CHUNK).expect("sized");
                let n_out = resampler.output_frames_next();
                let mut output =
                    InterleavedSlice::new_mut(&mut self.out[..], 2, n_out).expect("sized");
                if let Ok((_, produced)) = resampler.process_into_buffer(&input, &mut output, None)
                {
                    emit(&self.out[..produced * 2]);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passthrough_maps_channels() {
        let mut a = StreamAdapter::new(48_000, 1, 16).unwrap();
        let mut got = Vec::new();
        a.process(&[0.1, 0.2], |s| got.extend_from_slice(s));
        assert_eq!(got, [0.1, 0.1, 0.2, 0.2]);

        let mut a = StreamAdapter::new(48_000, 4, 16).unwrap();
        let mut got = Vec::new();
        a.process(&[1.0, 2.0, 3.0, 4.0], |s| got.extend_from_slice(s));
        assert_eq!(got, [1.0, 2.0]);
    }

    #[test]
    fn resamples_44100_to_48000() {
        let mut a = StreamAdapter::new(44_100, 2, 441).unwrap();
        let mut out = 0usize;
        let block: Vec<f32> = (0..441 * 2)
            .map(|i| ((i / 2) as f32 * 0.01).sin() * 0.5)
            .collect();
        for _ in 0..100 {
            // one second of 44.1 kHz input
            a.process(&block, |s| out += s.len() / 2);
        }
        // ≈48000 frames per second of input, minus up to one chunk in flight.
        assert!((47_000..=48_000).contains(&out), "{out}");
    }
}
