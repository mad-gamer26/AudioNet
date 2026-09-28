//! Platform-neutral render (playback) stream types.

use audionet_protocol::{DeviceFormat, EndpointId};
use serde::Serialize;

use crate::capture::MmcssStatus;

/// Produces audio for a render stream.
///
/// A backend calls [`prepare`](RenderSource::prepare) once on its render
/// thread when the device format is known (control-path work: allocation is
/// allowed), then [`render`](RenderSource::render) from the real-time
/// callback, which must fill the whole buffer without blocking, allocating,
/// locking or doing I/O.
pub trait RenderSource: Send + 'static {
    /// `max_frames` is the largest `render` request the backend will make.
    fn prepare(
        &mut self,
        device_rate: u32,
        channels: usize,
        max_frames: usize,
    ) -> Result<(), String>;

    /// Fills `out` (interleaved `f32`, device channel count). `now_ns` is
    /// [`crate::clock::now_ns`] at the callback's wakeup.
    fn render(&mut self, out: &mut [f32], now_ns: u64);
}

/// Describes an open render stream.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RenderStreamInfo {
    pub endpoint: EndpointId,
    /// The device's shared-mode mix format.
    pub format: DeviceFormat,
    pub device_period_ns: u64,
    /// OS render buffer size: the device-side part of output latency.
    pub os_buffer_frames: u32,
    pub mmcss: MmcssStatus,
}
