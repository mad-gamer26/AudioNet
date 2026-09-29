//! `audionet-visitor`: the AudioNet engine for the NVDA add-on.
//!
//! The add-on starts this program and drives it through JSON lines (see
//! [`protocol`]). It runs outside NVDA's process on purpose: audio threads,
//! networking and any failure stay away from the screen reader, which must
//! never be slowed or stopped by streaming.
//!
//! Each account is a visitor (see `audionet_node::agent`): signed in with a
//! web session like the web client, never a device of the account, it lists
//! the account's devices, listens to them on this computer, and sends this
//! computer's microphones and sounds to them.

#![forbid(unsafe_code)]
// No console window when NVDA starts it; its standard input and output are
// the add-on's pipes.
#![windows_subsystem = "windows"]

mod protocol;

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};

use audionet_node::agent::{Agent, AppEvent, Command, Control};
use audionet_node::audio::NodeAudio;
use audionet_node::config::NodeConfig;
#[cfg(not(windows))]
use audionet_node::cpal_audio::CpalNodeAudio as PlatformAudio;
use audionet_node::session::LocalMedia;
#[cfg(windows)]
use audionet_node::windows_audio::WasapiNodeAudio as PlatformAudio;
use audionet_protocol::signal::SessionMedia;
use audionet_protocol::{NodeId, Platform, SessionId};
use protocol::{Cmd, Request};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

/// Standard output, one JSON line at a time.
#[derive(Clone)]
struct Out(Arc<Mutex<std::io::Stdout>>);

impl Out {
    fn line(&self, v: &Value) {
        let mut out = self.0.lock().unwrap_or_else(|e| e.into_inner());
        // The add-on reading this is gone if writing fails; standard input
        // closing ends the program.
        let _ = writeln!(out, "{v}");
        let _ = out.flush();
    }

    fn reply(&self, id: u64, result: Result<Value, String>) {
        self.line(&match result {
            Ok(r) => json!({ "id": id, "ok": true, "result": r, "error": null }),
            Err(e) => json!({ "id": id, "ok": false, "result": null, "error": e }),
        });
    }
}

/// A connected account.
struct Account {
    commands: mpsc::UnboundedSender<Command>,
    stop: Option<oneshot::Sender<()>>,
}

/// What reaches the main loop.
enum Input {
    Request(Request),
    /// A line that is not a request (answered with its problem).
    Bad(String),
    /// Standard input closed.
    Closed,
}

fn this_platform() -> Platform {
    if cfg!(windows) {
        Platform::Windows
    } else if cfg!(target_os = "macos") {
        Platform::MacOs
    } else {
        Platform::Linux
    }
}

fn new_session_id() -> Result<SessionId, String> {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    SessionId::new(format!("nvda-{nanos:x}-{}", NEXT.fetch_add(1, Relaxed)))
        .map_err(|e| e.to_string())
}

/// An agent event as the add-on reads it.
fn event_json(account: &str, e: AppEvent) -> Value {
    match e {
        AppEvent::Connected { .. } => json!({ "event": "connected", "account": account }),
        AppEvent::Disconnected { reason } => {
            json!({ "event": "disconnected", "account": account, "reason": reason })
        }
        AppEvent::Devices(devices) => {
            json!({ "event": "devices", "account": account, "devices": devices })
        }
        AppEvent::DeviceUpdate(device) => {
            json!({ "event": "device_update", "account": account, "device": device })
        }
        AppEvent::Session {
            session_id,
            state,
            detail,
        } => json!({
            "event": "session", "account": account, "session_id": session_id,
            "state": state, "detail": detail,
        }),
        AppEvent::SessionEnded { session_id, reason } => json!({
            "event": "session_ended", "account": account, "session_id": session_id, "reason": reason,
        }),
        AppEvent::Diagnostics { session_id, text } => json!({
            "event": "diagnostics", "account": account, "session_id": session_id, "text": text,
        }),
        AppEvent::ServerError { message } => {
            json!({ "event": "server_error", "account": account, "message": message })
        }
    }
}

struct Engine {
    out: Out,
    accounts: HashMap<String, Account>,
    /// Which account each stream runs in.
    sessions: Arc<Mutex<HashMap<String, String>>>,
}

impl Engine {
    fn connect(&mut self, account: String, server: String, username: String, token: String) {
        self.disconnect(&account);
        let out = self.out.clone();
        let name = account.clone();
        let sessions = Arc::clone(&self.sessions);
        let (control, commands) = Control::new(Arc::new(move |e: AppEvent| {
            if let AppEvent::SessionEnded { session_id, .. } = &e {
                sessions
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(session_id.as_str());
            }
            out.line(&event_json(&name, e));
        }));
        let status_out = self.out.clone();
        let status_name = account.clone();
        let agent = Agent {
            config: NodeConfig {
                server_url: server,
                node_id: String::new(),
                token,
                name: "NVDA".into(),
                username,
            },
            audio: Arc::new(PlatformAudio),
            platform: this_platform(),
            software: format!("audionet-nvda {}", env!("CARGO_PKG_VERSION")),
            status: Arc::new(move |text: &str| {
                status_out
                    .line(&json!({ "event": "status", "account": status_name, "text": text }));
            }),
            #[cfg(windows)]
            thread_setup: Some(audionet_wasapi::capture::audio_thread_setup()),
            #[cfg(not(windows))]
            thread_setup: None,
            active_sessions: Default::default(),
            control: Some(control),
            relay_only: false,
            sharing: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            visitor: true,
        };
        let (stop_tx, stop_rx) = oneshot::channel::<()>();
        let out = self.out.clone();
        let name = account.clone();
        tokio::spawn(async move {
            let result = agent
                .run(async {
                    let _ = stop_rx.await;
                })
                .await;
            // The sign-in was refused for good (it ended or expired).
            if let Err(reason) = result {
                out.line(&json!({ "event": "stopped", "account": name, "reason": reason }));
            }
        });
        self.accounts.insert(
            account,
            Account {
                commands,
                stop: Some(stop_tx),
            },
        );
    }

    fn disconnect(&mut self, account: &str) {
        if let Some(mut a) = self.accounts.remove(account) {
            if let Some(stop) = a.stop.take() {
                let _ = stop.send(());
            }
        }
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|_, a| a != account);
    }

    fn command(&self, account: &str, c: Command) -> Result<(), String> {
        let a = self
            .accounts
            .get(account)
            .ok_or_else(|| format!("the account {account} is not connected"))?;
        a.commands
            .send(c)
            .map_err(|_| format!("the account {account} is not connected"))
    }

    fn account_of(&self, session_id: &str) -> Result<String, String> {
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(session_id)
            .cloned()
            .ok_or_else(|| "that stream has ended".to_owned())
    }

    fn start(
        &self,
        account: String,
        node_id: &str,
        remote: SessionMedia,
        local: LocalMedia,
    ) -> Result<Value, String> {
        let node_id = NodeId::new(node_id.to_owned()).map_err(|e| e.to_string())?;
        let session_id = new_session_id()?;
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(session_id.as_str().to_owned(), account.clone());
        self.command(
            &account,
            Command::Start {
                session_id: session_id.clone(),
                node_id,
                remote,
                local,
            },
        )?;
        Ok(json!({ "session_id": session_id }))
    }

    /// Handles one request; `false` when the program should exit.
    fn handle(&mut self, r: Request) -> bool {
        let id = r.id;
        let out = self.out.clone();
        let result = match r.cmd {
            Cmd::Hello => Ok(json!({
                "name": "audionet-visitor",
                "version": env!("CARGO_PKG_VERSION"),
                "protocol": 1,
            })),
            Cmd::SignIn {
                server,
                username,
                password,
            } => {
                // Network work off the main loop; the reply comes when done.
                tokio::task::spawn_blocking(move || {
                    let result =
                        audionet_node::config::normalize_server_url(&server).and_then(|url| {
                            audionet_node::account::visitor_sign_in(&url, &username, &password)
                                .map(|(user, token)| {
                                    json!({ "server": url, "username": user, "token": token })
                                })
                        });
                    out.reply(id, result);
                });
                return true;
            }
            Cmd::SignOut { server, token } => {
                tokio::task::spawn_blocking(move || {
                    out.reply(
                        id,
                        audionet_node::account::visitor_sign_out(&server, &token)
                            .map(|()| json!({})),
                    );
                });
                return true;
            }
            Cmd::Connect {
                account,
                server,
                username,
                token,
            } => {
                self.connect(account, server, username, token);
                Ok(json!({}))
            }
            Cmd::Disconnect { account } => {
                self.disconnect(&account);
                Ok(json!({}))
            }
            Cmd::ListDevices { account } => self
                .command(&account, Command::ListDevices)
                .map(|()| json!({})),
            Cmd::LocalAudio => {
                tokio::task::spawn_blocking(move || {
                    out.reply(
                        id,
                        PlatformAudio.endpoints().map(|(sources, destinations)| {
                            json!({ "sources": sources, "destinations": destinations })
                        }),
                    );
                });
                return true;
            }
            Cmd::Listen {
                account,
                node_id,
                source_id,
                destination_id,
            } => self.start(
                account,
                &node_id,
                SessionMedia::Listen { source_id },
                LocalMedia::Receive { destination_id },
            ),
            Cmd::Send {
                account,
                node_id,
                destination_id,
                source_id,
            } => self.start(
                account,
                &node_id,
                SessionMedia::Speak { destination_id },
                LocalMedia::Send { source_id },
            ),
            Cmd::Stop { session_id } => self.account_of(&session_id).and_then(|account| {
                let session_id = SessionId::new(session_id).map_err(|e| e.to_string())?;
                self.command(&account, Command::Stop { session_id })
                    .map(|()| json!({}))
            }),
            Cmd::SetVolume {
                session_id,
                volume,
                muted,
            } => self.account_of(&session_id).and_then(|account| {
                let session_id = SessionId::new(session_id).map_err(|e| e.to_string())?;
                self.command(
                    &account,
                    Command::SetVolume {
                        session_id,
                        volume: volume.clamp(0.0, 1.0),
                        muted,
                    },
                )
                .map(|()| json!({}))
            }),
            Cmd::Quit => {
                self.out.reply(id, Ok(json!({})));
                return false;
            }
        };
        self.out.reply(id, result);
        true
    }

    fn shut_down(&mut self) {
        let names: Vec<String> = self.accounts.keys().cloned().collect();
        for n in names {
            self.disconnect(&n);
        }
    }
}

fn main() {
    let out = Out(Arc::new(Mutex::new(std::io::stdout())));
    let (tx, mut rx) = mpsc::unbounded_channel::<Input>();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if line.trim().is_empty() {
                continue;
            }
            let input = match serde_json::from_str::<Request>(&line) {
                Ok(r) => Input::Request(r),
                Err(e) => Input::Bad(format!("not a request: {e}")),
            };
            if tx.send(input).is_err() {
                return;
            }
        }
        let _ = tx.send(Input::Closed);
    });
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            out.line(&json!({ "event": "fatal", "message": format!("could not start: {e}") }));
            return;
        }
    };
    rt.block_on(async {
        let mut engine = Engine {
            out: out.clone(),
            accounts: HashMap::new(),
            sessions: Arc::default(),
        };
        while let Some(input) = rx.recv().await {
            match input {
                Input::Request(r) => {
                    if !engine.handle(r) {
                        break;
                    }
                }
                Input::Bad(problem) => out.line(&json!({
                    "id": null, "ok": false, "result": null, "error": problem,
                })),
                Input::Closed => break,
            }
        }
        engine.shut_down();
        // Let the agents end their streams on the server.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    });
    rt.shutdown_timeout(std::time::Duration::from_secs(2));
}
