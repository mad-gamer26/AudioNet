//! Network-side receiver stage: datagram → authenticate → stream selection
//! → sequence tracking → reorder window → Opus decode (with FEC/PLC) →
//! playout ring.
//!
//! Runs on the dedicated network receive thread (AGENTS.md §17). It never
//! waits for the render side: decoded audio goes into the bounded playout
//! ring, and if that ring is full the ring counts the overflow.
//!
//! # Reordering and loss
//!
//! Packets are released in sequence order. A missing packet is waited for
//! as long as the playout ring still holds decoded audio: it is declared
//! lost only when the ring falls below [`ReceiverConfig::conceal_below_ms`]
//! (the render side is about to need that audio), when the reorder slots
//! are nearly all in use, or after [`ReceiverConfig::hold_timeout_ms`] with
//! nothing new. So the whole playout target, not a fixed packet count, is
//! the tolerance for late packets. A lost frame is concealed, using the
//! next packet's in-band FEC when it is already here, otherwise Opus PLC.
//!
//! Packets held behind a gap are still buffered audio: the stage publishes
//! them as *pending* frames ([`StreamControl::set_pending_frames`]), and the
//! playout side counts them in its depth, so waiting for a late packet does
//! not look like the buffer draining. A run of more than
//! [`ReceiverConfig::max_conceal_frames`] missing packets is an outage:
//! concealing it would only add stale latency, so the stage resynchronizes
//! to the newest audio instead and the playout side re-primes. Packets that
//! arrive after their slot was released are *late* and are dropped.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::Instant;

use audionet_audio::ring::RingProducer;
use audionet_codec::{MAX_FRAME_SAMPLES, MAX_OPUS_PACKET, OpusDecoder};
use audionet_protocol::StreamFormat;
use audionet_transport::crypto::{CryptoError, MediaCipher};
use audionet_transport::rtp::PT_OPUS;
use audionet_transport::seq::{Arrival, SequenceStats, SequenceTracker};
use serde::Serialize;

use crate::playout::StreamControl;
use crate::stats::{AtomicF64, DurationSnapshot, DurationStat};

/// Number of reorder slots (packets that can be held while waiting).
const SLOTS: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct ReceiverConfig {
    pub format: StreamFormat,
    /// A missing packet is declared lost once the playout ring holds less
    /// decoded audio than this: one render period plus scheduling slack.
    pub conceal_below_ms: u64,
    /// Longest run of lost frames that is concealed rather than skipped.
    pub max_conceal_frames: u64,
    /// Release held packets after this long with no new arrivals.
    pub hold_timeout_ms: u64,
    /// A different sender (SSRC) is adopted only after the current one has
    /// been silent this long, so two senders cannot fight over one stream.
    pub ssrc_switch_idle_ms: u64,
}

impl Default for ReceiverConfig {
    fn default() -> Self {
        Self {
            format: StreamFormat::BASELINE,
            conceal_below_ms: 15,
            max_conceal_frames: 5,
            hold_timeout_ms: 60,
            ssrc_switch_idle_ms: 1000,
        }
    }
}

/// Network-side receiver counters.
#[derive(Debug, Default)]
pub struct ReceiverStats {
    pub datagrams: AtomicU64,
    pub bytes: AtomicU64,
    pub auth_failures: AtomicU64,
    pub malformed: AtomicU64,
    pub wrong_payload_type: AtomicU64,
    pub foreign_ssrc: AtomicU64,
    pub stream_starts: AtomicU64,
    pub late_packets: AtomicU64,
    pub concealed_frames: AtomicU64,
    pub fec_attempts: AtomicU64,
    pub outage_resyncs: AtomicU64,
    pub decode_errors: AtomicU64,
    pub seq_received: AtomicU64,
    pub seq_expected: AtomicU64,
    pub seq_reordered: AtomicU64,
    pub seq_duplicates: AtomicU64,
    pub seq_resets: AtomicU64,
    pub jitter_ms: AtomicF64,
    pub interarrival: DurationStat,
    pub decode: DurationStat,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct ReceiverSnapshot {
    pub datagrams: u64,
    pub bytes: u64,
    pub auth_failures: u64,
    pub malformed: u64,
    pub wrong_payload_type: u64,
    pub foreign_ssrc: u64,
    pub stream_starts: u64,
    pub late_packets: u64,
    pub concealed_frames: u64,
    pub fec_attempts: u64,
    pub outage_resyncs: u64,
    pub decode_errors: u64,
    pub sequence: SequenceStats,
    pub loss_percent: f64,
    pub jitter_ms: f64,
    pub interarrival_ns: DurationSnapshot,
    pub decode_ns: DurationSnapshot,
}

impl ReceiverStats {
    pub fn snapshot(&self) -> ReceiverSnapshot {
        let sequence = SequenceStats {
            received: self.seq_received.load(Relaxed),
            expected: self.seq_expected.load(Relaxed),
            reordered: self.seq_reordered.load(Relaxed),
            duplicates: self.seq_duplicates.load(Relaxed),
            resets: self.seq_resets.load(Relaxed),
        };
        ReceiverSnapshot {
            datagrams: self.datagrams.load(Relaxed),
            bytes: self.bytes.load(Relaxed),
            auth_failures: self.auth_failures.load(Relaxed),
            malformed: self.malformed.load(Relaxed),
            wrong_payload_type: self.wrong_payload_type.load(Relaxed),
            foreign_ssrc: self.foreign_ssrc.load(Relaxed),
            stream_starts: self.stream_starts.load(Relaxed),
            late_packets: self.late_packets.load(Relaxed),
            concealed_frames: self.concealed_frames.load(Relaxed),
            fec_attempts: self.fec_attempts.load(Relaxed),
            outage_resyncs: self.outage_resyncs.load(Relaxed),
            decode_errors: self.decode_errors.load(Relaxed),
            sequence,
            loss_percent: sequence.loss_percent(),
            jitter_ms: self.jitter_ms.load(),
            interarrival_ns: self.interarrival.snapshot(),
            decode_ns: self.decode.snapshot(),
        }
    }
}

#[derive(Clone)]
struct Slot {
    ext: u64,
    len: usize,
    occupied: bool,
    data: [u8; MAX_OPUS_PACKET],
}

pub struct PacketStage {
    config: ReceiverConfig,
    /// Present for the PSK UDP path; `None` when a WebRTC stack decrypts.
    cipher: Option<MediaCipher>,
    decoder: OpusDecoder,
    tracker: SequenceTracker,
    ssrc: Option<u32>,
    last_arrival_ns: Option<u64>,
    next_release: Option<u64>,
    /// Highest extended sequence number accepted into the reorder slots.
    highest_accepted: Option<u64>,
    slots: Box<[Slot]>,
    held: usize,
    pcm: Vec<f32>,
    producer: RingProducer,
    control: Arc<StreamControl>,
    stats: Arc<ReceiverStats>,
    prev_transit: Option<f64>,
    jitter: f64,
    now_ns: u64,
    /// Duration of the most recent packet, per channel; lost packets are
    /// assumed to be the same length.
    frame_samples: usize,
}

impl std::fmt::Debug for PacketStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PacketStage")
            .field("ssrc", &self.ssrc)
            .field("next_release", &self.next_release)
            .finish_non_exhaustive()
    }
}

impl PacketStage {
    /// Control path: allocates all buffers.
    pub fn new(
        config: ReceiverConfig,
        cipher: impl Into<Option<MediaCipher>>,
        producer: RingProducer,
        control: Arc<StreamControl>,
    ) -> Result<Self, String> {
        let decoder = OpusDecoder::new(&config.format).map_err(|e| e.to_string())?;
        let pcm = vec![0.0; MAX_FRAME_SAMPLES * decoder.channels()];
        let frame_samples = decoder.frame_samples();
        let empty = Slot {
            ext: 0,
            len: 0,
            occupied: false,
            data: [0; MAX_OPUS_PACKET],
        };
        Ok(Self {
            config,
            cipher: cipher.into(),
            decoder,
            tracker: SequenceTracker::new(),
            ssrc: None,
            last_arrival_ns: None,
            next_release: None,
            highest_accepted: None,
            slots: vec![empty; SLOTS].into_boxed_slice(),
            held: 0,
            pcm,
            producer,
            control,
            stats: Arc::new(ReceiverStats::default()),
            prev_transit: None,
            jitter: 0.0,
            now_ns: 0,
            frame_samples,
        })
    }

    pub fn stats(&self) -> &Arc<ReceiverStats> {
        &self.stats
    }

    /// Handles one received datagram (decrypted in place). `arrival_ns` is on
    /// the clock the render side passes to `Playout::render`.
    pub fn on_datagram(&mut self, datagram: &mut [u8], arrival_ns: u64) {
        let cipher = self
            .cipher
            .as_ref()
            .expect("on_datagram needs a PSK cipher");
        let len = datagram.len();
        let opened = cipher.open(datagram);
        self.stats.datagrams.fetch_add(1, Relaxed);
        self.stats.bytes.fetch_add(len as u64, Relaxed);
        let (header, payload) = match opened {
            Ok(opened) => opened,
            Err(CryptoError::Authentication) => {
                self.stats.auth_failures.fetch_add(1, Relaxed);
                return;
            }
            Err(_) => {
                self.stats.malformed.fetch_add(1, Relaxed);
                return;
            }
        };
        if header.payload_type != PT_OPUS {
            self.stats.wrong_payload_type.fetch_add(1, Relaxed);
            return;
        }
        self.on_rtp(
            header.sequence,
            header.timestamp,
            header.ssrc,
            &datagram[payload],
            arrival_ns,
        );
    }

    /// Handles one already-authenticated RTP packet's Opus payload, for
    /// example from a WebRTC stack, which does SRTP itself.
    pub fn on_rtp(
        &mut self,
        sequence: u16,
        timestamp: u32,
        ssrc: u32,
        payload: &[u8],
        arrival_ns: u64,
    ) {
        self.now_ns = arrival_ns;
        let s = Arc::clone(&self.stats);
        if self.cipher.is_none() {
            s.datagrams.fetch_add(1, Relaxed);
            s.bytes.fetch_add(payload.len() as u64, Relaxed);
        }
        if payload.len() > MAX_OPUS_PACKET {
            s.malformed.fetch_add(1, Relaxed);
            return;
        }
        struct Header {
            sequence: u16,
            timestamp: u32,
            ssrc: u32,
        }
        let header = Header {
            sequence,
            timestamp,
            ssrc,
        };

        match self.ssrc {
            Some(current) if current == header.ssrc => {}
            Some(_) => {
                let idle = self.last_arrival_ns.is_none_or(|t| {
                    arrival_ns.saturating_sub(t) >= self.config.ssrc_switch_idle_ms * 1_000_000
                });
                if !idle {
                    s.foreign_ssrc.fetch_add(1, Relaxed);
                    return;
                }
                self.start_stream(header.ssrc);
            }
            None => self.start_stream(header.ssrc),
        }

        if let Some(last) = self.last_arrival_ns {
            s.interarrival.record(arrival_ns.saturating_sub(last));
        }
        self.last_arrival_ns = Some(arrival_ns);
        self.update_jitter(arrival_ns, header.timestamp);

        let (ext, arrival) = self.tracker.on_packet(header.sequence);
        self.publish_sequence();
        match arrival {
            Arrival::Duplicate => return,
            Arrival::Reset => {
                // Same SSRC, sequence jumped: a restarted sender.
                self.clear_slots();
                let _ = self.decoder.reset();
                self.next_release = Some(ext);
                self.highest_accepted = None;
                self.control.new_stream();
                s.stream_starts.fetch_add(1, Relaxed);
            }
            _ => {}
        }
        let next = *self.next_release.get_or_insert(ext);
        if ext < next {
            s.late_packets.fetch_add(1, Relaxed);
            self.control.note_late_packet();
            return;
        }
        // Buffered media time grows when a new highest packet arrives
        // (including any gap before it, which will be concealed).
        let step = match self.highest_accepted {
            Some(h) if ext > h => ext - h,
            Some(_) => 0,
            None => 1,
        };
        if step > 0 {
            self.highest_accepted = Some(ext);
            let frames = (step.min(SLOTS as u64) as usize) * self.frame_samples;
            self.control.frame_written(arrival_ns, frames);
        }
        if ext - next >= SLOTS as u64 {
            // Far ahead of what we can hold: an outage or a big burst loss.
            self.release_all_held();
            self.skip_gap_to(ext);
        }
        let slot = &mut self.slots[(ext % SLOTS as u64) as usize];
        if slot.occupied && slot.ext == ext {
            return;
        }
        slot.ext = ext;
        slot.len = payload.len();
        slot.occupied = true;
        slot.data[..payload.len()].copy_from_slice(payload);
        self.held += 1;
        self.release(false);
        self.publish_pending();
    }

    /// Call periodically (e.g. on receive timeout) so held packets are not
    /// stuck when the sender pauses right after a loss.
    pub fn on_tick(&mut self, now_ns: u64) {
        self.now_ns = now_ns;
        if self.held > 0
            && self.last_arrival_ns.is_some_and(|t| {
                now_ns.saturating_sub(t) >= self.config.hold_timeout_ms * 1_000_000
            })
        {
            self.release(true);
        } else if self.held > 0 {
            self.release(false);
        }
        self.publish_pending();
    }

    /// Frames between the next packet to release and the highest one held:
    /// audio that has arrived (or will be concealed) but is not yet in the
    /// playout ring.
    fn publish_pending(&self) {
        let pending = match (self.next_release, self.highest_accepted) {
            (Some(next), Some(high)) if self.held > 0 && high >= next => {
                (high - next + 1) as usize * self.frame_samples
            }
            _ => 0,
        };
        self.control.set_pending_frames(pending);
    }

    fn start_stream(&mut self, ssrc: u32) {
        self.ssrc = Some(ssrc);
        self.tracker = SequenceTracker::new();
        self.clear_slots();
        self.next_release = None;
        self.highest_accepted = None;
        self.prev_transit = None;
        self.jitter = 0.0;
        let _ = self.decoder.reset();
        self.control.new_stream();
        self.stats.stream_starts.fetch_add(1, Relaxed);
    }

    fn clear_slots(&mut self) {
        for slot in self.slots.iter_mut() {
            slot.occupied = false;
        }
        self.held = 0;
    }

    fn slot_index(ext: u64) -> usize {
        (ext % SLOTS as u64) as usize
    }

    fn has(&self, ext: u64) -> bool {
        let slot = &self.slots[Self::slot_index(ext)];
        slot.occupied && slot.ext == ext
    }

    /// Releases packets in order; conceals or skips gaps once they are
    /// judged lost (or unconditionally when `force`).
    fn release(&mut self, force: bool) {
        let Some(mut next) = self.next_release else {
            return;
        };
        let highest = self.tracker.highest().unwrap_or(next);
        loop {
            if self.has(next) {
                self.decode_slot(next);
                next += 1;
                continue;
            }
            if self.held == 0 {
                break;
            }
            // Wait for `next` while the render side has decoded audio to
            // play; give up when it runs low or the slots are nearly full.
            let ring_low = self.producer.depth_frames() < self.conceal_below_frames();
            let slots_full = highest >= next + SLOTS as u64 - 2;
            if !(ring_low || slots_full || force) {
                break;
            }
            // `next` is lost. How long is the missing run?
            let mut run = 1;
            while run <= self.config.max_conceal_frames
                && !self.has(next + run)
                && next + run <= highest
            {
                run += 1;
            }
            if run > self.config.max_conceal_frames {
                self.stats.outage_resyncs.fetch_add(1, Relaxed);
                next = (next..=highest)
                    .find(|e| self.has(*e))
                    .unwrap_or(highest + 1);
                continue;
            }
            self.conceal_one(next);
            next += 1;
        }
        self.next_release = Some(next);
    }

    fn conceal_below_frames(&self) -> usize {
        (self.config.conceal_below_ms * u64::from(self.config.format.sample_rate_hz) / 1000)
            as usize
    }

    fn release_all_held(&mut self) {
        self.release(true);
    }

    fn skip_gap_to(&mut self, ext: u64) {
        let next = self.next_release.unwrap_or(ext);
        if ext.saturating_sub(next) > self.config.max_conceal_frames {
            self.stats.outage_resyncs.fetch_add(1, Relaxed);
            self.next_release = Some(ext);
        }
    }

    fn decode_slot(&mut self, ext: u64) {
        let i = Self::slot_index(ext);
        let len = self.slots[i].len;
        let start = Instant::now();
        let result = self
            .decoder
            .decode(&self.slots[i].data[..len], &mut self.pcm);
        self.stats.decode.record(start.elapsed().as_nanos() as u64);
        self.slots[i].occupied = false;
        self.held -= 1;
        match result {
            Ok(n) => {
                let ch = self.decoder.channels();
                self.frame_samples = n.max(1);
                self.producer.write(&self.pcm[..n * ch]);
            }
            Err(_) => {
                self.stats.decode_errors.fetch_add(1, Relaxed);
                self.conceal_frame_only();
            }
        }
    }

    fn conceal_one(&mut self, lost: u64) {
        let next = lost + 1;
        let result = if self.has(next) {
            let i = Self::slot_index(next);
            let len = self.slots[i].len;
            self.stats.fec_attempts.fetch_add(1, Relaxed);
            self.decoder.decode_fec_samples(
                &self.slots[i].data[..len],
                &mut self.pcm,
                self.frame_samples,
            )
        } else {
            self.decoder
                .conceal_samples(&mut self.pcm, self.frame_samples)
        };
        self.stats.concealed_frames.fetch_add(1, Relaxed);
        match result {
            Ok(n) => {
                let ch = self.decoder.channels();
                self.producer.write(&self.pcm[..n * ch]);
            }
            Err(_) => {
                self.stats.decode_errors.fetch_add(1, Relaxed);
            }
        }
    }

    fn conceal_frame_only(&mut self) {
        if let Ok(n) = self
            .decoder
            .conceal_samples(&mut self.pcm, self.frame_samples)
        {
            let ch = self.decoder.channels();
            self.producer.write(&self.pcm[..n * ch]);
            self.stats.concealed_frames.fetch_add(1, Relaxed);
        }
    }

    /// RFC 3550 §6.4.1 interarrival jitter.
    fn update_jitter(&mut self, arrival_ns: u64, rtp_ts: u32) {
        let rate = f64::from(self.config.format.sample_rate_hz);
        let arrival_units = arrival_ns as f64 * rate / 1e9;
        let transit = arrival_units - f64::from(rtp_ts);
        if let Some(prev) = self.prev_transit {
            let mut d = (transit - prev).abs();
            // RTP timestamp wrap, or a sender pause (idle loopback sends
            // nothing): a timing discontinuity, not jitter.
            if d > rate {
                d = 0.0;
            }
            self.jitter += (d - self.jitter) / 16.0;
            self.stats.jitter_ms.store(self.jitter * 1000.0 / rate);
        }
        self.prev_transit = Some(transit);
    }

    fn publish_sequence(&self) {
        let t = self.tracker.stats();
        let s = &self.stats;
        s.seq_received.store(t.received, Relaxed);
        s.seq_expected.store(t.expected, Relaxed);
        s.seq_reordered.store(t.reordered, Relaxed);
        s.seq_duplicates.store(t.duplicates, Relaxed);
        s.seq_resets.store(t.resets, Relaxed);
    }
}
