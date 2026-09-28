//! WASAPI implementation of [`NodeAudio`] for Windows devices.
//!
//! Source ids are `input:<endpoint id>` and `loopback:<endpoint id>`;
//! destination ids are `output:<endpoint id>`. Capture and render threads
//! register with MMCSS "Pro Audio".

use audionet_audio::capture::CaptureMode;
use audionet_audio::render::RenderSource;
use audionet_audio::{EndpointEnumerator, EnumerationOptions};
use audionet_protocol::signal::{DestinationInfo, SourceInfo, SourceType};
use audionet_protocol::{AudioBackend, DefaultRole, Direction, EndpointId, EndpointState};
use audionet_wasapi::WasapiEnumerator;
use audionet_wasapi::capture::{CaptureConfig, CaptureStream};
use audionet_wasapi::render::{RenderConfig, RenderStream};

use crate::audio::{NodeAudio, OpenCapture, StreamGuard};

/// WASAPI-backed audio for the node agent. Source ids are
/// `input:<endpoint id>` and `loopback:<endpoint id>`; destination ids are
/// `output:<endpoint id>`.
#[derive(Debug, Default)]
pub struct WasapiNodeAudio;

struct CaptureGuard(CaptureStream);

impl StreamGuard for CaptureGuard {
    fn failure(&mut self) -> Option<String> {
        match self.0.poll_finished() {
            Some(Err(e)) => Some(e.to_string()),
            Some(Ok(())) => Some("capture stopped".into()),
            None => None,
        }
    }
}

struct RenderGuard(RenderStream);

impl StreamGuard for RenderGuard {
    fn failure(&mut self) -> Option<String> {
        match self.0.poll_finished() {
            Some(Err(e)) => Some(e.to_string()),
            Some(Ok(())) => Some("playback stopped".into()),
            None => None,
        }
    }
}

fn endpoint_name(id: &EndpointId) -> String {
    WasapiEnumerator
        .enumerate(EnumerationOptions::default())
        .ok()
        .and_then(|inv| {
            inv.endpoints()
                .iter()
                .find(|e| &e.id == id)
                .map(|e| e.name.clone())
        })
        .unwrap_or_else(|| id.native_id().to_owned())
}

fn parse_id(prefixed: &str, prefix: &str) -> Result<EndpointId, String> {
    let native = prefixed
        .strip_prefix(prefix)
        .ok_or_else(|| format!("unknown audio endpoint {prefixed}"))?;
    EndpointId::new(AudioBackend::Wasapi, native).map_err(|e| e.to_string())
}

impl NodeAudio for WasapiNodeAudio {
    fn endpoints(&self) -> Result<(Vec<SourceInfo>, Vec<DestinationInfo>), String> {
        let inv = WasapiEnumerator
            .enumerate(EnumerationOptions::default())
            .map_err(|e| e.to_string())?;
        let mut sources = Vec::new();
        let mut destinations = Vec::new();
        for e in inv
            .endpoints()
            .iter()
            .filter(|e| e.state == EndpointState::Active)
        {
            let is_default = e.default_roles.contains(&DefaultRole::Console);
            match e.direction {
                Direction::Input => sources.push(SourceInfo {
                    id: format!("input:{}", e.id.native_id()),
                    name: e.name.clone(),
                    source_type: SourceType::Input,
                    is_default,
                }),
                Direction::Output => {
                    sources.push(SourceInfo {
                        id: format!("loopback:{}", e.id.native_id()),
                        name: format!("Sound playing on {}", e.name),
                        source_type: SourceType::Loopback,
                        is_default,
                    });
                    destinations.push(DestinationInfo {
                        id: format!("output:{}", e.id.native_id()),
                        name: e.name.clone(),
                        is_default,
                    });
                }
            }
        }
        Ok((sources, destinations))
    }

    fn open_source(&self, source_id: &str) -> Result<OpenCapture, String> {
        let (mode, endpoint) = if source_id.starts_with("loopback:") {
            (CaptureMode::Loopback, parse_id(source_id, "loopback:")?)
        } else {
            (CaptureMode::Input, parse_id(source_id, "input:")?)
        };
        let name = endpoint_name(&endpoint);
        let (stream, consumer) = CaptureStream::start(CaptureConfig {
            endpoint,
            mode,
            ring_capacity_ms: 500,
            mmcss: true,
        })
        .map_err(|e| e.to_string())?;
        let sample_rate = stream.info().format.sample_rate_hz;
        let description = match mode {
            CaptureMode::Loopback => format!("the sound playing on {name}"),
            CaptureMode::Input => name,
        };
        Ok(OpenCapture {
            consumer,
            sample_rate,
            guard: Box::new(CaptureGuard(stream)),
            description,
        })
    }

    fn open_destination(
        &self,
        destination_id: &str,
        source: Box<dyn RenderSource>,
    ) -> Result<(Box<dyn StreamGuard>, String), String> {
        let endpoint = parse_id(destination_id, "output:")?;
        let name = endpoint_name(&endpoint);
        let stream = RenderStream::start(
            RenderConfig {
                endpoint,
                mmcss: true,
            },
            source,
        )
        .map_err(|e| e.to_string())?;
        Ok((Box::new(RenderGuard(stream)), name))
    }
}
