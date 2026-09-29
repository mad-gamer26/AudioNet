//! Application logic, independent of the window: signing in and running the
//! device agent on background threads, reporting back through callbacks.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed};
use std::thread::JoinHandle;

use audionet_node::agent::{Agent, AppEventFn, Command, Control};
use audionet_node::audio::NodeAudio;
use audionet_node::config::{NodeConfig, default_path};
use audionet_node::windows_audio::WasapiNodeAudio;
use audionet_protocol::Platform;
use audionet_protocol::signal::{DestinationInfo, SourceInfo};
use tokio::sync::{mpsc, oneshot};

/// The server URL official builds suggest, set at build time with
/// `AUDIONET_DEFAULT_SERVER`. Source builds leave it empty: nothing in the
/// code depends on a particular server.
pub fn default_server() -> &'static str {
    option_env!("AUDIONET_DEFAULT_SERVER").unwrap_or("")
}

/// Where this computer's other accounts are kept: one file per account next
/// to the first account's `node.toml` (which stays where the command-line
/// agent also looks), in `accounts\<device id>.toml`.
fn accounts_dir() -> std::path::PathBuf {
    default_path().parent().map_or_else(
        || std::path::PathBuf::from("accounts"),
        |p| p.join("accounts"),
    )
}

fn account_path(node_id: &str) -> std::path::PathBuf {
    // Device ids are the server's own ids; keep only safe characters.
    let safe: String = node_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    accounts_dir().join(format!("{safe}.toml"))
}

/// Every account this computer is signed in to: the first in `node.toml`,
/// then the others (oldest first).
pub fn load_configs() -> Vec<NodeConfig> {
    let mut all: Vec<NodeConfig> = NodeConfig::load(&default_path()).ok().into_iter().collect();
    let mut others: Vec<(std::time::SystemTime, NodeConfig)> = std::fs::read_dir(accounts_dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "toml"))
        .filter_map(|e| {
            let when = e.metadata().and_then(|m| m.modified()).ok()?;
            Some((when, NodeConfig::load(&e.path()).ok()?))
        })
        .collect();
    others.sort_by_key(|(when, _)| *when);
    all.extend(others.into_iter().map(|(_, c)| c));
    all
}

/// Keeps a new account's sign-in: in `node.toml` if it is the first one,
/// otherwise with the other accounts.
fn keep_config(c: &NodeConfig) -> Result<(), String> {
    if default_path().exists() {
        std::fs::create_dir_all(accounts_dir())
            .map_err(|e| format!("could not create {}: {e}", accounts_dir().display()))?;
        c.save(&account_path(&c.node_id))
    } else {
        c.save(&default_path())
    }
}

/// Forgets one account's sign-in on this computer. When it was the first
/// one, the next account takes its place in `node.toml`.
pub fn forget_config(node_id: &str) -> Result<(), String> {
    let first = NodeConfig::load(&default_path()).ok();
    if first.as_ref().is_some_and(|c| c.node_id == node_id) {
        let path = default_path();
        std::fs::remove_file(&path)
            .map_err(|e| format!("could not remove {}: {e}", path.display()))?;
        if let Some(next) = load_configs().into_iter().next() {
            next.save(&path)?;
            let _ = std::fs::remove_file(account_path(&next.node_id));
        }
        return Ok(());
    }
    let path = account_path(node_id);
    if path.exists() {
        std::fs::remove_file(&path)
            .map_err(|e| format!("could not remove {}: {e}", path.display()))?;
    }
    Ok(())
}

/// The account in words: "mad-gamer26 on audionet.example.com".
pub fn account_name(c: &NodeConfig) -> String {
    let host = c
        .server_url
        .split("://")
        .nth(1)
        .unwrap_or(&c.server_url)
        .trim_end_matches('/');
    format!("{} on {host}", c.username)
}

/// Whether two sign-ins are the same account (same server and name).
pub fn same_account(a: &NodeConfig, server: &str, username: &str) -> bool {
    a.username.eq_ignore_ascii_case(username)
        && a.server_url
            .trim_end_matches('/')
            .eq_ignore_ascii_case(server.trim_end_matches('/'))
}

pub fn computer_name() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_else(|_| "Windows PC".into())
}

/// Signs in with the account password on a background thread.
pub fn sign_in_async(
    server: String,
    username: String,
    password: String,
    name: String,
    done: impl FnOnce(Result<NodeConfig, String>) + Send + 'static,
) {
    std::thread::spawn(move || {
        let result = audionet_node::account::sign_in(
            &server,
            &username,
            &password,
            &name,
            Platform::Windows,
        )
        .and_then(|c| {
            keep_config(&c)?;
            Ok(c)
        });
        done(result);
    });
}

/// Removes this computer from an account on the server (signing out) on
/// a background thread; `done` gets the result.
pub fn remove_async(config: NodeConfig, done: impl FnOnce(Result<(), String>) + Send + 'static) {
    std::thread::spawn(move || done(audionet_node::account::remove_device(&config)));
}

/// This computer's audio sources and outputs (control path).
pub fn local_endpoints() -> Result<(Vec<SourceInfo>, Vec<DestinationInfo>), String> {
    WasapiNodeAudio.endpoints()
}

/// A running device agent: this computer online in one account.
pub struct Running {
    stop: Option<oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
    active_sessions: Arc<AtomicUsize>,
    commands: mpsc::UnboundedSender<Command>,
    /// Whether it shares this computer's audio there (the agent reads it
    /// on every connection).
    sharing: Arc<AtomicBool>,
}

impl Running {
    /// Starts the agent. `status` receives plain-text status lines; `ended`
    /// is called once when the agent stops (with an error if it failed).
    pub fn start(
        config: NodeConfig,
        sharing: bool,
        status: Arc<dyn Fn(&str) + Send + Sync>,
        events: AppEventFn,
        ended: impl FnOnce(Option<String>) + Send + 'static,
    ) -> Running {
        let (tx, rx) = oneshot::channel::<()>();
        let (control, commands) = Control::new(events);
        let active_sessions = Arc::new(AtomicUsize::new(0));
        let sessions = Arc::clone(&active_sessions);
        let sharing = Arc::new(AtomicBool::new(sharing));
        let agent_sharing = Arc::clone(&sharing);
        let thread = std::thread::spawn(move || {
            let rt = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    ended(Some(format!("could not start: {e}")));
                    return;
                }
            };
            let agent = Agent {
                config,
                audio: Arc::new(WasapiNodeAudio),
                platform: Platform::Windows,
                software: format!("audionet-desktop {}", env!("CARGO_PKG_VERSION")),
                status,
                thread_setup: Some(audionet_wasapi::capture::audio_thread_setup()),
                active_sessions: sessions,
                control: Some(control),
                relay_only: false,
                visitor: false,
                sharing: agent_sharing,
            };
            let result = rt.block_on(agent.run(async {
                let _ = rx.await;
            }));
            ended(result.err());
        });
        Running {
            stop: Some(tx),
            thread: Some(thread),
            active_sessions,
            commands,
            sharing,
        }
    }

    /// Whether this computer shares its audio in this account.
    pub fn is_sharing(&self) -> bool {
        self.sharing.load(Relaxed)
    }

    /// Starts or stops sharing: at once for the next connection, and the
    /// agent tells the server now (which ends what this computer sends).
    pub fn set_sharing(&self, on: bool) {
        self.sharing.store(on, Relaxed);
        self.command(Command::SetSharing { sharing: on });
    }

    /// Sends a remote-control command to the agent.
    pub fn command(&self, command: Command) {
        let _ = self.commands.send(command);
    }

    /// Audio sessions currently running (someone is listening or talking).
    pub fn active_sessions(&self) -> usize {
        self.active_sessions.load(Relaxed)
    }

    pub fn stop(&mut self) {
        if let Some(tx) = self.stop.take() {
            let _ = tx.send(());
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop();
    }
}
