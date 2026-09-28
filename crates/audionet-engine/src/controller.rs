//! Clock-drift and buffer-depth control for one playout stream.
//!
//! Two questions, two terms, two time scales (AGENTS.md §11–13):
//!
//! * **Clock drift** — do sender and receiver run at the same long-term
//!   rate? Estimated by an *integrator* on the filtered depth error: if the
//!   sender is faster, depth keeps rising and the integral grows until the
//!   receiver consumes exactly that much faster. Slow (≈25 s) and clamped to
//!   a plausible range.
//!
//!   The integrator only learns while *locked*. A linear PI loop recovering
//!   from a stall would otherwise integrate the drain transient as if it
//!   were drift and undershoot the target afterwards (by e⁻² ≈ 13.5 % of
//!   the disturbance). So the lock uses hysteresis: it opens when the error
//!   jumps past `unlock_ms` or when the playout reports a disturbance
//!   (priming, underrun, trim), and closes again once the depth term has
//!   brought the error under `relock_ms` or `settle_s` has passed (three
//!   depth-loop time constants: any transient has decayed to about 5 %, so
//!   the remaining offset is genuine drift, which the depth term alone
//!   holds at drift ÷ Kp, e.g. 2.5 ms for 200 ppm). Genuine drift up to
//!   1000 ppm peaks at about 9 ms of error, below `unlock_ms`, so it is
//!   still learned.
//! * **Buffer depth** — is the buffer near its target? A *proportional*
//!   bias pulls depth back after a stall or burst. Medium speed (≈12 s),
//!   clamped so the pitch change stays inaudible.
//!
//! Both are rate corrections in parts per million applied through a
//! continuous resampler, never frame drops or repeats. Network jitter is
//! removed first by a one-pole low-pass filter on the measured depth.
//!
//! The structure is a critically damped PI loop on the buffer fill level,
//! the same family as the delay-locked loops used by zita-njbridge and
//! PipeWire's adaptive resampling. With the default gains the closed loop
//! has a double pole at 0.04 rad/s: no overshoot, ~25 s time constant.

use serde::Serialize;

/// Controller tuning. Defaults are derived in the module docs; change them
/// only with simulation evidence (AGENTS.md §4).
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct ControllerConfig {
    /// Target playout depth.
    pub target_ms: f64,
    /// Time constant of the depth low-pass filter.
    pub filter_tau_s: f64,
    /// Proportional gain: ppm of rate bias per ms of depth error.
    pub kp_ppm_per_ms: f64,
    /// Integral gain: ppm per (ms of error × second).
    pub ki_ppm_per_ms_s: f64,
    /// Clamp on the depth bias (proportional term).
    pub max_bias_ppm: f64,
    /// Clamp on the drift estimate (integral term).
    pub max_drift_ppm: f64,
    /// Error beyond which the drift integrator stops learning.
    pub unlock_ms: f64,
    /// Error below which a stopped integrator resumes learning.
    pub relock_ms: f64,
    /// Time after which a stopped integrator resumes learning regardless.
    pub settle_s: f64,
}

impl ControllerConfig {
    pub fn with_target(target_ms: f64) -> Self {
        Self {
            target_ms,
            ..Self::default()
        }
    }
}

impl Default for ControllerConfig {
    fn default() -> Self {
        Self {
            target_ms: 40.0,
            filter_tau_s: 1.0,
            kp_ppm_per_ms: 80.0,
            ki_ppm_per_ms_s: 1.6,
            max_bias_ppm: 2000.0,
            max_drift_ppm: 1000.0,
            unlock_ms: 15.0,
            relock_ms: 1.0,
            settle_s: 40.0,
        }
    }
}

/// The controller's current outputs, for diagnostics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct ControllerState {
    pub filtered_depth_ms: f64,
    pub depth_error_ms: f64,
    /// Estimated sender-minus-receiver clock rate difference.
    pub drift_ppm: f64,
    /// Temporary correction pulling depth toward target.
    pub depth_bias_ppm: f64,
    /// Total correction: consume input this many ppm faster than nominal.
    pub correction_ppm: f64,
    /// Whether the drift integrator is currently allowed to learn.
    pub locked: bool,
}

#[derive(Clone, Debug)]
pub struct DriftDepthController {
    config: ControllerConfig,
    filtered: Option<f64>,
    drift_ppm: f64,
    locked: bool,
    unlocked_for_s: f64,
    unlock_events: u64,
    state: ControllerState,
}

impl DriftDepthController {
    pub fn new(config: ControllerConfig) -> Self {
        Self {
            config,
            filtered: None,
            drift_ppm: 0.0,
            locked: false,
            unlocked_for_s: 0.0,
            unlock_events: 0,
            state: ControllerState::default(),
        }
    }

    /// How many times a disturbance stopped drift learning.
    pub fn unlock_events(&self) -> u64 {
        self.unlock_events
    }

    /// Reports a disturbance (priming, underrun, trim, burst): stop drift
    /// learning until depth is back near target.
    pub fn disturbance(&mut self) {
        if self.locked {
            self.unlock_events += 1;
        }
        self.locked = false;
        self.unlocked_for_s = 0.0;
    }

    pub fn config(&self) -> &ControllerConfig {
        &self.config
    }

    pub fn state(&self) -> ControllerState {
        self.state
    }

    /// Forgets everything, for a new stream with an unrelated clock.
    pub fn reset(&mut self) {
        self.filtered = None;
        self.drift_ppm = 0.0;
        self.locked = false;
        self.unlocked_for_s = 0.0;
        self.state = ControllerState::default();
    }

    /// Moves the target (adaptive playout). The step is a transient, not
    /// drift, so drift learning pauses until depth has followed.
    pub fn set_target(&mut self, target_ms: f64) {
        self.config.target_ms = target_ms;
        self.disturbance();
    }

    /// Restarts the depth filter after re-priming without discarding the
    /// drift estimate (same stream, same clocks).
    pub fn resync_depth(&mut self) {
        self.filtered = None;
        self.disturbance();
    }

    /// Feeds one depth measurement taken `dt_s` after the previous one and
    /// returns the correction in ppm (positive: consume input faster).
    pub fn update(&mut self, depth_ms: f64, dt_s: f64) -> f64 {
        let c = &self.config;
        let filtered = match self.filtered {
            None => depth_ms,
            Some(prev) => {
                let alpha = 1.0 - (-dt_s / c.filter_tau_s).exp();
                prev + alpha * (depth_ms - prev)
            }
        };
        self.filtered = Some(filtered);
        let error = filtered - c.target_ms;
        if self.locked && error.abs() > c.unlock_ms {
            self.locked = false;
            self.unlocked_for_s = 0.0;
            self.unlock_events += 1;
        } else if !self.locked {
            self.unlocked_for_s += dt_s;
            if error.abs() < c.relock_ms
                || (self.unlocked_for_s >= c.settle_s && error.abs() < c.unlock_ms)
            {
                self.locked = true;
            }
        }
        let locked = self.locked;
        if locked {
            self.drift_ppm = (self.drift_ppm + c.ki_ppm_per_ms_s * error * dt_s)
                .clamp(-c.max_drift_ppm, c.max_drift_ppm);
        }
        let bias = (c.kp_ppm_per_ms * error).clamp(-c.max_bias_ppm, c.max_bias_ppm);
        let correction = self.drift_ppm + bias;
        self.state = ControllerState {
            filtered_depth_ms: filtered,
            depth_error_ms: error,
            drift_ppm: self.drift_ppm,
            depth_bias_ppm: bias,
            correction_ppm: correction,
            locked,
        };
        correction
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Simulates the ideal plant: depth changes at (drift − correction) ppm.
    fn run(
        c: &mut DriftDepthController,
        mut depth_ms: f64,
        true_drift_ppm: f64,
        seconds: f64,
        mut disturb: impl FnMut(f64) -> f64,
    ) -> f64 {
        let dt = 0.01;
        let steps = (seconds / dt) as usize;
        for i in 0..steps {
            let corr = c.update(depth_ms + disturb(i as f64 * dt), dt);
            depth_ms += (true_drift_ppm - corr) * 1e-6 * dt * 1000.0;
        }
        depth_ms
    }

    #[test]
    fn learns_positive_and_negative_drift() {
        for drift in [50.0, 200.0, -200.0] {
            let mut c = DriftDepthController::new(ControllerConfig::default());
            let depth = run(&mut c, 40.0, drift, 300.0, |_| 0.0);
            let s = c.state();
            assert!(
                (s.drift_ppm - drift).abs() < 2.0,
                "drift {drift}: estimate {}",
                s.drift_ppm
            );
            assert!((depth - 40.0).abs() < 0.2, "drift {drift}: depth {depth}");
        }
    }

    #[test]
    fn depth_error_stays_small_while_learning_drift() {
        let mut c = DriftDepthController::new(ControllerConfig::default());
        let mut worst: f64 = 0.0;
        let dt = 0.01;
        let mut depth = 40.0;
        for _ in 0..30_000 {
            let corr = c.update(depth, dt);
            depth += (200.0 - corr) * 1e-6 * dt * 1000.0;
            worst = worst.max((depth - 40.0).abs());
        }
        assert!(worst < 5.0, "worst depth error {worst} ms");
    }

    #[test]
    fn recovers_from_a_stall_without_corrupting_drift() {
        let mut c = DriftDepthController::new(ControllerConfig::default());
        let depth = run(&mut c, 40.0, 100.0, 300.0, |_| 0.0);
        let before = c.state().drift_ppm;
        // A 50 ms receiver stall adds 50 ms of depth at once.
        let depth = run(&mut c, depth + 50.0, 100.0, 60.0, |_| 0.0);
        assert!((depth - 40.0).abs() < 1.0, "not back near target: {depth}");
        assert!(c.unlock_events() >= 1);
        // The relock tail may nudge the estimate briefly, but not far...
        assert!(
            (c.state().drift_ppm - before).abs() < 40.0,
            "stall disturbed the drift estimate: {} -> {}",
            before,
            c.state().drift_ppm
        );
        // ...and it re-converges to the true drift.
        let depth = run(&mut c, depth, 100.0, 200.0, |_| 0.0);
        assert!(
            (c.state().drift_ppm - 100.0).abs() < 3.0,
            "{}",
            c.state().drift_ppm
        );
        assert!((depth - 40.0).abs() < 0.2);
        // During recovery the bias never exceeded its clamp.
        assert!(c.state().depth_bias_ppm.abs() <= 2000.0);
    }

    #[test]
    fn jitter_does_not_drive_the_rate() {
        // ±10 ms of measurement noise, deterministic.
        let mut seed = 0x1234_5678u64;
        let mut noise = move |_t: f64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % 2001) as f64 / 100.0 - 10.0
        };
        let mut c = DriftDepthController::new(ControllerConfig::default());
        let mut max_corr: f64 = 0.0;
        let dt = 0.01;
        let mut depth = 40.0;
        for i in 0..30_000 {
            let corr = c.update(depth + noise(0.0), dt);
            if i > 500 {
                max_corr = max_corr.max(corr.abs());
            }
            depth += (0.0 - corr) * 1e-6 * dt * 1000.0;
        }
        assert!(max_corr < 400.0, "jitter moved the rate by {max_corr} ppm");
        assert!(c.state().drift_ppm.abs() < 20.0);
    }

    #[test]
    fn clamps_hold() {
        let mut c = DriftDepthController::new(ControllerConfig::default());
        // Absurd 5000 ppm drift: estimate saturates at the plausibility clamp.
        run(&mut c, 40.0, 5000.0, 600.0, |_| 0.0);
        let s = c.state();
        assert!(s.drift_ppm <= 1000.0);
        assert!(s.depth_bias_ppm <= 2000.0);
        assert!(s.correction_ppm <= 3000.0);
    }

    #[test]
    fn reset_forgets_drift() {
        let mut c = DriftDepthController::new(ControllerConfig::default());
        run(&mut c, 40.0, 200.0, 200.0, |_| 0.0);
        c.reset();
        assert_eq!(c.state().drift_ppm, 0.0);
        c.update(40.0, 0.01);
        assert!(c.state().drift_ppm.abs() < 1e-9);
    }
}
