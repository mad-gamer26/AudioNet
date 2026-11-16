//! Linux: a device's sound settings through PulseAudio (or PipeWire's
//! PulseAudio server, `pipewire-pulse`): each output's and microphone's
//! volume and mute, the default output and input, and cards turned off
//! and on (the card's "Off" profile, as the system's sound settings do);
//! changing them when the account asks; and a subscription that notices
//! changes made anywhere (the desktop's volume control, keys, `pactl`,
//! other apps), so the account sees them at once.
//!
//! Uses the `pulseaudio` crate cpal already brings for PulseAudio (no new
//! dependency), through its blocking protocol functions: a short-lived
//! connection per read or change (control path only), and one long-lived
//! connection on its own thread for the subscription.
//!
//! Devices are named as cpal names them: `pulseaudio:<sink or source
//! name>`. Volume is PulseAudio's own scale (100% = 1.0, as its sliders);
//! a device boosted past 100% reports 1.0. A disabled card is listed as
//! `card:<card name>`.

use std::collections::HashMap;
use std::ffi::CString;
use std::io::BufReader;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use audionet_protocol::signal::{DisabledEndpoint, EndpointControl, EndpointKind};
use pulseaudio::protocol::{self, ChannelVolume, Command, Prop, Volume};

/// The profile that turns a card off.
const OFF: &str = "off";

struct Conn {
    sock: BufReader<UnixStream>,
    version: u16,
    seq: u32,
}

fn err(e: impl std::fmt::Display) -> String {
    format!("the sound server (PulseAudio or PipeWire) didn't answer: {e}")
}

impl Conn {
    fn open() -> Result<Self, String> {
        let path = pulseaudio::socket_path_from_env()
            .ok_or_else(|| "no sound server (PulseAudio or PipeWire) is running".to_owned())?;
        let stream = UnixStream::connect(path).map_err(err)?;
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .map_err(err)?;
        stream
            .set_write_timeout(Some(Duration::from_secs(3)))
            .map_err(err)?;
        let mut sock = BufReader::new(stream);
        let cookie = pulseaudio::cookie_path_from_env()
            .and_then(|p| std::fs::read(p).ok())
            .unwrap_or_default();
        protocol::write_command_message(
            sock.get_mut(),
            0,
            &Command::Auth(protocol::AuthParams {
                version: protocol::MAX_VERSION,
                supports_shm: false,
                supports_memfd: false,
                cookie,
            }),
            protocol::MAX_VERSION,
        )
        .map_err(err)?;
        let (_, auth) =
            protocol::read_reply_message::<protocol::AuthReply>(&mut sock, protocol::MAX_VERSION)
                .map_err(err)?;
        let version = protocol::MAX_VERSION.min(auth.version);
        let mut props = protocol::Props::new();
        props.set(
            Prop::ApplicationName,
            CString::new("AudioNet").expect("no NUL"),
        );
        protocol::write_command_message(sock.get_mut(), 1, &Command::SetClientName(props), version)
            .map_err(err)?;
        protocol::read_reply_message::<protocol::SetClientNameReply>(&mut sock, version)
            .map_err(err)?;
        Ok(Self {
            sock,
            version,
            seq: 2,
        })
    }

    fn reply<R: protocol::CommandReply>(&mut self, cmd: Command) -> Result<R, String> {
        let seq = self.seq;
        self.seq += 1;
        protocol::write_command_message(self.sock.get_mut(), seq, &cmd, self.version)
            .map_err(err)?;
        protocol::read_reply_message::<R>(&mut self.sock, self.version)
            .map(|(_, r)| r)
            .map_err(err)
    }

    fn ack(&mut self, cmd: Command) -> Result<(), String> {
        let seq = self.seq;
        self.seq += 1;
        protocol::write_command_message(self.sock.get_mut(), seq, &cmd, self.version)
            .map_err(err)?;
        protocol::read_ack_message(&mut self.sock)
            .map(|_| ())
            .map_err(err)
    }
}

/// Everything read at once.
#[derive(Debug)]
pub struct Snapshot {
    sinks: Vec<protocol::SinkInfo>,
    sources: Vec<protocol::SourceInfo>,
    cards: Vec<protocol::CardInfo>,
    default_sink: Option<String>,
    default_source: Option<String>,
}

fn text(c: &std::ffi::CStr) -> String {
    c.to_string_lossy().into_owned()
}

/// Reads the sound server's devices, cards and defaults.
pub fn snapshot() -> Result<Snapshot, String> {
    let mut c = Conn::open()?;
    let server: protocol::ServerInfo = c.reply(Command::GetServerInfo)?;
    Ok(Snapshot {
        sinks: c.reply(Command::GetSinkInfoList)?,
        sources: c.reply(Command::GetSourceInfoList)?,
        cards: c.reply(Command::GetCardInfoList)?,
        default_sink: server.default_sink_name.as_deref().map(text),
        default_source: server.default_source_name.as_deref().map(text),
    })
}

fn level(v: &ChannelVolume) -> f32 {
    let vs = v.channels();
    if vs.is_empty() {
        return 0.0;
    }
    let sum: u64 = vs.iter().map(|x| u64::from(x.as_u32())).sum();
    let avg = sum as f64 / vs.len() as f64;
    (avg / f64::from(Volume::NORM.as_u32())).min(1.0) as f32
}

/// Whether a card can be turned off (it has the "Off" profile).
fn can_turn_off(s: &Snapshot, card: Option<u32>) -> bool {
    card.and_then(|i| s.cards.iter().find(|c| c.index == i))
        .is_some_and(|c| {
            c.profiles
                .iter()
                .any(|p| p.name.to_bytes() == OFF.as_bytes())
        })
}

impl Snapshot {
    /// The settings of a sink (`input` false) or source by name, as cpal
    /// names them after "pulseaudio:".
    pub fn control(&self, name: &str, input: bool) -> Option<EndpointControl> {
        let (volume, muted, card, is_default) = if input {
            let s = self
                .sources
                .iter()
                .find(|s| s.name.to_bytes() == name.as_bytes())?;
            (
                level(&s.cvolume),
                s.muted,
                s.card_index,
                self.default_source.as_deref() == Some(name),
            )
        } else {
            let s = self
                .sinks
                .iter()
                .find(|s| s.name.to_bytes() == name.as_bytes())?;
            (
                level(&s.cvolume),
                s.muted,
                s.card_index,
                self.default_sink.as_deref() == Some(name),
            )
        };
        let _ = is_default;
        Some(EndpointControl {
            volume: Some(volume),
            muted: Some(muted),
            enabled: true,
            is_default_communications: false,
            can_set_volume: true,
            can_mute: true,
            can_set_default: true,
            can_set_default_communications: false,
            can_enable: can_turn_off(self, card),
        })
    }

    /// Cards turned off, to be turned back on.
    pub fn disabled(&self) -> Vec<DisabledEndpoint> {
        self.cards
            .iter()
            .filter(|c| {
                c.active_profile
                    .as_deref()
                    .is_some_and(|p| p.to_bytes() == OFF.as_bytes())
            })
            .map(|c| {
                let plays = c.profiles.iter().any(|p| p.num_sinks > 0);
                DisabledEndpoint {
                    id: format!("card:{}", text(&c.name)),
                    name: c
                        .props
                        .get(Prop::DeviceDescription)
                        .map(|b| String::from_utf8_lossy(b).trim_end_matches('\0').to_owned())
                        .unwrap_or_else(|| text(&c.name)),
                    kind: if plays {
                        EndpointKind::Output
                    } else {
                        EndpointKind::Input
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
            .collect()
    }
}

fn cname(s: &str) -> Result<CString, String> {
    CString::new(s).map_err(|_| "That sound device's name isn't valid.".to_owned())
}

/// Sets a sink's or source's volume (0.0 to 1.0) on every channel.
pub fn set_volume(name: &str, input: bool, volume: f32) -> Result<(), String> {
    let s = snapshot()?;
    let channels = if input {
        s.sources
            .iter()
            .find(|x| x.name.to_bytes() == name.as_bytes())
            .map(|x| x.cvolume.channels().len())
    } else {
        s.sinks
            .iter()
            .find(|x| x.name.to_bytes() == name.as_bytes())
            .map(|x| x.cvolume.channels().len())
    }
    .ok_or_else(|| "That sound device isn't connected any more.".to_owned())?;
    let raw = (f64::from(volume.clamp(0.0, 1.0)) * f64::from(Volume::NORM.as_u32())).round() as u32;
    let mut cv = ChannelVolume::empty();
    for _ in 0..channels.max(1) {
        cv.push(Volume::from_u32_clamped(raw));
    }
    let params = protocol::SetDeviceVolumeParams {
        device_index: None,
        device_name: Some(cname(name)?),
        volume: cv,
    };
    Conn::open()?.ack(if input {
        Command::SetSourceVolume(params)
    } else {
        Command::SetSinkVolume(params)
    })
}

pub fn set_muted(name: &str, input: bool, muted: bool) -> Result<(), String> {
    let params = protocol::SetDeviceMuteParams {
        device_index: None,
        device_name: Some(cname(name)?),
        mute: muted,
    };
    Conn::open()?.ack(if input {
        Command::SetSourceMute(params)
    } else {
        Command::SetSinkMute(params)
    })
}

pub fn set_default(name: &str, input: bool) -> Result<(), String> {
    let n = cname(name)?;
    Conn::open()?.ack(if input {
        Command::SetDefaultSource(n)
    } else {
        Command::SetDefaultSink(n)
    })
}

/// The profile each card had before AudioNet turned it off.
static BEFORE_OFF: Mutex<Option<HashMap<String, CString>>> = Mutex::new(None);

/// Turns off the card of a sink or source (`target` its name), or turns a
/// card back on (`target` "card:<name>"): to the profile it had, else its
/// best available one.
pub fn set_enabled(target: &str, input: bool, enabled: bool) -> Result<(), String> {
    let s = snapshot()?;
    let card = if let Some(card) = target.strip_prefix("card:") {
        s.cards
            .iter()
            .find(|c| c.name.to_bytes() == card.as_bytes())
    } else {
        let index = if input {
            s.sources
                .iter()
                .find(|x| x.name.to_bytes() == target.as_bytes())
                .and_then(|x| x.card_index)
        } else {
            s.sinks
                .iter()
                .find(|x| x.name.to_bytes() == target.as_bytes())
                .and_then(|x| x.card_index)
        };
        index.and_then(|i| s.cards.iter().find(|c| c.index == i))
    }
    .ok_or_else(|| {
        "That sound device can't be turned off or on (it has no sound card of its own).".to_owned()
    })?;
    let card_name = text(&card.name);
    let profile = if enabled {
        let remembered = BEFORE_OFF
            .lock()
            .ok()
            .and_then(|m| m.as_ref().and_then(|m| m.get(&card_name).cloned()));
        remembered
            .filter(|r| card.profiles.iter().any(|p| &p.name == r))
            .or_else(|| {
                card.profiles
                    .iter()
                    // available: 0 unknown, 1 no, 2 yes.
                    .filter(|p| p.name.to_bytes() != OFF.as_bytes() && p.available != 1)
                    .max_by_key(|p| p.priority)
                    .map(|p| p.name.clone())
            })
            .ok_or_else(|| "That sound card has nothing to turn on.".to_owned())?
    } else {
        if let (Some(active), Ok(mut m)) = (card.active_profile.clone(), BEFORE_OFF.lock())
            && active.to_bytes() != OFF.as_bytes()
        {
            m.get_or_insert_with(HashMap::new)
                .insert(card_name.clone(), active);
        }
        cname(OFF)?
    };
    Conn::open()?.ack(Command::SetCardProfile(protocol::SetCardProfileParams {
        card_index: Some(card.index),
        card_name: None,
        profile_name: profile,
    }))
}

/// The newest subscription; older ones stop.
static GENERATION: AtomicU64 = AtomicU64::new(0);

/// Calls `waker` whenever an output, microphone, card or default changes,
/// from a thread of its own that reconnects to the sound server as needed.
/// Replaces an earlier subscription.
pub fn watch(waker: Arc<dyn Fn() + Send + Sync>) {
    let generation = GENERATION.fetch_add(1, Relaxed) + 1;
    let _ = std::thread::Builder::new()
        .name("audionet-sound-settings".into())
        .spawn(move || {
            while GENERATION.load(Relaxed) == generation {
                if let Err(e) = subscribe(generation, &waker) {
                    tracing::debug!("sound settings notices: {e}");
                }
                std::thread::sleep(Duration::from_secs(2));
            }
        });
}

fn subscribe(generation: u64, waker: &Arc<dyn Fn() + Send + Sync>) -> Result<(), String> {
    let mut c = Conn::open()?;
    // Events come when they come: wait in slices, to notice a newer
    // subscription.
    c.sock
        .get_ref()
        .set_read_timeout(Some(Duration::from_secs(1)))
        .map_err(err)?;
    let mask = protocol::SubscriptionMask::SINK
        | protocol::SubscriptionMask::SOURCE
        | protocol::SubscriptionMask::SERVER
        | protocol::SubscriptionMask::CARD;
    let seq = c.seq;
    protocol::write_command_message(c.sock.get_mut(), seq, &Command::Subscribe(mask), c.version)
        .map_err(err)?;
    protocol::read_ack_message(&mut c.sock).map_err(err)?;
    while GENERATION.load(Relaxed) == generation {
        match protocol::read_command_message(&mut c.sock, c.version) {
            Ok((_, Command::SubscribeEvent(_))) => waker(),
            Ok(_) => {}
            Err(protocol::ProtocolError::Io(e))
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(e) => return Err(err(e)),
        }
    }
    Ok(())
}
