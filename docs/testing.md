# Testing AudioNet

AGENTS.md requires more than unit tests: behavior has to be shown under
loss, jitter, drift and stalls, on real hardware, for long enough to catch
slow problems. Report each kind of verification separately.

## 1. Unit and integration tests

```sh
cargo test
```

Covers packet framing and parsing, sequence wraparound, loss, reorder and
duplicate accounting, encryption (including tampering), Opus round trips,
PLC and FEC, ring overflow and stale trimming, callback timing, the
drift/depth controller, playout priming, underrun declicking, device-rate
and channel conversion, the signaling hub (authorization, routing, slow
clients), and the server over real HTTP and WebSocket connections.

## 2. Deterministic network simulation

```sh
cargo test -p audionet-engine --test sim -- --nocapture
```

`crates/audionet-engine/tests/sim.rs` runs the real sender, receiver and
playout code (real Opus, encryption and resampler) on virtual time with a
simulated network and independent sender and receiver clocks. Every
scenario asserts behavior: bounded latency, return to target, drift
convergence, bounded underruns, no safety trims.

| Scenario | Checks |
| --- | --- |
| Clean network, ±2 ms jitter | 0 underruns, depth stays at target |
| 1 % and 5 % loss with ±10 ms jitter | Loss measured correctly, concealment, bounded latency |
| Burst loss (200 ms outages) | Resynchronizes instead of adding latency |
| Reordering and duplicates | Absorbed, none counted as loss |
| Sender clock +50, +200, −200 ppm | Drift estimate within 15 ppm, depth within ±8 ms |
| 50 ms render stall with +200 ppm | Depth rises then returns to target, drift estimate preserved |
| 180 ms render stall | One controlled latency drain, back near target within seconds |
| 50 ms network-thread stall | At most one underrun, returns to target |
| 50 ms sender stall | Recovers to target |
| Stream restart (new sender) | Resets drift and playout, resumes |
| Adaptive target, clean network (15 min) | Settles from 40 ms to 25 to 30 ms, 0 underruns, at most 2 late packets |
| Adaptive target, ±20 ms jitter | At least 50 ms within a minute, no underruns afterwards, at most 4 changes |
| Adaptive target, network worsens after 10 min | Rises; no underruns and at most 5 late packets once adapted |
| Adaptive target, two 2 s sender outages | Does not rise |
| One hour, +120 ppm, 0.5 % loss, jitter, stalls (ignored by default) | No latency growth between the first and last ten minutes |

Run the hour-long soak simulation (about 2.5 minutes of wall time):

```sh
cargo test --release -p audionet-engine --test sim one_hour -- --ignored --nocapture
```

Measure underruns, late packets and depth for fixed targets across jitter
levels (a measurement, not a pass/fail test):

```sh
cargo test --release -p audionet-engine --test sim jitter_sweep -- --ignored --nocapture
```

## 3. Hardware tests (Windows)

Silent, repeatable tests use a virtual audio cable (for example VB-Audio
or Virtual Audio Cable): play a known tone into one virtual output, stream
it with AudioNet, record the far end from a virtual input.

Helper scripts in `scripts/test/` (Python 3 with `numpy` and `sounddevice`):

* `tone_stream.py NAME SECONDS`: plays a 997 Hz, −12 dBFS sine into the
  named output, with constant memory.
* `verify_wav.py FILE.wav [HZ]`: checks a recording's frequency and level and
  counts sample-level discontinuities (any gap, click or repeated sample).
* `latency.py OUT REF DST SECONDS`: measures end-to-end latency with noise
  bursts, aligned by the audio clock.

Example, native encrypted path on one PC:

```sh
audionet keygen --output test.key
audionet receive --key-file test.key --output "Line 1" --seconds 60 &
audionet send --loopback "UniMic Output" ... # see audionet list for numbers
python scripts/test/tone_stream.py "UniMic Output" 60 &
audionet capture-test --input 1 --seconds 20 --wav far-end.wav
python scripts/test/verify_wav.py far-end.wav
```

Receiver test hook: `audionet receive --simulate-render-stall 50 --stall-at 20`
stops consuming audio for 50 ms at 20 s to exercise depth recovery on real
hardware.

Watch for routing inside virtual cable software (repeaters, "listen to this
device"): it can create feedback loops or add a second copy of the test
signal, which shows up as level changes that are not AudioNet's.

## 4. Real-browser end-to-end test

`scripts/test/browser_e2e.py` drives headless Chrome (Selenium) against any
AudioNet server, signs this PC in as a temporary device, streams a tone to the
browser, measures frequency and level inside the page, optionally repeats
through the TURN relay only (`AUDIONET_RELAY=1`), optionally has two
browsers listen to the same source at once (`AUDIONET_LISTENERS=2`), and
removes the temporary device. See the script's header for its environment variables.

## 4a. Native devices end to end

`crates/audionet-server/tests/native_devices.rs` signs two devices in with
the account password against a real server; one drives the other as a
remote, and a 997 Hz tone flows through the real Opus, WebRTC and playout
path between simulated sound hardware, both directions (unity gain).

The ignored test `audio_flows_through_a_real_turn_relay` does the same
against a live server with both devices limited to relayed candidates
(`AUDIONET_RELAY_SIDE=pc` or `phone` relays one side only;
`AUDIONET_RELAY_ONLY=0` compares the direct path). It prints receive
diagnostics every two seconds and removes its temporary devices:

```sh
AUDIONET_URL=https://audionet.example.com AUDIONET_USER=… AUDIONET_PASSWORD=…   cargo test -p audionet-server --test native_devices -- --ignored --nocapture
```

### macOS app UI and accessibility

`scripts/test/mac_ui_tests.py` (run on Windows) runs
`apps/macos/AudioNetUITests` on the Mac with `xcodebuild test`. It puts a
temporary source device online on the PC and creates a temporary Mac
device through the server API. Only that device's token goes to the Mac,
in a file only its user can read, which the app deletes on reading. It
removes both devices afterwards. The app runs in a separate test profile
and never touches the Mac user's own AudioNet account. UI automation must
be allowed on the Mac once
(`sudo automationmodetool enable-automationmode-without-authentication`).

### macOS app updates

`scripts/test/mac_update_e2e.py` (run on Windows; see
[releasing.md](releasing.md#testing)) builds three test versions on the Mac
with a throwaway key and checks rejection, a real update and a rollback.

### macOS app engine, Mac and Windows

`scripts/test/mac_engine_check.py` (run on Windows; the Mac is reached over
SSH) drives `apps/macos/EngineCheck`: the macOS app's own Rust core and
Swift bindings, without the UI. A temporary device on the PC plays a
997 Hz tone. The Mac signs in with the password, listens to it (checking
diagnostics: packets arrive, no underruns), then sends an input back to
the PC while the PC records the output it plays on. With
`SEND_FROM="<loopback input>"` the Mac sends a loopback device carrying the
tone it receives, so the PC must hear 997 Hz, for the length of the send,
without gaps. The password travels in a file only the Mac user can read,
deleted as soon as it is read, and is masked in the Mac log.

`SEND_FROM="Sound playing on <output>"` sends the Mac's system audio (a
Core Audio process tap) instead: with PLAY_ON the same output, the tone
the Mac plays is recorded and sent back.

A MacBook with its lid closed disconnects the built-in microphone in
hardware: it records exact silence, and AudioNet then warns that the
microphone sends only digital silence. Use a loopback input in that case.

## 4b. The NVDA add-on

* `cargo test -p audionet-server --test native_devices a_visitor` signs a
  visitor in with a web session, listens to a device and sends to it
  (997 Hz both ways through the real Opus, WebRTC and playout path, with
  simulated sound hardware), checks that the account's devices are only
  the real device, and that signing out ends the session.
* `python scripts/test/nvda_engine.py` drives `audionet-visitor.exe`
  through the add-on's own engine client against a local server, with a
  command-line device on this computer (real WASAPI audio, silent virtual
  devices where present): sign-in, visitor connection, listening (packets
  arrive), volume, stop, sending, no device record, a wrong password in
  words, sign-out ending the session, and the engine exiting when the
  add-on goes away.
* `python -m unittest discover nvda-addon/tests`: the account store
  (DPAPI-encrypted tokens) and the words the add-on says.
* Not automated: the add-on inside a running NVDA (its window, settings
  panel, command layer and announcements). That needs a manual session
  with NVDA.

## 5. Soak tests

Run the native path for at least an hour with diagnostics every minute and
check that playout depth does not trend, drift converges, and underruns
correlate with measured scheduling gaps (render or capture callback gaps,
packet gaps), not with the controller.

```sh
audionet receive --key-file test.key --output "Line 1" --seconds 3600 --report-every 60 > soak.log
```

Results so far are recorded in [windows-audio.md](windows-audio.md).

## 6. Accessibility

Automated accessibility checks:

* Web client: `scripts/test/browser_e2e.py` (real Chrome via Selenium).
* Windows desktop app: `scripts/test/desktop_uia.py` (UI Automation names,
  roles and the sign-in/start flow) and `scripts/test/desktop_tray.py` (system
  tray: close to tray, Enter on the icon, the tray menu, a second start,
  start in tray, clean exit; no server needed, preferences restored).
  `desktop_uia.py` also checks sharing as the server sees it: online at
  once after signing in, not sharing; Start and Stop sharing; the choice
  kept across a restart; exiting while sharing asks first; Sign out
  (while sharing) removing the device from the account. Both run the app under
  `AUDIONET_TEST_PROFILE`, which gives it its own window class and
  preferences key, so they never touch a copy of AudioNet you are running
  or your settings.
* Several accounts: `scripts/test/second_account.py` creates a temporary
  second account on the server with its administration command over SSH
  (random password, never printed) and deletes it with its devices
  afterwards. `desktop_remote.py` adds it in the Windows app with the
  sign-in fields, checks the Accounts list, both connections, a tree item
  per account, and signing out of it; the Mac and iPhone UI tests sign in
  to both and check the per-account device sections and signing out of
  one.
* Stream volume and mute:
  * engine: `native_devices.rs` (two native devices through the server)
    measures half volume 12 dB quieter, mute as digital silence and full
    volume back, listening and sending;
  * Windows: `scripts/test/desktop_remote.py` sets the volume and mute of a
    real stream through UI Automation and measures what plays;
  * web: `scripts/test/web_stream_volume.py` (the controls' names in
    Chrome's accessibility tree, the listening volume, and what is sent
    measured inside the page);
  * Mac and iPhone: the signed-in UI tests set the slider and mute and
    check what VoiceOver reads.
* Sharing: `crates/audionet-server/tests/native_devices.rs`
  (`a_device_that_does_not_share_receives_but_does_not_send`): two real
  devices through a real server; the one not sharing is seen online but
  not sharing, cannot be listened to or send, plays what the other sends
  and listens to it; sharing, it is heard; stopping ends that stream on
  both sides. The hub test checks who may send and what ends.
  `scripts/test/web_sharing.py` (live server, headless Chrome): a device
  that does not share is listed "online, not sharing its audio" with no
  sounds offered and Listen unavailable, Send still available; when it
  starts sharing that is announced and its sounds appear.
  The Mac and iPhone UI tests (`testSignedIn...`) end by quitting and
  reopening the app twice: an account left sharing comes back sharing,
  and after turning it off it comes back not sharing (the Mac's menu bar
  item says so too).
* Creating an account: `crates/audionet-server/tests/sign_up.rs` (closed
  by default, reserved names, passwords containing the name, taken names,
  the per-address and server-wide limits including a spoofed header being
  ignored when none is configured, and that devices cannot create
  accounts). `scripts/test/web_create_account.py` checks the web form in headless Chrome against a local server started
  for the test (temporary database, so nothing is created on a real
  server): names, descriptions, focus, alerts and invalid marking.
* Email addresses and password reset: `cargo test -p audionet-server --test
  email` (over real HTTP with an in-memory mailer: confirming an address,
  single-use and newest-only links, changing the address with the
  password (the confirmed address kept, and told, until the new one is
  confirmed), waiting addresses not blocking the owner, the neutral
  "forgot" answer, no reset link to an unconfirmed address, reset signing out other browsers, the per-account and
  per-address limits, the server without email settings, and the real
  SMTP code path against a local SMTP responder).
  `scripts/test/web_email.py` checks the web client in headless Chrome
  against a local server whose email goes to an SMTP sink in the script:
  the email field, the "Confirm your email address" and "Add an email
  address" banners (named regions), following the emailed links, "Forgot
  your password?", choosing a new password, and adding an address, with
  announcements, focus and invalid marking. The Mac and iPhone UI tests
  (`testForgotPasswordNeedsTheServerAddress`) and `desktop_uia.py` check
  the apps' "Forgot password" control.
* Web status log: `scripts/test/web_status_log.py` (the Status log dialog
  in Chrome's accessibility tree: named modal dialog, focus in and back,
  Escape and Close). The Windows tests read the desktop app's status log
  through its window (`scripts/test/desktop_log.py`), and
  `desktop_uia.py` checks that focus goes into the log and back to its
  button.
* Windows desktop app as a remote: `scripts/test/desktop_remote.py` (live
  server; a temporary command-line device shares a 997 Hz tone; the app
  signs in with the password, chooses the device and sounds by keyboard,
  Listens and Sends; the audio is recorded and compared with a local tone
  on the same output; temporary devices removed).
* Automatic updates: `scripts/test/update_e2e.py` (throwaway key, local
  update source; a real update end to end, no loop, tampering rejected,
  a broken update rolled back). See [releasing.md](releasing.md).
* iPhone app: `scripts/test/ios_ui_tests.py` (run on Windows; the Mac is
  reached over SSH) runs `apps/ios/AudioNetUITests` in the iOS simulator:
  Xcode's accessibility audit on the sign-in screen and on the signed-in
  screen with a stream running and on the Status Log screen, an empty
  sign-in naming what is missing, a heading and a log row growing with
  Dynamic Type, a Microphone choice, and going online, expanding a
  temporary source device on this PC, listening until connected,
  collapsing, stopping and going offline. Temporary devices are removed
  afterwards.

See [accessibility.md](accessibility.md) for the manual screen-reader test
script and the current verification status.
