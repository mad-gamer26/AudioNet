//! `audionet`: the AudioNet command-line tool.

#![forbid(unsafe_code)]

#[cfg(windows)]
mod capture_test;
mod node_cmds;
#[cfg(windows)]
mod stream_cmds;

use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use audionet_audio::{EndpointEnumerator, EnumerationOptions};
use audionet_cli::list;
use audionet_cli::select::EndpointSelector;
use audionet_wasapi::WasapiEnumerator;
use clap::{ArgGroup, Parser, Subcommand};

/// AudioNet: low-latency network audio streaming and routing.
#[derive(Debug, Parser)]
#[command(name = "audionet", bin_name = "audionet", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum NodeAction {
    /// Add this computer to your account: sign in with your AudioNet account
    /// name and password (asked for without showing it).
    SignIn {
        /// Server address, e.g. https://audionet.example.com
        #[arg(long, value_name = "URL")]
        server: String,
        /// Your AudioNet account name.
        #[arg(long, value_name = "NAME")]
        user: String,
        /// Read the password from the first line of standard input instead
        /// of asking (for scripts).
        #[arg(long)]
        password_stdin: bool,
        /// Name for this device (defaults to the computer name).
        #[arg(long)]
        name: Option<String>,
        /// Settings file (defaults to the per-user AudioNet folder).
        #[arg(long, value_name = "FILE")]
        config: Option<PathBuf>,
    },
    /// Run as a device: stay connected and serve audio sessions.
    Run {
        #[arg(long, value_name = "FILE")]
        config: Option<PathBuf>,
        /// Run in the background without a window: start the device, print
        /// its process ID and log file, and return to the prompt.
        #[arg(short = 'b', long)]
        background: bool,
        /// Internal: this process is the background device started by
        /// --background.
        #[arg(long, hide = true)]
        background_child: bool,
    },
    /// Show which server and account this device is signed in to.
    Status {
        #[arg(long, value_name = "FILE")]
        config: Option<PathBuf>,
    },
}

#[derive(Debug, Subcommand)]
enum Command {
    /// List the audio input and output devices on this computer.
    List {
        /// Print machine-readable JSON instead of text.
        #[arg(long)]
        json: bool,
        /// Also list devices Windows remembers but that are not present.
        #[arg(long)]
        all: bool,
    },
    /// Capture audio and report capture timing and buffer diagnostics.
    ///
    /// DEVICE is "default", a device number from `audionet list` (for
    /// example 3 for "Input device 3 of 11"), or a device identifier.
    #[command(group(ArgGroup::new("source").required(true).args(["input", "loopback"])))]
    CaptureTest {
        /// Capture from this input device.
        #[arg(long, value_name = "DEVICE")]
        input: Option<EndpointSelector>,
        /// Capture what this output device is playing.
        #[arg(long, value_name = "DEVICE")]
        loopback: Option<EndpointSelector>,
        /// How long to capture, in seconds. 0 captures until Ctrl+C.
        #[arg(long, default_value_t = 10)]
        seconds: u64,
        /// Seconds between diagnostic reports.
        #[arg(long, default_value_t = 5, value_name = "SECONDS")]
        report_every: u64,
        /// Also write the captured audio to this WAV file (32-bit float).
        #[arg(long, value_name = "FILE")]
        wav: Option<PathBuf>,
        /// Capacity of the capture ring buffer, in milliseconds.
        #[arg(long, default_value_t = 500, value_name = "MS",
              value_parser = clap::value_parser!(u32).range(20..=10_000))]
        ring_ms: u32,
        /// Do not register the capture thread with MMCSS ("Pro Audio"). MMCSS is on by default:
        /// it keeps audio threads on time under heavy CPU load.
        #[arg(long = "no-mmcss", action = clap::ArgAction::SetFalse)]
        mmcss: bool,
        /// If the device disconnects, wait for it to return and resume.
        #[arg(long)]
        reconnect: bool,
        /// Test only: pause the consumer once, halfway through, for this many
        /// milliseconds, to exercise stale trimming and overflow handling.
        #[arg(long, value_name = "MS")]
        simulate_consumer_stall: Option<u64>,
        /// Print the final report as JSON; progress text goes to stderr.
        #[arg(long)]
        json: bool,
    },
    /// Play a steady 997 Hz test tone at -12 dBFS on an output device.
    ToneTest {
        /// Output device: "default", a number from `audionet list`, or an identifier.
        #[arg(long, value_name = "DEVICE", default_value = "default")]
        output: EndpointSelector,
        /// How long to play, in seconds.
        #[arg(long, default_value_t = 3)]
        seconds: u64,
    },
    /// Connect this computer to an AudioNet server as a device.
    Node {
        #[command(subcommand)]
        action: NodeAction,
    },
    /// Create a new pre-shared key file for encrypted streaming.
    Keygen {
        /// Where to write the key.
        #[arg(long, value_name = "FILE", default_value = "audionet.key")]
        output: PathBuf,
        /// Replace an existing key file.
        #[arg(long)]
        force: bool,
    },
    /// Stream audio from this computer to another AudioNet receiver.
    ///
    /// DEVICE is "default", a device number from `audionet list`, or an identifier.
    #[command(group(ArgGroup::new("source").required(true).args(["input", "loopback"])))]
    Send {
        /// Send from this input device.
        #[arg(long, value_name = "DEVICE")]
        input: Option<EndpointSelector>,
        /// Send what this output device is playing.
        #[arg(long, value_name = "DEVICE")]
        loopback: Option<EndpointSelector>,
        /// Receiver address: HOST or HOST:PORT (default port 5004).
        #[arg(long, value_name = "HOST:PORT")]
        to: String,
        /// Pre-shared key file (see `audionet keygen`).
        #[arg(long, value_name = "FILE", default_value = "audionet.key")]
        key_file: PathBuf,
        /// Opus bitrate in kilobits per second.
        #[arg(long, default_value_t = 128, value_name = "KBPS")]
        bitrate: u32,
        /// Use in-band forward error correction (about 4 ms more delay).
        #[arg(long)]
        resilient: bool,
        /// How long to send, in seconds. 0 sends until Ctrl+C.
        #[arg(long, default_value_t = 0)]
        seconds: u64,
        /// Seconds between diagnostic reports.
        #[arg(long, default_value_t = 10, value_name = "SECONDS")]
        report_every: u64,
        /// Do not register the capture thread with MMCSS ("Pro Audio"). MMCSS is on by default:
        /// it keeps audio threads on time under heavy CPU load.
        #[arg(long = "no-mmcss", action = clap::ArgAction::SetFalse)]
        mmcss: bool,
        /// Print the final report as JSON; progress text goes to stderr.
        #[arg(long)]
        json: bool,
    },
    /// Receive an AudioNet stream and play it on this computer.
    Receive {
        /// Port, or address and port, to listen on.
        #[arg(long, default_value = "5004", value_name = "[ADDRESS:]PORT")]
        listen: String,
        /// Pre-shared key file (see `audionet keygen`).
        #[arg(long, value_name = "FILE", default_value = "audionet.key")]
        key_file: PathBuf,
        /// Output device: "default", a number from `audionet list`, or an identifier.
        #[arg(long, value_name = "DEVICE", default_value = "default")]
        output: EndpointSelector,
        /// Playout buffer target in milliseconds. It adapts to the network
        /// (25 to 200 ms) unless --fixed-target is given; this is where it starts.
        #[arg(long, default_value_t = 40, value_name = "MS",
              value_parser = clap::value_parser!(u32).range(10..=500))]
        target_ms: u32,
        /// Keep the playout target at --target-ms instead of adapting it.
        #[arg(long)]
        fixed_target: bool,
        /// How long to receive, in seconds. 0 receives until Ctrl+C.
        #[arg(long, default_value_t = 0)]
        seconds: u64,
        /// Seconds between diagnostic reports.
        #[arg(long, default_value_t = 10, value_name = "SECONDS")]
        report_every: u64,
        /// Do not register the playback thread with MMCSS ("Pro Audio"). MMCSS is on by default:
        /// it keeps audio threads on time under heavy CPU load.
        #[arg(long = "no-mmcss", action = clap::ArgAction::SetFalse)]
        mmcss: bool,
        /// Print the final report as JSON; progress text goes to stderr.
        #[arg(long)]
        json: bool,
        /// Test only: stop consuming audio for this many milliseconds, once,
        /// to exercise buffer-depth recovery.
        #[arg(long, value_name = "MS")]
        simulate_render_stall: Option<u64>,
        /// Test only: when to simulate the stall, in seconds after playback starts.
        #[arg(long, default_value_t = 20, value_name = "SECONDS")]
        stall_at: u64,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::List { json, all } => run_list(json, all),
        Command::CaptureTest {
            input,
            loopback,
            seconds,
            report_every,
            wav,
            ring_ms,
            mmcss,
            reconnect,
            simulate_consumer_stall,
            json,
        } => {
            use audionet_audio::capture::CaptureMode;
            let (mode, selector) = match (input, loopback) {
                (Some(s), _) => (CaptureMode::Input, s),
                (None, Some(s)) => (CaptureMode::Loopback, s),
                (None, None) => unreachable!("clap requires one of --input or --loopback"),
            };
            run_capture_test(CaptureArgs {
                mode,
                selector,
                seconds,
                report_every,
                wav,
                ring_ms,
                mmcss,
                reconnect,
                simulate_consumer_stall_ms: simulate_consumer_stall,
                json,
            })
        }
        Command::Keygen { output, force } => finish(keygen(&output, force)),
        Command::Node { action } => finish(match action {
            NodeAction::SignIn {
                server,
                user,
                password_stdin,
                name,
                config,
            } => node_cmds::sign_in(&server, &user, password_stdin, name, config),
            NodeAction::Run {
                config,
                background: true,
                background_child: false,
            } => node_cmds::run_in_background(config),
            NodeAction::Run {
                config,
                background_child,
                ..
            } => node_cmds::run(config, background_child),
            NodeAction::Status { config } => node_cmds::status(config),
        }),
        #[cfg(windows)]
        Command::ToneTest { output, seconds } => finish(stream_cmds::tone_test(&output, seconds)),
        #[cfg(not(windows))]
        Command::ToneTest { .. } => {
            finish(Err("playback is only implemented for Windows so far".into()))
        }
        Command::Send {
            input,
            loopback,
            to,
            key_file,
            bitrate,
            resilient,
            seconds,
            report_every,
            mmcss,
            json,
        } => {
            use audionet_audio::capture::CaptureMode;
            let (mode, selector) = match (input, loopback) {
                (Some(s), _) => (CaptureMode::Input, s),
                (None, Some(s)) => (CaptureMode::Loopback, s),
                (None, None) => unreachable!("clap requires one of --input or --loopback"),
            };
            finish(send(SendCmd {
                mode,
                selector,
                to,
                key_file,
                bitrate_kbps: bitrate,
                resilient,
                seconds,
                report_every,
                mmcss,
                json,
            }))
        }
        Command::Receive {
            listen,
            key_file,
            output,
            target_ms,
            fixed_target,
            seconds,
            report_every,
            mmcss,
            json,
            simulate_render_stall,
            stall_at,
        } => finish(receive(ReceiveCmd {
            listen,
            key_file,
            output,
            target_ms,
            fixed_target,
            seconds,
            report_every,
            mmcss,
            json,
            simulate_render_stall_ms: simulate_render_stall,
            stall_at_s: stall_at,
        })),
    }
}

fn finish(result: Result<(), String>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("Error: {message}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(windows)]
use stream_cmds::{ReceiveArgs as ReceiveCmd, SendArgs as SendCmd, keygen, receive, send};

#[cfg(not(windows))]
fn keygen(_: &std::path::Path, _: bool) -> Result<(), String> {
    Err("this build does not support key generation on this platform yet".into())
}

/// Streaming commands need a platform audio backend; only Windows has one
/// so far. These stubs keep the CLI building (and its tests running) elsewhere.
#[cfg(not(windows))]
#[allow(dead_code)]
struct SendCmd {
    mode: audionet_audio::capture::CaptureMode,
    selector: EndpointSelector,
    to: String,
    key_file: PathBuf,
    bitrate_kbps: u32,
    resilient: bool,
    seconds: u64,
    report_every: u64,
    mmcss: bool,
    json: bool,
}

#[cfg(not(windows))]
#[allow(dead_code)]
struct ReceiveCmd {
    listen: String,
    key_file: PathBuf,
    output: EndpointSelector,
    target_ms: u32,
    fixed_target: bool,
    seconds: u64,
    report_every: u64,
    mmcss: bool,
    json: bool,
    simulate_render_stall_ms: Option<u64>,
    stall_at_s: u64,
}

#[cfg(not(windows))]
fn send(_: SendCmd) -> Result<(), String> {
    Err("audio streaming is only implemented for Windows so far".into())
}

#[cfg(not(windows))]
fn receive(_: ReceiveCmd) -> Result<(), String> {
    Err("audio streaming is only implemented for Windows so far".into())
}

#[cfg(windows)]
type CaptureArgs = capture_test::CaptureTestArgs;

#[cfg(windows)]
fn run_capture_test(args: CaptureArgs) -> ExitCode {
    match capture_test::run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("Error: {message}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(windows))]
#[allow(dead_code)]
struct CaptureArgs {
    mode: audionet_audio::capture::CaptureMode,
    selector: EndpointSelector,
    seconds: u64,
    report_every: u64,
    wav: Option<PathBuf>,
    ring_ms: u32,
    mmcss: bool,
    reconnect: bool,
    simulate_consumer_stall_ms: Option<u64>,
    json: bool,
}

#[cfg(not(windows))]
fn run_capture_test(_args: CaptureArgs) -> ExitCode {
    eprintln!("Error: audio capture is only implemented for Windows so far.");
    ExitCode::FAILURE
}

fn run_list(json: bool, all: bool) -> ExitCode {
    let enumerator = WasapiEnumerator;
    let options = EnumerationOptions {
        include_not_present: all,
    };
    let inventory = match enumerator.enumerate(options) {
        Ok(inventory) => inventory,
        Err(e) => {
            eprintln!("Error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let output = if json {
        list::render_json(enumerator.backend(), &inventory)
    } else {
        list::render_text(&inventory)
    };
    write_stdout(&output)
}

fn write_stdout(text: &str) -> ExitCode {
    let mut stdout = io::stdout().lock();
    match stdout
        .write_all(text.as_bytes())
        .and_then(|()| stdout.flush())
    {
        Ok(()) => ExitCode::SUCCESS,
        // The reader went away (e.g. piped into `head`); not our failure.
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("Error: could not write output: {e}");
            ExitCode::FAILURE
        }
    }
}
