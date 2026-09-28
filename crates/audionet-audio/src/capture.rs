//! Platform-neutral capture stream types: configuration, errors, and
//! diagnostics shared between a backend's capture thread and its consumers.

use core::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

use audionet_protocol::{AudioBackend, DeviceFormat, EndpointId};
use serde::Serialize;

use crate::ring::{RingSnapshot, RingStats};
use crate::timing::{CallbackTimingStats, TimingSnapshot};

/// What to capture from an endpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureMode {
    /// Record an input endpoint (microphone, line in, virtual cable).
    Input,
    /// Capture what an output endpoint is playing.
    Loopback,
}

/// Why a capture stream failed to open or stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamErrorKind {
    /// The endpoint ID does not exist (removed, or ID changed).
    DeviceNotFound,
    /// Input capture requested on an output endpoint, or loopback on an input.
    WrongDirection,
    /// The device was removed, disabled, or reconfigured while in use.
    /// Recoverable by re-resolving the endpoint and opening a new stream.
    DeviceDisconnected,
    /// Another application holds the device exclusively.
    DeviceInUse,
    /// The OS audio service is not running.
    AudioServiceUnavailable,
    /// The device's format cannot be converted.
    UnsupportedFormat,
    /// Any other OS failure.
    Other,
}

impl StreamErrorKind {
    /// Whether waiting and reopening might succeed.
    pub fn is_recoverable(self) -> bool {
        matches!(
            self,
            StreamErrorKind::DeviceDisconnected
                | StreamErrorKind::DeviceNotFound
                | StreamErrorKind::AudioServiceUnavailable
                | StreamErrorKind::DeviceInUse
        )
    }

    pub fn summary(self) -> &'static str {
        match self {
            StreamErrorKind::DeviceNotFound => "the device was not found",
            StreamErrorKind::WrongDirection => "the device is the wrong kind for this stream",
            StreamErrorKind::DeviceDisconnected => {
                "the device was disconnected, disabled, or reconfigured"
            }
            StreamErrorKind::DeviceInUse => "the device is in exclusive use by another application",
            StreamErrorKind::AudioServiceUnavailable => "the audio service is not running",
            StreamErrorKind::UnsupportedFormat => "the device's sample format is not supported",
            StreamErrorKind::Other => "an operating system error occurred",
        }
    }
}

/// Which kind of audio stream failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamKind {
    Capture,
    Render,
}

/// An audio stream failure, with the failing subsystem and operation in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamError {
    pub backend: AudioBackend,
    pub stream: StreamKind,
    pub kind: StreamErrorKind,
    /// What was being done, e.g. "reading captured audio".
    pub operation: String,
    /// OS detail, e.g. message and HRESULT.
    pub detail: String,
}

impl fmt::Display for StreamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} audio {} failed while {}: {}. Detail: {}",
            self.backend.display_name(),
            match self.stream {
                StreamKind::Capture => "capture",
                StreamKind::Render => "playback",
            },
            self.operation,
            self.kind.summary(),
            self.detail
        )
    }
}

impl std::error::Error for StreamError {}

/// Whether the capture thread joined an MMCSS task.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MmcssStatus {
    NotRequested,
    Registered,
    /// Requested but registration failed; capture continued without it.
    Failed,
}

/// Describes an open capture stream.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CaptureStreamInfo {
    pub endpoint: EndpointId,
    pub mode: CaptureMode,
    /// The format delivered by the device (and converted to f32 in the ring).
    pub format: DeviceFormat,
    /// The device's default period: the expected wakeup interval.
    pub device_period_ns: u64,
    /// Size of the OS capture buffer, in frames.
    pub os_buffer_frames: u32,
    pub ring_capacity_frames: u64,
    pub mmcss: MmcssStatus,
}

/// Counters written by a capture thread about what the OS delivered.
#[derive(Debug, Default)]
pub struct CaptureCounters {
    pub packets: AtomicU64,
    pub frames_captured: AtomicU64,
    /// OS-reported gaps in the captured stream (WASAPI
    /// `AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY`), excluding the first packet.
    pub device_discontinuities: AtomicU64,
    /// The same flag on the first packet after the stream starts, counted
    /// separately because it describes the start, not a gap in the stream.
    pub startup_discontinuities: AtomicU64,
    /// Packets flagged silent by the OS (delivered as zeros).
    pub silent_packets: AtomicU64,
    pub timestamp_errors: AtomicU64,
    /// Frames of silence written while a loopback endpoint delivered
    /// nothing (nothing playing), keeping the stream continuous.
    pub idle_fill_frames: AtomicU64,
}

/// All diagnostics for one capture stream.
#[derive(Debug)]
pub struct CaptureDiagnostics {
    pub counters: CaptureCounters,
    pub timing: Arc<CallbackTimingStats>,
    pub ring: Arc<RingStats>,
}

/// A point-in-time copy of [`CaptureDiagnostics`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct CaptureSnapshot {
    pub packets: u64,
    pub frames_captured: u64,
    pub device_discontinuities: u64,
    pub startup_discontinuities: u64,
    pub silent_packets: u64,
    pub timestamp_errors: u64,
    pub idle_fill_frames: u64,
    pub timing: TimingSnapshot,
    pub ring: RingSnapshot,
}

impl CaptureDiagnostics {
    /// Takes a snapshot; resets per-window maxima. Use one reader.
    pub fn snapshot(&self) -> CaptureSnapshot {
        let c = &self.counters;
        CaptureSnapshot {
            packets: c.packets.load(Relaxed),
            frames_captured: c.frames_captured.load(Relaxed),
            device_discontinuities: c.device_discontinuities.load(Relaxed),
            startup_discontinuities: c.startup_discontinuities.load(Relaxed),
            silent_packets: c.silent_packets.load(Relaxed),
            timestamp_errors: c.timestamp_errors.load(Relaxed),
            idle_fill_frames: c.idle_fill_frames.load(Relaxed),
            timing: self.timing.snapshot(),
            ring: self.ring.snapshot(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_text_names_subsystem_operation_and_cause() {
        let e = StreamError {
            backend: AudioBackend::Wasapi,
            stream: StreamKind::Capture,
            kind: StreamErrorKind::DeviceDisconnected,
            operation: "reading captured audio".into(),
            detail: "HRESULT 0x88890004".into(),
        };
        assert_eq!(
            e.to_string(),
            "WASAPI audio capture failed while reading captured audio: the device was \
             disconnected, disabled, or reconfigured. Detail: HRESULT 0x88890004"
        );
        assert!(e.kind.is_recoverable());
        assert!(!StreamErrorKind::UnsupportedFormat.is_recoverable());
    }
}
