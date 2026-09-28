//! Text and JSON output for `audionet capture-test`.
//!
//! Same conventions as `audionet list`: one "Label: value" fact per line,
//! the same lines in every report, units spelled out, no tables or color.

use core::fmt::Write as _;

use audionet_audio::capture::{CaptureMode, CaptureSnapshot, CaptureStreamInfo, MmcssStatus};
use audionet_audio::levels::LevelReading;
use serde::Serialize;

pub const CAPTURE_REPORT_SCHEMA: &str = "audionet.capture_report";
pub const CAPTURE_REPORT_SCHEMA_VERSION: u32 = 1;

fn ms(ns: u64) -> String {
    format!("{:.2} ms", ns as f64 / 1e6)
}

fn ms_precise(ns: u64) -> String {
    format!("{:.3} ms", ns as f64 / 1e6)
}

fn opt(v: Option<u64>, f: fn(u64) -> String) -> String {
    v.map_or_else(|| "not measured yet".to_owned(), f)
}

fn frames_ms(frames: u64, rate: u32) -> String {
    format!("{:.1} ms", frames as f64 * 1000.0 / f64::from(rate))
}

/// The header printed once when a stream opens.
pub fn render_stream_header(
    info: &CaptureStreamInfo,
    device_label: &str,
    device_name: &str,
) -> String {
    let f = &info.format;
    let mut out = String::new();
    let what = match info.mode {
        CaptureMode::Input => "Capturing from",
        CaptureMode::Loopback => "Capturing loopback from",
    };
    let _ = writeln!(out, "{what} {device_label}: {device_name}");
    let _ = writeln!(out, "Identifier: {}", info.endpoint);
    let _ = writeln!(
        out,
        "Device format: {} Hz, {} channels, {}",
        f.sample_rate_hz,
        f.channels,
        f.sample_format.describe()
    );
    let _ = writeln!(out, "Device period: {}", ms(info.device_period_ns));
    let _ = writeln!(
        out,
        "Windows capture buffer: {} ({} frames)",
        frames_ms(u64::from(info.os_buffer_frames), f.sample_rate_hz),
        info.os_buffer_frames
    );
    let _ = writeln!(
        out,
        "Ring capacity: {} ({} frames)",
        frames_ms(info.ring_capacity_frames, f.sample_rate_hz),
        info.ring_capacity_frames
    );
    let mmcss = match info.mmcss {
        MmcssStatus::NotRequested => "Not requested",
        MmcssStatus::Registered => "Registered as Pro Audio",
        MmcssStatus::Failed => "Requested but registration failed",
    };
    let _ = writeln!(out, "MMCSS: {mmcss}");
    out
}

/// What the non-real-time consumer observed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct ConsumerStats {
    pub frames_read: u64,
    /// Times the consumer saw a gap flagged by the ring (overflow or trim).
    pub discontinuities_seen: u64,
    pub levels: LevelReading,
}

/// One periodic report or the final summary.
pub fn render_report(
    title: &str,
    info: &CaptureStreamInfo,
    snap: &CaptureSnapshot,
    consumer: &ConsumerStats,
) -> String {
    let rate = info.format.sample_rate_hz;
    let t = &snap.timing;
    let r = &snap.ring;
    let mut out = String::new();
    let _ = writeln!(out, "{title}:");
    let _ = writeln!(
        out,
        "Audio captured: {:.2} seconds ({} frames in {} packets)",
        snap.frames_captured as f64 / f64::from(rate),
        snap.frames_captured,
        snap.packets
    );
    let _ = writeln!(out, "Callbacks with audio: {}", t.callbacks);
    let _ = writeln!(
        out,
        "Callback interval: average {}, minimum {}, maximum {}",
        opt(t.interval_avg_ns, ms),
        opt(t.interval_min_ns, ms),
        opt(t.interval_max_ns, ms)
    );
    let _ = writeln!(
        out,
        "Largest callback gap since last report: {}",
        opt(t.window_interval_max_ns, ms)
    );
    let _ = writeln!(
        out,
        "Callbacks later than twice the device period: {}",
        t.late_callbacks
    );
    let _ = writeln!(
        out,
        "Callback work time: average {}, maximum {}, maximum since last report {}",
        opt(t.work_avg_ns, ms_precise),
        opt(t.work_max_ns, ms_precise),
        opt(t.window_work_max_ns, ms_precise)
    );
    let _ = writeln!(out, "Wakeups with no audio: {}", t.idle_wakeups);
    if info.mode == CaptureMode::Loopback && t.idle_wakeups > 0 && t.callbacks == 0 {
        out.push_str(
            "Note: nothing is playing on the device yet, so Windows delivers no audio; AudioNet fills the time with silence.\n",
        );
    }
    let _ = writeln!(
        out,
        "Windows data discontinuities: {} during the stream, {} on the first packet",
        snap.device_discontinuities, snap.startup_discontinuities
    );
    let _ = writeln!(out, "Silent packets from Windows: {}", snap.silent_packets);
    if info.mode == CaptureMode::Loopback {
        let _ = writeln!(
            out,
            "Silence filled in while nothing was playing: {}",
            frames_ms(snap.idle_fill_frames, rate)
        );
    }
    let _ = writeln!(
        out,
        "Timestamp errors from Windows: {}",
        snap.timestamp_errors
    );
    let _ = writeln!(
        out,
        "Ring depth: {} now, maximum {}, maximum since last report {}",
        frames_ms(r.depth_frames, rate),
        frames_ms(r.max_depth_frames, rate),
        frames_ms(r.window_max_depth_frames, rate)
    );
    let _ = writeln!(
        out,
        "Ring overflow drops: {} events, {} frames",
        r.overflow_events, r.overflow_frames
    );
    let _ = writeln!(
        out,
        "Stale-audio trims: {} events, {} frames",
        r.stale_trim_events, r.stale_trim_frames
    );
    let _ = writeln!(
        out,
        "Frames read by consumer: {}; gaps seen by consumer: {}",
        consumer.frames_read, consumer.discontinuities_seen
    );
    let l = &consumer.levels;
    match (l.peak_dbfs, l.rms_dbfs) {
        (Some(peak), Some(rms)) => {
            let _ = writeln!(out, "Signal level: peak {peak:.1} dBFS, RMS {rms:.1} dBFS");
        }
        _ if l.samples == 0 => out.push_str("Signal level: no samples read\n"),
        _ => out.push_str("Signal level: digital silence\n"),
    }
    let _ = writeln!(
        out,
        "Invalid samples: {} not finite, {} above full scale",
        l.non_finite_samples, l.clipped_samples
    );
    out
}

/// Final JSON document for `capture-test --json`.
#[derive(Debug, Serialize)]
pub struct CaptureReportDocument<'a> {
    pub schema: &'static str,
    pub schema_version: u32,
    pub stream: &'a CaptureStreamInfo,
    pub elapsed_ms: u64,
    pub diagnostics: &'a CaptureSnapshot,
    pub consumer: &'a ConsumerStats,
    /// Why capture ended: "duration", "interrupted", or "error".
    pub end_reason: &'static str,
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use audionet_audio::ring::RingSnapshot;
    use audionet_audio::timing::TimingSnapshot;
    use audionet_protocol::{AudioBackend, DeviceFormat, EndpointId, SampleFormat};

    fn info() -> CaptureStreamInfo {
        CaptureStreamInfo {
            endpoint: EndpointId::new(AudioBackend::Wasapi, "{0.0.1.00000000}.{mic}").unwrap(),
            mode: CaptureMode::Input,
            format: DeviceFormat {
                sample_rate_hz: 48_000,
                channels: 2,
                sample_format: SampleFormat::F32,
                channel_mask: Some(3),
            },
            device_period_ns: 10_000_000,
            os_buffer_frames: 4800,
            ring_capacity_frames: 24_000,
            mmcss: MmcssStatus::NotRequested,
        }
    }

    #[test]
    fn header() {
        let text = render_stream_header(&info(), "input device 2 of 5", "Microphone (USB)");
        assert_eq!(
            text,
            "\
Capturing from input device 2 of 5: Microphone (USB)
Identifier: {0.0.1.00000000}.{mic}
Device format: 48000 Hz, 2 channels, 32-bit float
Device period: 10.00 ms
Windows capture buffer: 100.0 ms (4800 frames)
Ring capacity: 500.0 ms (24000 frames)
MMCSS: Not requested
"
        );
    }

    #[test]
    fn report_with_measurements() {
        let snap = CaptureSnapshot {
            packets: 500,
            frames_captured: 240_000,
            device_discontinuities: 1,
            startup_discontinuities: 0,
            silent_packets: 0,
            idle_fill_frames: 0,
            timestamp_errors: 0,
            timing: TimingSnapshot {
                nominal_period_ns: 10_000_000,
                callbacks: 500,
                idle_wakeups: 2,
                interval_avg_ns: Some(10_000_000),
                interval_min_ns: Some(9_500_000),
                interval_max_ns: Some(31_250_000),
                window_interval_max_ns: Some(10_600_000),
                late_callbacks: 1,
                work_avg_ns: Some(20_000),
                work_max_ns: Some(110_000),
                window_work_max_ns: Some(50_000),
            },
            ring: RingSnapshot {
                capacity_frames: 24_000,
                depth_frames: 480,
                max_depth_frames: 960,
                window_max_depth_frames: 480,
                frames_written: 240_000,
                frames_read: 239_520,
                overflow_events: 0,
                overflow_frames: 0,
                stale_trim_events: 0,
                stale_trim_frames: 0,
            },
        };
        let consumer = ConsumerStats {
            frames_read: 239_520,
            discontinuities_seen: 0,
            levels: LevelReading {
                samples: 100,
                peak_dbfs: Some(-6.02),
                rms_dbfs: Some(-22.44),
                non_finite_samples: 0,
                clipped_samples: 0,
            },
        };
        assert_eq!(
            render_report("Capture report at 5 seconds", &info(), &snap, &consumer),
            "\
Capture report at 5 seconds:
Audio captured: 5.00 seconds (240000 frames in 500 packets)
Callbacks with audio: 500
Callback interval: average 10.00 ms, minimum 9.50 ms, maximum 31.25 ms
Largest callback gap since last report: 10.60 ms
Callbacks later than twice the device period: 1
Callback work time: average 0.020 ms, maximum 0.110 ms, maximum since last report 0.050 ms
Wakeups with no audio: 2
Windows data discontinuities: 1 during the stream, 0 on the first packet
Silent packets from Windows: 0
Timestamp errors from Windows: 0
Ring depth: 10.0 ms now, maximum 20.0 ms, maximum since last report 10.0 ms
Ring overflow drops: 0 events, 0 frames
Stale-audio trims: 0 events, 0 frames
Frames read by consumer: 239520; gaps seen by consumer: 0
Signal level: peak -6.0 dBFS, RMS -22.4 dBFS
Invalid samples: 0 not finite, 0 above full scale
"
        );
    }

    #[test]
    fn report_before_any_audio_says_so_in_words() {
        let text = render_report(
            "Capture report at 1 second",
            &info(),
            &CaptureSnapshot::default(),
            &ConsumerStats::default(),
        );
        assert!(text.contains(
            "Callback interval: average not measured yet, minimum not measured yet, \
             maximum not measured yet"
        ));
        assert!(text.contains("Signal level: no samples read"));
    }
}
