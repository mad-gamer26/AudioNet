# CLAUDE.md — AudioNet

AudioNet is a cross-platform, low-latency network audio streaming and routing
system. Live audio moves in any direction between Windows, macOS, Linux,
iOS, Android and web browsers: system audio, microphones, individual
applications, one source to many listeners.

## AGENTS.md is authoritative

Read `AGENTS.md` in full before changing anything. It takes precedence over
this file and over task prompts unless the user explicitly overrides it.
This file summarizes how AGENTS.md applies to this repository; it does not
replace it.

## Repository map

| Path | Purpose |
| --- | --- |
| `crates/audionet-protocol` | Shared types, signaling messages. No I/O. |
| `crates/audionet-audio` | Platform-neutral audio: bounded rings, PCM conversion, callback timing, capture/render traits, shared clock. |
| `crates/audionet-codec` | Opus (libopus). |
| `crates/audionet-transport` | RTP framing, sequence tracking, PSK encryption. Sans-I/O. |
| `crates/audionet-engine` | Sender, receiver stage, playout, drift/depth controller, runtime threads, simulator (`tests/sim.rs`). |
| `crates/audionet-wasapi` | Windows backend. The only crate allowed `unsafe`. |
| `crates/audionet-node` | Device agent: sign-in, signaling, WebRTC sessions (str0m), TURN relay. |
| `crates/audionet-server` | Self-hostable coordination server (axum, SQLite). |
| `crates/audionet-cli` | The `audionet` executable. |
| `crates/audionet-ffi` | UniFFI bindings of the engine for Swift apps. |
| `apps/macos` | Native macOS app (SwiftUI, XcodeGen). |
| `apps/ios` | Native iPhone app (SwiftUI, XcodeGen). |
| `apps/shared` | Swift shared by both Apple apps. |
| `web/` | Browser client (no build step). |
| `deploy/` | Self-hosting templates; `deploy/official/` only for the official instance. |
| `scripts/test/` | Hardware and browser test helpers. |
| `docs/` | Architecture, protocol, self-hosting, testing, accessibility, Windows audio, developer setup. |

## Engineering priorities (AGENTS.md §1)

1. No source stalls caused by streaming. 2. Continuous playback. 3. Bounded
latency. 4. Automatic recovery. 5. Low latency. 6. Quality. 7. Bandwidth.
8. CPU. Never trade 1–4 for a smaller latency number.

## Real-time rules

Capture and render callbacks, and anything they call, must not: block, take
contended locks, wait on threads, allocate, do socket/file/console I/O, log,
do DNS/HTTP/IPC/UI, spawn tasks or threads, or run unbounded loops.

- All audio queues are bounded. Prefer preallocated SPSC rings.
- The render callback always returns the requested sample count; missing
  audio becomes concealment or faded silence, never a wait.
- Slow consumers never backpressure the source; drop stale audio instead.
- Count every drop by its cause (loss, overflow, stale trim, latency drain,
  catastrophic trim, late packet). No single generic "drops" counter.
- Clock-drift correction (slow, continuous resampling in ppm) and
  buffer-depth correction (a slow, clamped bias toward the target) are
  separate controllers on separate time scales. Catastrophic trimming is
  only a safety net.
- Hot-path diagnostics are atomics, timestamps and min/max accumulators,
  snapshotted from a non-real-time thread. No per-packet or per-callback logs.
- Apply the AGENTS.md §44 review checklist to every change on the real-time path.

## Measure before tuning

Do not change buffer sizes, jitter targets, frame sizes, thread priority,
timer resolution, bitrate, sample rate, pacing or resampling ratios because
audio "sounds wrong". Identify the failing stage with diagnostics first. For
every audio fix, write down: Hypothesis, Evidence, Change, Expected metric
change, Risks, Verification. If diagnostics can't tell, the first patch
adds instrumentation.

Follow the phase order: correctness → observability → long-term stability →
loss/jitter robustness → latency → efficiency. Do not jump to latency work.

## Accessibility (mandatory)

AudioNet must be fully usable by blind users (NVDA, JAWS, VoiceOver,
TalkBack; WCAG 2.2 AA on the web).

- Every important state has a text form. Nothing is available only as a
  graph, meter, color, waveform, hover text or animation.
- CLI output is linear "Label: value" lines, positions like "Output device
  2 of 4", with no tables, columns, color or cursor control. Offer `--json`
  where scripts need it.
- Error messages name the failing subsystem in words.
- Controls need accessible names, roles, state, keyboard operation and
  predictable focus.
- Never degrade screen-reader responsiveness to improve audio scheduling
  (e.g. no casual `REALTIME_PRIORITY_CLASS`).
- Golden-text tests are not screen-reader testing. State what still needs
  checking with a real screen reader.

## Security

Remote operation must be authenticated and encrypted. Never hard-code
credentials, commit secrets, store plaintext passwords, or disable
encryption to fix latency without evidence. Authentication and key exchange
never run in audio callbacks. Measure crypto cost rather than assuming it.

## Dependencies

Before adding a significant dependency, state what it solves, why it fits,
and what alternatives were considered (record it in the PR or docs). Prefer
mature implementations for hard problems: Opus, resampling, jitter
buffering, PLC, NAT traversal, congestion control. Do not reimplement these
casually (AGENTS.md §33). Current significant dependencies and why:
`opus` (libopus), `rubato` (sinc resampling), `rtrb` (lock-free SPSC ring),
`chacha20poly1305`/`hkdf`/`sha2` (RustCrypto), `str0m` with pure-Rust
crypto (WebRTC for devices), `axum`/`tokio`/`rusqlite`/`argon2` (server),
`tokio-tungstenite`/`ureq` with rustls and OS trust store (device agent),
`windows` (official bindings), `socket2` (socket buffer sizes), `clap`
(no color), `hound` (WAV for verification), `ctrlc`, `ed25519-dalek`
(update signature check, verify only) and `zip` (unpacking updates,
pure-Rust deflate) in the desktop app, `turn-client-proto` (TURN relay
client, sans-I/O like str0m) in the device agent.

## Project decisions

- Packet framing: RTP-style (RFC 3550 / RFC 7587), compatible with a later
  move to RTP/WebRTC. No proprietary format without a measured need.
- Security: encryption from day one, with no insecure default. For the LAN
  milestone: a pre-shared key and per-packet AEAD, with key setup off the
  real-time path.
- Order: capture is measured and stable before Opus or networking (done).
- Official hosted instance: `https://audionet.mad-gamer.com` (deploy files in
  `deploy/official/`). Never hard-code it in code; the server URL is
  always configuration.
- Audio threads use MMCSS "Pro Audio" by default (soak evidence); never
  `REALTIME_PRIORITY_CLASS`.

## Unsafe code

- Only platform backend crates may contain `unsafe`. All other crates use
  `#![forbid(unsafe_code)]`, except `audionet-ffi`, whose only `unsafe` is
  the FFI glue UniFFI generates (none written by hand).
- Every `unsafe` block has a `// SAFETY:` comment (enforced by
  `clippy::undocumented_unsafe_blocks` under `-D warnings`).
- Keep unsafe code in small functions; parse OS data structures in safe
  code from byte slices where possible (see `audionet-wasapi/src/waveformat.rs`).
- Document COM apartment and thread assumptions next to the code.

## Conventions

- JSON: `snake_case`; every field always present; absent is `null`, never
  omitted; lists are `[]`. Versioned schemas (see `docs/protocol.md`).
- Terminology: node, endpoint, source, destination, route, stream session
  (see `docs/protocol.md`). A route is never a socket or a peer connection.

## Checks

```sh
cargo build
cargo test
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
```

When reporting results, keep these kinds of verification separate: compile,
unit test, hardware/runtime, real audio, network simulation, screen reader,
soak. Never claim one that did not happen. A green build is not "fixed".

## Git

Don't modify global git config or invent author identity. Don't push or
rewrite history without explicit permission.
