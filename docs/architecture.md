# AudioNet architecture

`AGENTS.md` is authoritative for real-time audio engineering. This document
explains how AudioNet applies it, what exists, and why the main decisions
were made.

## 1. Priorities

No source stalls caused by streaming → continuous playback → bounded
latency → automatic recovery → low latency → quality → bandwidth → CPU.
A stable 40–60 ms stream beats a 10 ms stream that glitches.

## 2. Model and terminology

```
AudioSource ──► AudioRoute ──► AudioDestination (1..N)
```

* **Node**: a participating device (PC, phone, browser tab). Any node can
  be a source in one session and a destination in another.
* **Endpoint**: an OS audio input or output on a node.
* **Source**: input endpoint, output-endpoint loopback, process, or the
  node's default input. **Destination**: output endpoint or default output.
* **Route**: a logical source → destinations mapping (not a socket).
* **Session**: the runtime instance of media for a route, with its own
  audio clock, codec state and diagnostics. A new session means a new clock:
  drift state never carries over.
* **Server**: the coordination server. It carries no audio.

Types: `crates/audionet-protocol`; wire formats: `docs/protocol.md`.

## 3. Components

```
                         ┌───────────────────────────┐
   browser ──HTTPS/WSS──►│ audionet-server           │◄──WSS── node (audionet node run)
     │                   │ accounts, sign-in,        │           │
     │                   │ presence, signaling,      │           │
     │                   │ TURN credentials          │           │
     │                   └───────────────────────────┘           │
     │                                                           │
     └──────────── WebRTC (DTLS-SRTP, Opus) direct or via TURN ──┘
                   coturn relays only when a direct path fails

   node ── RTP over UDP, XChaCha20-Poly1305, pre-shared key ──► node   (LAN mode, no server)
```

| Crate | Role |
| --- | --- |
| `audionet-audio` | Bounded SPSC rings, sample conversion, callback timing, capture/render interfaces |
| `audionet-wasapi` | Windows capture, loopback, render |
| `audionet-codec` | Opus |
| `audionet-transport` | RTP framing, sequence tracking, PSK encryption |
| `audionet-engine` | Sender, receiver stage, playout, drift/depth controller, runtime threads, simulator |
| `audionet-node` | Device agent: sign-in, signaling client, WebRTC sessions, TURN relay |
| `audionet-server` | Coordination server |
| `audionet-cli` | `audionet` program |
| `audionet-visitor` | Engine program of the NVDA add-on (JSON lines on standard input and output) |
| `web/` | Browser client |
| `nvda-addon/` | NVDA add-on (Python) |

### Visitors

A visitor uses an account without being one of its devices: the web client,
and the NVDA add-on. It signs in with a web session (the password is used
once), connects to the signaling WebSocket as a browser-kind client, and
is never listed among the account's devices. It offers nothing to listen
to and receives no offers, but starts sessions with the account's devices:
listening to their sources, and sending to their outputs (a visitor may
always send, as browsers may). `Agent::visitor` gives the native agent this
role; the NVDA add-on runs one such agent per account in
`audionet-visitor.exe`, a separate process started by NVDA, so audio
threads, networking and any failure stay outside the screen reader. The
add-on stores only each account's web session token, encrypted with
Windows DPAPI for the Windows user; signing out ends the session on the
server (`POST /api/v1/logout` with the token).

## 4. Control path versus media path

| | Control path | Media path |
| --- | --- | --- |
| Examples | enumeration, sign-in, signaling, key setup, DNS, HTTP, database, diagnostics formatting | capture callback, encode, send, receive, decode, playout, render callback |
| May block, allocate, log | yes | callbacks: never; sender/receiver threads: bounded, preallocated |

The media path never waits on the control path. Cross-thread signals are
relaxed atomics and bounded SPSC rings.

## 5. Media pipeline

### Sender

```
capture callback (WASAPI thread, MMCSS)
  │ copy + convert to f32, atomics only
  ▼
  idle loopback (no packets for 25 ms): silence for the elapsed time
  ▼
capture ring (SPSC, bounded, drop incoming when full; consumer trims oldest)
  ▼
sender thread: adapt (channel map, fixed-ratio resample to 48 kHz)
  → Opus 10 ms low-delay → RTP → seal (PSK) or str0m writer (WebRTC)
  ▼
non-blocking UDP send (a full socket buffer drops and counts)
```

### Receiver

```
network thread: authenticate → select stream (SSRC) → sequence tracking
  → reorder slots (wait for a missing packet while the playout ring
    has audio) → Opus decode, FEC or PLC for losses
  → outages over 5 frames resynchronize instead of concealing
  ▼
playout ring (SPSC, bounded, 1 s)
  ▼
render callback (WASAPI thread, MMCSS): Playout
  priming → drift/depth-corrected sinc resampling (rubato) → device frames
  underrun: fade to silence, re-prime to target
```

### Buffer boundaries and drop accounting

| Boundary | Overflow | Underrun | Counters |
| --- | --- | --- | --- |
| Capture ring | producer drops the incoming block; consumer trims oldest past a threshold | encoder waits | `ring_overflow`, `stale_trim` |
| Socket send | packet dropped | n/a | `send_would_block`, `send_errors` |
| Kernel receive buffer (1 MB) | OS drops (shows as loss) | n/a | `lost` |
| Reorder slots (16) | n/a | loss declared when the playout ring falls below 15 ms, the slots are nearly full, or nothing arrives for 60 ms | `concealed_frames`, `fec_attempts`, `late_packets`, `outage_resyncs` |
| Playout ring | latency drain (sustained excess), catastrophic trim (safety net) | concealment/silence, re-prime | `underruns`, `latency_drains`, `catastrophic_trims` |

Every drop has its own counter; none are merged.

## 6. Clock drift and buffer depth

Two questions, two terms, two time scales (AGENTS.md §11–13), implemented
in `audionet-engine/src/controller.rs`:

* **Clock drift** (slow, about 25 s): an integrator on the filtered depth
  error estimates the sender-minus-receiver rate difference in ppm, clamped
  to ±1000 ppm.
* **Buffer depth** (medium, about 12 s): a proportional bias pulls depth
  back to target, clamped to ±2000 ppm (inaudible pitch change).

The sum is applied as a continuous resampling ratio, updated every render
callback with ramping. No frame drops or repeats are used for steady-state
correction. The pair forms a critically damped PI loop (the same family as
the delay-locked loops of zita-njbridge and PipeWire's adaptive resampling).

Decisions made from evidence:

* **Hysteresis lock.** A plain PI loop learned stall transients as drift
  and undershot the target by e⁻² ≈ 13.5 % of the disturbance (unit test).
  The integrator now pauses when the error jumps past 15 ms or the playout
  reports a disturbance (priming, underrun, trim), and resumes below 1 ms
  or after 40 s (three depth time constants).
* **Continuous depth measurement.** Packets arrive in 10 ms steps and render
  callbacks sample depth at a nearly fixed phase to those steps, so a raw
  frame count aliased 50 ppm of drift into 200-second flat stretches and
  10 ms jumps (simulation trace). Depth is now measured as if the newest
  packet arrived continuously (`StreamControl::frame_written`).
* **Controlled latency drain.** A hardware soak under heavy CPU load produced
  a 224 ms depth excursion that the clamped rate correction would take
  about 90 s to drain. A sustained excess above 80 ms for 1 s is now removed
  by one crossfaded trim to target+10 ms (`latency_drains`). Smaller excess
  stays with the rate controller; the +200 ms catastrophic trim remains
  only a safety net.

* **Depth-aware loss decision.** With a fixed reorder window of two
  packets, ±20 ms of jitter made 16 % of packets arrive after their audio
  had been concealed (4,241 late in 300 s), whatever the playout target:
  a bigger buffer could not help. A missing packet is now waited for while
  the playout ring still holds at least 15 ms of decoded audio, so the
  whole target is the tolerance for late packets (±20 ms at a 60 ms
  target: 1 late packet). Packets held behind a gap count as buffered
  depth (`StreamControl::set_pending_frames`), so waiting does not look
  like draining.
* **Adaptive target** (`audionet-engine/src/adaptive.rs`, AGENTS.md §25).
  It rises by 10 ms after two incidents within 30 s (late packets, or
  underruns whose audio resumes within 150 ms; longer silences are outages
  and are ignored, as is the reordered backlog just after one), then waits
  10 s for the queue to build. It falls by 5 ms after 120 s without
  incidents, and only if the lowest depth seen leaves 15 ms after the step;
  if a lowered target has to be raised again, the stable time needed
  doubles (up to 30 minutes), so it does not oscillate. Limits: 25 to
  200 ms on the native path (`audionet receive`, starting at 40 ms; use
  `--fixed-target` to disable), 40 to 200 ms for WebRTC speak sessions
  (starting at 60 ms). Every change is reported with its reason. In
  simulation: a clean network settles from 40 to 30 ms with no underruns;
  ±20 ms jitter raises it to 50 ms within a minute and late packets stop;
  a network that worsens after 10 minutes is followed; outages do not move
  it.

Verified in simulation for ±200 ppm drift, jitter, loss, stalls and a
one-hour soak (see `docs/testing.md`), and on hardware (see
`docs/windows-audio.md`).

## 7. Transport

**LAN mode (`audionet send`/`receive`).** RTP (RFC 3550) with Opus
(RFC 7587) over UDP. Each packet is `RTP header | 24-byte random nonce |
XChaCha20-Poly1305(payload) | tag`, the header authenticated, keyed by
HKDF-SHA256 from a 256-bit pre-shared key. Random nonces remove nonce-reuse
risk across restarts. Chosen as the simplest authenticated encryption that
fits the RTP framing (decision recorded with the user: RTP-style framing,
encryption from day one, no insecure default).

**Server-coordinated mode (production, browsers).** WebRTC with
DTLS-SRTP. Browsers use their own stack. Devices use
[str0m](https://github.com/algesten/str0m), a sans-I/O Rust WebRTC library,
with its pure-Rust crypto backend (no OpenSSL or native toolchains needed
on any platform). str0m provides ICE, DTLS, SRTP and RTP/RTCP. It does not
provide a jitter buffer or clock drift correction, so AudioNet's receiver
pipeline runs behind it when a device receives audio; browsers use their
own (NetEQ).

Negotiation is non-trickle: each side gathers all its candidates before
sending its offer or answer. Devices add host, STUN server-reflexive and
TURN relayed candidates. One offer, one answer, one end per session.

**Device to device.** Native apps are devices that also start sessions:
a device can offer a session to another device of the same account
(listen to its source here, or send a source from here to its output).
Each session knows whether it answers or offers, and whether this device
sends or receives; the capture and playback pipelines are the same either
way.

**TURN relay.** Devices allocate a UDP relayed address on the TURN server
the coordination server names (short-lived REST credentials), through the
session's own socket, using `turn-client-proto` (sans-I/O, like str0m).
Packets str0m sends from the relayed candidate go out as TURN data;
relayed data is handed to str0m as if it arrived at the relayed address.
`relay_only` (agent option) offers only the relayed candidate, for
diagnostics.

**str0m holds no audio.** str0m by default holds up to 15 audio packets
(150 ms at 10 ms frames) after a missing one, trying to deliver in order.
Measured through the relay, every such loss starved playout into an
underrun and then a latency jump (150 to 165 ms arrival gaps). AudioNet
sets str0m's audio reordering to 0 packets: its own receiver reorders and
conceals, waiting only as long as the playout buffer allows. After the
change, losses on the direct path are concealed without a single
re-buffering.

Measured through the official relay (both devices relay-only, same PC):
the tone arrives at unity gain; the route shows 0.2 to 0.9 % loss and
occasional 50 to 100 ms arrival gaps (the direct path: 15 to 25 ms), so a
jittery relayed route can re-buffer once or twice in its first seconds
while the adaptive target rises. Whether those gaps come from the relay
route or the relay client needs a longer comparison with a browser on the
same relay.

**Why both.** The LAN mode exercises and measures AudioNet's own pipeline
with no negotiation machinery; WebRTC is required for browsers and gives
mature NAT traversal and encryption for everything else. The media format
is the same (RTP + Opus), so the two stay conceptually aligned.

## 8. Coordination server

Accounts (Argon2id), browser sessions (HttpOnly SameSite=Strict cookies
with Origin checks), password sign-in for devices and device tokens (stored as
SHA-256 hashes), presence, endpoint lists, session routing between a user's
own connections only, and short-lived TURN credentials (coturn REST
scheme). Each connection has a bounded outgoing queue; a client that cannot
keep up is disconnected rather than allowed to delay others. SQLite keeps
self-hosting to one file. Every host-specific value is configuration
(`docs/self-hosting.md`).

### Device and network interruptions in node sessions

A node session does not end when its audio device stops (unplugged,
disabled, reconfigured). It reports the interruption, keeps the WebRTC
connection, and reopens the device every second for up to 30 seconds.
Speak sessions reopen with a fresh packet stage and playout buffer; listen
sessions rebuild the format adapter (the format may have changed) and
advance the RTP timestamp across the gap. ICE disconnections get a
10-second grace period. The web client announces each interruption and
recovery once. This is covered by `crates/audionet-node/tests/device_loss.rs`
with a fake device; a physical unplug has not been tested yet.

## 9. Threads and priorities (Windows)

| Thread | Priority | Blocking allowed |
| --- | --- | --- |
| Capture (event-driven WASAPI) | MMCSS "Pro Audio" (default; `--no-mmcss` disables) | wait on the audio event only |
| Render (event-driven WASAPI) | MMCSS "Pro Audio" | wait on the audio event only |
| Sender / WebRTC session | normal | socket I/O, bounded |
| Network receive | normal | socket receive with 20 ms timeout |
| Control (CLI, agent) | normal | yes |

MMCSS became the default after a soak under full-core compile load showed
starved callbacks (63 ms render gaps, 282 late callbacks, an 82 ms encode)
at normal priority. `REALTIME_PRIORITY_CLASS` is never used.

## 10. Diagnostics

Hot paths write only relaxed atomics (counts, sums, lifetime and per-window
extremes). Reports are produced on control threads as linear text or JSON:
`audionet capture-test`, `send`, `receive` (the AGENTS.md §22 snapshot),
node status lines, and the web client's text diagnostics from `getStats`.

## 11. Open work (in order)

1. Sleep/resume and network changes (a new local address) during node
   sessions; physical unplug test of device recovery.
2. Relay route jitter: compare the native relay client with a browser on
   the same TURN server over minutes; consider a higher starting target
   when ICE selects a relayed pair.
3. Large fan-out: today each listener gets its own session, capture and
   Opus encoder (two simultaneous browser listeners verified end to end;
   each encoder costs about 0.26 ms per 10 ms frame, roughly 2.6 % of one
   core). Encoding once with per-listener lanes, and an SFU for many
   listeners, are worth doing when measurements show that cost matters.
4. macOS system-audio capture (Core Audio process taps) and a macOS app
   bundle; Linux verification on PipeWire with real devices.
5. Lower-latency modes after measurements show margin (phase 5).
