//! Threaded runtime around the sans-I/O cores: UDP sockets, the network
//! receive thread, the sender (encoder) thread, and the render source.
//!
//! Thread model (AGENTS.md §2, §14, §17):
//!
//! ```text
//! capture callback → capture ring → [sender thread] adapt → encode → seal → UDP send
//! UDP → [receive thread] PacketStage → playout ring → [render callback] Playout → device
//! ```
//!
//! Neither audio callback touches a socket. The sender never blocks on the
//! network (non-blocking sends; a full socket buffer drops the packet and
//! counts it), and the receiver never waits for the render side.

use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use audionet_audio::clock;
use audionet_audio::render::RenderSource;
use audionet_audio::ring::{RingConsumer, audio_ring};
use audionet_audio::threads::{self, ThreadSetup};
use audionet_codec::EncoderConfig;
use audionet_transport::MAX_DATAGRAM;
use audionet_transport::crypto::{MediaCipher, PresharedKey};
use socket2::{Domain, Protocol, Socket, Type};

use crate::adapt::StreamAdapter;
use crate::playout::{Playout, PlayoutConfig, PlayoutStats, StreamControl};
use crate::receiver::{PacketStage, ReceiverConfig, ReceiverStats};
use crate::sender::{SenderCore, SenderStats};

/// Kernel receive buffer requested for media sockets. Survives scheduling
/// stalls; it is not the playout target (AGENTS.md §17).
const SOCKET_RECV_BUFFER: usize = 1 << 20;
/// Receive timeout: bounds stop latency and drives `PacketStage::on_tick`,
/// which must run well within `ReceiverConfig::conceal_below_ms` so a lost
/// packet is concealed before the playout ring runs dry.
const RECV_TIMEOUT: Duration = Duration::from_millis(5);
/// Sender poll interval when the capture ring is empty.
const SENDER_POLL: Duration = Duration::from_millis(2);
/// Sender trims capture audio older than this (encoder or network stalled).
const SENDER_STALE_MS: u64 = 200;
const SENDER_KEEP_MS: u64 = 20;

/// Creates a UDP socket bound to `addr` with a large receive buffer.
pub fn bind_media_socket(addr: SocketAddr) -> io::Result<UdpSocket> {
    let socket = Socket::new(Domain::for_address(addr), Type::DGRAM, Some(Protocol::UDP))?;
    let _ = socket.set_recv_buffer_size(SOCKET_RECV_BUFFER);
    socket.bind(&addr.into())?;
    Ok(socket.into())
}

// ─── receiver ───────────────────────────────────────────────────────────────

/// The network receive thread plus handles to its diagnostics.
#[derive(Debug)]
pub struct ReceiverRuntime {
    stats: Arc<ReceiverStats>,
    playout_stats: Arc<PlayoutStats>,
    playout_config: PlayoutConfig,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    local_addr: SocketAddr,
}

/// The render side of a receiver: hand it to a platform render stream.
pub struct PlayoutSource {
    pending: Option<(RingConsumer, Arc<StreamControl>)>,
    config: PlayoutConfig,
    stats: Arc<PlayoutStats>,
    playout: Option<Playout>,
}

impl std::fmt::Debug for PlayoutSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlayoutSource").finish_non_exhaustive()
    }
}

impl PlayoutSource {
    /// A render source for a playout ring filled by some other receiver
    /// (for example a WebRTC session feeding a `PacketStage`).
    pub fn new(
        consumer: RingConsumer,
        control: Arc<StreamControl>,
        config: PlayoutConfig,
    ) -> (PlayoutSource, Arc<PlayoutStats>) {
        let stats = Arc::new(PlayoutStats::default());
        (
            PlayoutSource {
                pending: Some((consumer, control)),
                config,
                stats: Arc::clone(&stats),
                playout: None,
            },
            stats,
        )
    }
}

impl RenderSource for PlayoutSource {
    fn prepare(
        &mut self,
        device_rate: u32,
        channels: usize,
        _max_frames: usize,
    ) -> Result<(), String> {
        let (consumer, control) = self
            .pending
            .take()
            .ok_or_else(|| "the playout source was already prepared".to_owned())?;
        self.playout = Some(Playout::new(
            consumer,
            control,
            self.config,
            device_rate,
            channels,
            Arc::clone(&self.stats),
        )?);
        Ok(())
    }

    fn render(&mut self, out: &mut [f32], now_ns: u64) {
        match &mut self.playout {
            Some(p) => p.render(out, now_ns),
            None => out.fill(0.0),
        }
    }
}

impl ReceiverRuntime {
    /// Starts receiving on `socket`. Returns the runtime and the render
    /// source to attach to an output device.
    pub fn start(
        socket: UdpSocket,
        key: &PresharedKey,
        receiver: ReceiverConfig,
        playout: PlayoutConfig,
        thread_setup: Option<ThreadSetup>,
    ) -> io::Result<(ReceiverRuntime, PlayoutSource)> {
        socket.set_read_timeout(Some(RECV_TIMEOUT))?;
        let local_addr = socket.local_addr()?;
        // One second of 48 kHz stereo: far above any playout target, bounded.
        let (producer, consumer) = audio_ring(48_000, usize::from(receiver.format.channels));
        let control = Arc::new(StreamControl::default());
        let mut stage = PacketStage::new(
            receiver,
            MediaCipher::new(key),
            producer,
            Arc::clone(&control),
        )
        .map_err(io::Error::other)?;
        let stats = Arc::clone(stage.stats());
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let thread = thread::Builder::new()
            .name("audionet-net-rx".into())
            .spawn(move || {
                let _priority = threads::enter(&thread_setup);
                let mut buf = vec![0u8; MAX_DATAGRAM + 64];
                while !thread_stop.load(Relaxed) {
                    match socket.recv_from(&mut buf) {
                        Ok((n, _from)) => {
                            let now = clock::now_ns();
                            stage.on_datagram(&mut buf[..n], now);
                        }
                        Err(e)
                            if matches!(
                                e.kind(),
                                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                            ) => {}
                        // ICMP port-unreachable surfaces as ConnectionReset on
                        // Windows; it is not fatal for a receiver.
                        Err(e) if e.kind() == io::ErrorKind::ConnectionReset => {}
                        Err(_) => thread::sleep(RECV_TIMEOUT),
                    }
                    stage.on_tick(clock::now_ns());
                }
            })?;
        let playout_stats = Arc::new(PlayoutStats::default());
        let source = PlayoutSource {
            pending: Some((consumer, control)),
            config: playout,
            stats: Arc::clone(&playout_stats),
            playout: None,
        };
        Ok((
            ReceiverRuntime {
                stats,
                playout_stats,
                playout_config: playout,
                stop,
                thread: Some(thread),
                local_addr,
            },
            source,
        ))
    }

    pub fn stats(&self) -> &Arc<ReceiverStats> {
        &self.stats
    }

    pub fn playout_stats(&self) -> &Arc<PlayoutStats> {
        &self.playout_stats
    }

    pub fn playout_config(&self) -> &PlayoutConfig {
        &self.playout_config
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for ReceiverRuntime {
    fn drop(&mut self) {
        self.shutdown();
    }
}

// ─── sender ─────────────────────────────────────────────────────────────────

/// Where the sender transmits. Shared so the destination can change (for
/// example after re-resolving a peer) without restarting capture.
pub type Destination = Arc<Mutex<SocketAddr>>;

#[derive(Debug)]
pub struct SenderRuntime {
    stats: Arc<SenderStats>,
    ssrc: u32,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl SenderRuntime {
    /// Starts the encoder thread reading `capture` (interleaved at
    /// `capture_rate`/`capture_channels`) and sending to `destination`.
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        mut capture: RingConsumer,
        capture_rate: u32,
        socket: UdpSocket,
        destination: Destination,
        key: &PresharedKey,
        encoder: EncoderConfig,
        thread_setup: Option<ThreadSetup>,
    ) -> io::Result<SenderRuntime> {
        socket.set_nonblocking(true)?;
        let channels = capture.channels();
        let block_frames = (capture_rate / 100) as usize * 4; // up to 40 ms per read
        let mut adapter =
            StreamAdapter::new(capture_rate, channels, block_frames).map_err(io::Error::other)?;
        let mut core = SenderCore::new(encoder, MediaCipher::new(key)).map_err(io::Error::other)?;
        let stats = Arc::clone(core.stats());
        let ssrc = core.ssrc();
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let stale = (u64::from(capture_rate) * SENDER_STALE_MS / 1000) as usize;
        let keep = (u64::from(capture_rate) * SENDER_KEEP_MS / 1000) as usize;
        let thread = thread::Builder::new()
            .name("audionet-sender".into())
            .spawn(move || {
                let _priority = threads::enter(&thread_setup);
                let mut buf = vec![0.0f32; block_frames * channels];
                let send_stats = Arc::clone(core.stats());
                while !thread_stop.load(Relaxed) {
                    capture.trim_stale(stale, keep);
                    let frames = capture.read(&mut buf);
                    if frames == 0 {
                        thread::sleep(SENDER_POLL);
                        continue;
                    }
                    let dest = *destination.lock().unwrap_or_else(|e| e.into_inner());
                    adapter.process(&buf[..frames * channels], |stereo| {
                        core.push(stereo, |packet| match socket.send_to(packet, dest) {
                            Ok(_) => {}
                            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                                send_stats.send_would_block.fetch_add(1, Relaxed);
                            }
                            Err(_) => {
                                send_stats.send_errors.fetch_add(1, Relaxed);
                            }
                        });
                    });
                }
            })?;
        Ok(SenderRuntime {
            stats,
            ssrc,
            stop,
            thread: Some(thread),
        })
    }

    pub fn stats(&self) -> &Arc<SenderStats> {
        &self.stats
    }

    pub fn ssrc(&self) -> u32 {
        self.ssrc
    }

    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for SenderRuntime {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use audionet_audio::ring::audio_ring;
    use std::time::Instant;

    /// Real sockets on localhost: sender thread → UDP → receive thread →
    /// playout, with the render side driven by this test.
    #[test]
    fn loopback_udp_end_to_end() {
        let key = PresharedKey::generate();
        let rx_socket = bind_media_socket("127.0.0.1:0".parse().unwrap()).unwrap();
        let (receiver, mut source) = ReceiverRuntime::start(
            rx_socket,
            &key,
            ReceiverConfig::default(),
            PlayoutConfig::default(),
            None,
        )
        .unwrap();
        let dest = Arc::new(Mutex::new(receiver.local_addr()));

        let (mut producer, consumer) = audio_ring(48_000, 2);
        let tx_socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        let sender = SenderRuntime::start(
            consumer,
            48_000,
            tx_socket,
            dest,
            &key,
            EncoderConfig::BASELINE,
            None,
        )
        .unwrap();

        source.prepare(48_000, 2, 480).unwrap();
        let mut out = vec![0.0f32; 960];
        let mut block = vec![0.0f32; 960];
        let start = Instant::now();
        let mut peak = 0.0f32;
        for tick in 0..150u64 {
            for (i, f) in block.chunks_mut(2).enumerate() {
                let t = (tick * 480 + i as u64) as f32 / 48_000.0;
                let v = 0.25 * (2.0 * std::f32::consts::PI * 997.0 * t).sin();
                f[0] = v;
                f[1] = v;
            }
            producer.write(&block);
            // Pace at real time so the network threads see realistic timing.
            let due = start + Duration::from_millis(tick * 10);
            if let Some(wait) = due.checked_duration_since(Instant::now()) {
                thread::sleep(wait);
            }
            source.render(&mut out, clock::now_ns());
            if tick > 50 {
                peak = out.iter().fold(peak, |m, v| m.max(v.abs()));
            }
        }
        let rx = receiver.stats().snapshot();
        let tx = sender.stats().snapshot();
        let playout = receiver.playout_stats().snapshot();
        assert!(tx.packets >= 140, "sent {}", tx.packets);
        assert!(rx.datagrams >= 140, "received {}", rx.datagrams);
        assert_eq!(rx.auth_failures, 0);
        assert!(playout.playing);
        assert!((0.2..0.3).contains(&peak), "decoded tone peak {peak}");
    }
}
