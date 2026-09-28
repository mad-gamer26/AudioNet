//! Deterministic network/clock simulation of the full media path
//! (AGENTS.md §29): the real `SenderCore`, `PacketStage` and `Playout`
//! (real Opus, real encryption, real resampler) driven by virtual time.
//!
//! Each scenario asserts behavior — bounded latency, recovery toward the
//! target, drift convergence, bounded underruns — not just "no crash".
//!
//! The hour-long soak is `#[ignore]`d; run it with
//! `cargo test -p audionet-engine --release --test sim -- --ignored`.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::sync::Arc;

use audionet_audio::ring::audio_ring;
use audionet_codec::EncoderConfig;
use audionet_engine::adaptive::AdaptiveConfig;
use audionet_engine::playout::{Playout, PlayoutConfig, PlayoutSnapshot, StreamControl};
use audionet_engine::receiver::{PacketStage, ReceiverConfig, ReceiverSnapshot};
use audionet_engine::sender::SenderCore;
use audionet_transport::crypto::{MediaCipher, PresharedKey};

const MS: u64 = 1_000_000;
const SEC: u64 = 1_000_000_000;

/// Deterministic xorshift PRNG.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}

#[derive(Clone)]
struct Scenario {
    seconds: u64,
    sender_ppm: f64,
    base_delay_ms: f64,
    jitter_ms: f64,
    loss: f64,
    /// (probability a burst starts per packet, burst length)
    burst: Option<(f64, u32)>,
    duplicate: f64,
    /// Network receive thread paused: deliveries held until the end.
    receiver_stalls: Vec<(u64, u64)>,
    /// Render callbacks paused (device thread starved): consumption stops.
    render_stalls: Vec<(u64, u64)>,
    sender_stalls: Vec<(u64, u64)>,
    restart_at_s: Option<u64>,
    target_ms: f64,
    /// Adapt the target (starting at `target_ms`).
    adaptive: Option<AdaptiveConfig>,
    /// From this second on, jitter becomes this many ms.
    jitter_change: Option<(u64, f64)>,
    seed: u64,
}

impl Default for Scenario {
    fn default() -> Self {
        Self {
            seconds: 60,
            sender_ppm: 0.0,
            base_delay_ms: 5.0,
            jitter_ms: 0.0,
            loss: 0.0,
            burst: None,
            duplicate: 0.0,
            receiver_stalls: vec![],
            render_stalls: vec![],
            sender_stalls: vec![],
            restart_at_s: None,
            target_ms: 40.0,
            adaptive: None,
            jitter_change: None,
            seed: 0x9E37_79B9_7F4A_7C15,
        }
    }
}

/// Depth samples taken once per simulated 100 ms.
struct Outcome {
    depth: Vec<(f64, f64, bool)>, // (seconds, depth ms, playing)
    targets: Vec<(f64, f64)>,     // (seconds, target ms)
    late_after: Vec<(f64, u64)>,  // (seconds, late packets so far)
    playout: PlayoutSnapshot,
    receiver: ReceiverSnapshot,
    underruns_after: Vec<(f64, u64)>,
}

impl Outcome {
    fn depth_between(&self, from_s: f64, to_s: f64) -> (f64, f64) {
        let v: Vec<f64> = self
            .depth
            .iter()
            .filter(|(t, _, playing)| *t >= from_s && *t <= to_s && *playing)
            .map(|(_, d, _)| *d)
            .collect();
        assert!(!v.is_empty(), "no playing samples in {from_s}..{to_s}");
        (
            v.iter().cloned().fold(f64::INFINITY, f64::min),
            v.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
        )
    }

    fn target_at(&self, t: f64) -> f64 {
        self.targets
            .iter()
            .filter(|(s, _)| *s <= t)
            .map(|(_, v)| *v)
            .next_back()
            .expect("sampled")
    }

    fn late_between(&self, from_s: f64, to_s: f64) -> u64 {
        let at = |t: f64| {
            self.late_after
                .iter()
                .filter(|(s, _)| *s <= t)
                .map(|(_, n)| *n)
                .next_back()
                .unwrap_or(0)
        };
        at(to_s) - at(from_s)
    }

    fn underruns_between(&self, from_s: f64, to_s: f64) -> u64 {
        let at = |t: f64| {
            self.underruns_after
                .iter()
                .filter(|(s, _)| *s <= t)
                .map(|(_, u)| *u)
                .next_back()
                .unwrap_or(0)
        };
        at(to_s) - at(from_s)
    }
}

fn tone(buf: &mut [f32], start: u64) {
    for (i, frame) in buf.chunks_mut(2).enumerate() {
        let v =
            0.3 * (2.0 * std::f64::consts::PI * 440.0 * (start + i as u64) as f64 / 48_000.0).sin();
        frame[0] = v as f32;
        frame[1] = v as f32;
    }
}

fn simulate(s: &Scenario) -> Outcome {
    let key = PresharedKey::from_bytes([7; 32]);
    let mut rng = Rng(s.seed);

    let (producer, consumer) = audio_ring(48_000, 2);
    let control = Arc::new(StreamControl::default());
    let mut stage = PacketStage::new(
        ReceiverConfig::default(),
        MediaCipher::new(&key),
        producer,
        Arc::clone(&control),
    )
    .unwrap();
    let mut playout = Playout::new(
        consumer,
        Arc::clone(&control),
        match s.adaptive {
            Some(a) => PlayoutConfig::adaptive(s.target_ms, a),
            None => PlayoutConfig::with_target(s.target_ms),
        },
        48_000,
        2,
        Arc::default(),
    )
    .unwrap();
    let mut sender = SenderCore::new(EncoderConfig::BASELINE, MediaCipher::new(&key)).unwrap();

    // Sender frame period at its (drifted) clock.
    let sender_period = 480.0 / (48_000.0 * (1.0 + s.sender_ppm * 1e-6)) * 1e9;
    let mut next_frame_ns = 0.0f64;
    let mut frames_sent = 0u64;
    let mut pcm = vec![0.0f32; 960];

    // Packets in flight: (delivery time, order) → bytes.
    let mut in_flight: BinaryHeap<Reverse<(u64, u64, Vec<u8>)>> = BinaryHeap::new();
    let mut order = 0u64;
    let mut burst_left = 0u32;

    let render_period = 10 * MS; // receiver device: exactly 480 frames per 10 ms
    let mut next_render = 0u64;
    let mut out = vec![0.0f32; 960];

    let end = s.seconds * SEC;
    let mut next_sample = 0u64;
    let mut outcome = Outcome {
        depth: Vec::new(),
        targets: Vec::new(),
        late_after: Vec::new(),
        playout: PlayoutSnapshot::default(),
        receiver: ReceiverSnapshot::default(),
        underruns_after: Vec::new(),
    };

    let in_stall = |stalls: &[(u64, u64)], t: u64| -> Option<u64> {
        stalls
            .iter()
            .find(|(at, dur)| t >= at * SEC && t < at * SEC + dur * MS)
            .map(|(at, dur)| at * SEC + dur * MS)
    };

    let mut now = 0u64;
    while now < end {
        // Next event time.
        let t_frame = next_frame_ns as u64;
        let t_deliver = in_flight.peek().map_or(u64::MAX, |Reverse((t, _, _))| *t);
        now = t_frame.min(t_deliver).min(next_render).min(next_sample);
        if now >= end {
            break;
        }

        if now == t_frame {
            if let Some(restart) = s.restart_at_s {
                if frames_sent == restart * 100 {
                    sender =
                        SenderCore::new(EncoderConfig::BASELINE, MediaCipher::new(&key)).unwrap();
                }
            }
            tone(&mut pcm, frames_sent * 480);
            frames_sent += 1;
            next_frame_ns += sender_period;
            // A sender stall delays transmission until it ends (frames are
            // queued in the capture ring and then sent in a burst).
            let send_at = in_stall(&s.sender_stalls, now).unwrap_or(now);
            let mut packets = Vec::new();
            sender.push(&pcm, |p| packets.push(p.to_vec()));
            for p in packets {
                let lost = if burst_left > 0 {
                    burst_left -= 1;
                    true
                } else if let Some((prob, len)) = s.burst.filter(|(prob, _)| rng.unit() < *prob) {
                    let _ = prob;
                    burst_left = len - 1;
                    true
                } else {
                    rng.unit() < s.loss
                };
                if lost {
                    continue;
                }
                let copies = if rng.unit() < s.duplicate { 2 } else { 1 };
                for _ in 0..copies {
                    let jitter_ms = match s.jitter_change {
                        Some((at, j)) if now >= at * SEC => j,
                        _ => s.jitter_ms,
                    };
                    let jitter = (rng.unit() * 2.0 - 1.0) * jitter_ms;
                    let delay = (s.base_delay_ms + jitter).max(0.0);
                    let mut arrive = send_at + (delay * MS as f64) as u64;
                    // A receiver stall holds deliveries until it ends.
                    if let Some(until) = in_stall(&s.receiver_stalls, arrive) {
                        arrive = until;
                    }
                    order += 1;
                    in_flight.push(Reverse((arrive, order, p.clone())));
                }
            }
            continue;
        }
        if now == t_deliver {
            let Reverse((t, _, mut bytes)) = in_flight.pop().unwrap();
            stage.on_datagram(&mut bytes, t);
            continue;
        }
        if now == next_render {
            stage.on_tick(now);
            if in_stall(&s.render_stalls, now).is_none() {
                playout.render(&mut out, now);
            }
            next_render += render_period;
            continue;
        }
        if now == next_sample {
            let snap = playout.stats().snapshot();
            outcome.depth.push((
                now as f64 / 1e9,
                snap.depth_us.current as f64 / 1000.0,
                snap.playing,
            ));
            outcome
                .underruns_after
                .push((now as f64 / 1e9, snap.underruns));
            outcome.targets.push((now as f64 / 1e9, snap.target_ms));
            outcome
                .late_after
                .push((now as f64 / 1e9, stage.stats().snapshot().late_packets));
            next_sample += 100 * MS;
        }
    }
    outcome.playout = playout.stats().snapshot();
    outcome.receiver = stage.stats().snapshot();
    outcome
}

#[test]
#[ignore = "diagnostic time series"]
fn trace_drift_50() {
    let o = simulate(&Scenario {
        seconds: 400,
        sender_ppm: 50.0,
        jitter_ms: 2.0,
        ..Default::default()
    });
    for (t, d, p) in o.depth.iter().step_by(50) {
        println!("t {t:6.1} depth {d:6.2} playing {p}");
    }
    report("trace", &o);
}

fn report(name: &str, o: &Outcome) {
    let p = &o.playout;
    let r = &o.receiver;
    println!(
        "{name}: underruns {} primings {} trims {} drift {:.1} ppm bias {:.1} ppm depth now {:.1} ms \
         (min {:?} max {:?} us) loss {:.2}% late {} concealed {} fec {} resyncs {} dup {} reorder {} jitter {:.2} ms",
        p.underruns,
        p.primings,
        p.catastrophic_trims,
        p.drift_ppm,
        p.depth_bias_ppm,
        p.depth_us.current as f64 / 1000.0,
        p.depth_us.min,
        p.depth_us.max,
        r.loss_percent,
        r.late_packets,
        r.concealed_frames,
        r.fec_attempts,
        r.outage_resyncs,
        r.sequence.duplicates,
        r.sequence.reordered,
        r.jitter_ms
    );
}

#[test]
fn clean_network_is_stable_at_target() {
    let o = simulate(&Scenario {
        seconds: 120,
        jitter_ms: 2.0,
        ..Default::default()
    });
    report("clean", &o);
    assert_eq!(o.playout.underruns, 0);
    assert_eq!(o.playout.catastrophic_trims, 0);
    let (lo, hi) = o.depth_between(20.0, 120.0);
    assert!(lo > 30.0 && hi < 55.0, "depth {lo}..{hi}");
    assert!(o.playout.drift_ppm.abs() < 20.0);
}

#[test]
fn loss_and_jitter_stay_bounded() {
    for (loss, jitter) in [(0.01, 10.0), (0.05, 10.0)] {
        // Until the adaptive target exists (phase 4), a ±10 ms network
        // needs a target above jitter spread + reorder wait: 60 ms, the
        // AGENTS.md "Reliable" range.
        let o = simulate(&Scenario {
            seconds: 120,
            loss,
            jitter_ms: jitter,
            base_delay_ms: 15.0,
            target_ms: 60.0,
            ..Default::default()
        });
        report(&format!("loss {loss} jitter {jitter}"), &o);
        let expected = loss * 100.0;
        assert!((o.receiver.loss_percent - expected).abs() < expected * 0.5 + 0.3);
        assert!(o.receiver.concealed_frames > 0);
        assert_eq!(o.playout.catastrophic_trims, 0);
        let (_, hi) = o.depth_between(20.0, 120.0);
        assert!(hi < 90.0, "latency grew to {hi} ms");
        assert!(
            o.underruns_between(20.0, 120.0) <= 1,
            "underruns {}",
            o.playout.underruns
        );
    }
}

#[test]
fn burst_loss_resyncs_instead_of_adding_latency() {
    let o = simulate(&Scenario {
        seconds: 120,
        burst: Some((0.002, 20)), // 200 ms outages
        jitter_ms: 2.0,
        ..Default::default()
    });
    report("burst", &o);
    assert!(o.receiver.outage_resyncs > 0);
    assert_eq!(o.playout.catastrophic_trims, 0);
    let (_, hi) = o.depth_between(20.0, 120.0);
    assert!(hi < 70.0, "outages inflated latency to {hi} ms");
}

#[test]
fn reordering_and_duplicates_are_absorbed() {
    let o = simulate(&Scenario {
        seconds: 60,
        jitter_ms: 8.0, // ±8 ms on a 10 ms packet interval reorders often
        base_delay_ms: 10.0,
        duplicate: 0.02,
        target_ms: 60.0,
        ..Default::default()
    });
    report("reorder+dup", &o);
    assert!(o.receiver.sequence.reordered > 50);
    assert!(o.receiver.sequence.duplicates > 50);
    assert_eq!(o.receiver.sequence.lost(), 0);
    assert!(o.underruns_between(10.0, 60.0) <= 2);
}

#[test]
fn clock_drift_is_tracked_both_ways() {
    for ppm in [50.0, 200.0, -200.0] {
        let o = simulate(&Scenario {
            seconds: 400,
            sender_ppm: ppm,
            jitter_ms: 2.0,
            ..Default::default()
        });
        report(&format!("drift {ppm}"), &o);
        assert!(
            (o.playout.drift_ppm - ppm).abs() < 15.0,
            "{ppm} ppm: estimate {}",
            o.playout.drift_ppm
        );
        let (lo, hi) = o.depth_between(200.0, 400.0);
        assert!(lo > 32.0 && hi < 48.0, "{ppm} ppm: depth {lo}..{hi}");
        assert_eq!(o.underruns_between(10.0, 400.0), 0, "{ppm} ppm");
        assert_eq!(o.playout.catastrophic_trims, 0);
    }
}

#[test]
fn recovers_after_a_50_ms_render_stall() {
    // The render thread misses 50 ms of callbacks while packets keep
    // arriving: depth jumps by 50 ms and must drain back (AGENTS.md §42).
    let o = simulate(&Scenario {
        seconds: 300,
        sender_ppm: 200.0,
        jitter_ms: 2.0,
        render_stalls: vec![(150, 50)],
        ..Default::default()
    });
    report("render stall", &o);
    let (_, peak) = o.depth_between(150.0, 152.0);
    assert!(peak > 80.0, "stall should raise depth, peak {peak}");
    assert_eq!(
        o.playout.latency_drains, 0,
        "a 50 ms excess is left to the rate controller"
    );
    let (lo, hi) = o.depth_between(210.0, 300.0);
    assert!(
        lo > 32.0 && hi < 48.0,
        "did not return to target: {lo}..{hi}"
    );
    assert!(
        (o.playout.drift_ppm - 200.0).abs() < 25.0,
        "drift {}",
        o.playout.drift_ppm
    );
    assert_eq!(
        o.playout.catastrophic_trims, 0,
        "normal recovery, not the safety net"
    );
}

#[test]
fn large_render_stall_is_drained_quickly_not_over_minutes() {
    // 180 ms of excess would take ~90 s at the 2000 ppm clamp; the
    // controlled latency drain removes it within a few seconds instead.
    let o = simulate(&Scenario {
        seconds: 120,
        jitter_ms: 2.0,
        render_stalls: vec![(60, 180)],
        ..Default::default()
    });
    report("large render stall", &o);
    assert_eq!(o.playout.latency_drains, 1);
    assert_eq!(o.playout.catastrophic_trims, 0);
    let (_, hi) = o.depth_between(63.0, 120.0);
    assert!(hi < 60.0, "excess not drained: {hi} ms");
}

#[test]
fn recovers_after_a_50_ms_network_thread_stall() {
    // The receive thread pauses 50 ms: longer than the 40 ms target, so one
    // underrun is expected; then a burst arrives and depth must settle.
    let o = simulate(&Scenario {
        seconds: 200,
        jitter_ms: 2.0,
        receiver_stalls: vec![(100, 50)],
        ..Default::default()
    });
    report("network stall", &o);
    assert!(o.underruns_between(99.0, 101.0) <= 1);
    assert_eq!(o.playout.catastrophic_trims, 0);
    let (lo, hi) = o.depth_between(160.0, 200.0);
    assert!(lo > 32.0 && hi < 48.0, "depth {lo}..{hi}");
}

#[test]
fn recovers_after_a_50_ms_sender_stall() {
    let o = simulate(&Scenario {
        seconds: 200,
        jitter_ms: 2.0,
        sender_stalls: vec![(100, 50)],
        ..Default::default()
    });
    report("sender stall", &o);
    assert!(o.underruns_between(100.0, 101.0) <= 1);
    let (lo, hi) = o.depth_between(160.0, 200.0);
    assert!(lo > 32.0 && hi < 48.0, "depth {lo}..{hi}");
}

#[test]
fn stream_restart_resets_and_resumes() {
    let o = simulate(&Scenario {
        seconds: 60,
        sender_ppm: 150.0,
        jitter_ms: 2.0,
        restart_at_s: Some(30),
        ..Default::default()
    });
    report("restart", &o);
    assert_eq!(o.receiver.stream_starts, 2);
    assert_eq!(o.playout.stream_resets, 2);
    let (lo, hi) = o.depth_between(35.0, 60.0);
    assert!(lo > 25.0 && hi < 60.0, "depth {lo}..{hi}");
}

#[test]
#[ignore = "one-hour soak; run with --release -- --ignored"]
fn one_hour_soak_has_no_latency_growth() {
    let o = simulate(&Scenario {
        seconds: 3600,
        sender_ppm: 120.0,
        jitter_ms: 5.0,
        base_delay_ms: 10.0,
        loss: 0.005,
        duplicate: 0.001,
        render_stalls: vec![(900, 30), (1800, 50), (2700, 40)],
        target_ms: 50.0,
        ..Default::default()
    });
    report("soak", &o);
    // Compare the first and last ten minutes: no growth.
    let (lo_a, hi_a) = o.depth_between(300.0, 900.0);
    let (lo_b, hi_b) = o.depth_between(3000.0, 3600.0);
    println!("depth early {lo_a:.1}..{hi_a:.1} ms, late {lo_b:.1}..{hi_b:.1} ms");
    assert!(hi_b < hi_a + 5.0 && lo_b > lo_a - 5.0);
    assert!(hi_b < 60.0);
    assert_eq!(o.playout.catastrophic_trims, 0);
    assert!((o.playout.drift_ppm - 120.0).abs() < 20.0);
}

/// Measurement, not a pass/fail check: underruns and depth for fixed
/// targets across network jitter levels.
/// `cargo test --release -p audionet-engine --test sim jitter_sweep -- --ignored --nocapture`
#[test]
#[ignore]
fn jitter_sweep_fixed_targets() {
    for jitter in [2.0, 5.0, 10.0, 20.0, 30.0] {
        for target in [25.0, 40.0, 60.0, 80.0] {
            let o = simulate(&Scenario {
                seconds: 300,
                jitter_ms: jitter,
                base_delay_ms: 5.0 + jitter,
                loss: 0.005,
                target_ms: target,
                ..Default::default()
            });
            let (lo, hi) = o.depth_between(30.0, 300.0);
            println!(
                "jitter ±{jitter:>4} ms  target {target:>3} ms: underruns {:>4}, late {:>4}, depth {lo:5.1}..{hi:5.1} ms",
                o.underruns_between(30.0, 300.0),
                o.receiver.late_packets
            );
        }
    }
}

// ── Adaptive target (AGENTS.md §25) ─────────────────────────────────────────

fn report_target(name: &str, o: &Outcome) {
    report(name, o);
    let p = &o.playout;
    println!(
        "{name}: target now {:.0} ms after {} changes; last {:?}",
        p.target_ms, p.target_changes, p.last_target_change
    );
}

#[test]
fn adaptive_target_lowers_on_a_clean_network() {
    let o = simulate(&Scenario {
        seconds: 900,
        jitter_ms: 2.0,
        loss: 0.005,
        adaptive: Some(AdaptiveConfig::NATIVE),
        ..Default::default()
    });
    report_target("adaptive clean", &o);
    // Starts at 40 ms and settles lower, without buying it with dropouts.
    assert!(
        o.playout.target_ms <= 30.0,
        "target {}",
        o.playout.target_ms
    );
    assert!(o.playout.target_ms >= 25.0);
    assert_eq!(o.underruns_between(5.0, 900.0), 0);
    assert!(
        o.receiver.late_packets <= 2,
        "late {}",
        o.receiver.late_packets
    );
}

#[test]
fn adaptive_target_rises_under_heavy_jitter() {
    let o = simulate(&Scenario {
        seconds: 300,
        jitter_ms: 20.0,
        base_delay_ms: 25.0,
        loss: 0.005,
        adaptive: Some(AdaptiveConfig::NATIVE),
        ..Default::default()
    });
    report_target("adaptive jitter 20", &o);
    // 40 ms is too small for ±20 ms (557 late packets in the fixed sweep):
    // the target rises within the first minute, then late packets stop.
    assert!(
        o.target_at(60.0) >= 50.0,
        "target at 60 s {}",
        o.target_at(60.0)
    );
    let late_early = o.receiver.late_packets;
    assert!(late_early < 150, "late {late_early}");
    assert_eq!(o.underruns_between(60.0, 300.0), 0);
    // No oscillation: a handful of changes, not a sawtooth.
    assert!(
        o.playout.target_changes <= 4,
        "changes {}",
        o.playout.target_changes
    );
}

#[test]
fn adaptive_target_follows_a_network_that_gets_worse() {
    let o = simulate(&Scenario {
        seconds: 900,
        jitter_ms: 2.0,
        base_delay_ms: 30.0,
        jitter_change: Some((600, 20.0)),
        loss: 0.005,
        adaptive: Some(AdaptiveConfig::NATIVE),
        ..Default::default()
    });
    report_target("adaptive worsening", &o);
    let before = o.target_at(599.0);
    assert!(before <= 30.0, "target before {before}");
    // It rises, and once it has, the worse network is absorbed.
    assert!(
        o.target_at(700.0) > before,
        "target after {}",
        o.target_at(700.0)
    );
    assert!(
        o.late_between(700.0, 900.0) <= 5,
        "late {}",
        o.late_between(700.0, 900.0)
    );
    assert_eq!(o.underruns_between(700.0, 900.0), 0);
}

#[test]
fn adaptive_target_ignores_outages() {
    // Two 2-second sender outages are not jitter: the target must not move up.
    let o = simulate(&Scenario {
        seconds: 120,
        jitter_ms: 2.0,
        sender_stalls: vec![(30, 2000), (60, 2000)],
        adaptive: Some(AdaptiveConfig::NATIVE),
        ..Default::default()
    });
    report_target("adaptive outages", &o);
    assert!(
        o.playout.target_ms <= 40.0,
        "target {}",
        o.playout.target_ms
    );
}
