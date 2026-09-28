//! `audionet capture-test`: opens a capture stream, consumes it on this
//! (non-real-time) thread, prints diagnostics, and optionally writes a WAV.
//!
//! Everything here is control-path code. The capture callback runs on the
//! backend's own thread; this thread only reads the ring, so WAV file I/O,
//! printing and sleeping here can never block audio capture. If this thread
//! stalls, the ring absorbs it, and beyond the ring's capacity the capture
//! thread drops audio and counts it.

use std::io::BufWriter;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::time::{Duration, Instant};

use audionet_audio::capture::{CaptureMode, CaptureStreamInfo, StreamError};
use audionet_audio::levels::LevelMeter;
use audionet_audio::ring::RingConsumer;
use audionet_audio::{
    EndpointEnumerator, EndpointResolution, EnumerationOptions, SavedEndpointRef,
    resolve_saved_endpoint,
};
use audionet_cli::capture_report::{
    CAPTURE_REPORT_SCHEMA, CAPTURE_REPORT_SCHEMA_VERSION, CaptureReportDocument, ConsumerStats,
    render_report, render_stream_header,
};
use audionet_cli::select::{EndpointSelector, select};
use audionet_protocol::{Direction, EndpointState};
use audionet_wasapi::WasapiEnumerator;
use audionet_wasapi::capture::{CaptureConfig, CaptureStream};

/// How often the consumer polls the ring. Not an audio deadline: the ring
/// holds far more than this.
const POLL_INTERVAL: Duration = Duration::from_millis(10);
/// Consumer trims the oldest audio when the ring holds more than this...
const STALE_THRESHOLD_MS: u64 = 250;
/// ...down to this much.
const STALE_KEEP_MS: u64 = 50;
const RECONNECT_POLL: Duration = Duration::from_secs(1);

#[derive(Debug)]
pub struct CaptureTestArgs {
    pub mode: CaptureMode,
    pub selector: EndpointSelector,
    /// 0 means until Ctrl+C.
    pub seconds: u64,
    pub report_every: u64,
    pub wav: Option<PathBuf>,
    pub ring_ms: u32,
    pub mmcss: bool,
    pub reconnect: bool,
    pub simulate_consumer_stall_ms: Option<u64>,
    pub json: bool,
}

enum EndReason {
    Duration,
    Interrupted,
    Failed(StreamError),
}

impl EndReason {
    fn tag(&self) -> &'static str {
        match self {
            EndReason::Duration => "duration",
            EndReason::Interrupted => "interrupted",
            EndReason::Failed(_) => "error",
        }
    }
}

/// Prints progress text to stdout, or to stderr in JSON mode so stdout
/// carries only the JSON document.
struct Out {
    json: bool,
}

impl Out {
    fn say(&self, text: &str) {
        let text = text.strip_suffix('\n').unwrap_or(text);
        if self.json {
            eprintln!("{text}");
        } else {
            println!("{text}");
        }
    }
}

pub fn run(args: CaptureTestArgs) -> Result<(), String> {
    let out = Out { json: args.json };
    let interrupted = Arc::new(AtomicBool::new(false));
    {
        let flag = Arc::clone(&interrupted);
        if let Err(e) = ctrlc::set_handler(move || flag.store(true, Relaxed)) {
            out.say(&format!(
                "Warning: Ctrl+C handling is unavailable ({e}); Ctrl+C will end the \
                 program without a summary or a complete WAV file."
            ));
        }
    }

    let direction = match args.mode {
        CaptureMode::Input => Direction::Input,
        CaptureMode::Loopback => Direction::Output,
    };
    let inventory = WasapiEnumerator
        .enumerate(EnumerationOptions::default())
        .map_err(|e| e.to_string())?;
    let selected = select(&inventory, direction, &args.selector).map_err(|e| e.to_string())?;
    let device_label = format!(
        "{} device {} of {}",
        direction.label().to_lowercase(),
        selected.position,
        selected.total
    );
    let device_name = selected.endpoint.name.clone();
    let saved = SavedEndpointRef {
        id: selected.endpoint.id.clone(),
        direction,
        name: device_name.clone(),
    };
    let config = CaptureConfig {
        endpoint: saved.id.clone(),
        mode: args.mode,
        ring_capacity_ms: args.ring_ms,
        mmcss: args.mmcss,
    };

    let started = Instant::now();
    let deadline = (args.seconds > 0).then(|| started + Duration::from_secs(args.seconds));
    let mut wav: Option<WavSink> = None;
    let mut stream_number = 0u32;

    let result = loop {
        stream_number += 1;
        let (stream, consumer) = match CaptureStream::start(config.clone()) {
            Ok(opened) => opened,
            Err(e) if args.reconnect && e.kind.is_recoverable() && stream_number > 1 => {
                out.say(&format!("Could not reopen the device yet: {e}"));
                if wait_for_device(&out, &saved, deadline, &interrupted) {
                    continue;
                }
                break Ok(());
            }
            Err(e) => break Err(e.to_string()),
        };
        let info = stream.info().clone();
        if stream_number > 1 {
            out.say("\nCapture restarted as a new stream. Diagnostics start again from zero.");
        }
        out.say(&render_stream_header(&info, &device_label, &device_name));
        out.say(&match args.seconds {
            0 => format!(
                "Capturing until Ctrl+C. Reports every {} seconds.\n",
                args.report_every
            ),
            s => format!(
                "Capturing for {s} seconds. Reports every {} seconds. Press Ctrl+C to stop early.\n",
                args.report_every
            ),
        });

        if let Some(path) = &args.wav {
            match &mut wav {
                None => match WavSink::create(path, &info) {
                    Ok(sink) => wav = Some(sink),
                    Err(e) => out.say(&format!(
                        "Warning: could not create WAV file {}: {e}. Continuing without it.",
                        path.display()
                    )),
                },
                Some(sink) if !sink.matches(&info) => {
                    out.say(
                        "Warning: the device format changed; the WAV file keeps only audio \
                         from before the change.",
                    );
                    sink.enabled = false;
                }
                Some(_) => {}
            }
        }

        let end = consume(
            stream,
            consumer,
            &info,
            &args,
            deadline,
            &interrupted,
            &mut wav,
            &out,
        );
        match end {
            EndReason::Failed(e) if args.reconnect && e.kind.is_recoverable() => {
                out.say(&format!("\nCapture stopped: {e}"));
                if !wait_for_device(&out, &saved, deadline, &interrupted) {
                    break Ok(());
                }
            }
            EndReason::Failed(e) => break Err(format!("Capture stopped: {e}")),
            EndReason::Duration | EndReason::Interrupted => break Ok(()),
        }
    };

    if let Some(sink) = wav {
        match sink.finish() {
            Ok(summary) => out.say(&summary),
            Err(e) => out.say(&format!("Warning: could not finish the WAV file: {e}")),
        }
    }
    result
}

#[allow(clippy::too_many_arguments)]
fn consume(
    mut stream: CaptureStream,
    mut consumer: RingConsumer,
    info: &CaptureStreamInfo,
    args: &CaptureTestArgs,
    deadline: Option<Instant>,
    interrupted: &AtomicBool,
    wav: &mut Option<WavSink>,
    out: &Out,
) -> EndReason {
    let rate = u64::from(info.format.sample_rate_hz);
    let channels = consumer.channels();
    let capacity = info.ring_capacity_frames;
    let threshold = (rate * STALE_THRESHOLD_MS / 1000).min(capacity * 3 / 4) as usize;
    let keep = (rate * STALE_KEEP_MS / 1000).min(threshold as u64 / 2) as usize;
    let mut buf = vec![0.0f32; capacity as usize * channels];

    let stream_start = Instant::now();
    let report_every = Duration::from_secs(args.report_every.max(1));
    let mut next_report = stream_start + report_every;
    let stall_at = args.simulate_consumer_stall_ms.map(|ms| {
        let offset = match args.seconds {
            0 => Duration::from_secs(2),
            s => Duration::from_millis(s * 500),
        };
        (stream_start + offset, Duration::from_millis(ms))
    });
    let mut stall_done = false;

    let mut window_levels = LevelMeter::default();
    let mut total_levels = LevelMeter::default();
    let mut stats = ConsumerStats::default();

    let reason = loop {
        if interrupted.load(Relaxed) {
            break EndReason::Interrupted;
        }
        if deadline.is_some_and(|d| Instant::now() >= d) {
            break EndReason::Duration;
        }
        if let Some(finished) = stream.poll_finished() {
            // Keep whatever audio arrived before the failure.
            drain(
                &mut consumer,
                &mut buf,
                &mut stats,
                &mut window_levels,
                &mut total_levels,
                wav,
            );
            break match finished {
                Err(e) => EndReason::Failed(e),
                Ok(()) => EndReason::Duration,
            };
        }
        if let Some((at, len)) = stall_at {
            if !stall_done && Instant::now() >= at {
                out.say(&format!(
                    "Simulating a consumer stall of {} ms. The capture thread keeps running.",
                    len.as_millis()
                ));
                std::thread::sleep(len);
                stall_done = true;
            }
        }

        consumer.trim_stale(threshold, keep);
        drain(
            &mut consumer,
            &mut buf,
            &mut stats,
            &mut window_levels,
            &mut total_levels,
            wav,
        );
        if consumer.take_discontinuity() {
            stats.discontinuities_seen += 1;
        }

        let now = Instant::now();
        if now >= next_report {
            let secs = (now - stream_start).as_secs_f64();
            let snap = stream.diagnostics().snapshot();
            let report_stats = ConsumerStats {
                levels: window_levels.take(),
                ..stats
            };
            let title = format!(
                "
Capture report at {secs:.1} seconds"
            );
            out.say(&render_report(&title, info, &snap, &report_stats));
            next_report += report_every;
        }
        std::thread::sleep(POLL_INTERVAL);
    };

    let elapsed = stream_start.elapsed();
    let error_text = match &reason {
        EndReason::Failed(e) => Some(e.to_string()),
        _ => None,
    };
    // Stop the thread (no-op if it already ended), then read what was still
    // in the ring, so the final snapshot, the consumer counts and the WAV
    // all cover exactly the same audio.
    let diagnostics = Arc::clone(stream.diagnostics());
    let _ = stream.stop();
    drain(
        &mut consumer,
        &mut buf,
        &mut stats,
        &mut window_levels,
        &mut total_levels,
        wav,
    );
    let snap = diagnostics.snapshot();
    stats.levels = total_levels.take();

    if args.json {
        let doc = CaptureReportDocument {
            schema: CAPTURE_REPORT_SCHEMA,
            schema_version: CAPTURE_REPORT_SCHEMA_VERSION,
            stream: info,
            elapsed_ms: elapsed.as_millis() as u64,
            diagnostics: &snap,
            consumer: &stats,
            end_reason: reason.tag(),
            error: error_text,
        };
        let json = serde_json::to_string_pretty(&doc).expect("report serializes");
        println!("{json}");
    } else {
        let why = match &reason {
            EndReason::Duration => "the requested duration was reached",
            EndReason::Interrupted => "stopped by Ctrl+C",
            EndReason::Failed(_) => "stopped by an error",
        };
        let title = format!(
            "
Capture summary after {:.1} seconds ({why})",
            elapsed.as_secs_f64()
        );
        let mut text = render_report(&title, info, &snap, &stats);
        text.push_str(&format!(
            "Audio captured compared with elapsed time: {:.3} seconds of audio in {:.3} seconds\n",
            snap.frames_captured as f64 / rate as f64,
            elapsed.as_secs_f64()
        ));
        out.say(&text);
    }
    reason
}

fn drain(
    consumer: &mut RingConsumer,
    buf: &mut [f32],
    stats: &mut ConsumerStats,
    window: &mut LevelMeter,
    total: &mut LevelMeter,
    wav: &mut Option<WavSink>,
) {
    let channels = consumer.channels();
    loop {
        let frames = consumer.read(buf);
        if frames == 0 {
            return;
        }
        let samples = &buf[..frames * channels];
        stats.frames_read += frames as u64;
        window.add(samples);
        total.add(samples);
        if let Some(sink) = wav {
            sink.write(samples);
        }
    }
}

/// Waits for a disconnected endpoint to come back. Returns `true` when it is
/// active again, `false` if time ran out or the user interrupted.
fn wait_for_device(
    out: &Out,
    saved: &SavedEndpointRef,
    deadline: Option<Instant>,
    interrupted: &AtomicBool,
) -> bool {
    out.say(&format!(
        "Waiting for {} to become available again. It will not be replaced by a \
         different device automatically.",
        saved.name
    ));
    let mut last_note = String::new();
    loop {
        if interrupted.load(Relaxed) || deadline.is_some_and(|d| Instant::now() >= d) {
            out.say("Stopped waiting for the device.");
            return false;
        }
        let note = match WasapiEnumerator.enumerate(EnumerationOptions::default()) {
            Err(e) => format!("Could not list devices while waiting: {e}"),
            Ok(inv) => match resolve_saved_endpoint(saved, inv.endpoints()) {
                EndpointResolution::Available(_) => {
                    out.say("The device is available again. Reopening it.");
                    return true;
                }
                EndpointResolution::Unavailable { state, .. } => format!(
                    "The device is present but {}.",
                    match state {
                        EndpointState::Disabled => "disabled",
                        EndpointState::Unplugged => "unplugged",
                        EndpointState::NotPresent => "not present",
                        EndpointState::Active => "active",
                    }
                ),
                EndpointResolution::MissingWithCandidate(c) => format!(
                    "The device is missing. A device with the same name exists with a \
                     different identifier ({}); it is not used automatically.",
                    c.id
                ),
                EndpointResolution::Missing => "The device is missing.".to_owned(),
            },
        };
        if note != last_note {
            out.say(&note);
            last_note = note;
        }
        std::thread::sleep(RECONNECT_POLL);
    }
}

/// Writes captured audio as 32-bit float WAV on the consumer thread.
struct WavSink {
    path: PathBuf,
    writer: Option<hound::WavWriter<BufWriter<std::fs::File>>>,
    channels: u16,
    sample_rate: u32,
    samples_written: u64,
    enabled: bool,
    error: Option<String>,
}

impl WavSink {
    fn create(path: &PathBuf, info: &CaptureStreamInfo) -> Result<Self, hound::Error> {
        let spec = hound::WavSpec {
            channels: info.format.channels,
            sample_rate: info.format.sample_rate_hz,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        Ok(Self {
            path: path.clone(),
            writer: Some(hound::WavWriter::create(path, spec)?),
            channels: info.format.channels,
            sample_rate: info.format.sample_rate_hz,
            samples_written: 0,
            enabled: true,
            error: None,
        })
    }

    fn matches(&self, info: &CaptureStreamInfo) -> bool {
        self.channels == info.format.channels && self.sample_rate == info.format.sample_rate_hz
    }

    fn write(&mut self, samples: &[f32]) {
        if !self.enabled {
            return;
        }
        let Some(writer) = &mut self.writer else {
            return;
        };
        for &s in samples {
            if let Err(e) = writer.write_sample(s) {
                self.error = Some(e.to_string());
                self.enabled = false;
                return;
            }
        }
        self.samples_written += samples.len() as u64;
    }

    fn finish(mut self) -> Result<String, hound::Error> {
        if let Some(writer) = self.writer.take() {
            writer.finalize()?;
        }
        let frames = self.samples_written / u64::from(self.channels);
        let mut text = format!(
            "WAV file written: {}, {} frames ({:.2} seconds), 32-bit float, {} channels, {} Hz.",
            self.path.display(),
            frames,
            frames as f64 / f64::from(self.sample_rate),
            self.channels,
            self.sample_rate
        );
        if let Some(e) = &self.error {
            text.push_str(&format!(" Writing stopped early because of an error: {e}."));
        }
        Ok(text)
    }
}

impl std::fmt::Debug for WavSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WavSink").field("path", &self.path).finish()
    }
}
