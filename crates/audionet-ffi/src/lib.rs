//! AudioNet's native engine for apps written in other languages (Swift on
//! macOS and iOS), through [UniFFI](https://mozilla.github.io/uniffi-rs/).
//!
//! * [`sign_in`] adds this device to an account (account name and
//!   password; the returned [`Account`] holds the device token, which the
//!   app keeps in the system keychain, never the password).
//! * [`Client`] runs the device agent: this device's audio is shared while
//!   it runs, and it lists the account's devices and starts streams with
//!   them (listen to another device here, or send from here to it).
//! * Everything the app needs to show arrives through [`EventListener`] as
//!   plain values and plain-language text (for screen readers).
//!
//! The audio itself (capture, Opus, WebRTC, playout with drift and depth
//! control) is the same engine as on Windows; on Apple platforms it uses
//! Core Audio through cpal. No audio work happens on the app's threads.
//!
//! The only `unsafe` in this crate is UniFFI's generated FFI glue.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use audionet_node::agent::{Agent, AppEvent, Command, Control};
use audionet_node::audio::NodeAudio;
use audionet_node::config::NodeConfig;
use audionet_node::session::LocalMedia;
use audionet_protocol::signal::{
    DestinationInfo, NodeSummary, SessionMedia, SessionState, SourceInfo, SourceType,
};
use audionet_protocol::{NodeId, Platform, SessionId};
use tokio::sync::{mpsc, oneshot};

uniffi::setup_scaffolding!();

#[cfg(not(windows))]
use audionet_node::cpal_audio::CpalNodeAudio as PlatformAudio;
#[cfg(windows)]
use audionet_node::windows_audio::WasapiNodeAudio as PlatformAudio;

fn this_platform() -> Platform {
    if cfg!(target_os = "ios") {
        Platform::Ios
    } else if cfg!(target_os = "macos") {
        Platform::MacOs
    } else if cfg!(windows) {
        Platform::Windows
    } else {
        Platform::Linux
    }
}

/// A failure, in words for the person using the app.
#[derive(Debug, uniffi::Error)]
#[uniffi(flat_error)]
pub enum AudioNetError {
    Failed { message: String },
}

impl std::fmt::Display for AudioNetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Failed { message } => f.write_str(message),
        }
    }
}

impl From<String> for AudioNetError {
    fn from(message: String) -> Self {
        Self::Failed { message }
    }
}

/// This device's membership in an account. `token` is a secret: keep it in
/// the system keychain.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct Account {
    pub server_url: String,
    pub node_id: String,
    pub token: String,
    pub device_name: String,
    pub username: String,
}

impl From<NodeConfig> for Account {
    fn from(c: NodeConfig) -> Self {
        Self {
            server_url: c.server_url,
            node_id: c.node_id,
            token: c.token,
            device_name: c.name,
            username: c.username,
        }
    }
}

impl From<Account> for NodeConfig {
    fn from(a: Account) -> Self {
        Self {
            server_url: a.server_url,
            node_id: a.node_id,
            token: a.token,
            name: a.device_name,
            username: a.username,
        }
    }
}

/// A sound a device can share.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct Source {
    pub id: String,
    pub name: String,
    /// A microphone or other input; otherwise what an output is playing.
    pub is_input: bool,
    pub is_default: bool,
}

/// Somewhere a device can play sound.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct Output {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

/// One of the account's devices.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct Device {
    pub node_id: String,
    pub name: String,
    /// Its AudioNet is running and connected.
    pub online: bool,
    /// It shares its audio: it can be listened to. An online device that
    /// does not share can still be sent to.
    pub sharing: bool,
    /// "windows", "macos", "ios", … or empty when unknown.
    pub platform: String,
    pub sources: Vec<Source>,
    pub outputs: Vec<Output>,
}

/// This device's own sound sources and outputs.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct LocalAudio {
    pub sources: Vec<Source>,
    pub outputs: Vec<Output>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum StreamState {
    Starting,
    Active,
    Ended,
    Failed,
}

/// Everything the app hears from the engine.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum Event {
    /// Online: signed in to the server; this device is shared.
    Connected,
    /// Lost the server; the engine reconnects by itself.
    Disconnected {
        reason: String,
    },
    /// A status line for the log (plain language).
    Status {
        text: String,
    },
    /// The account's other devices (this device is left out).
    Devices {
        devices: Vec<Device>,
    },
    /// One device changed (online, offline, or its sounds and outputs).
    DeviceChanged {
        device: Device,
    },
    /// Progress of a stream this app started.
    Stream {
        session_id: String,
        state: StreamState,
        detail: String,
    },
    StreamEnded {
        session_id: String,
        reason: String,
    },
    /// Periodic measurements of a stream this device receives (for a
    /// diagnostics view; never to be announced).
    Diagnostics {
        session_id: String,
        text: String,
    },
    /// A problem the server reported, in words.
    ServerProblem {
        message: String,
    },
    /// The engine stopped (with the reason if it failed).
    Stopped {
        error: Option<String>,
    },
}

/// Implemented by the app to receive [`Event`]s. Called on an engine
/// thread: hand the event to the main thread before touching the UI.
#[uniffi::export(with_foreign)]
pub trait EventListener: Send + Sync {
    fn on_event(&self, event: Event);
}

/// Implemented by the iPhone app: told how many audio streams are open and
/// how many record a microphone, before a stream opens (counted already)
/// and after one closes. Called on an engine thread and waited for: set the
/// audio session (a category change must settle before the stream opens),
/// then return.
#[uniffi::export(with_foreign)]
pub trait AudioUseListener: Send + Sync {
    fn audio_in_use(&self, streams: u32, microphones: u32);
}

/// Sets (or removes, with `None`) the [`AudioUseListener`].
#[uniffi::export]
pub fn set_audio_use_listener(listener: Option<Arc<dyn AudioUseListener>>) {
    audionet_node::audio::set_audio_use_hook(listener.map(|l| {
        Box::new(move |u: audionet_node::audio::AudioInUse| {
            l.audio_in_use(u.streams, u.microphones)
        }) as audionet_node::audio::AudioUseHook
    }));
}

/// Adds this device to an account: signs in with the account name and
/// password and returns this device's credential. Blocking (network): call
/// it off the main thread.
#[uniffi::export]
pub fn sign_in(
    server_url: String,
    username: String,
    password: String,
    device_name: String,
) -> Result<Account, AudioNetError> {
    audionet_node::account::sign_in(
        &server_url,
        &username,
        &password,
        &device_name,
        this_platform(),
    )
    .map(Account::from)
    .map_err(AudioNetError::from)
}

/// Removes this device from its account on the server (signing out): its
/// record and credential there are deleted, so nothing is left behind.
/// Blocking (network): call it off the main thread.
#[uniffi::export]
pub fn remove_device(account: Account) -> Result<(), AudioNetError> {
    audionet_node::account::remove_device(&account.into()).map_err(AudioNetError::from)
}

/// This device's sound sources and outputs.
#[uniffi::export]
pub fn local_audio() -> Result<LocalAudio, AudioNetError> {
    let (sources, outputs) = PlatformAudio.endpoints()?;
    Ok(LocalAudio {
        sources: sources.iter().map(source).collect(),
        outputs: outputs.iter().map(output).collect(),
    })
}

/// The engine's version, e.g. "0.4.0".
#[uniffi::export]
pub fn engine_version() -> String {
    env!("CARGO_PKG_VERSION").to_owned()
}

fn source(s: &SourceInfo) -> Source {
    Source {
        id: s.id.clone(),
        name: s.name.clone(),
        is_input: s.source_type == SourceType::Input,
        is_default: s.is_default,
    }
}

fn output(d: &DestinationInfo) -> Output {
    Output {
        id: d.id.clone(),
        name: d.name.clone(),
        is_default: d.is_default,
    }
}

fn device(n: NodeSummary) -> Device {
    Device {
        node_id: n.node_id.as_str().to_owned(),
        name: n.name,
        online: n.online,
        sharing: n.sharing,
        platform: n.platform.map(platform_name).unwrap_or_default(),
        sources: n.sources.iter().map(source).collect(),
        outputs: n.destinations.iter().map(output).collect(),
    }
}

fn platform_name(p: Platform) -> String {
    match p {
        Platform::Windows => "windows",
        Platform::MacOs => "macos",
        Platform::Linux => "linux",
        Platform::Ios => "ios",
        Platform::Android => "android",
        Platform::Browser => "browser",
    }
    .to_owned()
}

fn stream_state(s: SessionState) -> StreamState {
    match s {
        SessionState::Starting => StreamState::Starting,
        SessionState::Active => StreamState::Active,
        SessionState::Ended => StreamState::Ended,
        SessionState::Failed => StreamState::Failed,
    }
}

struct Running {
    stop: Option<oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
    commands: mpsc::UnboundedSender<Command>,
    active: Arc<AtomicUsize>,
}

impl Running {
    fn stop(&mut self) {
        if let Some(tx) = self.stop.take() {
            let _ = tx.send(());
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// The engine for one signed-in device.
#[derive(uniffi::Object)]
pub struct Client {
    account: Account,
    listener: Arc<dyn EventListener>,
    running: Mutex<Option<Running>>,
    /// This device's id, to leave it out of the device list.
    me: Arc<Mutex<Option<String>>>,
    /// Whether this device shares its audio in this account (see
    /// `set_sharing`); read on every connection.
    sharing: Arc<std::sync::atomic::AtomicBool>,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("device", &self.account.device_name)
            .finish_non_exhaustive()
    }
}

/// A session id unique to this run.
fn new_session_id() -> SessionId {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    SessionId::new(format!("app-{nanos:x}-{}", NEXT.fetch_add(1, Relaxed))).expect("valid id")
}

#[uniffi::export]
impl Client {
    #[uniffi::constructor]
    pub fn new(account: Account, listener: Arc<dyn EventListener>) -> Arc<Self> {
        Arc::new(Self {
            account,
            listener,
            running: Mutex::new(None),
            me: Arc::new(Mutex::new(None)),
            sharing: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        })
    }

    /// Whether this device shares its audio in this account: others may
    /// listen to it, and it may send its own. Not sharing, it stays online:
    /// it sees the devices, listens to them and plays what they send it.
    /// Set it before `start` (the app keeps the choice); changing it while
    /// running tells the server at once, which ends every stream this
    /// device sends.
    pub fn set_sharing(&self, sharing: bool) {
        self.sharing.store(sharing, Relaxed);
        if self.is_running() {
            let _ = self.command(Command::SetSharing { sharing });
        }
    }

    pub fn is_sharing(&self) -> bool {
        self.sharing.load(Relaxed)
    }

    /// Connects this device to its account: online (sharing or not, as
    /// set), with the remote. Does nothing if already running.
    pub fn start(&self) {
        let mut running = self.running.lock().unwrap_or_else(|e| e.into_inner());
        if running.is_some() {
            return;
        }
        let listener = Arc::clone(&self.listener);
        let me = Arc::clone(&self.me);
        let events: audionet_node::agent::AppEventFn = Arc::new(move |e| {
            let event = match e {
                AppEvent::Connected { node_id } => {
                    *me.lock().unwrap_or_else(|e| e.into_inner()) =
                        node_id.map(|n| n.as_str().to_owned());
                    Event::Connected
                }
                AppEvent::Disconnected { reason } => Event::Disconnected { reason },
                AppEvent::Devices(nodes) => {
                    let mine = me.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    Event::Devices {
                        devices: nodes
                            .into_iter()
                            .filter(|n| Some(n.node_id.as_str()) != mine.as_deref())
                            .map(device)
                            .collect(),
                    }
                }
                AppEvent::DeviceUpdate(node) => {
                    let mine = me.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    if Some(node.node_id.as_str()) == mine.as_deref() {
                        return;
                    }
                    Event::DeviceChanged {
                        device: device(node),
                    }
                }
                AppEvent::Session {
                    session_id,
                    state,
                    detail,
                } => Event::Stream {
                    session_id: session_id.as_str().to_owned(),
                    state: stream_state(state),
                    detail,
                },
                AppEvent::SessionEnded { session_id, reason } => Event::StreamEnded {
                    session_id: session_id.as_str().to_owned(),
                    reason,
                },
                AppEvent::Diagnostics { session_id, text } => Event::Diagnostics {
                    session_id: session_id.as_str().to_owned(),
                    text,
                },
                AppEvent::ServerError { message } => Event::ServerProblem { message },
            };
            listener.on_event(event);
        });
        let status_listener = Arc::clone(&self.listener);
        let status: audionet_node::agent::StatusFn = Arc::new(move |t: &str| {
            status_listener.on_event(Event::Status { text: t.to_owned() });
        });
        let (control, commands) = Control::new(events);
        let (stop_tx, stop_rx) = oneshot::channel::<()>();
        let active = Arc::new(AtomicUsize::new(0));
        let agent = Agent {
            config: self.account.clone().into(),
            audio: Arc::new(PlatformAudio),
            platform: this_platform(),
            software: format!("audionet-app {}", env!("CARGO_PKG_VERSION")),
            status,
            thread_setup: None,
            active_sessions: Arc::clone(&active),
            control: Some(control),
            relay_only: false,
            sharing: Arc::clone(&self.sharing),
        };
        let ended = Arc::clone(&self.listener);
        let thread = std::thread::Builder::new()
            .name("audionet-engine".into())
            .spawn(move || {
                let error = match tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt
                        .block_on(agent.run(async {
                            let _ = stop_rx.await;
                        }))
                        .err(),
                    Err(e) => Some(format!("could not start: {e}")),
                };
                ended.on_event(Event::Stopped { error });
            })
            .expect("spawning the engine thread");
        *running = Some(Running {
            stop: Some(stop_tx),
            thread: Some(thread),
            commands,
            active,
        });
    }

    /// Disconnects (the app is leaving, or signing out): every stream stops
    /// and this device is offline in this account.
    pub fn stop(&self) {
        let taken = self
            .running
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(mut r) = taken {
            r.stop();
        }
    }

    pub fn is_running(&self) -> bool {
        self.running
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
    }

    /// Streams running on this device right now (in either direction).
    pub fn active_streams(&self) -> u32 {
        self.running
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map_or(0, |r| r.active.load(Relaxed) as u32)
    }

    /// Asks for the device list again (answered with `Event::Devices`).
    pub fn refresh_devices(&self) {
        let _ = self.command(Command::ListDevices);
    }

    /// Listens to another device's source on one of this device's
    /// outputs. Returns the stream's id.
    pub fn listen(
        &self,
        node_id: String,
        source_id: String,
        output_id: String,
    ) -> Result<String, AudioNetError> {
        self.start_stream(
            node_id,
            SessionMedia::Listen { source_id },
            LocalMedia::Receive {
                destination_id: output_id,
            },
        )
    }

    /// Sends one of this device's sources to another device's output.
    /// Returns the stream's id.
    pub fn send(
        &self,
        node_id: String,
        source_id: String,
        output_id: String,
    ) -> Result<String, AudioNetError> {
        self.start_stream(
            node_id,
            SessionMedia::Speak {
                destination_id: output_id,
            },
            LocalMedia::Send { source_id },
        )
    }

    /// Stops a stream this app started.
    pub fn stop_stream(&self, session_id: String) {
        if let Ok(id) = SessionId::new(session_id) {
            let _ = self.command(Command::Stop { session_id: id });
        }
    }

    /// A stream's volume and mute on this device: for a stream this app
    /// listens to, what it plays here; for one it sends, what is sent.
    /// `volume` is the slider position, 0.0 to 1.0 (heard on a square
    /// curve). Changes ramp over 10 ms.
    pub fn set_stream_volume(&self, session_id: String, volume: f32, muted: bool) {
        if let Ok(id) = SessionId::new(session_id) {
            let _ = self.command(Command::SetVolume {
                session_id: id,
                volume,
                muted,
            });
        }
    }
}

impl Client {
    fn command(&self, c: Command) -> Result<(), AudioNetError> {
        let running = self.running.lock().unwrap_or_else(|e| e.into_inner());
        let r = running.as_ref().ok_or_else(|| {
            AudioNetError::from(
                "This account is not connected yet. Try again in a moment.".to_owned(),
            )
        })?;
        r.commands
            .send(c)
            .map_err(|_| AudioNetError::from("AudioNet stopped.".to_owned()))
    }

    fn start_stream(
        &self,
        node_id: String,
        remote: SessionMedia,
        local: LocalMedia,
    ) -> Result<String, AudioNetError> {
        let node_id = NodeId::new(node_id)
            .map_err(|e| AudioNetError::from(format!("unknown device: {e}")))?;
        let session_id = new_session_id();
        self.command(Command::Start {
            session_id: session_id.clone(),
            node_id,
            remote,
            local,
        })?;
        Ok(session_id.as_str().to_owned())
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_round_trips_through_the_node_config() {
        let a = Account {
            server_url: "https://audionet.example.com".into(),
            node_id: "node_x".into(),
            token: "ann_secret".into(),
            device_name: "Mac".into(),
            username: "alice".into(),
        };
        assert_eq!(Account::from(NodeConfig::from(a.clone())), a);
    }

    #[test]
    fn commands_need_a_running_engine() {
        struct Nothing;
        impl EventListener for Nothing {
            fn on_event(&self, _: Event) {}
        }
        let c = Client::new(
            Account {
                server_url: "https://audionet.example.com".into(),
                node_id: "node_x".into(),
                token: "ann_x".into(),
                device_name: "Mac".into(),
                username: "alice".into(),
            },
            Arc::new(Nothing),
        );
        let err = c
            .listen("node_y".into(), "s".into(), "o".into())
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "This account is not connected yet. Try again in a moment."
        );
        assert_eq!(c.active_streams(), 0);
        // Sharing is chosen before connecting (off until the app says).
        assert!(!c.is_sharing());
        c.set_sharing(true);
        assert!(c.is_sharing());
    }
}
