//! Event-driven WASAPI shared-mode render.
//!
//! Mirrors `capture.rs`: a dedicated thread opens the endpoint, asks the
//! [`RenderSource`] to prepare for the device format, prefills the buffer
//! with silence, starts the stream and then loops:
//!
//! wait(event, 100 ms) → `GetCurrentPadding` → `GetBuffer(free)` →
//! `source.render` into a preallocated f32 scratch buffer → convert to the
//! device format → `ReleaseBuffer`.
//!
//! The source always fills the whole request, so this loop never waits for
//! audio. Errors (device removed, format changed) end the loop and are
//! returned as a typed [`StreamError`].

use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Instant;

use audionet_audio::capture::{MmcssStatus, StreamError, StreamErrorKind, StreamKind};
use audionet_audio::clock;
use audionet_audio::pcm::PcmLayout;
use audionet_audio::render::{RenderSource, RenderStreamInfo};
use audionet_audio::timing::{CallbackTimingRecorder, CallbackTimingStats};
use audionet_protocol::{AudioBackend, Direction, EndpointId};
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
    DEVICE_STATE_ACTIVE, IAudioClient, IAudioRenderClient, IMMDeviceEnumerator, MMDeviceEnumerator,
};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows::core::HSTRING;

use crate::capture::{MixFormat, Mmcss, classify};
use crate::com::ComApartment;
use crate::enumerate::{data_flow, endpoint_state, hresult_text};
use crate::waveformat::parse_waveformat;

/// Requested render buffer (20 ms, two default periods). Unlike capture,
/// the render buffer *is* output latency, so it is kept small; underrun
/// safety comes from the playout buffer upstream.
const OS_BUFFER_HNS: i64 = 200_000;
const WAIT_TIMEOUT_MS: u32 = 100;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderConfig {
    pub endpoint: EndpointId,
    pub mmcss: bool,
}

/// A running render stream. Dropping it stops playback.
#[derive(Debug)]
pub struct RenderStream {
    info: RenderStreamInfo,
    timing: Arc<CallbackTimingStats>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<Result<(), StreamError>>>,
}

struct Ready {
    info: RenderStreamInfo,
    timing: Arc<CallbackTimingStats>,
}

fn error(kind: StreamErrorKind, operation: &str, detail: impl ToString) -> StreamError {
    StreamError {
        backend: AudioBackend::Wasapi,
        stream: StreamKind::Render,
        kind,
        operation: operation.into(),
        detail: detail.to_string(),
    }
}

fn os_error(operation: &str) -> impl Fn(windows::core::Error) -> StreamError + '_ {
    move |e| error(classify(e.code()), operation, hresult_text(&e))
}

impl RenderStream {
    /// Opens the endpoint, prepares `source`, and starts playback. Blocks
    /// until the device is running or has failed. Control path only.
    pub fn start(
        config: RenderConfig,
        source: Box<dyn RenderSource>,
    ) -> Result<RenderStream, StreamError> {
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let thread = thread::Builder::new()
            .name("audionet-render".into())
            .spawn(move || render_thread(config, source, thread_stop, ready_tx))
            .map_err(|e| error(StreamErrorKind::Other, "starting the render thread", e))?;
        match ready_rx.recv() {
            Ok(ready) => Ok(RenderStream {
                info: ready.info,
                timing: ready.timing,
                stop,
                thread: Some(thread),
            }),
            Err(_) => Err(join(thread).err().unwrap_or_else(|| {
                error(
                    StreamErrorKind::Other,
                    "starting the render thread",
                    "thread exited without reporting",
                )
            })),
        }
    }

    pub fn info(&self) -> &RenderStreamInfo {
        &self.info
    }

    pub fn timing(&self) -> &Arc<CallbackTimingStats> {
        &self.timing
    }

    /// If playback stopped on its own (usually a device error), returns why.
    pub fn poll_finished(&mut self) -> Option<Result<(), StreamError>> {
        if self.thread.as_ref().is_some_and(|t| t.is_finished()) {
            self.thread.take().map(join)
        } else {
            None
        }
    }

    pub fn stop(mut self) -> Result<(), StreamError> {
        self.stop.store(true, Relaxed);
        self.thread.take().map_or(Ok(()), join)
    }
}

impl Drop for RenderStream {
    fn drop(&mut self) {
        self.stop.store(true, Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = join(t);
        }
    }
}

fn join(thread: JoinHandle<Result<(), StreamError>>) -> Result<(), StreamError> {
    thread.join().unwrap_or_else(|_| {
        Err(error(
            StreamErrorKind::Other,
            "running the render thread",
            "the render thread panicked",
        ))
    })
}

struct OwnedEvent(HANDLE);

impl Drop for OwnedEvent {
    fn drop(&mut self) {
        // SAFETY: the handle came from CreateEventW and is closed only here.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

fn render_thread(
    config: RenderConfig,
    source: Box<dyn RenderSource>,
    stop: Arc<AtomicBool>,
    ready_tx: mpsc::SyncSender<Ready>,
) -> Result<(), StreamError> {
    let _com = ComApartment::enter().map_err(os_error("initializing COM"))?;
    let mmcss = Mmcss::enter(config.mmcss);
    let result = run(&config, source, &stop, ready_tx, mmcss.status());
    drop(mmcss);
    result
}

fn run(
    config: &RenderConfig,
    mut source: Box<dyn RenderSource>,
    stop: &AtomicBool,
    ready_tx: mpsc::SyncSender<Ready>,
    mmcss: MmcssStatus,
) -> Result<(), StreamError> {
    // SAFETY: COM is initialized on this thread (see `render_thread`).
    let enumerator: IMMDeviceEnumerator =
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
            .map_err(os_error("creating the audio device enumerator"))?;
    let id = HSTRING::from(config.endpoint.native_id());
    // SAFETY: `enumerator` is valid; `id` outlives the call.
    let device = unsafe { enumerator.GetDevice(&id) }.map_err(os_error("finding the endpoint"))?;
    let direction = data_flow(&device)
        .map_err(|d| error(StreamErrorKind::Other, "reading the endpoint direction", d))?;
    if direction != Direction::Output {
        return Err(error(
            StreamErrorKind::WrongDirection,
            "checking the endpoint direction",
            "playback needs an output endpoint",
        ));
    }
    // SAFETY: `device` is valid.
    let state = unsafe { device.GetState() }.map_err(os_error("reading the endpoint state"))?;
    if state != DEVICE_STATE_ACTIVE {
        let text = endpoint_state(state).map_or_else(
            || format!("in unknown state 0x{:X}", state.0),
            |s| s.label().to_lowercase(),
        );
        return Err(error(
            StreamErrorKind::DeviceDisconnected,
            "checking the endpoint state",
            format!("the endpoint is {text}, not active"),
        ));
    }
    // SAFETY: `device` is valid; plain activation.
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
            "choosing a sample writer",
            e,
        )
    })?;
    // SAFETY: `mix.ptr` is valid until `mix` drops; shared mode needs periodicity 0.
    unsafe {
        client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
            OS_BUFFER_HNS,
            0,
            mix.ptr,
            None,
        )
    }
    .map_err(os_error("initializing the audio client"))?;
    let mut period_hns = 0i64;
    // SAFETY: initialized client; valid out pointer.
    unsafe { client.GetDevicePeriod(Some(&mut period_hns), None) }
        .map_err(os_error("reading the device period"))?;
    // SAFETY: initialized client.
    let buffer_frames =
        unsafe { client.GetBufferSize() }.map_err(os_error("reading the buffer size"))?;
    // SAFETY: unnamed auto-reset event with default security.
    let event = OwnedEvent(
        unsafe { CreateEventW(None, false, false, None) }
            .map_err(os_error("creating the audio event"))?,
    );
    // SAFETY: the client was initialized with EVENTCALLBACK; `event` outlives it.
    unsafe { client.SetEventHandle(event.0) }.map_err(os_error("registering the audio event"))?;
    // SAFETY: initialized client.
    let render: IAudioRenderClient =
        unsafe { client.GetService() }.map_err(os_error("getting the render service"))?;

    let channels = usize::from(format.channels);
    source
        .prepare(format.sample_rate_hz, channels, buffer_frames as usize)
        .map_err(|e| error(StreamErrorKind::Other, "preparing the audio source", e))?;
    let scratch = vec![0.0f32; buffer_frames as usize * channels];
    let period_ns = (period_hns.max(0) as u64) * 100;
    let (timing, recorder) = CallbackTimingStats::new(period_ns);

    // Prefill with silence so the first period does not glitch.
    // SAFETY: requesting the whole (empty) buffer before Start is valid;
    // released with the SILENT flag, so its contents are not read.
    unsafe {
        render
            .GetBuffer(buffer_frames)
            .map_err(os_error("prefilling the buffer"))?;
        render
            .ReleaseBuffer(buffer_frames, AUDCLNT_BUFFERFLAGS_SILENT.0 as u32)
            .map_err(os_error("prefilling the buffer"))?;
    }
    // SAFETY: initialized client.
    unsafe { client.Start() }.map_err(os_error("starting playback"))?;

    let info = RenderStreamInfo {
        endpoint: config.endpoint.clone(),
        format,
        device_period_ns: period_ns,
        os_buffer_frames: buffer_frames,
        mmcss,
    };
    if ready_tx
        .send(Ready {
            info,
            timing: Arc::clone(&timing),
        })
        .is_err()
    {
        return Ok(());
    }

    let mut rt = RealtimeRender {
        client: &client,
        render: &render,
        event: event.0,
        layout,
        channels,
        buffer_frames,
        source,
        scratch,
        recorder,
    };
    let result = rt.run_loop(stop);
    // SAFETY: started client; stopping is always valid.
    let _ = unsafe { client.Stop() };
    drop(rt);
    drop(render);
    drop(client);
    drop(event);
    result
}

struct RealtimeRender<'a> {
    client: &'a IAudioClient,
    render: &'a IAudioRenderClient,
    event: HANDLE,
    layout: PcmLayout,
    channels: usize,
    buffer_frames: u32,
    source: Box<dyn RenderSource>,
    scratch: Vec<f32>,
    recorder: CallbackTimingRecorder,
}

impl RealtimeRender<'_> {
    fn run_loop(&mut self, stop: &AtomicBool) -> Result<(), StreamError> {
        let origin = Instant::now();
        while !stop.load(Relaxed) {
            // SAFETY: valid event handle.
            let wait = unsafe { WaitForSingleObject(self.event, WAIT_TIMEOUT_MS) };
            let wake = Instant::now();
            if wait != WAIT_OBJECT_0 && wait != WAIT_TIMEOUT {
                return Err(error(
                    StreamErrorKind::Other,
                    "waiting for the playback event",
                    format!("wait returned 0x{:X}", wait.0),
                ));
            }
            let wrote = self.fill(clock::to_ns(wake))?;
            if wrote {
                self.recorder.record_callback(
                    (wake - origin).as_nanos() as u64,
                    wake.elapsed().as_nanos() as u64,
                );
            } else {
                self.recorder.record_idle();
            }
        }
        Ok(())
    }

    /// Real-time path: fills the free part of the device buffer.
    fn fill(&mut self, now_ns: u64) -> Result<bool, StreamError> {
        // SAFETY: initialized, started client.
        let padding = unsafe { self.client.GetCurrentPadding() }
            .map_err(os_error("reading the playback position"))?;
        let frames = self.buffer_frames.saturating_sub(padding) as usize;
        if frames == 0 {
            return Ok(false);
        }
        let n = frames * self.channels;
        self.source.render(&mut self.scratch[..n], now_ns);
        // SAFETY: requesting at most the free space reported by padding.
        let data = unsafe { self.render.GetBuffer(frames as u32) }
            .map_err(os_error("getting the playback buffer"))?;
        let bytes = n * self.layout.bytes_per_sample();
        // SAFETY: GetBuffer returned a writable buffer of `frames` frames in
        // the mix format, valid until ReleaseBuffer.
        let dst = unsafe { core::slice::from_raw_parts_mut(data, bytes) };
        write_samples(self.layout, &self.scratch[..n], dst);
        // SAFETY: releases exactly the frames obtained.
        unsafe { self.render.ReleaseBuffer(frames as u32, 0) }
            .map_err(os_error("releasing the playback buffer"))?;
        Ok(true)
    }
}

/// Converts f32 samples into the device layout. Clamps to full scale.
fn write_samples(layout: PcmLayout, src: &[f32], dst: &mut [u8]) {
    match layout {
        PcmLayout::F32 => {
            for (d, s) in dst.chunks_exact_mut(4).zip(src) {
                d.copy_from_slice(&s.to_le_bytes());
            }
        }
        PcmLayout::I16 => {
            for (d, s) in dst.chunks_exact_mut(2).zip(src) {
                let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
                d.copy_from_slice(&v.to_le_bytes());
            }
        }
        PcmLayout::I24Packed => {
            for (d, s) in dst.chunks_exact_mut(3).zip(src) {
                let v = (f64::from(s.clamp(-1.0, 1.0)) * 8_388_607.0) as i32;
                d.copy_from_slice(&v.to_le_bytes()[..3]);
            }
        }
        PcmLayout::I32 => {
            for (d, s) in dst.chunks_exact_mut(4).zip(src) {
                let v = (f64::from(s.clamp(-1.0, 1.0)) * 2_147_483_647.0) as i32;
                d.copy_from_slice(&v.to_le_bytes());
            }
        }
    }
}
