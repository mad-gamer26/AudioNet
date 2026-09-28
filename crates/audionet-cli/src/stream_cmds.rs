//! `audionet keygen`, `audionet send` and `audionet receive`: the native
//! encrypted RTP-over-UDP path (pre-shared key, LAN milestone).
//!
//! All of this is control-path code on the main thread: device selection,
//! DNS resolution, key loading, periodic reports. Audio runs on the capture,
//! sender, receive and render threads started here.

use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use audionet_audio::capture::CaptureMode;
use audionet_audio::{EndpointEnumerator, EnumerationOptions};
use audionet_cli::net_report::{
    ReceiverReport, SenderReport, render_receiver_report, render_sender_report,
};
use audionet_cli::select::{EndpointSelector, Selected, select};
use audionet_codec::{EncoderConfig, EncoderMode};
use audionet_engine::adaptive::AdaptiveConfig;
use audionet_engine::playout::PlayoutConfig;
use audionet_engine::receiver::ReceiverConfig;
use audionet_engine::runtime::{ReceiverRuntime, SenderRuntime, bind_media_socket};
use audionet_protocol::Direction;
use audionet_transport::crypto::PresharedKey;
use audionet_wasapi::WasapiEnumerator;
use audionet_wasapi::capture::{CaptureConfig, CaptureStream};
use audionet_wasapi::render::{RenderConfig, RenderStream};
use serde::Serialize;

pub const DEFAULT_PORT: u16 = 5004;

pub fn keygen(output: &Path, force: bool) -> Result<(), String> {
    if output.exists() && !force {
        return Err(format!(
            "{} already exists. Use --force to replace it; every device using the old key will need the new one.",
            output.display()
        ));
    }
    let key = PresharedKey::generate();
    std::fs::write(output, format!("{}\n", key.to_hex()))
        .map_err(|e| format!("could not write {}: {e}", output.display()))?;
    println!(
        "New AudioNet key written to {}. Copy it to the other device by a secure means, \
         such as a USB drive or an encrypted channel. Anyone with this file can listen to \
         and inject audio into streams that use it.",
        output.display()
    );
    Ok(())
}

pub fn load_key(path: &Path) -> Result<PresharedKey, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("could not read the key file {}: {e}", path.display()))?;
    PresharedKey::from_hex(&text).map_err(|e| format!("{}: {e}", path.display()))
}

fn install_ctrl_c() -> Arc<AtomicBool> {
    let flag = Arc::new(AtomicBool::new(false));
    let f = Arc::clone(&flag);
    if ctrlc::set_handler(move || f.store(true, Relaxed)).is_err() {
        eprintln!("Warning: Ctrl+C handling is unavailable; Ctrl+C will end without a summary.");
    }
    flag
}

fn select_device(
    direction: Direction,
    selector: &EndpointSelector,
) -> Result<(String, String, audionet_protocol::EndpointId), String> {
    let inventory = WasapiEnumerator
        .enumerate(EnumerationOptions::default())
        .map_err(|e| e.to_string())?;
    let Selected {
        endpoint,
        position,
        total,
    } = select(&inventory, direction, selector).map_err(|e| e.to_string())?;
    Ok((
        format!(
            "{} device {position} of {total}",
            direction.label().to_lowercase()
        ),
        endpoint.name.clone(),
        endpoint.id.clone(),
    ))
}

fn resolve(target: &str) -> Result<SocketAddr, String> {
    let with_port = if target
        .rsplit_once(':')
        .is_some_and(|(_, p)| p.parse::<u16>().is_ok())
    {
        target.to_owned()
    } else {
        format!("{target}:{DEFAULT_PORT}")
    };
    with_port
        .to_socket_addrs()
        .map_err(|e| format!("could not resolve {target}: {e}"))?
        .next()
        .ok_or_else(|| format!("{target} did not resolve to any address"))
}

/// Runs `tick` every `report_every` until time runs out or Ctrl+C; `tick`
/// returns an error to stop early.
fn run_loop(
    seconds: u64,
    report_every: u64,
    interrupted: &AtomicBool,
    mut poll: impl FnMut() -> Result<(), String>,
    mut report: impl FnMut(f64),
) -> Result<&'static str, String> {
    let start = Instant::now();
    let deadline = (seconds > 0).then(|| start + Duration::from_secs(seconds));
    let every = Duration::from_secs(report_every.max(1));
    let mut next = start + every;
    loop {
        if interrupted.load(Relaxed) {
            return Ok("stopped by Ctrl+C");
        }
        if deadline.is_some_and(|d| Instant::now() >= d) {
            return Ok("the requested duration was reached");
        }
        poll()?;
        if Instant::now() >= next {
            report(start.elapsed().as_secs_f64());
            next += every;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[derive(Debug)]
pub struct SendArgs {
    pub mode: CaptureMode,
    pub selector: EndpointSelector,
    pub to: String,
    pub key_file: PathBuf,
    pub bitrate_kbps: u32,
    pub resilient: bool,
    pub seconds: u64,
    pub report_every: u64,
    pub mmcss: bool,
    pub json: bool,
}

#[derive(Serialize)]
struct SendJson<'a> {
    schema: &'static str,
    schema_version: u32,
    end_reason: &'a str,
    error: Option<String>,
    report: SenderReport<'a>,
}

pub fn send(args: SendArgs) -> Result<(), String> {
    let key = load_key(&args.key_file)?;
    let direction = match args.mode {
        CaptureMode::Input => Direction::Input,
        CaptureMode::Loopback => Direction::Output,
    };
    let (label, name, endpoint) = select_device(direction, &args.selector)?;
    let dest = resolve(&args.to)?;
    let interrupted = install_ctrl_c();

    let (mut capture, consumer) = CaptureStream::start(CaptureConfig {
        endpoint,
        mode: args.mode,
        ring_capacity_ms: 500,
        mmcss: args.mmcss,
    })
    .map_err(|e| e.to_string())?;
    let rate = capture.info().format.sample_rate_hz;
    let bind: SocketAddr = if dest.is_ipv6() {
        "[::]:0"
    } else {
        "0.0.0.0:0"
    }
    .parse()
    .expect("literal address");
    let socket = UdpSocket::bind(bind).map_err(|e| format!("could not open a UDP socket: {e}"))?;
    let encoder = EncoderConfig {
        mode: if args.resilient {
            EncoderMode::Resilient
        } else {
            EncoderMode::LowDelay
        },
        bitrate_bps: (args.bitrate_kbps.clamp(16, 510) * 1000) as i32,
        expected_loss_percent: if args.resilient { 5 } else { 0 },
        ..EncoderConfig::BASELINE
    };
    let setup = args
        .mmcss
        .then(audionet_wasapi::capture::audio_thread_setup);
    let sender = SenderRuntime::start(
        consumer,
        rate,
        socket,
        Arc::new(Mutex::new(dest)),
        &key,
        encoder,
        setup,
    )
    .map_err(|e| format!("could not start the sender: {e}"))?;

    let say = |text: &str| {
        if args.json {
            eprintln!("{text}");
        } else {
            println!("{text}");
        }
    };
    let what = if args.mode == CaptureMode::Loopback {
        "loopback of"
    } else {
        "from"
    };
    say(&format!(
        "Sending {what} {label}: {name}\nDestination: {dest}\nEncryption: on (pre-shared key)\n\
         Codec: Opus {} kbit/s, 10 ms frames, {} mode\nDevice format: {} Hz, {} channels",
        args.bitrate_kbps,
        if args.resilient {
            "resilient (in-band FEC)"
        } else {
            "low-delay"
        },
        rate,
        capture.info().format.channels
    ));

    let diag = Arc::clone(capture.diagnostics());
    let stats = Arc::clone(sender.stats());
    let mut failure = None;
    let end = run_loop(
        args.seconds,
        args.report_every,
        &interrupted,
        || match capture.poll_finished() {
            Some(Err(e)) => Err(e.to_string()),
            _ => Ok(()),
        },
        |t| {
            if !args.json {
                let (c, s) = (diag.snapshot(), stats.snapshot());
                say(&render_sender_report(
                    &format!("Sender report at {t:.1} seconds"),
                    &SenderReport {
                        elapsed_s: t,
                        capture: &c,
                        sender: &s,
                        capture_rate: rate,
                    },
                ));
            }
        },
    );
    let end_reason = match &end {
        Ok(r) => *r,
        Err(e) => {
            failure = Some(e.clone());
            "stopped by an error"
        }
    };
    let elapsed = stats.snapshot().frames_encoded as f64 / 100.0;
    sender.stop();
    let _ = capture.stop();
    let (c, s) = (diag.snapshot(), stats.snapshot());
    let report = SenderReport {
        elapsed_s: elapsed,
        capture: &c,
        sender: &s,
        capture_rate: rate,
    };
    if args.json {
        let doc = SendJson {
            schema: "audionet.send_report",
            schema_version: 1,
            end_reason,
            error: failure.clone(),
            report,
        };
        println!(
            "{}",
            serde_json::to_string_pretty(&doc).expect("serializes")
        );
    } else {
        say(&render_sender_report(
            &format!("Sender summary ({end_reason})"),
            &report,
        ));
    }
    match failure {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

#[derive(Debug)]
pub struct ReceiveArgs {
    pub listen: String,
    pub key_file: PathBuf,
    pub output: EndpointSelector,
    pub target_ms: u32,
    pub fixed_target: bool,
    pub seconds: u64,
    pub report_every: u64,
    pub mmcss: bool,
    pub json: bool,
    /// Test only: at `stall_at_s`, stop consuming audio for this long.
    pub simulate_render_stall_ms: Option<u64>,
    pub stall_at_s: u64,
}

/// Test wrapper: once, stops consuming from the playout for a while (the
/// device hears silence), exactly as if the render thread had been starved.
/// Nothing blocks; the point is to push playout depth up by the stall and
/// watch the depth controller bring it back.
struct StallInjector<S> {
    inner: S,
    stall_at_ns: u64,
    stall_ns: u64,
    started_ns: Option<u64>,
    done: bool,
}

impl<S: audionet_audio::render::RenderSource> audionet_audio::render::RenderSource
    for StallInjector<S>
{
    fn prepare(&mut self, rate: u32, channels: usize, max: usize) -> Result<(), String> {
        self.inner.prepare(rate, channels, max)
    }

    fn render(&mut self, out: &mut [f32], now_ns: u64) {
        let t0 = *self.started_ns.get_or_insert(now_ns);
        let since = now_ns - t0;
        if !self.done && since >= self.stall_at_ns && since < self.stall_at_ns + self.stall_ns {
            out.fill(0.0);
            return;
        }
        if since >= self.stall_at_ns + self.stall_ns {
            self.done = true;
        }
        self.inner.render(out, now_ns);
    }
}

#[derive(Serialize)]
struct ReceiveJson<'a> {
    schema: &'static str,
    schema_version: u32,
    end_reason: &'a str,
    error: Option<String>,
    report: ReceiverReport<'a>,
}

pub fn receive(args: ReceiveArgs) -> Result<(), String> {
    let key = load_key(&args.key_file)?;
    let listen: SocketAddr = if let Ok(port) = args.listen.parse::<u16>() {
        SocketAddr::from(([0, 0, 0, 0], port))
    } else {
        args.listen
            .parse()
            .map_err(|_| format!("{} is not a port or an address with a port", args.listen))?
    };
    let (label, name, endpoint) = select_device(Direction::Output, &args.output)?;
    let interrupted = install_ctrl_c();
    let socket =
        bind_media_socket(listen).map_err(|e| format!("could not listen on {listen}: {e}"))?;
    let start = f64::from(args.target_ms);
    let playout = if args.fixed_target {
        PlayoutConfig::with_target(start)
    } else {
        PlayoutConfig::adaptive(start, AdaptiveConfig::NATIVE)
    };
    let setup = args
        .mmcss
        .then(audionet_wasapi::capture::audio_thread_setup);
    let (receiver, source) =
        ReceiverRuntime::start(socket, &key, ReceiverConfig::default(), playout, setup)
            .map_err(|e| format!("could not start the receiver: {e}"))?;
    let source: Box<dyn audionet_audio::render::RenderSource> = match args.simulate_render_stall_ms
    {
        Some(ms) => Box::new(StallInjector {
            inner: source,
            stall_at_ns: args.stall_at_s * 1_000_000_000,
            stall_ns: ms * 1_000_000,
            started_ns: None,
            done: false,
        }),
        None => Box::new(source),
    };
    let mut render = RenderStream::start(
        RenderConfig {
            endpoint,
            mmcss: args.mmcss,
        },
        source,
    )
    .map_err(|e| e.to_string())?;

    let say = |text: &str| {
        if args.json {
            eprintln!("{text}");
        } else {
            println!("{text}");
        }
    };
    let f = &render.info().format;
    say(&format!(
        "Receiving on {}\nPlaying to {label}: {name}\nDevice format: {} Hz, {} channels; \
         Windows playback buffer {:.1} ms\nPlayout target: {}\nEncryption: on (pre-shared key)",
        receiver.local_addr(),
        f.sample_rate_hz,
        f.channels,
        f64::from(render.info().os_buffer_frames) * 1000.0 / f64::from(f.sample_rate_hz),
        match playout.adaptive {
            None => format!("{} ms, fixed", args.target_ms),
            Some(a) => format!(
                "starts at {} ms and adapts to the network ({:.0} to {:.0} ms)",
                args.target_ms, a.min_ms, a.max_ms
            ),
        }
    ));

    let rx = Arc::clone(receiver.stats());
    let po = Arc::clone(receiver.playout_stats());
    let timing = Arc::clone(render.timing());
    let start = Instant::now();
    let mut failure = None;
    let end = run_loop(
        args.seconds,
        args.report_every,
        &interrupted,
        || match render.poll_finished() {
            Some(Err(e)) => Err(e.to_string()),
            _ => Ok(()),
        },
        |t| {
            if !args.json {
                let (n, p, r) = (rx.snapshot(), po.snapshot(), timing.snapshot());
                say(&render_receiver_report(
                    &format!("Receiver report at {t:.1} seconds"),
                    &ReceiverReport {
                        elapsed_s: t,
                        receiver: &n,
                        playout: &p,
                        render: &r,
                    },
                ));
            }
        },
    );
    let end_reason = match &end {
        Ok(r) => *r,
        Err(e) => {
            failure = Some(e.clone());
            "stopped by an error"
        }
    };
    let elapsed = start.elapsed().as_secs_f64();
    let _ = render.stop();
    receiver.stop();
    let (n, p, r) = (rx.snapshot(), po.snapshot(), timing.snapshot());
    let report = ReceiverReport {
        elapsed_s: elapsed,
        receiver: &n,
        playout: &p,
        render: &r,
    };
    if args.json {
        let doc = ReceiveJson {
            schema: "audionet.receive_report",
            schema_version: 1,
            end_reason,
            error: failure.clone(),
            report,
        };
        println!(
            "{}",
            serde_json::to_string_pretty(&doc).expect("serializes")
        );
    } else {
        say(&render_receiver_report(
            &format!("Receiver summary after {elapsed:.1} seconds ({end_reason})"),
            &report,
        ));
    }
    match failure {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// Plays a steady test tone: a quick, accessible way to check an output
/// device, and a known signal for verifying the playback path.
struct ToneSource {
    rate: f64,
    channels: usize,
    phase: f64,
}

impl audionet_audio::render::RenderSource for ToneSource {
    fn prepare(&mut self, device_rate: u32, channels: usize, _max: usize) -> Result<(), String> {
        self.rate = f64::from(device_rate);
        self.channels = channels;
        Ok(())
    }

    fn render(&mut self, out: &mut [f32], _now_ns: u64) {
        let step = 2.0 * std::f64::consts::PI * 997.0 / self.rate;
        for frame in out.chunks_mut(self.channels) {
            let v = (0.25 * self.phase.sin()) as f32;
            for (i, s) in frame.iter_mut().enumerate() {
                *s = if i < 2 { v } else { 0.0 };
            }
            self.phase = (self.phase + step) % (2.0 * std::f64::consts::PI);
        }
    }
}

pub fn tone_test(output: &EndpointSelector, seconds: u64) -> Result<(), String> {
    let (label, name, endpoint) = select_device(Direction::Output, output)?;
    let render = RenderStream::start(
        RenderConfig {
            endpoint,
            mmcss: false,
        },
        Box::new(ToneSource {
            rate: 48_000.0,
            channels: 2,
            phase: 0.0,
        }),
    )
    .map_err(|e| e.to_string())?;
    println!("Playing a 997 Hz test tone at -12 dBFS on {label}: {name} for {seconds} seconds.");
    std::thread::sleep(Duration::from_secs(seconds));
    let t = render.timing().snapshot();
    render.stop().map_err(|e| e.to_string())?;
    println!(
        "Done. Playback callbacks: {}; later than twice the period: {}.",
        t.callbacks, t.late_callbacks
    );
    Ok(())
}
