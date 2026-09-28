//! `audionet node …`: sign this computer in to an AudioNet account and run
//! the node agent (WASAPI audio on Windows, cpal elsewhere).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use audionet_node::agent::{Agent, print_status};
use audionet_node::audio::NodeAudio;
use audionet_node::config::{NodeConfig, default_path};
#[cfg(not(windows))]
use audionet_node::cpal_audio::CpalNodeAudio as PlatformAudio;
#[cfg(windows)]
use audionet_node::windows_audio::WasapiNodeAudio as PlatformAudio;
use audionet_protocol::Platform;

fn this_platform() -> Platform {
    if cfg!(windows) {
        Platform::Windows
    } else if cfg!(target_os = "macos") {
        Platform::MacOs
    } else {
        Platform::Linux
    }
}

fn default_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .or_else(|| {
            std::process::Command::new("hostname")
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| "AudioNet device".into())
}

fn config_path(path: Option<PathBuf>) -> PathBuf {
    path.unwrap_or_else(default_path)
}

pub fn sign_in(
    server: &str,
    user: &str,
    password_stdin: bool,
    name: Option<String>,
    config: Option<PathBuf>,
) -> Result<(), String> {
    let name = name.unwrap_or_else(default_name);
    let path = config_path(config);
    let password = if password_stdin {
        let mut line = String::new();
        std::io::stdin()
            .read_line(&mut line)
            .map_err(|e| format!("could not read the password: {e}"))?;
        line.trim_end_matches(['\r', '\n']).to_owned()
    } else {
        rpassword::prompt_password(format!("Password for {user}: "))
            .map_err(|e| format!("could not read the password: {e}"))?
    };
    if password.is_empty() {
        return Err("no password was entered".into());
    }
    let node = audionet_node::account::sign_in(server, user, &password, &name, this_platform())?;
    node.save(&path)?;
    println!(
        "Signed in \"{}\" to account {} on {}. Settings saved to {}.\nStart it with: audionet node run",
        node.name,
        node.username,
        node.server_url,
        path.display()
    );
    Ok(())
}

pub fn status(config: Option<PathBuf>) -> Result<(), String> {
    let path = config_path(config);
    let c = NodeConfig::load(&path)?;
    println!("Device name: {}", c.name);
    println!("Server: {}", c.server_url);
    println!("Account: {}", c.username);
    println!("Device identifier: {}", c.node_id);
    println!("Settings file: {}", path.display());
    Ok(())
}

/// How long `--background` watches the new process for a startup failure
/// before reporting it as running.
const BACKGROUND_STARTUP_CHECK: Duration = Duration::from_secs(2);

/// The log of a background device: next to its settings file, with the same
/// name (`node.toml` logs to `node.log`). Replaced at each start.
fn background_log_path(config: &Path) -> PathBuf {
    config.with_extension("log")
}

/// `audionet node run --background`: starts this program again as a
/// separate process without a window (the device itself, writing to a log
/// file), checks it got going, and returns.
pub fn run_in_background(config: Option<PathBuf>) -> Result<(), String> {
    let path = config_path(config);
    // Settings problems are reported here, not only in the log.
    let c = NodeConfig::load(&path)?;
    let path = std::path::absolute(&path)
        .map_err(|e| format!("could not find the settings file {}: {e}", path.display()))?;
    let log_path = background_log_path(&path);
    let log = std::fs::File::create(&log_path)
        .map_err(|e| format!("could not create the log file {}: {e}", log_path.display()))?;
    let log_err = log
        .try_clone()
        .map_err(|e| format!("could not open the log file {}: {e}", log_path.display()))?;
    let exe =
        std::env::current_exe().map_err(|e| format!("could not find the audionet program: {e}"))?;
    let mut cmd = Command::new(exe);
    cmd.args(["node", "run", "--background-child", "--config"])
        .arg(&path)
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(log_err);
    detach(&mut cmd);
    #[cfg(windows)]
    audionet_wasapi::process::stop_inheriting_std_handles();
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("could not start the background device: {e}"))?;

    let started = Instant::now();
    while started.elapsed() < BACKGROUND_STARTUP_CHECK {
        if let Some(status) = child
            .try_wait()
            .map_err(|e| format!("could not check the background device: {e}"))?
        {
            let log_text = std::fs::read_to_string(&log_path).unwrap_or_default();
            let reason = log_text.trim();
            return Err(format!(
                "the background device stopped at once ({status}).{}{} Log file: {}",
                if reason.is_empty() { "" } else { " It said: " },
                reason,
                log_path.display()
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let pid = child.id();
    println!("AudioNet node \"{}\" is running in the background.", c.name);
    println!("Process ID: {pid}");
    println!("Log file: {}", log_path.display());
    if cfg!(windows) {
        println!("To stop it: Stop-Process -Id {pid} (in PowerShell) or taskkill /PID {pid} /F");
    } else {
        println!("To stop it: kill {pid}");
    }
    Ok(())
}

/// Windows: no console window (the device has none to show or close) and
/// its own process group, so Ctrl+C in the starting window does not reach it.
#[cfg(windows)]
fn detach(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
}

/// macOS and Linux: its own process group, so Ctrl+C in the terminal, or the
/// shell's hang-up when the terminal closes, does not reach it.
#[cfg(unix)]
fn detach(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    cmd.process_group(0);
}

#[cfg(not(any(windows, unix)))]
fn detach(_cmd: &mut Command) {}

pub fn run(config: Option<PathBuf>, background: bool) -> Result<(), String> {
    let path = config_path(config);
    let c = NodeConfig::load(&path)?;
    let (source_count, dest_count) = PlatformAudio
        .endpoints()
        .map(|(s, d)| (s.len(), d.len()))
        .unwrap_or((0, 0));
    let how_to_stop = if background {
        format!(
            "Running in the background as process {}.",
            std::process::id()
        )
    } else {
        "Press Ctrl+C to stop.".to_owned()
    };
    println!(
        "AudioNet node \"{}\" starting. {} sources and {} destinations available. {}",
        c.name, source_count, dest_count, how_to_stop
    );
    let agent = Agent {
        config: c,
        audio: Arc::new(PlatformAudio),
        platform: this_platform(),
        software: format!("audionet-node {}", env!("CARGO_PKG_VERSION")),
        status: print_status(),
        #[cfg(windows)]
        thread_setup: Some(audionet_wasapi::capture::audio_thread_setup()),
        #[cfg(not(windows))]
        thread_setup: None,
        active_sessions: Default::default(),
        control: None,
        relay_only: false,
        // A plain device shares: that is what it runs for.
        sharing: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
    };
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    let result = rt.block_on(agent.run(async {
        let _ = tokio::signal::ctrl_c().await;
    }));
    println!("AudioNet node stopped.");
    result
}
