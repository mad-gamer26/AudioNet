//! [`NodeAudio`] for macOS, Linux and other non-Windows platforms, using
//! cpal (Core Audio on macOS, PipeWire/ALSA on Linux, AAudio on Android).
//!
//! Source ids are `input:<cpal device id>`, and on macOS also
//! `loopback:<cpal device id>` for what an output device plays (system
//! audio); destination ids are `output:<cpal device id>`.
//!
//! macOS system audio: an input stream on an output device makes cpal
//! create a Core Audio process tap of every process's output to that device
//! (unmuted: the sound still plays) inside a private aggregate device
//! (macOS 14.2 or later). macOS asks once for "System Audio Recording"
//! permission; until it is given the tap delivers silence. Like Windows
//! loopback, this includes what AudioNet itself plays on that device.
//! With "mute this Mac's sound while it is streamed" on, the stream's
//! thread also holds an [`audionet_coreaudio::OutputMute`] on the device
//! (AudioNet's own muting tap; the system volume and mute are untouched).
//!
//! Real-time rules: the cpal data callbacks only write into / read from the
//! bounded rings and the playout (no allocation, locks or I/O). Each stream
//! lives on its own thread because cpal streams are not `Send` on every
//! platform; the thread builds the stream, keeps it alive, and exits when
//! its guard is dropped.

use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

use audionet_audio::clock;
use audionet_audio::render::RenderSource;
use audionet_audio::ring::audio_ring;
use audionet_protocol::signal::{DestinationInfo, SourceInfo, SourceType};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::audio::{self, FORMAT_CHANGED, NodeAudio, OpenCapture, StreamGuard, StreamUse};

#[derive(Debug, Default)]
pub struct CpalNodeAudio;

struct ThreadGuard {
    stop: Arc<AtomicBool>,
    failure: Arc<Mutex<Option<String>>>,
    thread: Option<JoinHandle<()>>,
}

impl StreamGuard for ThreadGuard {
    fn failure(&mut self) -> Option<String> {
        self.failure.lock().ok().and_then(|mut f| f.take())
    }
}

/// An open stream: it closes before its [`StreamUse`] ends (fields drop in
/// order).
struct CountedGuard {
    stream: ThreadGuard,
    _use: StreamUse,
}

impl StreamGuard for CountedGuard {
    fn failure(&mut self) -> Option<String> {
        self.stream.failure()
    }
}

impl Drop for ThreadGuard {
    fn drop(&mut self) {
        self.stop.store(true, Relaxed);
        if let Some(t) = self.thread.take() {
            t.thread().unpark();
            let _ = t.join();
        }
    }
}

fn device_name(d: &cpal::Device) -> String {
    d.description()
        .map(|desc| desc.name().to_owned())
        .unwrap_or_else(|_| "Unnamed device".into())
}

/// iOS has one "Default Device": the system routes it (built-in microphone
/// and speaker, headphones, Bluetooth). Names that say what it is, for
/// input and output.
const IOS_NAMES: (&str, &str) = ("Microphone", "Speaker, headphones or Bluetooth");

/// A stream's device, in words (on iOS, what the system routes).
fn stream_device_name(d: &cpal::Device, input: bool) -> String {
    match (cfg!(target_os = "ios"), input) {
        (true, true) => IOS_NAMES.0.to_owned(),
        (true, false) => IOS_NAMES.1.to_owned(),
        _ => device_name(d),
    }
}

fn find(id: &str, input: bool) -> Result<cpal::Device, String> {
    let host = cpal::default_host();
    let parsed: cpal::DeviceId = id
        .parse()
        .map_err(|e| format!("unknown device {id}: {e}"))?;
    let device = host
        .device_by_id(&parsed)
        .ok_or_else(|| format!("the audio device {id} is not available"))?;
    let ok = if input {
        device.supports_input()
    } else {
        device.supports_output()
    };
    if ok {
        Ok(device)
    } else {
        Err(format!(
            "the audio device {id} cannot be used for {}",
            if input { "input" } else { "output" }
        ))
    }
}

/// Handles a stream error from cpal: stores it as the stream's failure (the
/// session then reopens the stream), unless it is an iOS reroute the stream
/// survives. iOS moves a running stream to the new route by itself (a
/// microphone starting, a category change, headphones); cpal reports that
/// as "Audio route changed". Only a changed sample rate or channel count
/// needs a new stream: the ring, resampler and codec were set up for the
/// old format. Reopening for every reroute cost seconds of audio at the
/// start of each iPhone microphone stream. Called on the notification
/// thread, never an audio callback.
fn on_stream_error(
    failure: &Mutex<Option<String>>,
    e: &cpal::Error,
    format: (u32, u16),
    current: impl Fn() -> Option<(u32, u16)>,
) {
    let mut why = e.to_string();
    if cfg!(target_os = "ios") {
        match e.kind() {
            cpal::ErrorKind::DeviceChanged => return,
            cpal::ErrorKind::StreamInvalidated => match current() {
                Some(now) if now == format => {
                    tracing::info!("iOS audio route changed; the stream continues ({e})");
                    return;
                }
                Some((rate, channels)) => {
                    why = format!(
                        "{FORMAT_CHANGED} ({} Hz, {} channels to {rate} Hz, {channels} channels)",
                        format.0, format.1
                    );
                }
                None => {}
            },
            _ => {}
        }
    }
    if let Ok(mut f) = failure.lock() {
        *f = Some(why);
    }
}

/// Runs `build` on a dedicated thread that owns the stream until `stop`.
/// Returns the build result (sample rate and description) or its error.
/// `tick` runs on that thread once the stream plays and then every 200 ms
/// until `stop` (never in an audio callback); it is dropped just before the
/// stream.
fn spawn_stream(
    name: &str,
    mut tick: impl FnMut() + Send + 'static,
    build: impl FnOnce(Arc<Mutex<Option<String>>>) -> Result<(cpal::Stream, u32, String), String>
    + Send
    + 'static,
) -> Result<(ThreadGuard, u32, String), String> {
    let stop = Arc::new(AtomicBool::new(false));
    let failure = Arc::new(Mutex::new(None));
    let (tx, rx) = mpsc::sync_channel(1);
    let thread_stop = Arc::clone(&stop);
    let thread_failure = Arc::clone(&failure);
    let thread = std::thread::Builder::new()
        .name(name.into())
        .spawn(move || match build(thread_failure) {
            Ok((stream, rate, desc)) => {
                if let Err(e) = stream.play() {
                    let _ = tx.send(Err(format!("could not start the audio stream: {e}")));
                    return;
                }
                let _ = tx.send(Ok((rate, desc)));
                while !thread_stop.load(Relaxed) {
                    tick();
                    std::thread::park_timeout(Duration::from_millis(200));
                }
                drop(tick);
                drop(stream);
            }
            Err(e) => {
                let _ = tx.send(Err(e));
            }
        })
        .map_err(|e| format!("could not start an audio thread: {e}"))?;
    match rx.recv() {
        Ok(Ok((rate, desc))) => Ok((
            ThreadGuard {
                stop,
                failure,
                thread: Some(thread),
            },
            rate,
            desc,
        )),
        Ok(Err(e)) => {
            let _ = thread.join();
            Err(e)
        }
        Err(_) => Err("the audio thread stopped unexpectedly".into()),
    }
}

/// For a stream of what the output `device_id` plays: keeps that output
/// muted (AudioNet's own mute) while [`audio::mute_streamed_output`] is on.
/// Runs as the stream thread's tick, so turning the setting on or off takes
/// effect within 200 ms; the mute ends when the stream does.
#[cfg(target_os = "macos")]
fn output_mute(device_id: String) -> Box<dyn FnMut() + Send> {
    use audionet_coreaudio::OutputMute;
    // A cpal device id is "coreaudio:<Core Audio UID>".
    let device_uid = device_id
        .split_once(':')
        .map_or(device_id.clone(), |(_, uid)| uid.to_owned());
    let name = find(&device_id, false)
        .map(|d| device_name(&d))
        .unwrap_or_else(|_| device_uid.clone());
    let report = audio::OutputMuteReport::new();
    let mut mute: Option<OutputMute> = None;
    // After a failure, tried again only once the setting is turned off and
    // on (not every 200 ms).
    let mut failed = false;
    Box::new(move || {
        if !audio::mute_streamed_output() {
            if mute.take().is_some() {
                tracing::info!("{name} plays its own sound again (muting is off)");
            }
            failed = false;
            return;
        }
        // AudioNet began using audio since the mute was made: make it
        // again, leaving AudioNet's own sound out (the new one first, so
        // nothing leaks in between).
        let renew = mute.as_ref().is_some_and(|m| m.own_sound_changed());
        if (mute.is_none() || renew) && !failed {
            match OutputMute::new(&device_uid) {
                Ok(m) => {
                    mute = Some(m);
                    report.set(format!("Muting {name} while it is streamed."));
                    tracing::info!("{name} is muted while its sound is streamed");
                }
                Err(e) => {
                    report.set(format!("Could not mute {name}: {e}."));
                    tracing::warn!("could not mute {name} while streaming: {e}");
                    failed = true;
                }
            }
        }
    })
}

#[cfg(not(target_os = "macos"))]
fn output_mute(_device_id: String) -> Box<dyn FnMut() + Send> {
    Box::new(|| {})
}

impl NodeAudio for CpalNodeAudio {
    fn endpoints(&self) -> Result<(Vec<SourceInfo>, Vec<DestinationInfo>), String> {
        let host = cpal::default_host();
        let default_in = host.default_input_device().and_then(|d| d.id().ok());
        let default_out = host.default_output_device().and_then(|d| d.id().ok());
        let mut sources = Vec::new();
        let mut destinations = Vec::new();
        let devices = host
            .devices()
            .map_err(|e| format!("could not list audio devices: {e}"))?;
        for d in devices {
            let Ok(id) = d.id() else { continue };
            let name = device_name(&d);
            let (input_name, output_name) = if cfg!(target_os = "ios") {
                (IOS_NAMES.0.to_owned(), IOS_NAMES.1.to_owned())
            } else {
                (name.clone(), name.clone())
            };
            if d.supports_input() {
                sources.push(SourceInfo {
                    id: format!("input:{id}"),
                    name: input_name,
                    source_type: SourceType::Input,
                    is_default: default_in.as_ref() == Some(&id),
                });
            }
            if d.supports_output() {
                #[cfg(target_os = "macos")]
                sources.push(SourceInfo {
                    id: format!("loopback:{id}"),
                    name: format!("Sound playing on {name}"),
                    source_type: SourceType::Loopback,
                    is_default: default_out.as_ref() == Some(&id),
                });
                destinations.push(DestinationInfo {
                    id: format!("output:{id}"),
                    name: output_name,
                    is_default: default_out.as_ref() == Some(&id),
                });
            }
        }
        Ok((sources, destinations))
    }

    fn open_source(&self, source_id: &str) -> Result<OpenCapture, String> {
        let (id, loopback) = if let Some(id) = source_id.strip_prefix("input:") {
            (id.to_owned(), false)
        } else if let Some(id) = source_id
            .strip_prefix("loopback:")
            .filter(|_| cfg!(target_os = "macos"))
        {
            (id.to_owned(), true)
        } else {
            return Err(format!("this device cannot capture {source_id}"));
        };
        let counted = StreamUse::begin(!loopback);
        let (ring_tx, ring_rx) = mpsc::sync_channel(1);
        let tick = if loopback {
            output_mute(id.clone())
        } else {
            Box::new(|| {}) as Box<dyn FnMut() + Send>
        };
        let (guard, rate, description) = spawn_stream("audionet-capture", tick, move |failure| {
            let device = find(&id, !loopback)?;
            // Loopback records in the output's own format.
            let config = if loopback {
                device.default_output_config()
            } else {
                device.default_input_config()
            }
            .map_err(|e| format!("could not read the input format: {e}"))?;
            let rate = config.sample_rate();
            let channels = usize::from(config.channels());
            let format = (rate, config.channels());
            let probe = device.clone();
            let (mut producer, consumer) = audio_ring((rate as usize) / 2, channels);
            let _ = ring_tx.send(consumer);
            let stream = device
                .build_input_stream::<f32, _, _>(
                    config.config(),
                    move |data: &[f32], _| {
                        let whole = data.len() - data.len() % channels;
                        let _ = producer.write(&data[..whole]);
                    },
                    move |e| {
                        on_stream_error(&failure, &e, format, || {
                            let c = if loopback {
                                probe.default_output_config()
                            } else {
                                probe.default_input_config()
                            };
                            c.ok().map(|c| (c.sample_rate(), c.channels()))
                        })
                    },
                    None,
                )
                .map_err(|e| {
                    if loopback {
                        format!("could not record the sound playing on this output (system audio needs macOS 14.2 or later): {e}")
                    } else {
                        format!("could not open the input device: {e}")
                    }
                })?;
            let name = stream_device_name(&device, !loopback);
            // The format in words: whether a microphone records in stereo
            // is something to check, not assume.
            let layout = match channels {
                1 => "mono".to_owned(),
                2 => "stereo".to_owned(),
                n => format!("{n} channels"),
            };
            Ok((
                stream,
                rate,
                if loopback {
                    format!("sound playing on {name} ({layout}, {rate} Hz)")
                } else {
                    format!("{name} ({layout}, {rate} Hz)")
                },
            ))
        })?;
        let consumer = ring_rx
            .recv()
            .map_err(|_| "the capture stream did not start".to_owned())?;
        Ok(OpenCapture {
            consumer,
            sample_rate: rate,
            guard: Box::new(CountedGuard {
                stream: guard,
                _use: counted,
            }),
            description,
        })
    }

    fn open_destination(
        &self,
        destination_id: &str,
        mut source: Box<dyn RenderSource>,
    ) -> Result<(Box<dyn StreamGuard>, String), String> {
        let id = destination_id
            .strip_prefix("output:")
            .ok_or_else(|| format!("this device cannot play to {destination_id}"))?
            .to_owned();
        let counted = StreamUse::begin(false);
        let (guard, _rate, description) = spawn_stream(
            "audionet-render",
            || {},
            move |failure| {
                let device = find(&id, false)?;
                let config = device
                    .default_output_config()
                    .map_err(|e| format!("could not read the output format: {e}"))?;
                let rate = config.sample_rate();
                let channels = usize::from(config.channels());
                // Control path: allocate playout buffers for this device format.
                source.prepare(rate, channels, rate as usize)?;
                let format = (rate, config.channels());
                let probe = device.clone();
                let stream = device
                    .build_output_stream::<f32, _, _>(
                        config.config(),
                        move |data: &mut [f32], _| source.render(data, clock::now_ns()),
                        move |e| {
                            on_stream_error(&failure, &e, format, || {
                                probe
                                    .default_output_config()
                                    .ok()
                                    .map(|c| (c.sample_rate(), c.channels()))
                            })
                        },
                        None,
                    )
                    .map_err(|e| {
                        format!(
                            "could not open the output device ({rate} Hz, {channels} channels): {e}"
                        )
                    })?;
                Ok((stream, rate, stream_device_name(&device, false)))
            },
        )?;
        Ok((
            Box::new(CountedGuard {
                stream: guard,
                _use: counted,
            }),
            description,
        ))
    }
}
