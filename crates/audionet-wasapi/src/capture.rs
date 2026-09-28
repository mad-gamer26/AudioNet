//! Event-driven WASAPI capture (input endpoints and output loopback).
//!
//! ```text
//! control thread                     capture thread ("audionet-capture")
//! ──────────────                     ─────────────────────────────────
//! CaptureStream::start ──spawn──►    COM (MTA) → open device → IAudioClient
//!                                    → Initialize(shared, event[, loopback])
//!          ◄── ready: info, consumer ─ allocate ring + scratch, Start()
//!                                    loop:
//!                                      wait(event, 100 ms)       ← only blocking point
//!                                      drain packets → f32 → ring (drop if full)
//!                                      update atomics
//! CaptureStream::stop ─ stop flag ─► exits within one wait timeout
//! ```
//!
//! The "callback" is the body of the loop after each wakeup. It does not
//! allocate, lock, log, or do I/O other than the WASAPI buffer calls
//! themselves. Errors end the loop and are returned through the thread's
//! join handle, so a device disconnect surfaces as a typed
//! [`StreamError`] rather than a hang.

use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use audionet_audio::capture::{
    CaptureCounters, CaptureDiagnostics, CaptureMode, CaptureStreamInfo, MmcssStatus, StreamError,
    StreamErrorKind, StreamKind,
};
use audionet_audio::pcm::PcmLayout;
use audionet_audio::ring::{RingConsumer, RingProducer, audio_ring};
use audionet_audio::timing::{CallbackTimingRecorder, CallbackTimingStats};
use audionet_protocol::{AudioBackend, DeviceFormat, Direction, EndpointId};
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY, AUDCLNT_BUFFERFLAGS_SILENT,
    AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR, AUDCLNT_E_DEVICE_IN_USE, AUDCLNT_E_DEVICE_INVALIDATED,
    AUDCLNT_E_RESOURCES_INVALIDATED, AUDCLNT_E_SERVICE_NOT_RUNNING, AUDCLNT_E_UNSUPPORTED_FORMAT,
    AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_LOOPBACK,
    DEVICE_STATE_ACTIVE, IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator,
    MMDeviceEnumerator, WAVEFORMATEX,
};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance, CoTaskMemFree};
use windows::Win32::System::Threading::{
    AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW, CreateEventW,
    WaitForSingleObject,
};
use windows::core::{HRESULT, HSTRING, w};

use crate::com::ComApartment;
use crate::enumerate::{data_flow, endpoint_state, hresult_text};
use crate::waveformat::parse_waveformat;

/// Requested WASAPI buffer, in 100 ns units (100 ms).
///
/// This is slack for a late wakeup, not added latency: each wakeup drains
/// everything available, so in steady state the buffer holds at most about
/// one device period. A larger buffer only matters when the thread is late,
/// and then it prevents the OS from discarding audio.
const OS_BUFFER_HNS: i64 = 1_000_000;

/// Upper bound on one wait for input capture. Bounds stop latency.
const WAIT_TIMEOUT_MS: u32 = 100;

/// Loopback delivers no packets at all while the endpoint plays nothing
/// (measured: 0 packets in 4 s). After this long without a packet the
/// capture thread writes silence for the elapsed time instead, so senders
/// keep a continuous stream and receivers do not run dry and re-buffer.
/// Above the worst capture interval measured under full CPU load (15.2 ms),
/// so a late but active stream is never padded.
const IDLE_FILL_AFTER: Duration = Duration::from_millis(25);

/// `HRESULT_FROM_WIN32(ERROR_NOT_FOUND)`, returned by `GetDevice` for an
/// unknown endpoint ID.
const E_NOTFOUND: HRESULT = HRESULT(0x8007_0490_u32 as i32);

/// Parameters for opening a capture stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureConfig {
    pub endpoint: EndpointId,
    pub mode: CaptureMode,
    /// Capacity of the ring between the capture thread and the consumer.
    pub ring_capacity_ms: u32,
    /// Register the capture thread with MMCSS ("Pro Audio" task).
    pub mmcss: bool,
}

/// A running capture stream. Dropping it stops the capture thread.
#[derive(Debug)]
pub struct CaptureStream {
    info: CaptureStreamInfo,
    diagnostics: Arc<CaptureDiagnostics>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<Result<(), StreamError>>>,
}

struct Ready {
    info: CaptureStreamInfo,
    diagnostics: Arc<CaptureDiagnostics>,
    consumer: RingConsumer,
}

impl CaptureStream {
    /// Opens and starts a capture stream. Blocks until the device is
    /// running or has failed to open. Control-path only.
    pub fn start(config: CaptureConfig) -> Result<(CaptureStream, RingConsumer), StreamError> {
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let thread = thread::Builder::new()
            .name("audionet-capture".into())
            .spawn(move || capture_thread(config, thread_stop, ready_tx))
            .map_err(|e| error(StreamErrorKind::Other, "starting the capture thread", e))?;

        match ready_rx.recv() {
            Ok(ready) => Ok((
                CaptureStream {
                    info: ready.info,
                    diagnostics: ready.diagnostics,
                    stop,
                    thread: Some(thread),
                },
                ready.consumer,
            )),
            // The thread exited before becoming ready: return its error.
            Err(_) => Err(join(thread).err().unwrap_or_else(|| {
                error(
                    StreamErrorKind::Other,
                    "starting the capture thread",
                    "thread exited without reporting",
                )
            })),
        }
    }

    pub fn info(&self) -> &CaptureStreamInfo {
        &self.info
    }

    pub fn diagnostics(&self) -> &Arc<CaptureDiagnostics> {
        &self.diagnostics
    }

    /// If the capture thread has stopped on its own (normally because of an
    /// error such as a device disconnect), returns why. Non-blocking.
    pub fn poll_finished(&mut self) -> Option<Result<(), StreamError>> {
        if self.thread.as_ref().is_some_and(|t| t.is_finished()) {
            self.thread.take().map(join)
        } else {
            None
        }
    }

    /// Stops capture and returns any error the thread hit.
    pub fn stop(mut self) -> Result<(), StreamError> {
        self.stop.store(true, Relaxed);
        self.thread.take().map_or(Ok(()), join)
    }
}

impl Drop for CaptureStream {
    fn drop(&mut self) {
        self.stop.store(true, Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = join(thread);
        }
    }
}

fn join(thread: JoinHandle<Result<(), StreamError>>) -> Result<(), StreamError> {
    thread.join().unwrap_or_else(|_| {
        Err(error(
            StreamErrorKind::Other,
            "running the capture thread",
            "the capture thread panicked",
        ))
    })
}

fn error(kind: StreamErrorKind, operation: &str, detail: impl ToString) -> StreamError {
    StreamError {
        backend: AudioBackend::Wasapi,
        stream: StreamKind::Capture,
        kind,
        operation: operation.into(),
        detail: detail.to_string(),
    }
}

pub(crate) fn classify(code: HRESULT) -> StreamErrorKind {
    match code {
        AUDCLNT_E_DEVICE_INVALIDATED | AUDCLNT_E_RESOURCES_INVALIDATED => {
            StreamErrorKind::DeviceDisconnected
        }
        AUDCLNT_E_DEVICE_IN_USE => StreamErrorKind::DeviceInUse,
        AUDCLNT_E_SERVICE_NOT_RUNNING => StreamErrorKind::AudioServiceUnavailable,
        AUDCLNT_E_UNSUPPORTED_FORMAT => StreamErrorKind::UnsupportedFormat,
        E_NOTFOUND => StreamErrorKind::DeviceNotFound,
        _ => StreamErrorKind::Other,
    }
}

fn os_error(operation: &str) -> impl Fn(windows::core::Error) -> StreamError + '_ {
    move |e| error(classify(e.code()), operation, hresult_text(&e))
}

// ─── capture thread ─────────────────────────────────────────────────────────

fn capture_thread(
    config: CaptureConfig,
    stop: Arc<AtomicBool>,
    ready_tx: mpsc::SyncSender<Ready>,
) -> Result<(), StreamError> {
    let _com = ComApartment::enter().map_err(os_error("initializing COM"))?;
    let mmcss = Mmcss::enter(config.mmcss);
    // All COM interfaces live inside `run`, so they are released before
    // `_com` uninitializes COM.
    let result = run(&config, &stop, ready_tx, mmcss.status());
    drop(mmcss);
    result
}

fn run(
    config: &CaptureConfig,
    stop: &AtomicBool,
    ready_tx: mpsc::SyncSender<Ready>,
    mmcss: MmcssStatus,
) -> Result<(), StreamError> {
    let opened = open(config)?;
    let channels = usize::from(opened.format.channels);
    let rate = opened.format.sample_rate_hz;

    // Allocation happens here, before the real-time loop starts.
    let ring_frames =
        (u64::from(rate) * u64::from(config.ring_capacity_ms.max(1)) / 1000).max(1) as usize;
    let (producer, consumer) = audio_ring(ring_frames, channels);
    let (timing, recorder) = CallbackTimingStats::new(opened.period_ns);
    let diagnostics = Arc::new(CaptureDiagnostics {
        counters: CaptureCounters::default(),
        timing,
        ring: Arc::clone(producer.stats()),
    });
    let scratch = vec![0.0f32; opened.buffer_frames as usize * channels];

    // SAFETY: `client` is an initialized IAudioClient.
    unsafe { opened.client.Start() }.map_err(os_error("starting the audio stream"))?;

    let info = CaptureStreamInfo {
        endpoint: config.endpoint.clone(),
        mode: config.mode,
        format: opened.format,
        device_period_ns: opened.period_ns,
        os_buffer_frames: opened.buffer_frames,
        ring_capacity_frames: ring_frames as u64,
        mmcss,
    };
    if ready_tx
        .send(Ready {
            info,
            diagnostics: Arc::clone(&diagnostics),
            consumer,
        })
        .is_err()
    {
        // The caller is gone; nothing to capture for.
        return Ok(());
    }

    let mut rt = RealtimeState {
        capture: &opened.capture,
        event: opened.event.0,
        layout: opened.layout,
        channels,
        block_align: channels * opened.layout.bytes_per_sample(),
        producer,
        recorder,
        counters: &diagnostics.counters,
        scratch,
        idle_fill: (config.mode == CaptureMode::Loopback).then(|| IdleFill {
            rate: u64::from(rate),
            // About one device period, so filled silence is as smooth as
            // real packets.
            wait_ms: opened
                .period_ns
                .div_ceil(1_000_000)
                .clamp(1, u64::from(WAIT_TIMEOUT_MS)) as u32,
            last_audio: Instant::now(),
            filled: 0,
        }),
    };
    let result = rt.run_loop(stop);
    // SAFETY: `client` is a started IAudioClient; stopping is always valid.
    // A failure here (e.g. device already gone) changes nothing.
    let _ = unsafe { opened.client.Stop() };
    result
}

/// Owns a Win32 event handle.
struct OwnedEvent(HANDLE);

impl Drop for OwnedEvent {
    fn drop(&mut self) {
        // SAFETY: the handle came from CreateEventW and is closed only here.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

struct Opened {
    // Field order is drop order: the capture service and the audio client
    // are released before the event handle they signal is closed.
    capture: IAudioCaptureClient,
    client: IAudioClient,
    event: OwnedEvent,
    format: DeviceFormat,
    layout: PcmLayout,
    period_ns: u64,
    buffer_frames: u32,
}

/// Opens and initializes the endpoint. Control-path code: runs on the
/// capture thread before the real-time loop begins.
fn open(config: &CaptureConfig) -> Result<Opened, StreamError> {
    // SAFETY: COM is initialized on this thread (see `capture_thread`).
    let enumerator: IMMDeviceEnumerator =
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
            .map_err(os_error("creating the audio device enumerator"))?;
    let id = HSTRING::from(config.endpoint.native_id());
    // SAFETY: `enumerator` is valid; `id` is a NUL-terminated wide string
    // that outlives the call.
    let device = unsafe { enumerator.GetDevice(&id) }.map_err(os_error("finding the endpoint"))?;

    let direction = data_flow(&device).map_err(|detail| {
        error(
            StreamErrorKind::Other,
            "reading the endpoint direction",
            detail,
        )
    })?;
    let (expected, wrong) = match config.mode {
        CaptureMode::Input => (Direction::Input, "input capture needs an input endpoint"),
        CaptureMode::Loopback => (
            Direction::Output,
            "loopback capture needs an output endpoint",
        ),
    };
    if direction != expected {
        return Err(error(
            StreamErrorKind::WrongDirection,
            "checking the endpoint direction",
            wrong,
        ));
    }

    // SAFETY: `device` is valid.
    let state = unsafe { device.GetState() }.map_err(os_error("reading the endpoint state"))?;
    if state != DEVICE_STATE_ACTIVE {
        let state_text = endpoint_state(state).map_or_else(
            || format!("in unknown state 0x{:X}", state.0),
            |s| s.label().to_lowercase(),
        );
        return Err(error(
            StreamErrorKind::DeviceDisconnected,
            "checking the endpoint state",
            format!("the endpoint is {state_text}, not active"),
        ));
    }

    // SAFETY: `device` is valid; activating IAudioClient with no parameters
    // is the documented usage.
    let client: IAudioClient = unsafe { device.Activate(CLSCTX_ALL, None) }
        .map_err(os_error("activating the audio client"))?;

    let mix = MixFormat::get(&client)?;
    let format = parse_waveformat(mix.bytes()).map_err(|e| {
        error(
            StreamErrorKind::UnsupportedFormat,
            "reading the mix format",
            e,
        )
    })?;
    let layout = PcmLayout::from_format(&format).map_err(|e| {
        error(
            StreamErrorKind::UnsupportedFormat,
            "choosing a sample converter",
            e,
        )
    })?;

    let mut flags = AUDCLNT_STREAMFLAGS_EVENTCALLBACK;
    if config.mode == CaptureMode::Loopback {
        flags |= AUDCLNT_STREAMFLAGS_LOOPBACK;
    }
    // SAFETY: `mix.ptr` is the valid WAVEFORMATEX returned by GetMixFormat,
    // alive until `mix` drops at the end of this function. Shared mode
    // requires periodicity 0.
    unsafe {
        client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            flags,
            OS_BUFFER_HNS,
            0,
            mix.ptr,
            None,
        )
    }
    .map_err(os_error("initializing the audio client"))?;

    let mut period_hns = 0i64;
    // SAFETY: `client` is initialized; the out pointer is valid.
    unsafe { client.GetDevicePeriod(Some(&mut period_hns), None) }
        .map_err(os_error("reading the device period"))?;
    // SAFETY: `client` is initialized.
    let buffer_frames =
        unsafe { client.GetBufferSize() }.map_err(os_error("reading the buffer size"))?;

    // SAFETY: an unnamed auto-reset event with default security.
    let event = OwnedEvent(
        unsafe { CreateEventW(None, false, false, None) }
            .map_err(os_error("creating the audio event"))?,
    );
    // SAFETY: `client` was initialized with EVENTCALLBACK; the handle stays
    // open for the client's lifetime (see `Opened` drop order).
    unsafe { client.SetEventHandle(event.0) }.map_err(os_error("registering the audio event"))?;
    // SAFETY: `client` is initialized.
    let capture: IAudioCaptureClient =
        unsafe { client.GetService() }.map_err(os_error("getting the capture service"))?;

    Ok(Opened {
        capture,
        event,
        client,
        format,
        layout,
        period_ns: (period_hns.max(0) as u64) * 100,
        buffer_frames,
    })
}

/// The mix format returned by `GetMixFormat`, freed on drop.
pub(crate) struct MixFormat {
    pub(crate) ptr: *const WAVEFORMATEX,
}

impl MixFormat {
    pub(crate) fn get(client: &IAudioClient) -> Result<Self, StreamError> {
        // SAFETY: `client` is valid. The result is CoTaskMemAlloc'd and
        // owned by the returned value.
        let ptr = unsafe { client.GetMixFormat() }.map_err(os_error("reading the mix format"))?;
        Ok(Self { ptr })
    }

    pub(crate) fn bytes(&self) -> &[u8] {
        // SAFETY: `ptr` is a valid WAVEFORMATEX (packed, so read unaligned),
        // followed by `cbSize` extension bytes in the same allocation.
        let cb_size = unsafe { core::ptr::addr_of!((*self.ptr).cbSize).read_unaligned() };
        let len = size_of::<WAVEFORMATEX>() + usize::from(cb_size);
        // SAFETY: the allocation is at least `len` bytes, as just computed,
        // and lives as long as `self`.
        unsafe { core::slice::from_raw_parts(self.ptr.cast::<u8>(), len) }
    }
}

impl Drop for MixFormat {
    fn drop(&mut self) {
        // SAFETY: `ptr` came from GetMixFormat and is freed only here.
        unsafe { CoTaskMemFree(Some(self.ptr.cast())) };
    }
}

/// MMCSS registration for the current thread, reverted on drop.
pub(crate) struct Mmcss {
    handle: Option<HANDLE>,
    status: MmcssStatus,
}

/// Standard MMCSS task classes AudioNet uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MmcssTask {
    /// Device capture and render threads.
    ProAudio,
    /// Audio-related worker threads (encoder, network, WebRTC sessions).
    Audio,
}

impl Mmcss {
    pub(crate) fn enter(requested: bool) -> Self {
        Self::enter_task(requested, MmcssTask::ProAudio)
    }

    pub(crate) fn enter_task(requested: bool, task: MmcssTask) -> Self {
        if !requested {
            return Self {
                handle: None,
                status: MmcssStatus::NotRequested,
            };
        }
        let name = match task {
            MmcssTask::ProAudio => w!("Pro Audio"),
            MmcssTask::Audio => w!("Audio"),
        };
        let mut task_index = 0u32;
        // SAFETY: both names are standard MMCSS task names (static wide
        // strings); the index out pointer is valid.
        match unsafe { AvSetMmThreadCharacteristicsW(name, &mut task_index) } {
            Ok(handle) => Self {
                handle: Some(handle),
                status: MmcssStatus::Registered,
            },
            Err(_) => Self {
                handle: None,
                status: MmcssStatus::Failed,
            },
        }
    }

    pub(crate) fn status(&self) -> MmcssStatus {
        self.status
    }
}

impl Drop for Mmcss {
    fn drop(&mut self) {
        if let Some(handle) = self.handle {
            // SAFETY: `handle` came from AvSetMmThreadCharacteristicsW on
            // this thread and is reverted once.
            let _ = unsafe { AvRevertMmThreadCharacteristics(handle) };
        }
    }
}

// ─── real-time loop ─────────────────────────────────────────────────────────

struct RealtimeState<'a> {
    capture: &'a IAudioCaptureClient,
    event: HANDLE,
    layout: PcmLayout,
    channels: usize,
    block_align: usize,
    producer: RingProducer,
    recorder: CallbackTimingRecorder,
    counters: &'a CaptureCounters,
    /// Preallocated conversion buffer, one OS buffer's worth of frames.
    scratch: Vec<f32>,
    /// Silence synthesis for idle loopback; `None` for input capture.
    idle_fill: Option<IdleFill>,
}

/// Clock for synthesizing silence while a loopback endpoint is idle.
struct IdleFill {
    rate: u64,
    wait_ms: u32,
    /// When the last real packet was read.
    last_audio: Instant,
    /// Frames of silence written since `last_audio`.
    filled: u64,
}

impl RealtimeState<'_> {
    fn run_loop(&mut self, stop: &AtomicBool) -> Result<(), StreamError> {
        let origin = Instant::now();
        let timeout = self
            .idle_fill
            .as_ref()
            .map_or(WAIT_TIMEOUT_MS, |f| f.wait_ms);
        if let Some(fill) = &mut self.idle_fill {
            fill.last_audio = origin;
        }
        while !stop.load(Relaxed) {
            // SAFETY: `event` is a valid event handle owned by `Opened`.
            let wait = unsafe { WaitForSingleObject(self.event, timeout) };
            let wake = Instant::now();
            if wait != WAIT_OBJECT_0 && wait != WAIT_TIMEOUT {
                return Err(error(
                    StreamErrorKind::Other,
                    "waiting for captured audio",
                    format!("wait returned 0x{:X}", wait.0),
                ));
            }
            let got_audio = self.drain()?;
            self.fill_idle(got_audio, wake);
            let work = wake.elapsed();
            if got_audio {
                self.recorder
                    .record_callback((wake - origin).as_nanos() as u64, work.as_nanos() as u64);
            } else {
                self.recorder.record_idle();
            }
        }
        Ok(())
    }

    /// Moves every available packet into the ring. Real-time path: no
    /// allocation, locks, logging or I/O. Returns whether any packet was read.
    fn drain(&mut self) -> Result<bool, StreamError> {
        let mut got_audio = false;
        loop {
            // SAFETY: `capture` is a valid capture client of a started stream.
            let next = unsafe { self.capture.GetNextPacketSize() }
                .map_err(os_error("checking for captured audio"))?;
            if next == 0 {
                return Ok(got_audio);
            }
            let mut data: *mut u8 = core::ptr::null_mut();
            let mut frames = 0u32;
            let mut flags = 0u32;
            // SAFETY: all out pointers are valid locals; device and QPC
            // positions are not requested.
            unsafe {
                self.capture
                    .GetBuffer(&mut data, &mut frames, &mut flags, None, None)
            }
            .map_err(os_error("reading captured audio"))?;

            self.store_packet(data, frames as usize, flags);

            // SAFETY: releases exactly the frames obtained by GetBuffer.
            unsafe { self.capture.ReleaseBuffer(frames) }
                .map_err(os_error("releasing captured audio"))?;
            got_audio = true;
        }
    }

    /// Loopback only: after [`IDLE_FILL_AFTER`] without packets, writes
    /// silence covering all the time since the last real packet, so the
    /// stream stays continuous at the nominal rate. Allocation-free.
    fn fill_idle(&mut self, got_audio: bool, now: Instant) {
        let Some(fill) = &mut self.idle_fill else {
            return;
        };
        if got_audio {
            fill.last_audio = now;
            fill.filled = 0;
            return;
        }
        let idle = now.saturating_duration_since(fill.last_audio);
        if idle < IDLE_FILL_AFTER {
            return;
        }
        let due = (idle.as_nanos() * u128::from(fill.rate) / 1_000_000_000) as u64;
        let mut missing = due.saturating_sub(fill.filled);
        fill.filled = due;
        self.counters.idle_fill_frames.fetch_add(missing, Relaxed);
        let max_frames = self.scratch.len() / self.channels;
        while missing > 0 {
            let n = (missing as usize).min(max_frames);
            let out = &mut self.scratch[..n * self.channels];
            out.fill(0.0);
            // Overflow is counted inside the ring, as for real audio.
            let _ = self.producer.write(out);
            missing -= n as u64;
        }
    }

    fn store_packet(&mut self, data: *const u8, frames: usize, flags: u32) {
        let c = self.counters;
        let first_packet = c.packets.fetch_add(1, Relaxed) == 0;
        c.frames_captured.fetch_add(frames as u64, Relaxed);
        if flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32 != 0 {
            if first_packet {
                c.startup_discontinuities.fetch_add(1, Relaxed);
            } else {
                c.device_discontinuities.fetch_add(1, Relaxed);
            }
        }
        if flags & AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR.0 as u32 != 0 {
            c.timestamp_errors.fetch_add(1, Relaxed);
        }
        let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || data.is_null();
        if silent {
            c.silent_packets.fetch_add(1, Relaxed);
        }

        // Process in scratch-sized chunks. A packet never exceeds the OS
        // buffer (which sized `scratch`), but this keeps the loop bounded
        // and allocation-free regardless.
        let max_frames = self.scratch.len() / self.channels;
        let mut done = 0;
        while done < frames {
            let n = (frames - done).min(max_frames);
            let out = &mut self.scratch[..n * self.channels];
            if silent {
                out.fill(0.0);
            } else {
                // SAFETY: GetBuffer returned `frames` frames of
                // `block_align` bytes at `data`, valid until ReleaseBuffer;
                // this reads frames `done..done + n` of them.
                let bytes = unsafe {
                    core::slice::from_raw_parts(
                        data.add(done * self.block_align),
                        n * self.block_align,
                    )
                };
                self.layout.convert(bytes, out);
            }
            // Overflow is counted inside the ring; the callback never waits.
            let _ = self.producer.write(out);
            done += n;
        }
    }
}

/// Keeps the current thread registered with MMCSS until dropped.
pub struct ThreadPriorityGuard {
    _inner: Mmcss,
    status: MmcssStatus,
}

impl std::fmt::Debug for ThreadPriorityGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ThreadPriorityGuard")
            .field("status", &self.status)
            .finish()
    }
}

impl ThreadPriorityGuard {
    pub fn status(&self) -> MmcssStatus {
        self.status
    }
}

/// Registers the calling thread with an MMCSS task. Call at the start of a
/// thread; registration ends when the guard drops (on the same thread).
pub fn register_thread(task: MmcssTask) -> ThreadPriorityGuard {
    let inner = Mmcss::enter_task(true, task);
    let status = inner.status();
    ThreadPriorityGuard {
        _inner: inner,
        status,
    }
}

/// A [`audionet_audio::threads::ThreadSetup`] that registers engine threads
/// with the MMCSS "Audio" task.
pub fn audio_thread_setup() -> audionet_audio::threads::ThreadSetup {
    std::sync::Arc::new(|| Box::new(register_thread(MmcssTask::Audio)) as Box<dyn std::any::Any>)
}
