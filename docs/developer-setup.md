# Developer setup

## Repository layout

| Path | What it is |
| --- | --- |
| `crates/audionet-protocol` | Shared types: identifiers, devices, routes, sessions, signaling messages |
| `crates/audionet-audio` | Platform-neutral audio pieces: bounded rings, sample conversion, callback timing, capture/render interfaces |
| `crates/audionet-codec` | Opus encoder and decoder (libopus, built from source) |
| `crates/audionet-transport` | RTP framing, sequence tracking, per-packet encryption |
| `crates/audionet-engine` | Sender, receiver, playout, drift/depth controller, threaded runtime, simulator |
| `crates/audionet-wasapi` | Windows backend (the only crate with `unsafe`) |
| `crates/audionet-node` | Device agent: sign-in, signaling, WebRTC sessions (str0m), TURN relay |
| `crates/audionet-server` | Coordination server |
| `crates/audionet-cli` | The `audionet` program |
| `crates/audionet-desktop` | Windows desktop app (`audionet-desktop`) |
| `crates/audionet-ffi` | The engine for Swift apps (UniFFI bindings; its only `unsafe` is UniFFI's generated glue) |
| `apps/macos` | Native macOS app (SwiftUI on the Rust engine, XcodeGen project) |
| `apps/ios` | Native iPhone app (SwiftUI on the Rust engine, XcodeGen project) |
| `apps/shared` | Swift code both Apple apps use (the sign-in file) |
| `web/` | Browser client (plain HTML/CSS/JS, no build step) |
| `deploy/` | Self-hosting templates; `deploy/official/` is the official instance only |
| `scripts/test/` | Hardware and browser test helpers |
| `docs/` | Documentation |

## Windows (primary platform)

1. Install Rust with the MSVC toolchain (`rustup`), plus the Visual Studio
   Build Tools (C++ workload) and CMake (for libopus).
2. `cargo build --release` builds everything, including the Windows audio
   backend and the `audionet` program.
3. `cargo test` runs all tests. Some tests use real UDP sockets on
   localhost.
4. Optional, for hardware tests: a virtual audio cable, Python 3 with
   `numpy`, `sounddevice` and `selenium`, and Google Chrome.

## Linux

The server, protocol, codec, transport, engine and simulator build and
test on Linux (`cargo test`). The device agent's audio backend (cpal)
additionally needs the ALSA development files (`libasound2-dev` and `pkg-
config` on Debian and Ubuntu). `audionet node` uses the portable cpal
backend (ALSA, which PipeWire and PulseAudio also serve); it has not yet
been tested with real Linux audio devices. The local test commands
(`keygen`, `capture-test`, `tone-test`, `send`, `receive`) are Windows-
only for now.

Server only: `cargo build --release -p audionet-server` (needs gcc for the
bundled SQLite).

## macOS

`cargo build --release` and `cargo test` work with the Xcode command-line
tools and CMake. The device agent (`audionet node sign-in` / `node run`) uses
the portable cpal backend (Core Audio underneath): microphones, outputs,
and system audio. Each output is also offered as "Sound playing on
<output>", recorded through a Core Audio process tap (macOS 14.2 or later;
it includes what AudioNet itself plays there). macOS asks for microphone
and "System Audio Recording" permission the first time; a program started
from a terminal inherits the terminal's permissions (System Settings >
Privacy & Security).

### The macOS app (`apps/macos`)

A native SwiftUI app on the same Rust engine as Windows, through
`crates/audionet-ffi` (UniFFI). No web view. Needs Xcode 16 or later and
XcodeGen (`brew install xcodegen`).

```sh
cd apps/macos
./build-core.sh              # Rust static library + Swift bindings into Generated/
                             # (--universal for arm64 + x86_64)
xcodegen generate            # AudioNet.xcodeproj from project.yml
xcodebuild -scheme AudioNet -configuration Release build
```

`Generated/`, `build/` and the Xcode project are generated, not committed.
The app signs in with the account name and password (only the device
token is kept, in a file only the user can read, like the Windows app's
settings), is online in its accounts while it runs, shares this Mac's
microphone and outputs in each account where sharing is on, lists the
account's devices, and listens to them or sends to them. It stays in the
menu bar when its window is closed (without a Dock icon), can start in
the menu bar, and can start at login. Launch it with
`-AudioNetProfile NAME` to keep a test account apart from yours.

`./package.sh` builds a release (see [releasing.md](releasing.md#the-mac-app)).
Builds are ad-hoc signed unless a Developer ID certificate is in the
keychain; with one, `package.sh` signs with it and, given an App Store
Connect API key, notarizes. macOS treats each ad-hoc build as a new app,
so it asks for permissions again after a rebuild.

## The NVDA add-on (`nvda-addon`)

Python for NVDA 2026.1 or later, with the engine as a separate program
(`crates/audionet-visitor`, built for Windows x64). To package it:

```sh
python nvda-addon/build.py                      # dist/audionet-<version>.nvda-addon
python nvda-addon/build.py --default-server https://audionet.example.com
```

The version is the workspace version. `--default-server` fills in the
server address offered when adding an account; the add-on works with any
server. Install the package by opening it (NVDA asks, then restarts).
The engine client, account store and words are tested outside NVDA (see
[testing.md](testing.md#4b-the-nvda-add-on)).

## The iPhone app (`apps/ios`)

A native SwiftUI app on the same Rust engine as the Mac and Windows apps,
through `crates/audionet-ffi`. No web view. Needs Xcode (tested with
Xcode 27), XcodeGen, and the Rust iOS targets
(`rustup target add aarch64-apple-ios aarch64-apple-ios-sim`).

```sh
cd apps/ios
./build-core.sh              # engine for iPhone and simulator, as
                             # Generated/AudioNetCore.xcframework, + Swift bindings
xcodegen generate
xcodebuild -project AudioNet.xcodeproj -scheme AudioNet \
  -destination 'platform=iOS Simulator,name=iPhone 17 Pro' test
```

The app signs in with the account name and password (only the device
token is kept, in a file only the app can read; with several accounts,
one token each in `accounts.json`, shared code in `apps/shared`), shares the iPhone's
microphone and output while online, lists the account's devices (each a
disclosure), and listens to them or sends the microphone to them. It
mixes with other apps' audio and never lowers or stops it. The Microphone
row opens a list like UniMic's Input Source: a section per input (the
built-in microphone, a wired headset) and a row per source (bottom, front,
back), from `availableInputs`. AudioNet never uses Bluetooth HFP:
Bluetooth microphones are not listed, and Bluetooth headphones keep
playing in full quality (A2DP). Choosing a microphone while the
microphone is being sent takes effect at once: iOS moves the running
capture, and the engine reopens it immediately only if the format changed
(for example from a stereo source to a mono one).

Recording follows Apple's stereo recipe, as UniMic does: the input and its
data source chosen explicitly, the stereo polar pattern, two preferred
input channels, 48 kHz, and, like UniMic, no stereo orientation set (iOS's
default). On an iPhone 16 Pro the
built-in microphone's front and back sources can record in stereo and the
bottom one (iOS's default) only in mono, so with no choice made AudioNet
records from the front source. The status log says what is recording
("Microphone in use: iPhone Microphone, Front, stereo pattern, 2 channels
at 48000 Hz."), and so does the capture's status ("Capturing Microphone
(stereo, 48000 Hz)"). The list is read again when a headset comes or goes,
ignoring the route notices its own reading causes.

Like UniMic, the audio session is active only while AudioNet has a stream.
The engine reports its open streams and microphones
(`set_audio_use_listener`) before a stream opens and after one closes, and
the session follows: inactive with no stream; playback while only playing,
so listening never involves the microphone; play-and-record with the
chosen microphone while one is recorded, waiting until the route uses
that microphone before the capture opens. When the last microphone stream
ends, the preferred input is released and the session returns to
playback or becomes inactive at once, letting go of the microphone.
A running stream survives these changes: iOS reroutes it by itself, and
the engine reopens a stream only when the route's sample rate or channel
count changed (reopening on every reroute used to cost seconds of audio
at the start of each microphone stream). Streams keep running in the background
(background audio mode).

Per-stream volume and mute use the engine's `set_stream_volume`
(audionet-audio `gain.rs`: an atomic target read once per block, a 10 ms
ramp, untouched at full volume).

On a Mac, playing on an output device that also has inputs (a virtual
device such as Jump Desktop Audio, a USB headset) can make macOS ask for
microphone access, even though nothing records. The Mac UI tests
therefore play on an output-only device.

The UI tests run Xcode's accessibility audit; the signed-in test needs a
temporary device, which `scripts/test/ios_ui_tests.py` creates. Running on
a phone needs your own Apple development team
(`DEVELOPMENT_TEAM=... -allowProvisioningUpdates`). The app has no
built-in server: set `AUDIONET_DEFAULT_SERVER` in `project.yml` to prefill
one for your own builds.

## Android

There is no Android app yet: a native one on the Rust engine, like the
iPhone app, comes after the Windows, Mac and iPhone apps. On Android, use
the web client in the browser.

## Web client

Edit `web/` directly; there is no build step. Run a local server with
`web_root` pointing at the directory:

```toml
# local.toml
public_url = "http://127.0.0.1:8740"
database = "local.db"
web_root = "web"
allow_registration = true
```

```sh
cargo run -p audionet-server -- --config local.toml serve
```

Then open `http://127.0.0.1:8740/`. Browsers allow microphone access on
`localhost` without HTTPS. Devices accept `http://` server URLs only for
`localhost` and `127.0.0.1`.

## Checks before a pull request

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```
