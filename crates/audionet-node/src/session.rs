//! One WebRTC media session, on its own thread, using str0m (sans-I/O).
//!
//! A session either answers a peer's offer or makes one (`Negotiation`),
//! and this device either sends or receives audio (`LocalMedia`):
//!
//! * **Send**: capture ring → format adapter → Opus (10 ms, low delay) →
//!   str0m writer (RTP/SRTP) → peer. The peer's jitter buffer and clock
//!   drift control run on its side (NetEQ in a browser, AudioNet's playout
//!   in a native app).
//! * **Receive**: str0m media events → `PacketStage` (reorder, FEC/PLC) →
//!   playout ring → drift/depth-controlled `Playout` → local output device.
//!
//! A browser or app listening to this device's source makes this device
//! answer and send; a native app listening to another device makes an offer
//! and receives.
//!
//! Real-time audio callbacks never run here; this thread is the "encoder
//! thread" / "network thread" of AGENTS.md, and it never blocks capture or
//! render (both hand off through bounded rings).

use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use audionet_audio::clock;
use audionet_audio::gain::{GainRamp, GainedSource, StreamGain};
use audionet_audio::ring::audio_ring;
use audionet_codec::{EncoderConfig, MAX_OPUS_PACKET, OpusEncoder};
use audionet_engine::adapt::StreamAdapter;
use audionet_engine::adaptive::AdaptiveConfig;
use audionet_engine::playout::{PlayoutConfig, StreamControl};
use audionet_engine::receiver::{PacketStage, ReceiverConfig};
use audionet_engine::runtime::PlayoutSource;
use audionet_protocol::SessionId;
use audionet_protocol::signal::{IceServer, SessionMedia, SessionState};
use str0m::change::{SdpAnswer, SdpOffer, SdpPendingOffer};
use str0m::format::Codec;
use str0m::media::{Direction, Frequency, MediaKind, MediaTime, Mid};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc, RtcConfig};

use crate::audio::{NodeAudio, StreamGuard};
use crate::relay::Relay;

/// How long ICE may take to connect before the session gives up.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
/// How long a disconnected session may try to recover.
const DISCONNECT_GRACE: Duration = Duration::from_secs(10);
/// Loop cadence while idle, so captured audio is encoded promptly.
const PUMP_INTERVAL: Duration = Duration::from_millis(5);
/// Deadlines already past that one trip round the media loop hands to
/// str0m at once (each releases a packet): enough for any backlog of a few
/// hundred milliseconds, while still returning to read the network.
const MAX_IMMEDIATE_TIMEOUTS: u32 = 64;
/// How long a session waits for a removed or reconfigured audio device to
/// come back before it ends. The network connection stays up meanwhile.
const DEVICE_RETURN_GRACE: Duration = Duration::from_secs(30);
/// How often the device is reopened while waiting.
const DEVICE_RETRY: Duration = Duration::from_secs(1);
/// A microphone that delivers nothing but exact zeros this long is almost
/// always blocked (operating-system permission, hardware mute), not quiet:
/// real microphones always pick up some noise.
const SILENT_INPUT_WARNING: Duration = Duration::from_secs(3);
/// How often a receiving session reports its diagnostics.
const DIAGNOSTICS_EVERY: Duration = Duration::from_secs(2);
/// How often a sending session reports what it sent (into the sending
/// device's status log, so not as often as receive diagnostics).
const SEND_REPORT_EVERY: Duration = Duration::from_secs(10);
/// How long to wait for the TURN server to allocate a relayed address.
const RELAY_ALLOCATE_TIMEOUT: Duration = Duration::from_secs(3);

/// What this device does with audio in a session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LocalMedia {
    /// Capture this source and send it to the peer.
    Send { source_id: String },
    /// Play what the peer sends on this destination.
    Receive { destination_id: String },
}

impl LocalMedia {
    /// This device's part when answering a peer's request: the peer
    /// listening to one of our sources means we send it, and so on.
    pub fn answering(media: &SessionMedia) -> Self {
        match media {
            SessionMedia::Listen { source_id } => Self::Send {
                source_id: source_id.clone(),
            },
            SessionMedia::Speak { destination_id } => Self::Receive {
                destination_id: destination_id.clone(),
            },
        }
    }
}

/// How a session is negotiated.
#[derive(Debug)]
pub enum Negotiation {
    /// Answer a peer's offer.
    Answer { offer_sdp: String },
    /// Make an offer (`SessionEvent::Offer`); the peer's answer arrives
    /// through `SessionHandle::deliver_answer`.
    Offer,
}

/// Network settings for a session.
#[derive(Clone, Debug, Default)]
pub struct SessionNetwork {
    /// STUN and TURN servers from the coordination server.
    pub ice_servers: Vec<IceServer>,
    /// Diagnostics: offer only the relayed candidate, to test the relay.
    pub relay_only: bool,
}

/// What a session reports back to the agent.
#[derive(Debug)]
pub enum SessionEvent {
    Answer {
        session_id: SessionId,
        sdp: String,
    },
    Offer {
        session_id: SessionId,
        sdp: String,
    },
    Status {
        session_id: SessionId,
        state: SessionState,
        detail: String,
    },
    /// Periodic measurements in words (not announced; for diagnostics).
    Diagnostics {
        session_id: SessionId,
        text: String,
    },
    /// The session ended on its own (not because the agent stopped it).
    Ended {
        session_id: SessionId,
        reason: String,
    },
}

pub type EventSink = Arc<dyn Fn(SessionEvent) + Send + Sync>;

pub struct SessionHandle {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    answers: std::sync::mpsc::Sender<String>,
    gain: Arc<StreamGain>,
}

impl std::fmt::Debug for SessionHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SessionHandle")
    }
}

impl SessionHandle {
    /// Hands the peer's answer to a session that made an offer.
    pub fn deliver_answer(&self, sdp: String) {
        let _ = self.answers.send(sdp);
    }

    /// This stream's volume and mute on this device: what it plays, or what
    /// it sends. `volume` is the slider position, 0.0 to 1.0.
    pub fn set_volume(&self, volume: f32, muted: bool) {
        self.gain.set(volume, muted);
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for SessionHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Starts a session on its own thread.
pub fn start(
    session_id: SessionId,
    local: LocalMedia,
    negotiation: Negotiation,
    network: SessionNetwork,
    audio: Arc<dyn NodeAudio>,
    events: EventSink,
    thread_setup: Option<audionet_audio::threads::ThreadSetup>,
) -> SessionHandle {
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let (answers, answer_rx) = std::sync::mpsc::channel();
    let gain = Arc::new(StreamGain::default());
    let thread_gain = Arc::clone(&gain);
    let thread = thread::Builder::new()
        .name(format!("audionet-session-{session_id}"))
        .spawn(move || {
            let _priority = audionet_audio::threads::enter(&thread_setup);
            let result = run(
                &session_id,
                &local,
                negotiation,
                &answer_rx,
                &network,
                audio.as_ref(),
                &events,
                &thread_stop,
                &thread_gain,
            );
            if let Err(reason) = result {
                events(SessionEvent::Status {
                    session_id: session_id.clone(),
                    state: SessionState::Failed,
                    detail: reason.clone(),
                });
                events(SessionEvent::Ended { session_id, reason });
            } else if !thread_stop.load(Relaxed) {
                events(SessionEvent::Ended {
                    session_id,
                    reason: "The connection closed.".into(),
                });
            }
        })
        .expect("spawning a session thread");
    SessionHandle {
        stop,
        thread: Some(thread),
        answers,
        gain,
    }
}

/// The address of the interface that routes to the internet (no packets
/// are sent: connecting a UDP socket only selects a route).
fn primary_ipv4(probe: Option<SocketAddr>) -> IpAddr {
    let target = probe.unwrap_or_else(|| SocketAddr::from(([192, 0, 2, 1], 9)));
    UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| {
            s.connect(target)?;
            s.local_addr()
        })
        .map(|a| a.ip())
        .unwrap_or(IpAddr::from([127, 0, 0, 1]))
}

/// `Ok` while the device runs; `Err` while waiting for it to come back.
type DeviceSlot<T> = Result<T, DeviceLoss>;

enum Pipeline {
    Listen {
        source_id: String,
        input: DeviceSlot<ListenInput>,
        encoder: OpusEncoder,
        frame: Vec<f32>,
        fill: usize,
        packet: Vec<u8>,
        rtp_time: u64,
    },
    Speak {
        destination_id: String,
        output: DeviceSlot<SpeakOutput>,
    },
}

/// The capture side of a listen session; rebuilt when the device returns
/// (its format may have changed).
struct ListenInput {
    capture: crate::audio::OpenCapture,
    adapter: StreamAdapter,
    read_buf: Vec<f32>,
    silence: SilenceWatch,
    /// This stream's volume on what is sent (see [`StreamGain`]).
    ramp: GainRamp,
}

/// What to check when a microphone sends only digital silence, for this
/// platform.
const SILENT_MICROPHONE: &str = if cfg!(target_os = "ios") {
    "Warning: the microphone is sending only digital silence. \
     Allow microphone access for AudioNet (Settings, Apps, AudioNet, Microphone), \
     and check that no other app is using the microphone."
} else if cfg!(target_os = "macos") {
    "Warning: the microphone is sending only digital silence. \
     Allow microphone access for the program running AudioNet \
     (System Settings, Privacy and Security, Microphone), and start it from that program. \
     Also check that the microphone is not muted; \
     a MacBook's built-in microphone is switched off while its lid is closed."
} else if cfg!(windows) {
    "Warning: the microphone is sending only digital silence. \
     Allow microphone access for desktop apps (Settings, Privacy and security, Microphone), \
     and check that the microphone is not muted."
} else {
    "Warning: the microphone is sending only digital silence. \
     Check that the microphone is not muted and that AudioNet may use it."
};

/// Notices a microphone that sends only digital silence.
struct SilenceWatch {
    /// Only input devices: silent loopback just means nothing is playing.
    enabled: bool,
    since: Instant,
    heard: bool,
    warned: bool,
}

impl SilenceWatch {
    /// Feeds captured samples; returns a message to show, at most once per
    /// change (silent for too long, then sound after a warning).
    fn observe(&mut self, samples: &[f32], now: Instant) -> Option<&'static str> {
        if !self.enabled || self.heard {
            return None;
        }
        if samples.iter().any(|s| *s != 0.0) {
            self.heard = true;
            return self
                .warned
                .then_some("Sound is now arriving from the microphone.");
        }
        if !self.warned && now.saturating_duration_since(self.since) >= SILENT_INPUT_WARNING {
            self.warned = true;
            return Some(SILENT_MICROPHONE);
        }
        None
    }
}

fn open_listen(audio: &dyn NodeAudio, source_id: &str) -> Result<ListenInput, String> {
    let capture = audio.open_source(source_id)?;
    let channels = capture.consumer.channels();
    let block = (capture.sample_rate / 100) as usize * 4;
    let adapter = StreamAdapter::new(capture.sample_rate, channels, block)?;
    let ramp = GainRamp::new(capture.sample_rate);
    Ok(ListenInput {
        capture,
        adapter,
        ramp,
        read_buf: vec![0.0; block * channels],
        silence: SilenceWatch {
            enabled: source_id.starts_with("input:"),
            since: Instant::now(),
            heard: false,
            warned: false,
        },
    })
}

/// The playback side of a speak session: a fresh packet stage, playout
/// buffer and output stream each time the device opens.
struct SpeakOutput {
    stage: PacketStage,
    guard: Box<dyn StreamGuard>,
    playout: Arc<audionet_engine::playout::PlayoutStats>,
}

/// How a packet reached this device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Route {
    /// Straight from the other device, on this network.
    DirectLocal,
    /// Straight from the other device, across the internet.
    DirectInternet,
    /// Through this device's allocation on the TURN relay.
    OurRelay,
    /// From the other device's allocation on the same TURN relay.
    TheirRelay,
}

impl Route {
    /// A packet that arrived on the session's own socket from `source`.
    /// `relay_ip`: the TURN relay's public address, if this session has an
    /// allocation there.
    fn of_direct(source: SocketAddr, relay_ip: Option<std::net::IpAddr>) -> Self {
        if relay_ip == Some(source.ip()) {
            Self::TheirRelay
        } else if is_local_address(source.ip()) {
            Self::DirectLocal
        } else {
            Self::DirectInternet
        }
    }

    fn words(self) -> &'static str {
        match self {
            Self::DirectLocal => "direct, on the local network",
            Self::DirectInternet => "direct, across the internet",
            Self::OurRelay => "through the relay (this device's allocation)",
            Self::TheirRelay => "through the relay (the other device's allocation)",
        }
    }
}

fn is_local_address(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            v4.is_private() || v4.is_link_local() || v4.is_loopback()
                // Carrier-grade NAT (100.64.0.0/10) is not the internet side.
                || (v4.octets()[0] == 100 && (v4.octets()[1] & 0xC0) == 64)
        }
        std::net::IpAddr::V6(v6) => {
            v6.is_loopback()
                || (v6.segments()[0] & 0xfe00) == 0xfc00 // unique local
                || (v6.segments()[0] & 0xffc0) == 0xfe80 // link local
        }
    }
}

/// Which way received packets came, between two reports: the route in
/// words for the diagnostics. Counted off the audio path (the network
/// loop), only as small integers.
#[derive(Debug, Default)]
struct RouteCounter {
    counts: [u64; 4],
    /// When counting started (the last report).
    since: Option<Instant>,
}

impl RouteCounter {
    fn count(&mut self, route: Route) {
        self.counts[route as usize] += 1;
    }

    /// "Route: …" for the packets since the last call, with how many
    /// datagrams a second reached this device from the network (before
    /// WebRTC handles them: audio, and a few checks and reports), and
    /// starts again.
    fn take_text(&mut self, now: Instant) -> String {
        let secs = self
            .since
            .replace(now)
            .map(|at| now.saturating_duration_since(at).as_secs_f64());
        let rate = match secs {
            Some(s) if s > 0.0 => format!(
                " Network: {:.1} datagrams a second.",
                self.counts.iter().sum::<u64>() as f64 / s
            ),
            _ => String::new(),
        };
        self.route_text() + &rate
    }

    fn route_text(&mut self) -> String {
        const ALL: [Route; 4] = [
            Route::DirectLocal,
            Route::DirectInternet,
            Route::OurRelay,
            Route::TheirRelay,
        ];
        let total: u64 = self.counts.iter().sum();
        let (top, n) = ALL
            .iter()
            .map(|r| (*r, self.counts[*r as usize]))
            .max_by_key(|(_, n)| *n)
            .unwrap_or((Route::DirectInternet, 0));
        self.counts = [0; 4];
        if total == 0 {
            return "Route: nothing arrived since the last report.".into();
        }
        let share = n * 100 / total;
        if share >= 90 {
            format!("Route: {}.", top.words())
        } else {
            format!(
                "Route: mostly {} ({share} % of packets), the rest by other paths.",
                top.words()
            )
        }
    }
}

/// One line of receive diagnostics, in words.
fn receive_diagnostics(o: &SpeakOutput) -> String {
    let r = o.stage.stats().snapshot();
    let p = o.playout.snapshot();
    format!(
        "Receiving: {} packets, {:.2} % lost, {} late, {} concealed; buffer {:.1} ms (lowest {} since last report), target {:.0} ms; underruns {}, re-bufferings {}, latency drains {}, jitter {:.1} ms, largest gap between packets {}.",
        r.datagrams,
        r.loss_percent,
        r.late_packets,
        r.concealed_frames,
        p.depth_us.current as f64 / 1000.0,
        p.depth_us
            .window_min
            .map_or("not measured".into(), |v| format!(
                "{:.1} ms",
                v as f64 / 1000.0
            )),
        p.target_ms,
        p.underruns,
        p.primings,
        p.latency_drains,
        r.jitter_ms,
        r.interarrival_ns
            .window_max_ns
            .map_or("not measured".into(), |v| format!(
                "{:.1} ms",
                v as f64 / 1e6
            ))
    ) + &format!(
        " Clock difference {:+.0} ppm ({}).",
        p.drift_ppm,
        if p.drift_locked {
            "locked"
        } else {
            "still measuring"
        }
    )
}

/// What a sending session delivered between reports: packets a second,
/// the capture against real time, and what was dropped by cause. A sender
/// that falls short of real time makes its listeners run dry however good
/// the network is; this says where the audio went.
struct SendMeter {
    at: Instant,
    rtp: u64,
    refused: u64,
    totals: crate::audio::CaptureTotals,
    ring: audionet_audio::ring::RingSnapshot,
}

impl SendMeter {
    fn new(now: Instant, rtp: u64, refused: u64, capture: &crate::audio::OpenCapture) -> Self {
        Self {
            at: now,
            rtp,
            refused,
            totals: capture.guard.capture_totals().unwrap_or_default(),
            ring: capture.consumer.stats().snapshot(),
        }
    }

    fn report(
        &mut self,
        now: Instant,
        rtp: u64,
        refused: u64,
        frame_samples: u64,
        capture: &crate::audio::OpenCapture,
    ) -> String {
        let secs = now.duration_since(self.at).as_secs_f64().max(1e-3);
        let totals = capture.guard.capture_totals();
        let ring = capture.consumer.stats().snapshot();
        let rate = f64::from(capture.sample_rate.max(1));
        let ms = |frames: u64| frames as f64 * 1000.0 / rate;
        let packets = rtp.saturating_sub(self.rtp) / frame_samples.max(1);
        let lost = refused.saturating_sub(self.refused).min(packets);
        let mut text = format!(
            "Sending: {:.1} packets a second ({} encoded frames refused by WebRTC for falling behind). Capture at {} Hz",
            (packets - lost) as f64 / secs,
            lost,
            capture.sample_rate
        );
        if let Some(t) = totals {
            let captured = t.captured.saturating_sub(self.totals.captured);
            let filled = t.filled.saturating_sub(self.totals.filled);
            let delivered = (captured + filled) as f64;
            text.push_str(&format!(
                ": {:.1} % of real time ({:.1} % of it silence filled in while nothing played), {} device glitches",
                delivered / (secs * rate) * 100.0,
                if delivered > 0.0 {
                    filled as f64 / delivered * 100.0
                } else {
                    0.0
                },
                t.glitches.saturating_sub(self.totals.glitches),
            ));
            self.totals = t;
        }
        text.push_str(&format!(
            "; dropped {:.0} ms for falling behind, {:.0} ms for a full buffer.",
            ms(ring
                .stale_trim_frames
                .saturating_sub(self.ring.stale_trim_frames)),
            ms(ring
                .overflow_frames
                .saturating_sub(self.ring.overflow_frames)),
        ));
        self.at = now;
        self.rtp = rtp;
        self.refused = refused;
        self.ring = ring;
        text
    }
}

/// Packets per second between reports: a sender that delivers less audio
/// than real time (native devices send 100 packets a second, browsers 50)
/// shows here, whatever the network does.
#[derive(Debug, Default)]
struct RateMeter {
    last: Option<(Instant, u64)>,
}

impl RateMeter {
    fn take_text(&mut self, datagrams: u64, now: Instant) -> String {
        let text = match self.last {
            Some((at, before)) if now > at && datagrams >= before => {
                let secs = now.duration_since(at).as_secs_f64();
                format!(
                    "Arriving: {:.1} packets a second.",
                    (datagrams - before) as f64 / secs
                )
            }
            _ => "Arriving: measured from the next report.".into(),
        };
        self.last = Some((now, datagrams));
        text
    }
}

fn open_speak(
    audio: &dyn NodeAudio,
    destination_id: &str,
    gain: &Arc<StreamGain>,
) -> Result<(SpeakOutput, String), String> {
    let (producer, consumer) = audio_ring(48_000, 2);
    let control = Arc::new(StreamControl::default());
    // Browsers send 20 ms frames by default: start at 60 ms for WAN paths
    // and let the target adapt (40 to 200 ms).
    let stage = PacketStage::new(
        ReceiverConfig::default(),
        None,
        producer,
        Arc::clone(&control),
    )?;
    let (source, playout) = PlayoutSource::new(
        consumer,
        control,
        PlayoutConfig::adaptive(60.0, AdaptiveConfig::WAN),
    );
    // Played at this stream's volume (see [`StreamGain`]).
    let source = GainedSource::new(Box::new(source), Arc::clone(gain));
    let (guard, description) = audio.open_destination(destination_id, Box::new(source))?;
    Ok((
        SpeakOutput {
            stage,
            guard,
            playout,
        },
        description,
    ))
}

/// A device that stopped during a session and is being waited for.
#[derive(Debug)]
struct DeviceLoss {
    since: Instant,
    next_try: Instant,
    why: String,
}

impl DeviceLoss {
    fn new(why: String, now: Instant) -> Self {
        // A format change leaves the device there: reopen at once.
        let retry = if why.contains(crate::audio::FORMAT_CHANGED) {
            Duration::ZERO
        } else {
            DEVICE_RETRY
        };
        Self {
            since: now,
            next_try: now + retry,
            why,
        }
    }

    /// Whether to try reopening now. `Err` with the reason once the device
    /// has been gone longer than [`DEVICE_RETURN_GRACE`].
    fn should_retry(&mut self, now: Instant) -> Result<bool, String> {
        if now.saturating_duration_since(self.since) > DEVICE_RETURN_GRACE {
            return Err(format!(
                "{} The device did not come back within {} seconds.",
                self.why,
                DEVICE_RETURN_GRACE.as_secs()
            ));
        }
        if now < self.next_try {
            return Ok(false);
        }
        self.next_try = now + DEVICE_RETRY;
        Ok(true)
    }
}

fn waiting_text(why: &str) -> String {
    if why.contains(crate::audio::FORMAT_CHANGED) {
        return format!("{why} Reopening it in the new format.");
    }
    format!(
        "{why} Waiting up to {} seconds for the device to come back.",
        DEVICE_RETURN_GRACE.as_secs()
    )
}

// The session thread's inputs, passed straight through from `start`.
#[allow(clippy::too_many_arguments)]
fn run(
    session_id: &SessionId,
    local_media: &LocalMedia,
    negotiation: Negotiation,
    answers: &std::sync::mpsc::Receiver<String>,
    network: &SessionNetwork,
    audio: &dyn NodeAudio,
    events: &EventSink,
    stop: &AtomicBool,
    gain: &Arc<StreamGain>,
) -> Result<(), String> {
    let status = |state, detail: String| {
        events(SessionEvent::Status {
            session_id: session_id.clone(),
            state,
            detail,
        })
    };
    // Technical detail (addresses, candidates, packet counts): a
    // measurement, which apps show only when asked to, not a status in words.
    let note = |detail: String| {
        events(SessionEvent::Diagnostics {
            session_id: session_id.clone(),
            text: detail,
        })
    };

    // ── Network setup ──
    let ice_servers = &network.ice_servers;
    let stun = ice_servers
        .iter()
        .flat_map(|s| s.urls.iter())
        .find(|u| u.starts_with("stun:"))
        .and_then(|u| crate::stun::resolve_stun_url(u));
    let local_ip = primary_ipv4(stun);
    // Bind to all interfaces and let the OS route each destination. Binding
    // to one interface address makes some systems (macOS with VPN tunnels
    // and interface-scoped routes) refuse sends with "no route to host".
    // The host candidate advertises the primary address; received packets
    // are attributed to it.
    let socket = UdpSocket::bind(SocketAddr::new(IpAddr::from([0, 0, 0, 0]), 0))
        .map_err(|e| format!("could not open a UDP socket: {e}"))?;
    let port = socket.local_addr().map_err(|e| e.to_string())?.port();
    let local = SocketAddr::new(local_ip, port);

    let mut rtc = RtcConfig::new()
        .clear_codecs()
        .enable_opus(true)
        // Deliver audio packets as they arrive. str0m would otherwise hold
        // up to 15 packets (150 ms at 10 ms frames) after a missing one,
        // starving playout into an underrun and then a latency jump
        // (measured through the TURN relay: every loss caused a 150-165 ms
        // arrival gap). AudioNet's PacketStage reorders and conceals, with
        // a wait tied to the playout buffer.
        .set_reordering_size_audio(0)
        .build(Instant::now());
    let public = stun.and_then(|server| crate::stun::query(&socket, server));
    // A relayed address on the TURN server, for peers we cannot reach
    // directly (mobile networks, strict NATs).
    let mut relay = match crate::relay::find_turn(ice_servers) {
        Some(server) => match Relay::allocate(&socket, local, &server, RELAY_ALLOCATE_TIMEOUT) {
            Ok(r) => Some(r),
            Err(e) => {
                note(format!("Relay unavailable: {e}."));
                None
            }
        },
        None => None,
    };
    if network.relay_only && relay.is_none() {
        return Err("Relay-only mode was requested, but no relay is available.".into());
    }
    if !network.relay_only {
        let host = Candidate::host(local, "udp").map_err(|e| format!("bad host candidate: {e}"))?;
        rtc.add_local_candidate(host);
        if let Some(public) = public {
            if public != local {
                if let Ok(c) = Candidate::server_reflexive(public, local, "udp") {
                    rtc.add_local_candidate(c);
                }
            }
        }
    }
    if let Some(r) = &relay {
        let c = Candidate::relayed(r.relayed(), local, "udp")
            .map_err(|e| format!("bad relayed candidate: {e}"))?;
        rtc.add_local_candidate(c);
    }
    let mut public_text = match (public, stun) {
        (Some(p), _) => p.to_string(),
        (None, Some(_)) => "unknown (the STUN server did not answer)".into(),
        (None, None) => "unknown (no STUN server configured)".into(),
    };
    if let Some(r) = &relay {
        public_text.push_str(&format!("; relay address {}", r.relayed()));
        if network.relay_only {
            public_text.push_str(" (relay only)");
        }
    }

    // ── Negotiate ──
    let mut mdns: Option<mpsc::Receiver<HiddenAddress>> = None;
    let mut mid: Option<Mid> = None;
    let mut pending: Option<SdpPendingOffer> = None;
    let local_sdp = match negotiation {
        Negotiation::Answer { offer_sdp } => {
            note(format!(
                "Network: local address {local}; public address {public_text}. Peer offered {}.",
                describe_candidates(&offer_sdp)
            ));
            mdns = find_hidden_addresses(&offer_sdp, local_ip, network.relay_only);
            let offer = SdpOffer::from_sdp_string(&offer_sdp)
                .map_err(|e| format!("the offer is not valid SDP: {e}"))?;
            let answer = rtc
                .sdp_api()
                .accept_offer(offer)
                .map_err(|e| format!("could not accept the offer: {e}"))?;
            SessionEvent::Answer {
                session_id: session_id.clone(),
                sdp: answer.to_sdp_string(),
            }
        }
        Negotiation::Offer => {
            note(format!(
                "Network: local address {local}; public address {public_text}."
            ));
            let direction = match local_media {
                LocalMedia::Send { .. } => Direction::SendOnly,
                LocalMedia::Receive { .. } => Direction::RecvOnly,
            };
            let mut change = rtc.sdp_api();
            mid = Some(change.add_media(MediaKind::Audio, direction, None, None, None));
            let (offer, pend) = change.apply().ok_or("could not create an offer")?;
            pending = Some(pend);
            SessionEvent::Offer {
                session_id: session_id.clone(),
                sdp: offer.to_sdp_string(),
            }
        }
    };

    // ── Audio setup (before answering, so failures are reported cleanly) ──
    let mut pipeline = match local_media {
        LocalMedia::Send { source_id } => {
            let input = open_listen(audio, source_id)?;
            let encoder = OpusEncoder::new(&EncoderConfig::BASELINE).map_err(|e| e.to_string())?;
            let frame_len = encoder.frame_samples() * 2;
            status(
                SessionState::Starting,
                format!("Capturing {}.", input.capture.description),
            );
            let mut r = [0u8; 4];
            rand::fill(&mut r);
            Pipeline::Listen {
                source_id: source_id.clone(),
                input: Ok(input),
                encoder,
                frame: vec![0.0; frame_len],
                fill: 0,
                packet: vec![0; MAX_OPUS_PACKET],
                rtp_time: u64::from(u32::from_le_bytes(r)),
            }
        }
        LocalMedia::Receive { destination_id } => {
            let (output, description) = open_speak(audio, destination_id, gain)?;
            status(
                SessionState::Starting,
                format!("Ready to play on {description}."),
            );
            Pipeline::Speak {
                destination_id: destination_id.clone(),
                output: Ok(output),
            }
        }
    };

    events(local_sdp);

    // ── Media loop ──
    let started = Instant::now();
    let mut connected = false;
    let mut disconnected_at: Option<Instant> = None;
    let mut buf = vec![0u8; 2000];
    // Connection diagnostics while ICE is still checking.
    let (mut sent, mut send_errors, mut received) = (0u64, 0u64, 0u64);
    let mut route = RouteCounter::default();
    let mut rate = RateMeter::default();
    let mut send_meter: Option<SendMeter> = None;
    // Encoded frames WebRTC refused (see `write_media`).
    let mut refused_frames = 0u64;
    // Test only: the shortest wait for network input, to reproduce PCs whose
    // short waits last one 15.6 ms timer tick (no program raised the timer
    // resolution). Unset in normal use: 1 ms.
    let min_wait = Duration::from_millis(
        std::env::var("AUDIONET_TEST_MIN_WAIT_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1),
    );
    let mut last_send_error = String::new();
    let mut last_report = Instant::now();
    let mut last_diagnostics = Instant::now();

    loop {
        if stop.load(Relaxed) {
            return Ok(());
        }
        if pending.is_some() {
            if let Ok(sdp) = answers.try_recv() {
                mdns = find_hidden_addresses(&sdp, local_ip, network.relay_only);
                let answer = SdpAnswer::from_sdp_string(&sdp)
                    .map_err(|e| format!("the answer is not valid SDP: {e}"))?;
                let pend = pending.take().expect("checked");
                rtc.sdp_api()
                    .accept_answer(pend, answer)
                    .map_err(|e| format!("could not accept the answer: {e}"))?;
                note(format!(
                    "The other device answered ({}).",
                    describe_candidates(&sdp)
                ));
            }
        }
        // Hidden local addresses the lookup found: direct paths to try.
        // Shown while connecting; once connected the stream's state stays
        // "Connected" (apps show the latest status as the state) and this
        // goes to the log only.
        while let Some(found) = mdns.as_ref().and_then(|rx| rx.try_recv().ok()) {
            let text = match found {
                HiddenAddress::Found { name, addr } => {
                    let Ok(c) = Candidate::host(addr, "udp") else {
                        continue;
                    };
                    rtc.add_remote_candidate(c);
                    format!(
                        "Found the other device's hidden local address ({name}): {addr}. Trying the direct path on this network."
                    )
                }
                HiddenAddress::NotFound { names } => format!(
                    "The other device's hidden local address ({names}) did not answer on this network; it is probably elsewhere."
                ),
            };
            if connected {
                tracing::info!(session = %session_id, "{text}");
            } else {
                note(text);
            }
        }
        if !connected && last_report.elapsed() >= Duration::from_secs(5) {
            last_report = Instant::now();
            let mut text = format!(
                "Still connecting: {sent} network packets sent, {received} received, {send_errors} send errors."
            );
            if !last_send_error.is_empty() {
                text.push_str(&format!(" Last send error: {last_send_error}."));
            }
            note(text);
        }
        if !connected && started.elapsed() > CONNECT_TIMEOUT {
            return Err("Could not connect to the other side. A firewall or network may be blocking the audio connection.".into());
        }
        if disconnected_at.is_some_and(|t| t.elapsed() > DISCONNECT_GRACE) {
            return Err("The audio connection was lost.".into());
        }

        // Audio work. A device that stops (removed, disabled, reconfigured)
        // is reopened for a while before the session gives up; the network
        // connection stays up meanwhile.
        match &mut pipeline {
            Pipeline::Listen {
                source_id,
                input,
                encoder,
                frame,
                fill,
                packet,
                rtp_time,
            } => {
                if let Err(loss) = input {
                    if loss.should_retry(Instant::now())? {
                        if let Ok(reopened) = open_listen(audio, source_id) {
                            // Keep RTP time in step with wall time across
                            // the gap, in whole Opus frames.
                            let step = encoder.frame_samples() as u64;
                            let gap = loss.since.elapsed().as_nanos() as u64 * 48 / 1_000_000;
                            *rtp_time += gap / step * step;
                            *fill = 0;
                            status(
                                if connected {
                                    SessionState::Active
                                } else {
                                    SessionState::Starting
                                },
                                format!(
                                    "The audio device is back. Capturing {}.",
                                    reopened.capture.description
                                ),
                            );
                            *input = Ok(reopened);
                        }
                    }
                }
                let failure = match input {
                    Ok(i) => i.capture.guard.failure(),
                    Err(_) => None,
                };
                if let Some(why) = failure {
                    let why = format!("Audio capture stopped: {why}.");
                    status(SessionState::Starting, waiting_text(&why));
                    *input = Err(DeviceLoss::new(why, Instant::now()));
                }
                if let Ok(ListenInput {
                    capture,
                    adapter,
                    read_buf,
                    silence,
                    ramp,
                }) = input
                {
                    // Keep the capture ring near the live edge while not sending.
                    let rate = capture.sample_rate as usize;
                    capture.consumer.trim_stale(rate / 5, rate / 50);
                    let ch = capture.consumer.channels();
                    loop {
                        let frames = capture.consumer.read(read_buf);
                        if frames == 0 {
                            break;
                        }
                        if let Some(text) =
                            silence.observe(&read_buf[..frames * ch], Instant::now())
                        {
                            let state = if connected {
                                SessionState::Active
                            } else {
                                SessionState::Starting
                            };
                            status(state, text.into());
                        }
                        ramp.apply(&mut read_buf[..frames * ch], ch, gain.target());
                        adapter.process(&read_buf[..frames * ch], |stereo| {
                            let mut src = stereo;
                            while !src.is_empty() {
                                let take = (frame.len() - *fill).min(src.len());
                                frame[*fill..*fill + take].copy_from_slice(&src[..take]);
                                *fill += take;
                                src = &src[take..];
                                if *fill == frame.len() {
                                    *fill = 0;
                                    if let Ok(n) = encoder.encode(frame, packet) {
                                        if let (true, Some(m)) = (connected, mid) {
                                            if !write_media(&mut rtc, m, *rtp_time, &packet[..n]) {
                                                refused_frames += 1;
                                            }
                                        }
                                    }
                                    *rtp_time += encoder.frame_samples() as u64;
                                }
                            }
                        });
                    }
                    // What this device sent, every few seconds while connected.
                    if connected {
                        let now = Instant::now();
                        match &mut send_meter {
                            None => {
                                send_meter =
                                    Some(SendMeter::new(now, *rtp_time, refused_frames, capture))
                            }
                            Some(m) if now.duration_since(m.at) >= SEND_REPORT_EVERY => {
                                let text = m.report(
                                    now,
                                    *rtp_time,
                                    refused_frames,
                                    encoder.frame_samples() as u64,
                                    capture,
                                );
                                events(SessionEvent::Diagnostics {
                                    session_id: session_id.clone(),
                                    text,
                                });
                            }
                            Some(_) => {}
                        }
                    }
                } else {
                    // The device is gone: start measuring again when it is back.
                    send_meter = None;
                }
            }
            Pipeline::Speak {
                destination_id,
                output,
            } => {
                if let Err(loss) = output {
                    if loss.should_retry(Instant::now())? {
                        if let Ok((reopened, description)) = open_speak(audio, destination_id, gain)
                        {
                            status(
                                if connected {
                                    SessionState::Active
                                } else {
                                    SessionState::Starting
                                },
                                format!("The audio device is back. Playing on {description}."),
                            );
                            *output = Ok(reopened);
                        }
                    }
                }
                let failure = match output {
                    Ok(o) => o.guard.failure(),
                    Err(_) => None,
                };
                if let Some(why) = failure {
                    let why = format!("Audio playback stopped: {why}.");
                    status(SessionState::Starting, waiting_text(&why));
                    *output = Err(DeviceLoss::new(why, Instant::now()));
                }
                if let Ok(o) = output {
                    o.stage.on_tick(clock::now_ns());
                    if connected && last_diagnostics.elapsed() >= DIAGNOSTICS_EVERY {
                        last_diagnostics = Instant::now();
                        events(SessionEvent::Diagnostics {
                            session_id: session_id.clone(),
                            text: format!(
                                "{} {} {}",
                                receive_diagnostics(o),
                                rate.take_text(
                                    o.stage.stats().snapshot().datagrams,
                                    Instant::now()
                                ),
                                route.take_text(Instant::now())
                            ),
                        });
                    }
                }
            }
        }

        // Drive str0m until it wants input. Its pacer releases one packet per
        // poll and then asks to be woken at once (a deadline already past):
        // that wake-up is given here, not after a wait on the socket, because
        // a short wait can last a whole 15.6 ms timer tick on many PCs. With
        // the socket wait in between, such a sender got out fewer than 100
        // packets a second and WebRTC refused the rest (measured 79 a second
        // with 15 ms waits, and about 92 from a laptop), so every listener
        // ran dry. Bounded, so the loop still reads the network.
        let mut due_now = 0;
        let deadline = loop {
            match rtc
                .poll_output()
                .map_err(|e| format!("WebRTC error: {e}"))?
            {
                Output::Timeout(t) => {
                    let now = Instant::now();
                    if t <= now && due_now < MAX_IMMEDIATE_TIMEOUTS {
                        due_now += 1;
                        rtc.handle_input(Input::Timeout(now))
                            .map_err(|e| format!("WebRTC error: {e}"))?;
                        continue;
                    }
                    break t;
                }
                Output::Transmit(t) => {
                    // From the relayed candidate: send through the relay.
                    if let Some(r) = relay.as_mut().filter(|r| t.source == r.relayed()) {
                        r.send(&socket, t.destination, &t.contents, Instant::now());
                        sent += 1;
                        continue;
                    }
                    match socket.send_to(&t.contents, t.destination) {
                        Ok(_) => sent += 1,
                        Err(e) => {
                            send_errors += 1;
                            last_send_error = format!("{e} (to {})", t.destination);
                        }
                    }
                }
                Output::Event(e) => match e {
                    Event::Connected => {
                        connected = true;
                        status(SessionState::Active, "Connected. Audio is flowing.".into());
                    }
                    Event::IceConnectionStateChange(IceConnectionState::Disconnected) => {
                        disconnected_at.get_or_insert_with(Instant::now);
                        status(
                            SessionState::Starting,
                            "Network connection interrupted; trying to recover.".into(),
                        );
                    }
                    Event::IceConnectionStateChange(IceConnectionState::Checking) => {
                        status(
                            SessionState::Starting,
                            "Checking network paths to the other side.".into(),
                        );
                    }
                    Event::IceConnectionStateChange(
                        IceConnectionState::Connected | IceConnectionState::Completed,
                    ) => {
                        if disconnected_at.take().is_some() {
                            status(
                                SessionState::Active,
                                "Network connection restored. Audio is flowing.".into(),
                            );
                        }
                    }
                    Event::MediaAdded(m) => {
                        mid.get_or_insert(m.mid);
                    }
                    Event::MediaData(d) => {
                        // Packets arriving while the output device is gone
                        // are dropped; the session status says why.
                        if let Pipeline::Speak { output: Ok(o), .. } = &mut pipeline {
                            let seq = **d.seq_range.start() as u16;
                            let ts = d.time.numer() as u32;
                            o.stage.on_rtp(seq, ts, 1, &d.data, clock::now_ns());
                        }
                    }
                    _ => {}
                },
            }
        };

        let wait = deadline
            .saturating_duration_since(Instant::now())
            .min(PUMP_INTERVAL.max(min_wait))
            .max(min_wait);
        let _ = socket.set_read_timeout(Some(wait));
        let received_packet = socket.recv_from(&mut buf);
        if received_packet.is_ok() {
            received += 1;
        }
        if let Some(r) = relay.as_mut() {
            let now = Instant::now();
            if let Ok((n, source)) = &received_packet {
                if r.is_from_server(*source) {
                    // Relayed data from peers, as if it arrived at the
                    // relayed address.
                    let relayed = r.relayed();
                    for (peer, data) in r.receive(&socket, &buf[..*n], now) {
                        route.count(Route::OurRelay);
                        if let Ok(contents) = data.as_slice().try_into() {
                            rtc.handle_input(Input::Receive(
                                now,
                                Receive {
                                    proto: Protocol::Udp,
                                    source: peer,
                                    destination: relayed,
                                    contents,
                                },
                            ))
                            .map_err(|e| format!("WebRTC error: {e}"))?;
                        }
                    }
                    continue;
                }
            }
            r.tick(&socket, now);
        }
        if let Ok((_, source)) = &received_packet {
            route.count(Route::of_direct(
                *source,
                relay.as_ref().map(|r| r.relayed().ip()),
            ));
        }
        let input = match received_packet {
            Ok((n, source)) => match buf[..n].try_into() {
                Ok(contents) => Input::Receive(
                    Instant::now(),
                    Receive {
                        proto: Protocol::Udp,
                        source,
                        destination: local,
                        contents,
                    },
                ),
                Err(_) => continue, // not a WebRTC packet (e.g. stray STUN reply)
            },
            Err(_) => Input::Timeout(Instant::now()),
        };
        rtc.handle_input(input)
            .map_err(|e| format!("WebRTC error: {e}"))?;
    }
}

/// Hands one encoded frame to WebRTC. `false` when it was refused (str0m
/// holds at most about 100 frames it has not yet packetized: a sending loop
/// that falls behind loses frames here, before they get sequence numbers,
/// so receivers see fewer packets but no loss).
fn write_media(rtc: &mut Rtc, mid: Mid, rtp_time: u64, data: &[u8]) -> bool {
    let Some(writer) = rtc.writer(mid) else {
        return false;
    };
    let Some(pt) = writer
        .payload_params()
        .find(|p| p.spec().codec == Codec::Opus)
        .map(|p| p.pt())
    else {
        return false;
    };
    let time = MediaTime::new(rtp_time, Frequency::FORTY_EIGHT_KHZ);
    writer
        .write(pt, Instant::now(), time, data.to_vec())
        .is_ok()
}

/// What the lookup of a peer's hidden (`.local`) addresses found.
enum HiddenAddress {
    Found { name: String, addr: SocketAddr },
    NotFound { names: String },
}

/// How long the lookup may go on. Browsers answer within milliseconds on
/// the same network, but Wi-Fi drops multicast now and then (measured: a
/// closed MacBook missed 3 seconds of queries). Nothing waits for it: the
/// connection starts on the other paths and moves to the direct one if the
/// address turns up.
const MDNS_TIMEOUT: Duration = Duration::from_secs(10);

/// Starts looking up the `.local` host candidates in the peer's SDP on a
/// helper thread (see `mdns`); results arrive on the returned channel while
/// ICE already runs with the other candidates, so nothing waits for it.
fn find_hidden_addresses(
    sdp: &str,
    local_ip: IpAddr,
    relay_only: bool,
) -> Option<mpsc::Receiver<HiddenAddress>> {
    let hidden = crate::mdns::hidden_candidates(sdp);
    if hidden.is_empty() || relay_only {
        return None;
    }
    let (tx, rx) = mpsc::channel();
    let names: Vec<String> = hidden.iter().map(|c| c.name.clone()).collect();
    std::thread::Builder::new()
        .name("audionet-mdns".into())
        .spawn(move || {
            let mut found_names = Vec::new();
            let result = crate::mdns::resolve(&names, local_ip, MDNS_TIMEOUT, |name, ip| {
                found_names.push(name.to_owned());
                for c in hidden.iter().filter(|c| c.name == name) {
                    let _ = tx.send(HiddenAddress::Found {
                        name: name.to_owned(),
                        addr: SocketAddr::new(IpAddr::V4(ip), c.port),
                    });
                }
            });
            let missing: Vec<&str> = names
                .iter()
                .filter(|n| !found_names.contains(n))
                .map(String::as_str)
                .collect();
            if result.is_err() || !missing.is_empty() {
                let _ = tx.send(HiddenAddress::NotFound {
                    names: missing.join(", "),
                });
            }
        })
        .ok()?;
    Some(rx)
}

/// Summarizes the ICE candidates in an SDP offer by type, in words, for
/// diagnostics ("2 local, 1 hidden local (mDNS), 1 public, 1 relay").
fn describe_candidates(sdp: &str) -> String {
    let (mut host, mut mdns, mut srflx, mut relay, mut other) = (0, 0, 0, 0, 0);
    for line in sdp.lines().filter(|l| l.starts_with("a=candidate:")) {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let address = fields.get(4).copied().unwrap_or("");
        match fields
            .iter()
            .position(|f| *f == "typ")
            .and_then(|i| fields.get(i + 1))
        {
            Some(&"host") if address.ends_with(".local") => mdns += 1,
            Some(&"host") => host += 1,
            Some(&"srflx") | Some(&"prflx") => srflx += 1,
            Some(&"relay") => relay += 1,
            _ => other += 1,
        }
    }
    let total = host + mdns + srflx + relay + other;
    if total == 0 {
        return "no network candidates".into();
    }
    format!("{host} local, {mdns} hidden local (mDNS), {srflx} public, {relay} relay candidates")
}

#[cfg(test)]
mod tests {
    use super::{
        DEVICE_RETURN_GRACE, DeviceLoss, Duration, Instant, SilenceWatch, describe_candidates,
    };

    #[test]
    fn silent_microphone_is_reported_once_then_recovery() {
        let t0 = Instant::now();
        let mut w = SilenceWatch {
            enabled: true,
            since: t0,
            heard: false,
            warned: false,
        };
        let zeros = [0.0f32; 480];
        assert_eq!(w.observe(&zeros, t0 + Duration::from_secs(2)), None);
        let warning = w.observe(&zeros, t0 + Duration::from_secs(3)).unwrap();
        assert!(warning.starts_with("Warning: the microphone is sending only digital silence."));
        assert_eq!(w.observe(&zeros, t0 + Duration::from_secs(9)), None);
        let mut sound = zeros;
        sound[7] = 1e-5;
        assert_eq!(
            w.observe(&sound, t0 + Duration::from_secs(10)),
            Some("Sound is now arriving from the microphone.")
        );
        assert_eq!(w.observe(&zeros, t0 + Duration::from_secs(20)), None);
    }

    #[test]
    fn quiet_microphone_and_loopback_are_not_reported() {
        let t0 = Instant::now();
        let mut noisy = SilenceWatch {
            enabled: true,
            since: t0,
            heard: false,
            warned: false,
        };
        assert_eq!(noisy.observe(&[0.0, 3e-6, 0.0], t0), None);
        assert_eq!(
            noisy.observe(&[0.0; 480], t0 + Duration::from_secs(60)),
            None
        );
        let mut loopback = SilenceWatch {
            enabled: false,
            since: t0,
            heard: false,
            warned: false,
        };
        assert_eq!(
            loopback.observe(&[0.0; 480], t0 + Duration::from_secs(60)),
            None
        );
    }

    #[test]
    fn a_format_change_is_reopened_at_once() {
        let t0 = Instant::now();
        let why = format!(
            "Audio capture stopped: {} (48000 Hz, 2 channels to 16000 Hz, 1 channels).",
            crate::audio::FORMAT_CHANGED
        );
        let mut loss = DeviceLoss::new(why, t0);
        assert_eq!(loss.should_retry(t0), Ok(true));
        // Should that fail, the next attempts wait the usual second.
        assert_eq!(
            loss.should_retry(t0 + Duration::from_millis(500)),
            Ok(false)
        );
    }

    #[test]
    fn device_loss_retries_every_second_then_gives_up() {
        let t0 = Instant::now();
        let mut loss = DeviceLoss::new("Audio capture stopped: unplugged.".into(), t0);
        assert_eq!(loss.should_retry(t0), Ok(false));
        assert_eq!(
            loss.should_retry(t0 + Duration::from_millis(999)),
            Ok(false)
        );
        assert_eq!(loss.should_retry(t0 + Duration::from_secs(1)), Ok(true));
        // The next attempt waits another full second.
        assert_eq!(
            loss.should_retry(t0 + Duration::from_millis(1500)),
            Ok(false)
        );
        assert_eq!(loss.should_retry(t0 + Duration::from_secs(2)), Ok(true));
        assert_eq!(loss.should_retry(t0 + DEVICE_RETURN_GRACE), Ok(true));
        let err = loss
            .should_retry(t0 + DEVICE_RETURN_GRACE + Duration::from_millis(1))
            .unwrap_err();
        assert_eq!(
            err,
            "Audio capture stopped: unplugged. The device did not come back within 30 seconds."
        );
    }

    #[test]
    fn counts_candidate_types() {
        let sdp = "v=0\r\na=candidate:1 1 udp 1 192.168.1.5 5000 typ host\r\n\
                   a=candidate:2 1 udp 1 abcd.local 5001 typ host\r\n\
                   a=candidate:3 1 udp 1 203.0.113.9 6000 typ srflx raddr 0.0.0.0 rport 0\r\n\
                   a=candidate:4 1 udp 1 198.51.100.2 7000 typ relay raddr 0.0.0.0 rport 0\r\n";
        assert_eq!(
            describe_candidates(sdp),
            "1 local, 1 hidden local (mDNS), 1 public, 1 relay candidates"
        );
        assert_eq!(describe_candidates("v=0"), "no network candidates");
    }
}

#[cfg(test)]
mod route_tests {
    use super::*;

    #[test]
    fn packets_a_second() {
        let t0 = Instant::now();
        let mut m = RateMeter::default();
        assert_eq!(
            m.take_text(0, t0),
            "Arriving: measured from the next report."
        );
        assert_eq!(
            m.take_text(200, t0 + Duration::from_secs(2)),
            "Arriving: 100.0 packets a second."
        );
        assert_eq!(
            m.take_text(300, t0 + Duration::from_secs(4)),
            "Arriving: 50.0 packets a second."
        );
    }

    #[test]
    fn routes_in_words() {
        let relay: std::net::IpAddr = "203.0.113.7".parse().unwrap();
        let at = |s: &str| s.parse::<SocketAddr>().unwrap();
        assert_eq!(
            Route::of_direct(at("192.168.5.106:5000"), Some(relay)),
            Route::DirectLocal
        );
        assert_eq!(
            Route::of_direct(at("100.72.1.2:5000"), None),
            Route::DirectLocal,
            "carrier-grade NAT"
        );
        assert_eq!(
            Route::of_direct(at("198.51.100.20:5000"), Some(relay)),
            Route::DirectInternet
        );
        assert_eq!(
            Route::of_direct(at("203.0.113.7:49318"), Some(relay)),
            Route::TheirRelay
        );
        assert_eq!(
            Route::of_direct(at("[fe80::1]:5000"), None),
            Route::DirectLocal
        );

        let mut c = RouteCounter::default();
        assert_eq!(
            c.route_text(),
            "Route: nothing arrived since the last report."
        );
        for _ in 0..95 {
            c.count(Route::DirectInternet);
        }
        for _ in 0..5 {
            c.count(Route::OurRelay);
        }
        assert_eq!(c.route_text(), "Route: direct, across the internet.");
        for _ in 0..60 {
            c.count(Route::TheirRelay);
        }
        for _ in 0..40 {
            c.count(Route::DirectInternet);
        }
        assert_eq!(
            c.route_text(),
            "Route: mostly through the relay (the other device's allocation) (60 % of packets), the rest by other paths."
        );
        assert_eq!(
            c.route_text(),
            "Route: nothing arrived since the last report.",
            "starts again after each report"
        );
    }
}
