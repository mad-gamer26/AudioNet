//! Adaptive playout target (AGENTS.md §25): the smallest *stable* latency.
//!
//! * Start at a conservative target.
//! * **Raise quickly** when the buffer proves too small: packets arriving
//!   after their audio was already concealed (*late*), or underruns that
//!   end within [`AdaptiveConfig::outage_gap_ms`] (a longer silence is an
//!   outage, which a bigger buffer would not have prevented). Events less
//!   than a second apart count as one incident, and one isolated incident
//!   never changes the target: it takes
//!   [`AdaptiveConfig::raise_after_incidents`] within
//!   [`AdaptiveConfig::incident_window_s`].
//! * **Lower slowly** after [`AdaptiveConfig::stable_s`] without incidents,
//!   and only if the lowest depth seen meanwhile leaves
//!   [`AdaptiveConfig::headroom_ms`] after the step.
//! * **No oscillation**: when a lowered target has to be raised again, the
//!   stable time needed before the next lowering doubles.
//!
//! Raising lets the queue build through the depth controller (or through
//! re-priming after an underrun) rather than inserting silence; lowering
//! drains the small excess the same way (AGENTS.md §26).
//!
//! Real-time safe: plain arithmetic, no allocation. Every change carries
//! its reason so reports can say why the target moved.

use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct AdaptiveConfig {
    pub min_ms: f64,
    pub max_ms: f64,
    pub step_up_ms: f64,
    pub step_down_ms: f64,
    pub raise_after_incidents: u32,
    pub incident_window_s: f64,
    /// Events closer together than this are one incident.
    pub incident_merge_s: f64,
    /// After a raise, incidents are ignored this long while the queue builds.
    pub raise_holdoff_s: f64,
    /// Incident-free time needed before lowering (doubles after a raise that
    /// followed a lowering, up to `max_stable_s`).
    pub stable_s: f64,
    pub max_stable_s: f64,
    /// Depth that must remain, at the lowest point seen, after lowering.
    pub headroom_ms: f64,
    /// An underrun whose audio resumes within this is a jitter underrun;
    /// longer is an outage and does not raise the target.
    pub outage_gap_ms: f64,
    /// After an outage, late packets are ignored this long: the backlog
    /// arriving at once, reordered, says nothing about the target.
    pub outage_quiet_s: f64,
}

impl AdaptiveConfig {
    /// Native path defaults: AGENTS.md advises against running below
    /// 25 ms until diagnostics show the margin.
    pub const NATIVE: Self = Self {
        min_ms: 25.0,
        max_ms: 200.0,
        step_up_ms: 10.0,
        step_down_ms: 5.0,
        raise_after_incidents: 2,
        incident_window_s: 30.0,
        incident_merge_s: 1.0,
        raise_holdoff_s: 10.0,
        stable_s: 120.0,
        max_stable_s: 1800.0,
        headroom_ms: 15.0,
        outage_gap_ms: 150.0,
        outage_quiet_s: 2.0,
    };

    /// WAN paths (WebRTC, browser 20 ms frames): a higher floor.
    pub const WAN: Self = Self {
        min_ms: 40.0,
        ..Self::NATIVE
    };
}

/// Why the target changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeReason {
    /// Packets arrived after their audio had been concealed.
    LatePackets,
    /// The buffer ran dry and audio resumed shortly after.
    Underruns,
    /// No incidents for the stable period, with depth to spare.
    Stable,
}

impl ChangeReason {
    pub fn code(self) -> u64 {
        match self {
            Self::LatePackets => 1,
            Self::Underruns => 2,
            Self::Stable => 3,
        }
    }

    pub fn from_code(code: u64) -> Option<Self> {
        match code {
            1 => Some(Self::LatePackets),
            2 => Some(Self::Underruns),
            3 => Some(Self::Stable),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct TargetChange {
    pub from_ms: f64,
    pub to_ms: f64,
    pub reason: ChangeReason,
    /// For raises: incidents in the window; for lowering: seconds stable.
    pub evidence: f64,
}

#[derive(Clone, Debug)]
pub struct AdaptiveTarget {
    config: AdaptiveConfig,
    target_ms: f64,
    now_s: f64,
    /// Start times of recent incidents (a small ring; only the count within
    /// the window matters).
    incidents: [f64; 8],
    incident_count: usize,
    last_event_s: f64,
    last_incident_reason: ChangeReason,
    holdoff_until_s: f64,
    stable_for_s: f64,
    stable_needed_s: f64,
    min_depth_ms: f64,
    last_change_was_lower: bool,
    /// Time of an underrun awaiting classification (jitter or outage).
    pending_underrun_ns: Option<u64>,
    /// Late packets before this time are outage recovery, not jitter.
    quiet_until_s: f64,
}

impl AdaptiveTarget {
    pub fn new(config: AdaptiveConfig, start_ms: f64) -> Self {
        Self {
            target_ms: start_ms.clamp(config.min_ms, config.max_ms),
            now_s: 0.0,
            incidents: [f64::NEG_INFINITY; 8],
            incident_count: 0,
            last_event_s: f64::NEG_INFINITY,
            last_incident_reason: ChangeReason::LatePackets,
            holdoff_until_s: 0.0,
            stable_for_s: 0.0,
            stable_needed_s: config.stable_s,
            min_depth_ms: f64::INFINITY,
            last_change_was_lower: false,
            pending_underrun_ns: None,
            quiet_until_s: 0.0,
            config,
        }
    }

    pub fn target_ms(&self) -> f64 {
        self.target_ms
    }

    pub fn config(&self) -> &AdaptiveConfig {
        &self.config
    }

    /// Seconds without incidents so far.
    pub fn stable_for_s(&self) -> f64 {
        self.stable_for_s
    }

    /// Incident-free time currently required before lowering.
    pub fn stable_needed_s(&self) -> f64 {
        self.stable_needed_s
    }

    /// `count` packets arrived after their slot had been concealed. Ignored
    /// while an underrun awaits classification and just after an outage.
    pub fn late_packets(&mut self, count: u64) -> Option<TargetChange> {
        if count == 0 || self.pending_underrun_ns.is_some() || self.now_s < self.quiet_until_s {
            return None;
        }
        self.event(ChangeReason::LatePackets)
    }

    /// The playout ran dry at `now_ns`; classified once audio arrives again.
    pub fn underrun(&mut self, now_ns: u64) {
        self.pending_underrun_ns.get_or_insert(now_ns);
    }

    /// New audio last arrived at `arrival_ns` (see `StreamControl`).
    pub fn arrival(&mut self, arrival_ns: u64) -> Option<TargetChange> {
        let at = self.pending_underrun_ns?;
        if arrival_ns <= at {
            return None;
        }
        self.pending_underrun_ns = None;
        let gap_ms = (arrival_ns - at) as f64 / 1e6;
        if gap_ms <= self.config.outage_gap_ms {
            self.event(ChangeReason::Underruns)
        } else {
            // An outage: a bigger buffer would not have helped.
            self.quiet_until_s = self.now_s + self.config.outage_quiet_s;
            self.reset_stable();
            None
        }
    }

    /// Advances time by `dt_s`. `depth_ms` is the measured depth while
    /// playing, `None` while priming.
    pub fn tick(&mut self, dt_s: f64, depth_ms: Option<f64>) -> Option<TargetChange> {
        self.now_s += dt_s;
        let depth = depth_ms?;
        if self.now_s < self.holdoff_until_s {
            return None;
        }
        self.stable_for_s += dt_s;
        self.min_depth_ms = self.min_depth_ms.min(depth);
        let c = self.config;
        if self.stable_for_s < self.stable_needed_s || self.target_ms <= c.min_ms {
            return None;
        }
        let to = (self.target_ms - c.step_down_ms).max(c.min_ms);
        let step = self.target_ms - to;
        if self.min_depth_ms - step < c.headroom_ms {
            // Not enough margin to go lower; keep watching.
            self.stable_for_s = 0.0;
            self.min_depth_ms = f64::INFINITY;
            return None;
        }
        let change = TargetChange {
            from_ms: self.target_ms,
            to_ms: to,
            reason: ChangeReason::Stable,
            evidence: self.stable_for_s,
        };
        self.target_ms = to;
        self.last_change_was_lower = true;
        self.reset_stable();
        Some(change)
    }

    fn reset_stable(&mut self) {
        self.stable_for_s = 0.0;
        self.min_depth_ms = f64::INFINITY;
    }

    fn event(&mut self, reason: ChangeReason) -> Option<TargetChange> {
        self.reset_stable();
        if self.now_s < self.holdoff_until_s {
            return None;
        }
        let c = self.config;
        if self.now_s - self.last_event_s >= c.incident_merge_s {
            self.incidents[self.incident_count % self.incidents.len()] = self.now_s;
            self.incident_count += 1;
            self.last_incident_reason = reason;
        } else if reason == ChangeReason::Underruns {
            // An underrun is the stronger reason within one incident.
            self.last_incident_reason = reason;
        }
        self.last_event_s = self.now_s;
        let recent = self
            .incidents
            .iter()
            .filter(|t| self.now_s - **t <= c.incident_window_s)
            .count();
        if recent < c.raise_after_incidents as usize || self.target_ms >= c.max_ms {
            return None;
        }
        let to = (self.target_ms + c.step_up_ms).min(c.max_ms);
        let change = TargetChange {
            from_ms: self.target_ms,
            to_ms: to,
            reason: self.last_incident_reason,
            evidence: recent as f64,
        };
        self.target_ms = to;
        self.holdoff_until_s = self.now_s + c.raise_holdoff_s;
        self.incidents = [f64::NEG_INFINITY; 8];
        if self.last_change_was_lower {
            self.stable_needed_s = (self.stable_needed_s * 2.0).min(c.max_stable_s);
        }
        self.last_change_was_lower = false;
        Some(change)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DT: f64 = 0.01;

    fn run(a: &mut AdaptiveTarget, seconds: f64, depth: f64) -> Vec<TargetChange> {
        let mut changes = Vec::new();
        for _ in 0..(seconds / DT) as u64 {
            changes.extend(a.tick(DT, Some(depth)));
        }
        changes
    }

    #[test]
    fn one_isolated_incident_changes_nothing() {
        let mut a = AdaptiveTarget::new(AdaptiveConfig::NATIVE, 40.0);
        run(&mut a, 5.0, 40.0);
        assert_eq!(a.late_packets(3), None);
        // A burst of late packets within a second is still one incident.
        run(&mut a, 0.5, 40.0);
        assert_eq!(a.late_packets(1), None);
        assert_eq!(a.target_ms(), 40.0);
    }

    #[test]
    fn two_incidents_raise_then_hold_off() {
        let mut a = AdaptiveTarget::new(AdaptiveConfig::NATIVE, 40.0);
        run(&mut a, 5.0, 40.0);
        assert_eq!(a.late_packets(1), None);
        run(&mut a, 3.0, 40.0);
        let change = a.late_packets(1).expect("raised");
        assert_eq!((change.from_ms, change.to_ms), (40.0, 50.0));
        assert_eq!(change.reason, ChangeReason::LatePackets);
        // During the holdoff further incidents are ignored.
        run(&mut a, 2.0, 45.0);
        assert_eq!(a.late_packets(1), None);
        run(&mut a, 2.0, 45.0);
        assert_eq!(a.late_packets(1), None);
        assert_eq!(a.target_ms(), 50.0);
    }

    #[test]
    fn short_underruns_raise_but_outages_do_not() {
        let mut a = AdaptiveTarget::new(AdaptiveConfig::NATIVE, 40.0);
        run(&mut a, 5.0, 40.0);
        // Two outages: audio came back after 2 s each time.
        for i in 0..2u64 {
            let t = (10 + i * 10) * 1_000_000_000;
            a.underrun(t);
            assert_eq!(a.arrival(t + 2_000_000_000), None);
        }
        assert_eq!(a.target_ms(), 40.0);
        // Two jitter underruns (audio back within 30 ms).
        a.underrun(40_000_000_000);
        assert_eq!(a.arrival(40_030_000_000), None);
        run(&mut a, 2.0, 40.0);
        a.underrun(42_000_000_000);
        let change = a.arrival(42_030_000_000).expect("raised");
        assert_eq!(change.reason, ChangeReason::Underruns);
        assert_eq!(change.to_ms, 50.0);
    }

    #[test]
    fn late_packets_during_outage_recovery_are_ignored() {
        let mut a = AdaptiveTarget::new(AdaptiveConfig::NATIVE, 40.0);
        run(&mut a, 5.0, 40.0);
        for i in 0..3u64 {
            let t = (10 + i * 5) * 1_000_000_000;
            a.underrun(t);
            // Late packets before the audio is back, then the backlog.
            assert_eq!(a.late_packets(5), None);
            assert_eq!(a.arrival(t + 2_000_000_000), None);
            assert_eq!(a.late_packets(20), None);
            run(&mut a, 5.0, 40.0);
        }
        assert_eq!(a.target_ms(), 40.0);
    }

    #[test]
    fn lowers_slowly_with_headroom_only() {
        let mut a = AdaptiveTarget::new(AdaptiveConfig::NATIVE, 40.0);
        // Depth dips to 30 ms at worst: 30 - 5 >= 15, so it may lower.
        assert!(run(&mut a, 119.0, 30.0).is_empty());
        let changes = run(&mut a, 2.0, 30.0);
        assert_eq!(changes.len(), 1);
        assert_eq!((changes[0].from_ms, changes[0].to_ms), (40.0, 35.0));
        assert_eq!(changes[0].reason, ChangeReason::Stable);
        // Depth dipping to 18 ms leaves too little margin: no further change.
        assert!(run(&mut a, 600.0, 18.0).is_empty());
        assert_eq!(a.target_ms(), 35.0);
    }

    #[test]
    fn never_below_minimum_or_above_maximum() {
        let mut a = AdaptiveTarget::new(AdaptiveConfig::NATIVE, 30.0);
        run(&mut a, 3600.0, 100.0);
        assert_eq!(a.target_ms(), 25.0);
        let mut b = AdaptiveTarget::new(AdaptiveConfig::NATIVE, 195.0);
        run(&mut b, 1.0, 100.0);
        b.late_packets(1);
        run(&mut b, 2.0, 100.0);
        b.late_packets(1);
        assert_eq!(b.target_ms(), 200.0);
    }

    #[test]
    fn raising_after_lowering_doubles_the_stable_time() {
        let mut a = AdaptiveTarget::new(AdaptiveConfig::NATIVE, 40.0);
        run(&mut a, 121.0, 40.0);
        assert_eq!(a.target_ms(), 35.0);
        a.late_packets(1);
        run(&mut a, 2.0, 35.0);
        assert!(a.late_packets(1).is_some());
        assert_eq!(a.target_ms(), 45.0);
        assert_eq!(a.stable_needed_s(), 240.0);
        // Needs 240 s (after the 10 s holdoff) before lowering again.
        assert!(run(&mut a, 200.0, 45.0).is_empty());
        assert_eq!(run(&mut a, 60.0, 45.0).len(), 1);
    }
}
