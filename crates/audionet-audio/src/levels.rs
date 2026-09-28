//! Signal level measurement for verifying that captured audio is real.
//!
//! Runs on non-real-time consumers. Reports peak and RMS in dBFS plus
//! counts of samples that indicate invalid data.

use serde::Serialize;

/// Accumulates level statistics over a window.
#[derive(Clone, Debug, Default)]
pub struct LevelMeter {
    samples: u64,
    peak: f32,
    sum_squares: f64,
    non_finite: u64,
    clipped: u64,
}

/// Levels over one window. dBFS values are `None` when the window was
/// entirely digital silence (or empty).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct LevelReading {
    pub samples: u64,
    pub peak_dbfs: Option<f64>,
    pub rms_dbfs: Option<f64>,
    /// NaN or infinite samples: always a bug or corrupt data.
    pub non_finite_samples: u64,
    /// Samples with magnitude above 1.0 (possible with float mix formats).
    pub clipped_samples: u64,
}

fn dbfs(linear: f64) -> Option<f64> {
    (linear > 0.0).then(|| 20.0 * linear.log10())
}

impl LevelMeter {
    pub fn add(&mut self, samples: &[f32]) {
        for &s in samples {
            if !s.is_finite() {
                self.non_finite += 1;
                continue;
            }
            let a = s.abs();
            if a > 1.0 {
                self.clipped += 1;
            }
            self.peak = self.peak.max(a);
            self.sum_squares += f64::from(s) * f64::from(s);
        }
        self.samples += samples.len() as u64;
    }

    /// Returns the window's reading and starts a new window.
    pub fn take(&mut self) -> LevelReading {
        let finite = self.samples - self.non_finite;
        let reading = LevelReading {
            samples: self.samples,
            peak_dbfs: dbfs(f64::from(self.peak)),
            rms_dbfs: if finite > 0 {
                dbfs((self.sum_squares / finite as f64).sqrt())
            } else {
                None
            },
            non_finite_samples: self.non_finite,
            clipped_samples: self.clipped,
        };
        *self = LevelMeter::default();
        reading
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_scale_square_wave_is_0_dbfs() {
        let mut m = LevelMeter::default();
        m.add(&[1.0, -1.0, 1.0, -1.0]);
        let r = m.take();
        assert!(r.peak_dbfs.unwrap().abs() < 1e-9);
        assert!(r.rms_dbfs.unwrap().abs() < 1e-9);
    }

    #[test]
    fn half_scale_is_minus_6_dbfs() {
        let mut m = LevelMeter::default();
        m.add(&[0.5, -0.5]);
        let r = m.take();
        assert!((r.peak_dbfs.unwrap() + 6.0206).abs() < 1e-3);
    }

    #[test]
    fn silence_has_no_level() {
        let mut m = LevelMeter::default();
        m.add(&[0.0; 16]);
        let r = m.take();
        assert_eq!((r.samples, r.peak_dbfs, r.rms_dbfs), (16, None, None));
    }

    #[test]
    fn counts_invalid_samples_and_resets() {
        let mut m = LevelMeter::default();
        m.add(&[f32::NAN, f32::INFINITY, 1.5, 0.1]);
        let r = m.take();
        assert_eq!(r.non_finite_samples, 2);
        assert_eq!(r.clipped_samples, 1);
        assert_eq!(m.take().samples, 0);
    }
}
