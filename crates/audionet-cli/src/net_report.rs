//! Plain-text diagnostic reports for `audionet send` and `audionet receive`
//! (the AGENTS.md §22 snapshot, as linear "Label: value" lines).

use core::fmt::Write as _;

use audionet_audio::capture::CaptureSnapshot;
use audionet_audio::timing::TimingSnapshot;
use audionet_engine::adaptive::ChangeReason;
use audionet_engine::playout::PlayoutSnapshot;
use audionet_engine::receiver::ReceiverSnapshot;
use audionet_engine::sender::SenderSnapshot;
use audionet_engine::stats::DurationSnapshot;
use serde::Serialize;

fn ms(ns: Option<u64>) -> String {
    ns.map_or_else(
        || "not measured yet".into(),
        |v| format!("{:.2} ms", v as f64 / 1e6),
    )
}

fn ms3(ns: Option<u64>) -> String {
    ns.map_or_else(
        || "not measured yet".into(),
        |v| format!("{:.3} ms", v as f64 / 1e6),
    )
}

fn durations(label: &str, d: &DurationSnapshot, out: &mut String) {
    let _ = writeln!(
        out,
        "{label}: average {}, maximum {}, maximum since last report {}",
        ms3(d.avg_ns),
        ms3(d.max_ns),
        ms3(d.window_max_ns)
    );
}

fn callbacks(label: &str, t: &TimingSnapshot, out: &mut String) {
    let _ = writeln!(
        out,
        "{label} callback interval: average {}, maximum {}, maximum since last report {}; \
         later than twice the period: {}",
        ms(t.interval_avg_ns),
        ms(t.interval_max_ns),
        ms(t.window_interval_max_ns),
        t.late_callbacks
    );
    let _ = writeln!(
        out,
        "{label} callback work time: average {}, maximum {}",
        ms3(t.work_avg_ns),
        ms3(t.work_max_ns)
    );
}

fn plural(n: u64, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// Everything a sender report shows.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct SenderReport<'a> {
    pub elapsed_s: f64,
    pub capture: &'a CaptureSnapshot,
    pub sender: &'a SenderSnapshot,
    pub capture_rate: u32,
}

pub fn render_sender_report(title: &str, r: &SenderReport<'_>) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "\n{title}:");
    let c = r.capture;
    let _ = writeln!(
        out,
        "Audio captured: {:.2} seconds",
        c.frames_captured as f64 / f64::from(r.capture_rate.max(1))
    );
    callbacks("Capture", &c.timing, &mut out);
    if c.idle_fill_frames > 0 {
        let _ = writeln!(
            out,
            "Silence filled in while nothing was playing: {:.2} seconds",
            c.idle_fill_frames as f64 / f64::from(r.capture_rate.max(1))
        );
    }
    let _ = writeln!(
        out,
        "Capture buffer drops: {} overflow, {} stale-audio trims; Windows discontinuities {}",
        plural(c.ring.overflow_events, "event", "events"),
        plural(c.ring.stale_trim_events, "event", "events"),
        c.device_discontinuities
    );
    let s = r.sender;
    let pps = if r.elapsed_s > 0.0 {
        s.packets as f64 / r.elapsed_s
    } else {
        0.0
    };
    let _ = writeln!(
        out,
        "Encoded frames: {}; packets sent: {} ({:.1} per second, {} kilobytes)",
        s.frames_encoded,
        s.packets,
        pps,
        s.bytes / 1000
    );
    durations("Encode time", &s.encode_ns, &mut out);
    let _ = writeln!(
        out,
        "Send errors: {}; packets dropped because the socket buffer was full: {}; encode errors: {}",
        s.send_errors, s.send_would_block, s.encode_errors
    );
    out
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct ReceiverReport<'a> {
    pub elapsed_s: f64,
    pub receiver: &'a ReceiverSnapshot,
    pub playout: &'a PlayoutSnapshot,
    pub render: &'a TimingSnapshot,
}

pub fn render_receiver_report(title: &str, r: &ReceiverReport<'_>) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "\n{title}:");
    let p = r.playout;
    let n = r.receiver;
    let state = if p.playing {
        "playing"
    } else if n.datagrams == 0 {
        "waiting for audio"
    } else {
        "buffering"
    };
    let _ = writeln!(out, "Playback state: {state}");
    let seq = &n.sequence;
    let _ = writeln!(
        out,
        "Packets received: {} ({} kilobytes); lost: {} of {} ({:.2} percent)",
        n.datagrams,
        n.bytes / 1000,
        seq.lost(),
        seq.expected,
        n.loss_percent
    );
    let _ = writeln!(
        out,
        "Reordered packets: {}; duplicates: {}; late packets dropped: {}",
        seq.reordered, seq.duplicates, n.late_packets
    );
    let _ = writeln!(
        out,
        "Network jitter: {:.2} ms; largest gap between packets: {}, since last report {}",
        n.jitter_ms,
        ms(n.interarrival_ns.max_ns),
        ms(n.interarrival_ns.window_max_ns)
    );
    let _ = writeln!(
        out,
        "Rejected packets: {} failed authentication, {} malformed, {} from another sender",
        n.auth_failures, n.malformed, n.foreign_ssrc
    );
    let _ = writeln!(
        out,
        "Concealed frames: {} (with next packet available: {}); outage resynchronizations: {}; decode errors: {}",
        n.concealed_frames, n.fec_attempts, n.outage_resyncs, n.decode_errors
    );
    durations("Decode time", &n.decode_ns, &mut out);
    let d = &p.depth_us;
    let fmt_us = |v: Option<u64>| {
        v.map_or_else(
            || "not measured yet".into(),
            |v| format!("{:.1} ms", v as f64 / 1000.0),
        )
    };
    let _ = writeln!(
        out,
        "Playout buffer: {:.1} ms now, target {:.0} ms; range since last report {} to {}",
        d.current as f64 / 1000.0,
        p.target_ms,
        fmt_us(d.window_min),
        fmt_us(d.window_max)
    );
    if p.adaptive {
        let _ = writeln!(out, "{}", target_change_text(p));
    }
    let _ = writeln!(
        out,
        "Underruns: {}; re-bufferings: {}; latency drains: {} ({} frames); stale-audio safety trims: {} ({} frames); stream starts: {}",
        p.underruns,
        p.primings,
        p.latency_drains,
        p.latency_drain_frames,
        p.catastrophic_trims,
        p.catastrophic_trim_frames,
        n.stream_starts
    );
    let _ = writeln!(
        out,
        "Clock drift estimate: {:+.1} ppm ({}); depth correction: {:+.1} ppm; depth error: {:+.2} ms; resampling ratio: {:.7}",
        p.drift_ppm,
        if p.drift_locked {
            "learning"
        } else {
            "paused during recovery"
        },
        p.depth_bias_ppm,
        p.depth_error_ms,
        p.resample_ratio
    );
    callbacks("Playback", r.render, &mut out);
    out
}

/// Why the adaptive playout target is where it is, in one line.
fn target_change_text(p: &PlayoutSnapshot) -> String {
    let changes = plural(p.target_changes, "change", "changes");
    let Some(c) = p.last_target_change else {
        return format!("Adaptive target: {changes} so far");
    };
    let why = match c.reason {
        ChangeReason::LatePackets => format!(
            "packets arrived too late to play ({} incidents within 30 seconds)",
            c.evidence
        ),
        ChangeReason::Underruns => format!(
            "the buffer ran dry ({} incidents within 30 seconds)",
            c.evidence
        ),
        ChangeReason::Stable => format!(
            "{:.0} seconds with no underruns or late packets",
            c.evidence
        ),
    };
    let verb = if c.to_ms > c.from_ms {
        "raised"
    } else {
        "lowered"
    };
    format!(
        "Adaptive target: {changes}; last {verb} from {:.0} ms to {:.0} ms because {why}",
        c.from_ms, c.to_ms
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receiver_report_is_linear_text() {
        let text = render_receiver_report(
            "Receiver report at 5.0 seconds",
            &ReceiverReport {
                elapsed_s: 5.0,
                receiver: &ReceiverSnapshot::default(),
                playout: &PlayoutSnapshot {
                    target_ms: 40.0,
                    ..Default::default()
                },
                render: &TimingSnapshot::default(),
            },
        );
        assert!(text.contains("Playback state: waiting for audio\n"));
        assert!(text.contains("Playout buffer: 0.0 ms now, target 40 ms; range since last report not measured yet to not measured yet\n"));
        assert!(!text.contains('\t'));
        assert!(text.lines().all(|l| l.is_empty() || l.contains(':')));
    }

    #[test]
    fn adaptive_target_change_is_explained() {
        use audionet_engine::adaptive::TargetChange;
        let mut p = PlayoutSnapshot {
            adaptive: true,
            ..Default::default()
        };
        assert_eq!(target_change_text(&p), "Adaptive target: 0 changes so far");
        p.target_changes = 3;
        p.last_target_change = Some(TargetChange {
            from_ms: 30.0,
            to_ms: 40.0,
            reason: ChangeReason::LatePackets,
            evidence: 2.0,
        });
        assert_eq!(
            target_change_text(&p),
            "Adaptive target: 3 changes; last raised from 30 ms to 40 ms because packets arrived too late to play (2 incidents within 30 seconds)"
        );
        p.last_target_change = Some(TargetChange {
            from_ms: 40.0,
            to_ms: 35.0,
            reason: ChangeReason::Stable,
            evidence: 120.0,
        });
        assert_eq!(
            target_change_text(&p),
            "Adaptive target: 3 changes; last lowered from 40 ms to 35 ms because 120 seconds with no underruns or late packets"
        );
    }

    #[test]
    fn sender_report_counts_rate() {
        let text = render_sender_report(
            "Sender report",
            &SenderReport {
                elapsed_s: 2.0,
                capture: &CaptureSnapshot::default(),
                sender: &SenderSnapshot {
                    packets: 200,
                    frames_encoded: 200,
                    ..Default::default()
                },
                capture_rate: 48_000,
            },
        );
        assert!(text.contains("packets sent: 200 (100.0 per second"));
    }
}
