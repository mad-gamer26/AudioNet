//! WASAPI implementation of [`NodeAudio`] for Windows devices.
//!
//! Source ids are `input:<endpoint id>` and `loopback:<endpoint id>`;
//! destination ids are `output:<endpoint id>`. Capture and render threads
//! register with MMCSS "Pro Audio".

use audionet_audio::capture::CaptureMode;
use audionet_audio::render::RenderSource;
use audionet_audio::{EndpointEnumerator, EnumerationOptions};
use audionet_protocol::signal::{
    ControlAction, DestinationInfo, DeviceVolume, DisabledEndpoint, EndpointControl, EndpointKind,
    SourceInfo, SourceType,
};
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

    fn capture_totals(&self) -> Option<crate::audio::CaptureTotals> {
        use std::sync::atomic::Ordering::Relaxed;
        let c = &self.0.diagnostics().counters;
        Some(crate::audio::CaptureTotals {
            captured: c.frames_captured.load(Relaxed),
            filled: c.idle_fill_frames.load(Relaxed),
            glitches: c.device_discontinuities.load(Relaxed),
        })
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

/// An active endpoint's settings for the account's device control.
fn endpoint_control(native_id: &str, is_default_communications: bool) -> Option<EndpointControl> {
    let s = audionet_wasapi::settings::settings(native_id);
    Some(EndpointControl {
        volume: s.as_ref().map(|s| s.volume),
        muted: s.as_ref().map(|s| s.muted),
        enabled: true,
        is_default_communications,
        can_set_volume: s.is_some(),
        can_mute: s.is_some(),
        can_set_default: true,
        can_set_default_communications: true,
        can_enable: true,
    })
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
            let control = endpoint_control(
                e.id.native_id(),
                e.default_roles.contains(&DefaultRole::Communications),
            );
            match e.direction {
                Direction::Input => sources.push(SourceInfo {
                    id: format!("input:{}", e.id.native_id()),
                    name: e.name.clone(),
                    source_type: SourceType::Input,
                    is_default,
                    control,
                }),
                Direction::Output => {
                    sources.push(SourceInfo {
                        id: format!("loopback:{}", e.id.native_id()),
                        name: format!("Sound playing on {}", e.name),
                        source_type: SourceType::Loopback,
                        is_default,
                        control: None,
                    });
                    destinations.push(DestinationInfo {
                        id: format!("output:{}", e.id.native_id()),
                        name: e.name.clone(),
                        is_default,
                        control,
                    });
                }
            }
        }
        Ok((sources, destinations))
    }

    fn extra_endpoints(&self) -> Result<(Vec<DisabledEndpoint>, Vec<DeviceVolume>), String> {
        let inv = WasapiEnumerator
            .enumerate(EnumerationOptions::default())
            .map_err(|e| e.to_string())?;
        let disabled = inv
            .endpoints()
            .iter()
            .filter(|e| e.state == EndpointState::Disabled)
            .map(|e| {
                let input = e.direction == Direction::Input;
                DisabledEndpoint {
                    id: format!(
                        "{}:{}",
                        if input { "input" } else { "output" },
                        e.id.native_id()
                    ),
                    name: e.name.clone(),
                    kind: if input {
                        EndpointKind::Input
                    } else {
                        EndpointKind::Output
                    },
                    control: EndpointControl {
                        volume: None,
                        muted: None,
                        enabled: false,
                        is_default_communications: false,
                        can_set_volume: false,
                        can_mute: false,
                        can_set_default: false,
                        can_set_default_communications: false,
                        can_enable: true,
                    },
                }
            })
            .collect();
        Ok((disabled, Vec::new()))
    }

    fn control(&self, action: &ControlAction) -> Result<(), String> {
        use audionet_wasapi::settings;
        let target = action.target();
        // The sound playing on an output ("loopback:") is that output's.
        let native = ["input:", "output:", "loopback:"]
            .iter()
            .find_map(|p| target.strip_prefix(p))
            .ok_or_else(|| "That isn't one of this computer's sound devices.".to_owned())?;
        match action {
            ControlAction::SetVolume { volume, .. } => settings::set_volume(native, *volume),
            ControlAction::SetMuted { muted, .. } => settings::set_muted(native, *muted),
            ControlAction::SetDefault { communications, .. } => {
                settings::set_default(native, *communications)
            }
            ControlAction::SetEnabled { enabled, .. } => settings::set_enabled(native, *enabled),
        }
    }

    fn watch_changes(&self, changed: crate::audio::ChangeWaker) {
        // One watch at a time (a newer connection's replaces it).
        static WATCH: std::sync::Mutex<Option<audionet_wasapi::settings::SettingsWatch>> =
            std::sync::Mutex::new(None);
        let watch = audionet_wasapi::settings::watch(changed);
        if let Ok(mut w) = WATCH.lock() {
            *w = watch.ok();
        }
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
