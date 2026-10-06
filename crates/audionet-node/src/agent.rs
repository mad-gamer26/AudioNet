//! The node agent: keeps a signaling connection to the server, advertises
//! this device's sources and destinations, and runs WebRTC sessions.
//!
//! Native apps also drive it as a remote (`Control`): list the account's
//! devices, and start sessions with them (listen to another device's
//! source here, or send a source from here to another device's output).
//!
//! A visitor (`Agent::visitor`) is such a remote without being a device:
//! like the web client, it signs in with a web session instead of a device
//! credential, is not listed among the account's devices, offers nothing to
//! listen to and receives nothing unasked, but listens to devices and sends
//! to them. The NVDA add-on runs one per account.
//!
//! Reconnects with backoff after network problems (1, 2, 4, 8, 16, then
//! every 30 seconds; back to 1 second after a connection that lasted, see
//! [`next_backoff`]). Stops for good only if
//! the server rejects the credential (the device was removed, or the
//! visitor's web session ended).

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed};
use std::time::{Duration, Instant};

use audionet_protocol::signal::{
    ClientInfo, ClientKind, ClientMessage, DestinationInfo, IceServer, NodeSummary, ServerMessage,
    SessionMedia, SessionState, SourceInfo,
};
use audionet_protocol::{NodeId, PROTOCOL_VERSION, Platform, SessionId};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;

use crate::audio::NodeAudio;
use crate::config::NodeConfig;
use crate::session::{self, LocalMedia, Negotiation, SessionEvent, SessionHandle};

const ENDPOINT_REFRESH: Duration = Duration::from_secs(5);
const MAX_BACKOFF: Duration = Duration::from_secs(30);
/// A connection signed in for at least this long counts as having worked:
/// the next wait starts again at 1 second. Shorter ones (a server that
/// accepts devices and drops them at once) keep the growing waits, so
/// devices do not retry every second.
const STABLE_CONNECTION: Duration = Duration::from_secs(10);

/// Plain-text status lines for the user (printed by the CLI, shown in
/// GUIs). Kept short and self-contained for screen readers.
pub type StatusFn = Arc<dyn Fn(&str) + Send + Sync>;

/// What an app asks the agent to do.
#[derive(Debug)]
pub enum Command {
    /// Ask for the account's devices (answered with `AppEvent::Devices`).
    ListDevices,
    /// Start a session with another device of the account. `remote` is the
    /// request as the other device sees it (listen to its source, or speak
    /// to its output); `local` is this device's part.
    Start {
        session_id: SessionId,
        node_id: NodeId,
        remote: SessionMedia,
        local: LocalMedia,
    },
    Stop {
        session_id: SessionId,
    },
    /// A stream's volume and mute on this device (what it plays here, or
    /// what it sends from here). `volume` is the slider position, 0.0 to 1.0.
    SetVolume {
        session_id: SessionId,
        volume: f32,
        muted: bool,
    },
    /// Start or stop sharing this device's audio in this account (see
    /// [`Agent::sharing`]). Stopping ends every stream it sends; what it
    /// receives goes on.
    SetSharing {
        sharing: bool,
    },
}

/// What the agent tells an app.
#[derive(Clone, Debug)]
pub enum AppEvent {
    /// Signed in to the server; `node_id` is this device.
    Connected {
        node_id: Option<NodeId>,
    },
    Disconnected {
        reason: String,
    },
    Devices(Vec<NodeSummary>),
    DeviceUpdate(NodeSummary),
    /// Progress of a session this app started, from either device.
    Session {
        session_id: SessionId,
        state: SessionState,
        detail: String,
    },
    SessionEnded {
        session_id: SessionId,
        reason: String,
    },
    /// Periodic measurements of a session this device receives, in words.
    Diagnostics {
        session_id: SessionId,
        text: String,
    },
    /// A problem the server reported, in words.
    ServerError {
        message: String,
    },
}

pub type AppEventFn = Arc<dyn Fn(AppEvent) + Send + Sync>;

/// The remote-control channel of a native app.
pub struct Control {
    pub commands: tokio::sync::Mutex<mpsc::UnboundedReceiver<Command>>,
    pub events: AppEventFn,
}

impl std::fmt::Debug for Control {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Control")
    }
}

impl Control {
    /// A control channel and the sender the app keeps.
    pub fn new(events: AppEventFn) -> (Self, mpsc::UnboundedSender<Command>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (
            Self {
                commands: tokio::sync::Mutex::new(rx),
                events,
            },
            tx,
        )
    }
}

pub struct Agent {
    pub config: NodeConfig,
    pub audio: Arc<dyn NodeAudio>,
    pub platform: Platform,
    pub software: String,
    pub status: StatusFn,
    /// Platform hook for session threads (e.g. MMCSS on Windows).
    pub thread_setup: Option<audionet_audio::threads::ThreadSetup>,
    /// Number of audio sessions currently running, kept up to date for
    /// callers (the desktop app installs updates only while it is zero).
    pub active_sessions: Arc<AtomicUsize>,
    /// Remote control for native apps; `None` for a plain device.
    pub control: Option<Control>,
    /// Diagnostics: sessions offer only relayed candidates (tests the TURN
    /// relay, like the web client's `?ice=relay`).
    pub relay_only: bool,
    /// Whether this device shares its audio in this account: others may
    /// listen to it and it may send its own. Not sharing, it stays online:
    /// it sees the other devices, listens to them and plays what they send
    /// it. Changed with [`Command::SetSharing`] (the app keeps its choice).
    pub sharing: Arc<AtomicBool>,
    /// Connect as a visitor rather than a device: `config.token` is a web
    /// session (`ans_…`), nothing about this computer's audio is announced,
    /// and sending needs no sharing (as in the web client).
    pub visitor: bool,
    /// Whether measurements go to the status log too: technical notes of
    /// every session, and the reports of sessions other devices asked for
    /// (receive reports every 2 seconds stay with the stream). Off in the
    /// apps unless the person turns on "Show measurements"; on in the
    /// command line.
    pub measurements_in_status: Arc<AtomicBool>,
}

impl std::fmt::Debug for Agent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Agent")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

enum Outcome {
    /// Connection lost; try again.
    Retry(String),
    /// Credential rejected; stop.
    Fatal(String),
    Shutdown,
}

/// The wait before reconnecting, given the previous wait (zero before the
/// first retry) and how long the lost connection had been signed in (None:
/// it never signed in). Doubles from 1 second up to [`MAX_BACKOFF`], and
/// starts again at 1 second after a connection that lasted
/// [`STABLE_CONNECTION`].
fn next_backoff(previous: Duration, lasted: Option<Duration>) -> Duration {
    if previous.is_zero() || lasted.is_some_and(|d| d >= STABLE_CONNECTION) {
        Duration::from_secs(1)
    } else {
        (previous * 2).min(MAX_BACKOFF)
    }
}

impl Agent {
    /// Runs until `shutdown` resolves or the device credential is rejected.
    pub async fn run(self, shutdown: impl std::future::Future<Output = ()>) -> Result<(), String> {
        tokio::pin!(shutdown);
        let mut backoff = Duration::ZERO;
        loop {
            // When this attempt signed in, if it did (set by connect_once).
            let signed_in = std::sync::Mutex::new(None::<Instant>);
            let outcome = tokio::select! {
                o = self.connect_once(&signed_in) => o,
                () = &mut shutdown => Outcome::Shutdown,
            };
            match outcome {
                Outcome::Shutdown => return Ok(()),
                Outcome::Fatal(e) => return Err(e),
                Outcome::Retry(why) => {
                    let lasted = signed_in.lock().ok().and_then(|t| *t).map(|t| t.elapsed());
                    backoff = next_backoff(backoff, lasted);
                    self.emit(AppEvent::Disconnected {
                        reason: why.clone(),
                    });
                    (self.status)(&format!(
                        "Disconnected from the server: {why}. Reconnecting in {} seconds.",
                        backoff.as_secs()
                    ));
                    tokio::select! {
                        () = tokio::time::sleep(backoff) => {}
                        () = &mut shutdown => return Ok(()),
                    }
                }
            }
        }
    }

    fn emit(&self, event: AppEvent) {
        if let Some(c) = &self.control {
            (c.events)(event);
        }
    }

    async fn endpoints(&self) -> Option<ClientMessage> {
        let audio = Arc::clone(&self.audio);
        match tokio::task::spawn_blocking(move || audio.endpoints()).await {
            Ok(Ok((sources, destinations))) => Some(ClientMessage::Endpoints {
                sources,
                destinations,
            }),
            Ok(Err(e)) => {
                (self.status)(&format!("Could not list audio devices: {e}"));
                None
            }
            Err(_) => None,
        }
    }

    async fn connect_once(&self, signed_in: &std::sync::Mutex<Option<Instant>>) -> Outcome {
        let mut req = match self.config.ws_url().into_client_request() {
            Ok(r) => r,
            Err(e) => return Outcome::Fatal(format!("invalid server URL: {e}")),
        };
        let bearer = HeaderValue::from_str(&format!("Bearer {}", self.config.token))
            .expect("token is ASCII");
        req.headers_mut().insert("authorization", bearer);
        let tls = match tls_config() {
            Ok(c) => c,
            Err(e) => return Outcome::Retry(format!("could not set up TLS ({e})")),
        };
        let ws = match tokio_tungstenite::connect_async_tls_with_config(req, None, false, Some(tls))
            .await
        {
            Ok((ws, _)) => ws,
            Err(tokio_tungstenite::tungstenite::Error::Http(resp)) if resp.status() == 401 => {
                return Outcome::Fatal(if self.visitor {
                    "The sign-in to this account has ended (it expired, or the password was changed). Sign in again.".into()
                } else {
                    "The server no longer recognizes this device. It may have been removed from your account. Sign it in again with `audionet node sign-in`.".into()
                });
            }
            Err(e) => return Outcome::Retry(format!("could not connect ({e})")),
        };
        let (mut sink, mut stream) = ws.split();
        let hello = ClientMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            client: ClientInfo {
                kind: if self.visitor {
                    ClientKind::Browser
                } else {
                    ClientKind::Node
                },
                software: self.software.clone(),
                platform: Some(self.platform),
            },
        };
        let send =
            |m: &ClientMessage| Message::Text(serde_json::to_string(m).expect("serializes").into());
        if sink.send(send(&hello)).await.is_err() {
            return Outcome::Retry("the connection closed during sign-in".into());
        }
        let (ice_servers, my_node_id): (Vec<IceServer>, Option<NodeId>) =
            match tokio::time::timeout(Duration::from_secs(10), stream.next()).await {
                Ok(Some(Ok(Message::Text(t)))) => {
                    match serde_json::from_str::<ServerMessage>(t.as_str()) {
                        Ok(ServerMessage::Welcome {
                            ice_servers,
                            node_id,
                            ..
                        }) => (ice_servers, node_id),
                        Ok(ServerMessage::Error { message, .. }) => return Outcome::Fatal(message),
                        _ => return Outcome::Retry("unexpected reply from the server".into()),
                    }
                }
                _ => return Outcome::Retry("no reply from the server".into()),
            };
        if self.visitor {
            (self.status)(&format!(
                "Connected to {} as a visitor for account {}.",
                self.config.server_url, self.config.username
            ));
        } else {
            (self.status)(&format!(
                "Connected to {} as \"{}\" for account {}.",
                self.config.server_url, self.config.name, self.config.username
            ));
            // Before anything else: whether this device shares.
            let sharing_now = ClientMessage::Sharing {
                sharing: self.sharing.load(Relaxed),
            };
            if sink.send(send(&sharing_now)).await.is_err() {
                return Outcome::Retry("the connection closed during sign-in".into());
            }
        }
        // A visitor offers nothing: it announces no sounds or outputs.
        let mut last_endpoints = if self.visitor {
            None
        } else {
            self.endpoints().await
        };
        // Other devices as the server last described them (for names).
        let mut known: std::collections::HashMap<String, NodeSummary> =
            std::collections::HashMap::new();
        // Sessions we started whose offer has gone to the server: until
        // then the server does not know them, and forwarding their status
        // would only get "That session has ended" back.
        let mut offered: std::collections::HashSet<String> = std::collections::HashSet::new();
        if let Some(m) = &last_endpoints {
            let _ = sink.send(send(m)).await;
        }
        if let Ok(mut t) = signed_in.lock() {
            *t = Some(Instant::now());
        }
        self.emit(AppEvent::Connected {
            node_id: my_node_id,
        });
        // An app's commands, held for this connection.
        let mut commands = match &self.control {
            Some(c) => {
                let _ = sink.send(send(&ClientMessage::ListNodes)).await;
                Some(c.commands.lock().await)
            }
            None => None,
        };
        // Sessions this device started: where the offer goes, and what it asks.
        let mut outgoing: HashMap<String, (NodeId, SessionMedia)> = HashMap::new();

        let (event_tx, mut event_rx) = mpsc::unbounded_channel::<SessionEvent>();
        let sink_events: session::EventSink = Arc::new(move |e| {
            let _ = event_tx.send(e);
        });
        let mut sessions: HashMap<String, SessionHandle> = HashMap::new();
        let mut refresh = tokio::time::interval(ENDPOINT_REFRESH);
        refresh.tick().await;

        let outcome = loop {
            tokio::select! {
                incoming = stream.next() => match incoming {
                    Some(Ok(Message::Text(t))) => {
                        let Ok(msg) = serde_json::from_str::<ServerMessage>(t.as_str()) else { continue };
                        match msg {
                            ServerMessage::SessionOffer { session_id, media, sdp, .. } => {
                                // Not sharing: nobody may listen to this device.
                                // (The server refuses that too; this also holds
                                // with an older server.)
                                if matches!(media, SessionMedia::Listen { .. }) && !self.sharing.load(Relaxed) {
                                    (self.status)(&format!("Session {session_id} refused: this device is not sharing its audio."));
                                    let refuse = ClientMessage::SessionEnd {
                                        session_id,
                                        reason: format!("\"{}\" is not sharing its audio.", self.config.name),
                                    };
                                    if sink.send(send(&refuse)).await.is_err() {
                                        break Outcome::Retry("the connection closed".into());
                                    }
                                    continue;
                                }
                                let (sources, destinations) = match &last_endpoints {
                                    Some(ClientMessage::Endpoints { sources, destinations }) => (sources.as_slice(), destinations.as_slice()),
                                    _ => (&[][..], &[][..]),
                                };
                                (self.status)(&format!("Session {session_id} requested: {}.", describe(&media, sources, destinations)));
                                let handle = session::start(
                                    session_id.clone(),
                                    LocalMedia::answering(&media),
                                    Negotiation::Answer { offer_sdp: sdp },
                                    session::SessionNetwork {
                                        ice_servers: ice_servers.clone(),
                                        relay_only: self.relay_only,
                                    },
                                    Arc::clone(&self.audio),
                                    Arc::clone(&sink_events),
                                    self.thread_setup.clone(),
                                );
                                if let Some(mut old) = sessions.insert(session_id.as_str().to_owned(), handle) {
                                    old.stop();
                                }
                                self.active_sessions.store(sessions.len(), Relaxed);
                            }
                            ServerMessage::SessionEnd { session_id, reason } => {
                                if let Some(mut h) = sessions.remove(session_id.as_str()) {
                                    tokio::task::spawn_blocking(move || h.stop());
                                    (self.status)(&format!("Session {session_id} ended: {reason}"));
                                }
                                self.active_sessions.store(sessions.len(), Relaxed);
                                if outgoing.remove(session_id.as_str()).is_some() {
                                    self.emit(AppEvent::SessionEnded { session_id, reason });
                                }
                            }
                            ServerMessage::SessionAnswer { session_id, sdp } => {
                                if let Some(h) = sessions.get(session_id.as_str()) {
                                    h.deliver_answer(sdp);
                                }
                            }
                            ServerMessage::SessionStatus { session_id, state, detail } => {
                                // The other device's view of a session we started.
                                if outgoing.contains_key(session_id.as_str()) {
                                    if let Some(detail) = detail {
                                        self.emit(AppEvent::Session { session_id, state, detail });
                                    }
                                }
                            }
                            ServerMessage::Nodes { nodes } => {
                                known = nodes.iter().map(|n| (n.node_id.as_str().to_owned(), n.clone())).collect();
                                self.emit(AppEvent::Devices(nodes));
                            }
                            ServerMessage::NodeUpdate { node } => {
                                known.insert(node.node_id.as_str().to_owned(), node.clone());
                                self.emit(AppEvent::DeviceUpdate(node));
                            }
                            ServerMessage::Error { message, session_id: Some(session_id), .. }
                                if outgoing.contains_key(session_id.as_str()) =>
                            {
                                // An offer the server could not deliver (the
                                // device is offline or does not share).
                                if let Some(mut h) = sessions.remove(session_id.as_str()) {
                                    tokio::task::spawn_blocking(move || h.stop());
                                }
                                offered.remove(session_id.as_str());
                                outgoing.remove(session_id.as_str());
                                self.active_sessions.store(sessions.len(), Relaxed);
                                (self.status)(&format!("Session {session_id} could not start: {message}"));
                                self.emit(AppEvent::SessionEnded { session_id, reason: message });
                            }
                            ServerMessage::Error { message, .. } => {
                                (self.status)(&format!("Server reported a problem: {message}"));
                                self.emit(AppEvent::ServerError { message });
                            }
                            _ => {}
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break Outcome::Retry("the server closed the connection".into()),
                    Some(Err(e)) => break Outcome::Retry(e.to_string()),
                    Some(Ok(_)) => {}
                },
                event = event_rx.recv() => {
                    let Some(event) = event else { continue };
                    let msg = match event {
                        SessionEvent::Answer { session_id, sdp } => ClientMessage::SessionAnswer { session_id, sdp },
                        SessionEvent::Diagnostics { session_id, text } => {
                            if self.measurements_in_status.load(Relaxed) && !text.starts_with("Receiving:") {
                                (self.status)(&format!("Session {session_id}: {text}"));
                            }
                            self.emit(AppEvent::Diagnostics { session_id, text });
                            continue;
                        }
                        SessionEvent::Offer { session_id, sdp } => {
                            let Some((node_id, media)) = outgoing.get(session_id.as_str()).cloned() else { continue };
                            offered.insert(session_id.as_str().to_owned());
                            ClientMessage::SessionOffer { session_id, node_id, media, sdp }
                        }
                        SessionEvent::Status { session_id, state, detail } => {
                            // A session already let go (refused, stopped) may
                            // still report once while its thread winds down:
                            // the server no longer knows it.
                            if !sessions.contains_key(session_id.as_str()) {
                                continue;
                            }
                            (self.status)(&format!("Session {session_id}: {detail}"));
                            if outgoing.contains_key(session_id.as_str()) {
                                self.emit(AppEvent::Session { session_id: session_id.clone(), state, detail: detail.clone() });
                                if !offered.contains(session_id.as_str()) {
                                    continue;
                                }
                            }
                            ClientMessage::SessionStatus { session_id, state, detail: Some(detail) }
                        }
                        SessionEvent::Ended { session_id, reason } => {
                            if sessions.remove(session_id.as_str()).is_none() {
                                continue; // already let go, and the server told
                            }
                            let was_offered = offered.remove(session_id.as_str());
                            self.active_sessions.store(sessions.len(), Relaxed);
                            (self.status)(&format!("Session {session_id} ended: {reason}"));
                            let was_outgoing = outgoing.remove(session_id.as_str()).is_some();
                            if was_outgoing {
                                self.emit(AppEvent::SessionEnded { session_id: session_id.clone(), reason: reason.clone() });
                                if !was_offered {
                                    continue; // the server never knew it
                                }
                            }
                            ClientMessage::SessionEnd { session_id, reason }
                        }
                    };
                    if sink.send(send(&msg)).await.is_err() {
                        break Outcome::Retry("the connection closed".into());
                    }
                }
                command = async {
                    match commands.as_mut() {
                        Some(rx) => rx.recv().await,
                        None => std::future::pending().await,
                    }
                } => {
                    let Some(command) = command else { continue };
                    let msg = match command {
                        Command::ListDevices => Some(ClientMessage::ListNodes),
                        Command::Start { session_id, node_id, remote, local } => {
                            if sessions.contains_key(session_id.as_str()) {
                                None
                            } else if matches!(local, LocalMedia::Send { .. }) && !self.visitor && !self.sharing.load(Relaxed) {
                                // Sending needs sharing; nothing reaches the network.
                                self.emit(AppEvent::SessionEnded {
                                    session_id,
                                    reason: "This device is not sharing its audio. Start sharing to send it.".into(),
                                });
                                None
                            } else {
                                let device = known.get(node_id.as_str());
                                let (sources, destinations) = device
                                    .map(|d| (d.sources.as_slice(), d.destinations.as_slice()))
                                    .unwrap_or((&[][..], &[][..]));
                                (self.status)(&format!(
                                    "Starting session {session_id} with {}: {}.",
                                    device.map_or(node_id.as_str(), |d| d.name.as_str()),
                                    describe(&remote, sources, destinations)
                                ));
                                let handle = session::start(
                                    session_id.clone(),
                                    local,
                                    Negotiation::Offer,
                                    session::SessionNetwork {
                                        ice_servers: ice_servers.clone(),
                                        relay_only: self.relay_only,
                                    },
                                    Arc::clone(&self.audio),
                                    Arc::clone(&sink_events),
                                    self.thread_setup.clone(),
                                );
                                outgoing.insert(session_id.as_str().to_owned(), (node_id, remote));
                                sessions.insert(session_id.as_str().to_owned(), handle);
                                self.active_sessions.store(sessions.len(), Relaxed);
                                None
                            }
                        }
                        Command::SetSharing { sharing } => {
                            self.sharing.store(sharing, Relaxed);
                            (self.status)(if sharing {
                                "Sharing: other devices can listen to this device, and it can send its audio."
                            } else {
                                "Not sharing: this device still receives audio, but sends none of its own."
                            });
                            // The server ends the streams this device sends
                            // (telling both sides).
                            Some(ClientMessage::Sharing { sharing })
                        }
                        Command::SetVolume { session_id, volume, muted } => {
                            if let Some(h) = sessions.get(session_id.as_str()) {
                                h.set_volume(volume, muted);
                            }
                            None
                        }
                        Command::Stop { session_id } => {
                            if let Some(mut h) = sessions.remove(session_id.as_str()) {
                                tokio::task::spawn_blocking(move || h.stop());
                            }
                            self.active_sessions.store(sessions.len(), Relaxed);
                            outgoing.remove(session_id.as_str());
                            self.emit(AppEvent::SessionEnded {
                                session_id: session_id.clone(),
                                reason: "You stopped it.".into(),
                            });
                            Some(ClientMessage::SessionEnd { session_id, reason: "Stopped in the app.".into() })
                        }
                    };
                    if let Some(m) = msg {
                        if sink.send(send(&m)).await.is_err() {
                            break Outcome::Retry("the connection closed".into());
                        }
                    }
                }
                _ = refresh.tick(), if !self.visitor => {
                    let now = self.endpoints().await;
                    if now.is_some() && now != last_endpoints {
                        if let Some(m) = &now {
                            if sink.send(send(m)).await.is_err() {
                                break Outcome::Retry("the connection closed".into());
                            }
                        }
                        last_endpoints = now;
                    }
                }
            }
        };
        // Sessions depend on this signaling connection (the server has
        // already told their peers they ended).
        for (_, mut h) in sessions.drain() {
            tokio::task::spawn_blocking(move || h.stop());
        }
        self.active_sessions.store(0, Relaxed);
        for (id, _) in outgoing.drain() {
            if let Ok(session_id) = SessionId::new(id) {
                self.emit(AppEvent::SessionEnded {
                    session_id,
                    reason: "The connection to the server was lost.".into(),
                });
            }
        }
        outcome
    }
}

/// What a session does, in words, with the sound's or output's name when
/// known (its id otherwise).
fn describe(
    media: &audionet_protocol::signal::SessionMedia,
    sources: &[SourceInfo],
    destinations: &[DestinationInfo],
) -> String {
    match media {
        audionet_protocol::signal::SessionMedia::Listen { source_id } => {
            let name = sources
                .iter()
                .find(|s| &s.id == source_id)
                .map_or(source_id.as_str(), |s| s.name.as_str());
            format!("send audio from {name}")
        }
        audionet_protocol::signal::SessionMedia::Speak { destination_id } => {
            let name = destinations
                .iter()
                .find(|d| &d.id == destination_id)
                .map_or(destination_id.as_str(), |d| d.name.as_str());
            format!("play received audio on {name}")
        }
    }
}

/// Convenience for tests and tools: a status sink that prints lines.
pub fn print_status() -> StatusFn {
    Arc::new(|s| println!("{s}"))
}

#[allow(dead_code)]
fn _assert_send(_: SessionState, _: SessionId) {}

/// TLS for the signaling WebSocket: certificates are checked by the operating
/// system's verifier (as ureq does for HTTP), which also works on iOS, where
/// no native root store can be read.
fn tls_config() -> Result<tokio_tungstenite::Connector, rustls::Error> {
    use rustls_platform_verifier::BuilderVerifierExt;
    let provider = std::sync::Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_platform_verifier()?
        .with_no_client_auth();
    Ok(tokio_tungstenite::Connector::Rustls(std::sync::Arc::new(
        config,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    #[test]
    fn backoff_doubles_to_thirty_seconds() {
        let mut wait = Duration::ZERO;
        let mut waits = Vec::new();
        for _ in 0..8 {
            wait = next_backoff(wait, None);
            waits.push(wait.as_secs());
        }
        assert_eq!(waits, [1, 2, 4, 8, 16, 30, 30, 30]);
    }

    #[test]
    fn backoff_starts_again_after_a_connection_that_lasted() {
        // Several failures, then a connection that held for an hour.
        assert_eq!(next_backoff(secs(30), Some(secs(3600))), secs(1));
        assert_eq!(next_backoff(secs(16), Some(STABLE_CONNECTION)), secs(1));
    }

    #[test]
    fn backoff_keeps_growing_after_connections_dropped_at_once() {
        // Signed in, then dropped within seconds: still a failing server.
        assert_eq!(next_backoff(secs(4), Some(secs(2))), secs(8));
        assert_eq!(next_backoff(secs(30), Some(secs(9))), secs(30));
    }
}
