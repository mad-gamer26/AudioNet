# AudioNet

AudioNet is open-source software for moving live audio between computers,
phones and browsers with low latency: system audio from a PC to a phone,
a microphone to another room, one application's sound to a browser tab.
Audio can flow in any direction, and every part of AudioNet is designed to
be fully usable by blind people with a screen reader.

AudioNet has two parts:

* **Devices** (called *nodes*) own audio sources and outputs: a Windows PC
  (fully supported), or a Mac or Linux computer (early). A node runs the
  `audionet` program or, on Windows, the AudioNet desktop app.
* **Clients** listen to and talk to devices: the web client in any modern
  browser, and the native Windows, Mac and iPhone apps (which are devices
  too). A native Android app is planned; on Android, use the web client
  in the browser for now.
* A **coordination server** handles accounts, device sign-in, presence and
  connection setup. It never touches audio: sound travels directly between
  devices, encrypted, and is relayed only when a direct path is impossible.
  Anyone can run their own server; the project also operates an official
  hosted instance at `https://audionet.mad-gamer.com`.

## Status

Early but working. What exists and is tested today:

| Area | State |
| --- | --- |
| Windows audio (WASAPI) | Device listing, microphone capture, system (loopback) capture with silence filled in while nothing plays, playback; event-driven, bounded, measured |
| Codec and transport | Opus, RTP framing, per-packet authenticated encryption (pre-shared-key LAN mode), WebRTC (DTLS-SRTP) for the server-coordinated path |
| Receiver | Reordering, loss concealment, bounded playout buffer that adapts to the network, clock-drift correction, buffer-depth correction, stall recovery |
| Server | Accounts, password sign-in for devices, device tokens, presence, WebRTC signaling, TURN credentials; self-hostable |
| Web client | Sign in, see devices, listen to a device's sound, send your microphone to a device, text diagnostics |
| Windows desktop app | Sign in with your account; share this computer; listen to your other devices and send audio to them; system tray, signed automatic updates; standard Win32 controls with UI Automation announcements |
| iPhone app | Native SwiftUI app on the same engine: sign in, share this iPhone's microphone and output, listen to your other devices and send the microphone to them, background audio, mixes with other apps' audio; Xcode accessibility audit and a listen flow pass in the simulator; not yet in the App Store |
| macOS app | Native SwiftUI app on the same engine: sign in, share this Mac, listen to your other devices and send to them, menu bar, start at login; microphones, outputs and system audio (Core Audio process taps); engine tested end to end between a Mac and a Windows PC, system audio included; Xcode accessibility audit passes; signed automatic updates; download from the GitHub releases; Developer ID signed and notarized |
| NVDA add-on | Listen to your devices and send audio to them from NVDA, as a visitor of your accounts (no device record, like the web client): an AudioNet window, NVDA settings for accounts, an NVDA+Alt+A command layer; the engine runs as a separate program; engine tested end to end on Windows audio; not yet tested inside a running NVDA |
| Linux devices | `audionet node` runs through a portable audio backend (cpal); untested with real devices |
| Accessibility | Built for screen readers; automated checks (browser, UI Automation, Xcode audit) pass; no manual screen-reader session yet (see [docs/accessibility.md](docs/accessibility.md)) |

Measured results (Windows, details in [docs/windows-audio.md](docs/windows-audio.md)):
67.6 ms end-to-end on the native path at the default 40 ms buffer target,
recovery from a 50 ms playback stall without underruns, and a one-hour soak
with no latency growth.

## Quick start: use a server

1. Sign in to the web client of your AudioNet server (for example the
   official instance, or your own; see *Run your own server*).
2. On the Windows PC you want to use, open the AudioNet desktop app and
   sign in with the server address, your account name and your password.
   On a computer without a screen, sign in from a terminal instead (it asks
   for the password):

   ```text
   audionet node sign-in --server https://audionet.example.com --user YOUR-NAME
   audionet node run
   ```

   To keep it running without a terminal window, use `audionet node run -b`
   (`--background`). It prints the process ID, the log file (`node.log`
   next to the settings file) and how to stop it, then returns to the
   prompt. Don't also run the desktop app with the same settings file.

3. The PC appears under **Your devices**. Choose a sound source and press
   **Listen**, or choose an output and press **Send my microphone**.

## Quick start: direct LAN streaming (no server)

Two Windows PCs on the same network can stream directly with a shared key:

```text
audionet keygen --output audionet.key          # copy this file to both PCs securely
audionet receive --key-file audionet.key                         # on the listening PC
audionet send --loopback default --to LISTENING-PC --key-file audionet.key   # on the source PC
```

Other commands: `audionet list` (audio devices, screen-reader friendly;
`--json` for scripts), `audionet capture-test`, `audionet tone-test`.
Every command has `--help`.

## Run your own server

The server is a single program with a SQLite database; put it behind a TLS
reverse proxy and add coturn for relaying. Step-by-step instructions,
example configuration (nginx, systemd, coturn) and troubleshooting are in
[docs/self-hosting.md](docs/self-hosting.md). Devices and the web client
work with any server URL; nothing is tied to the official instance.

## Build from source

Requires Rust 1.85 or newer (and on Windows the MSVC toolchain and CMake).

```text
cargo build --release
cargo test
```

Binaries: `target/release/audionet` (devices and tools) and
`target/release/audionet-server`. See
[docs/developer-setup.md](docs/developer-setup.md) for platform details.

## Documentation

* [docs/architecture.md](docs/architecture.md): how the audio pipeline, clock-drift control and transport fit together
* [docs/protocol.md](docs/protocol.md): identifiers, signaling messages, HTTP API, JSON formats
* [docs/self-hosting.md](docs/self-hosting.md): running a server
* [docs/windows-audio.md](docs/windows-audio.md): the Windows backend and measurements
* [docs/releasing.md](docs/releasing.md): building releases and how the Windows app updates itself
* [docs/accessibility.md](docs/accessibility.md): accessibility commitments and test status
* [docs/testing.md](docs/testing.md): automated, simulated, hardware and soak testing
* [docs/developer-setup.md](docs/developer-setup.md): building each part
* [CONTRIBUTING.md](CONTRIBUTING.md), [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md) and [SECURITY.md](SECURITY.md)
* [AGENTS.md](AGENTS.md): the engineering rules for real-time audio work (authoritative)

## License

AudioNet is licensed under the [MIT license](LICENSE). Contributions are
accepted under the same terms.
