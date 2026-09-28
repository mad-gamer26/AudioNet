# Windows audio backend (`audionet-wasapi`)

Status: endpoint enumeration and event-driven capture (input endpoints and
output loopback) are implemented and have been run against real hardware.
Process capture and render are designed here but **not yet implemented**.

## 1. API choice

- **WASAPI + MMDevice API** (documented Windows Core Audio) is the backend.
  It covers shared and exclusive mode, event-driven I/O, system loopback,
  and per-process loopback on Windows 10 2004+ / Windows 11.
- **Bindings:** Microsoft's official `windows` crate (0.62). Alternatives
  considered: `cpal` (cross-platform, but hides loopback, process capture,
  endpoint roles and event timing, all of which AudioNet must control and
  measure); `wasapi` crate (a convenient wrapper, but adds a layer over the
  same COM calls, and we need direct control of COM threading and error
  detail). Using `windows` directly keeps one well-maintained dependency.
- **ASIO** is not used initially. Nothing requires it yet, WASAPI covers
  loopback and virtual devices, and ASIO drivers are exclusive and
  vendor-specific. Revisit only with a concrete requirement.

## 2. COM initialization and threads

- `ComApartment` (`src/com.rs`) calls `CoInitializeEx(COINIT_MULTITHREADED)`
  and balances it with `CoUninitialize` on drop, on the same thread (the
  guard is `!Send`).
- If the thread already belongs to an STA (`RPC_E_CHANGED_MODE`), AudioNet
  uses it as-is and does not uninitialize. A library must not change the
  apartment of a thread it does not own; the MMDevice API works in both.
- Every interface obtained under the guard is released before the guard
  drops (enumeration keeps all interfaces inside an inner function).
- Enumeration is control-path work: it blocks, allocates and may take tens
  of milliseconds. It never runs on an audio thread.
- Stream threads: one dedicated thread per stream ("audionet-capture"),
  MTA, event-driven (`AUDCLNT_STREAMFLAGS_EVENTCALLBACK` +
  `WaitForSingleObject`). MMCSS ("Pro Audio") is available through
  `capture-test --mmcss` but is **off by default**, because measurements so
  far show no difference (see §9). Never `REALTIME_PRIORITY_CLASS`: it can
  starve the screen reader and the system.

## 3. Endpoint enumeration

`IMMDeviceEnumerator::EnumAudioEndpoints(eAll, mask)`, where `mask` is active
| disabled | unplugged, plus not-present with `audionet list --all`. For each
device:

| Data | Source |
| --- | --- |
| ID | `IMMDevice::GetId` |
| State | `IMMDevice::GetState` |
| Direction | `IMMEndpoint::GetDataFlow` |
| Name | `PKEY_Device_FriendlyName` |
| Description | `PKEY_Device_DeviceDesc` |
| Adapter | `PKEY_DeviceInterface_FriendlyName` |
| Format | `PKEY_AudioEngine_DeviceFormat` (a `WAVEFORMATEX[TENSIBLE]` blob) |
| Default roles | `GetDefaultAudioEndpoint` for each flow × {console, multimedia, communications} |

**No `IAudioClient` is activated during enumeration.** Reading the property
store cannot open streams, change Bluetooth profiles (for example, force
hands-free mode on a headset) or disturb playing audio. The format shown is
therefore the engine's configured device format. When a stream opens,
`IAudioClient::GetMixFormat` is authoritative.

`WAVEFORMATEX` parsing is safe Rust over a byte slice (`src/waveformat.rs`),
tested without Windows. A failure on one endpoint (unreadable property,
malformed format) becomes a warning on that endpoint; enumeration continues.

Endpoint IDs are persisted as `EndpointId { backend: wasapi, native_id }`
but treated as non-permanent (see `docs/protocol.md` §3).

## 4. Loopback

Four distinct facts, never conflated:

1. The endpoint exists (enumeration).
2. It is expected to support loopback: WASAPI documents loopback on
   shared-mode render endpoints, so an **active output** endpoint is
   reported as `expected`.
3. A loopback client initializes (`AUDCLNT_STREAMFLAGS_LOOPBACK`):
   `client_initialized`.
4. Loopback actually delivers buffers: `capture_verified`.

Probing policy: routine enumeration never probes (no activation, no
initialization). Facts 3–4 come from opening a real loopback stream, or from a
future explicit `audionet probe loopback <endpoint>` command. Known side
effects to document when probing is added: initializing a client can wake a
sleeping endpoint, may open the device on some drivers, and on Bluetooth
endpoints may trigger a profile switch.

Loopback characteristics to handle in the capture implementation: loopback
delivers no packets while nothing is playing (silence must be synthesized
from the device clock, not treated as a stall), and event-driven loopback
has historically needed care on older Windows versions. Verify on target
systems.

## 5. Capture stream (implemented: `src/capture.rs`)

Setup runs on the capture thread before audio flows (control-path work):
`GetDevice(id)`, then direction and state checks (input capture needs an
input endpoint, loopback an output endpoint), `Activate(IAudioClient)`,
`GetMixFormat` (parsed with `waveformat.rs`; a converter is chosen once:
f32, i16, packed i24 or i32 container), `Initialize(SHARED, EVENTCALLBACK
[| LOOPBACK], 100 ms buffer, 0)`, `GetDevicePeriod`, `GetBufferSize`,
`CreateEventW` + `SetEventHandle`, `GetService<IAudioCaptureClient>`, then
allocation of the ring, timing stats and a conversion scratch buffer of one
OS buffer, then `Start`. Setup errors reach the caller before
`CaptureStream::start` returns.

The 100 ms OS buffer is slack for late wakeups, not latency: every wakeup
drains all available packets, so in steady state it holds about one period.

The real-time loop (the "callback"): wait on the event with a 100 ms
timeout (bounds stop latency and keeps polling if the event is not
signaled), drain every packet (`GetNextPacketSize`, `GetBuffer`, convert to
f32 in the preallocated scratch buffer, write to the ring, `ReleaseBuffer`),
then record timing with relaxed atomics. No allocation, locks, logging or
I/O. A packet flagged `AUDCLNT_BUFFERFLAGS_SILENT` is written as zeros.
Ring overflow is counted, never waited on.

Errors end the loop and come back through the thread's join handle as a
typed `CaptureError` (`device_disconnected`, `device_not_found`,
`device_in_use`, `audio_service_unavailable`, `unsupported_format`,
`wrong_direction`, `other`) with the operation and HRESULT in words.
`CaptureStream::poll_finished` lets the consumer notice without blocking.
`capture-test --reconnect` then waits, re-resolves the saved endpoint
(never substituting a different device automatically), and opens a new
stream, with fresh diagnostics, when the device returns.

WASAPI behaviors measured on the development machine:

- **Idle loopback delivers nothing.** Loopback of an output with nothing
  playing produced 0 packets and 0 events in 4 s (40 timeout wakeups).
  Endpoints that another application keeps open (here, VAC Line 1) deliver
  continuous near-silent packets instead. AudioNet therefore fills idle
  loopback with silence (see *Idle loopback* below).
- **`DATA_DISCONTINUITY` on the first packet.** Every run flagged exactly
  the first packet after `Start` and none afterwards, and sample-level
  analysis of the captured tone showed no missing audio. It is counted
  separately (`startup_discontinuities`) so real mid-stream glitches are
  not masked.

## 6. Process (per-application) capture

Planned: `ActivateAudioInterfaceAsync` with
`AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK` (include or exclude a process
tree), available on Windows 10 build 20348+ / Windows 11. Activation is
asynchronous and completes on a COM callback, so it belongs on the control
path, with the capture thread started afterwards. Processes are resolved from
the route's executable name at session start, since PIDs are not stable. If
the OS lacks support, report a clear textual error rather than silently
falling back to global loopback.

## 7. Errors (HRESULTs)

Errors carry the failing operation in words plus the system message and
hex HRESULT, e.g. "WASAPI audio backend failed while listing audio endpoints:
<message> (HRESULT 0x...)". Stream code will treat as recoverable:
`AUDCLNT_E_DEVICE_INVALIDATED` (device removed, format changed: tear down
and re-resolve the endpoint), `AUDCLNT_E_SERVICE_NOT_RUNNING`, and
`AUDCLNT_E_RESOURCES_INVALIDATED`. None of these may panic or wedge a
thread.

## 8. Device removal and reappearance

Planned: register an `IMMNotificationClient` on the control thread for
added/removed/state-changed/default-changed notifications. Its callbacks
must return quickly (Windows forbids blocking there), so they post an event
to the control thread, which re-enumerates, re-resolves saved endpoint
references (`resolve_saved_endpoint`), and restarts affected streams with a
**new session ID** (new clock, fresh drift state). A missing endpoint is
shown as unavailable in text, and AudioNet does not silently substitute another.

## 9. Timing and measurement

- Measure capture/render callback intervals, maximum gap and work time from
  the event-wait loop with `QueryPerformanceCounter`. Do not assume the
  device period.
- No `Sleep`-based pacing. If a pacing wait is ever needed, use a
  high-resolution waitable timer and measure its actual wake intervals.
  Any `timeBeginPeriod` use is scoped and reference-counted.
- MMCSS registration is measured before and after, never assumed.

Capture measurements (debug build, development machine, 48 kHz stereo
float, 10 ms device period, 15 s runs):

| Condition | Interval avg / min / max | Late (over 2× period) | Overflow |
| --- | --- | --- | --- |
| Idle | 10.00 / 5.50 / 14.02 ms | 0 | 0 |
| Idle, MMCSS | 10.00 / 6.48 / 13.80 ms | 0 | 0 |
| All 12 cores busy | 10.00 / 4.45 / 15.22 ms | 0 | 0 |
| All 12 cores busy, MMCSS | 10.00 / 6.22 / 14.00 ms | 0 | 0 |

Callback work time was about 0.05 to 0.06 ms on average, with maxima of
0.24 to 0.37 ms. These are single runs; the MMCSS difference is within
run-to-run noise.

### Native path measurements (send/receive, encrypted UDP, release build)

End-to-end latency through Virtual Audio Cable, from the source-side
recording to the far-end recording: **67.6 ms median (67.5 to 67.8 ms over
10 bursts)** at the default 40 ms playout target. Components: capture
period (10 ms), Opus frame (10 ms), low-delay lookahead (2.5 ms), playout
target (40 ms), and part of the 22 ms Windows render buffer.

Recovery from an injected 50 ms render stall: depth rose to about 90 ms and
the depth controller drained it back to the 40 ms target within about 40 s,
without underruns or trims.

One-hour soaks (loopback source to Line 1 through localhost, 48 kHz stereo,
concurrent full-core compile load during part of each run):

| | Normal thread priority | MMCSS on all audio threads (default) |
| --- | --- | --- |
| Packets lost | 11 of 359,805 | **0** of 359,808 |
| Underruns during the run | 11 | **0** |
| Render callbacks later than 2× period | 322 (max gap 63.8 ms) | **0** (max gap 18.6 ms) |
| Capture callbacks later than 2× period | 71 (max gap 57.7 ms) | **0** (max gap 18.9 ms) |
| Longest encode (wall time) | 82.2 ms | 2.3 ms |
| Latency growth | none | none (drift estimate −0.9 ppm) |

Both runs end with one underrun because the sender stops two seconds before
the receiver. This comparison is why capture and render threads use MMCSS
"Pro Audio" and the sender, network and WebRTC threads use MMCSS "Audio"
by default.

### Idle loopback

A loopback source whose endpoint plays nothing delivers no packets, so the
sender used to stop sending and the receiver ran dry. For loopback capture
the capture thread now wakes about once per device period while idle and,
once 25 ms pass without a packet (above the 15.2 ms worst capture interval
measured under full load), writes silence covering all the time since the
last real packet. The stream stays continuous at the nominal rate; the
filled amount is reported as "Silence filled in while nothing was playing".
Input capture is unchanged.

Measured with the same 45 s run before and after (loopback of an idle
Yeti output → receiver on UniMic Output; a 10 s tone played into the Yeti
from 15 s to 25 s):

| | Before | After |
| --- | --- | --- |
| Packets while idle | 0 | 100 per second |
| Underruns / re-bufferings while the sender ran | 3 / 3, then stuck buffering | 0 / 0 |
| Largest gap between packets | 100.9 ms | 31.7 ms |
| Playout depth range (target 40 ms) | 0.2 to 93.9 ms | 27.5 to 55.5 ms |

Real audio (10.40 s) plus filled silence (34.64 s) accounted for the whole
45 s. While idle, depth swings about ±12 ms because the 10 ms wait timeout
is rounded to the default Windows timer tick (about 15.6 ms); a
high-resolution waitable timer could tighten this if it ever matters.

## 10. Verification status

| Check | Status |
| --- | --- |
| Compile (Windows x64 MSVC) | done |
| Unit tests (format parsing, ordering, output, ring, timing, conversion) | done |
| Enumeration on real hardware | done: 7 outputs, 11 inputs, virtual cables, disabled and unplugged endpoints |
| Input capture on real hardware | done: a known 997 Hz tone through Virtual Audio Cable, WAV verified (frequency, level, zero sample discontinuities over 480,000 frames); the Yeti microphone opened and ran but read digital silence (likely hardware-muted) |
| Loopback capture on real hardware | done: same tone and WAV checks; idle-loopback behavior observed |
| Overflow and stale trim on real hardware | done with `--simulate-consumer-stall` (300 ms and 1.2 s) |
| Error text: wrong direction, bad number, disabled device | done |
| Device disconnect and reconnect during capture | **not tested** (needs a physical unplug) |
| Screen reader (NVDA / JAWS) reading the CLI output | **not yet done** |
| Hour-scale soak of capture | **not yet done** |
| Render | done: event-driven WASAPI render verified with tone tests (zero discontinuities) and the native path above |
