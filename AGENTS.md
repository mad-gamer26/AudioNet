# AGENTS.md

## Purpose

This repository is a low-latency network audio project.

The primary goal is to transport live audio from a server/source machine to one or more remote clients with:

- Very low perceived latency.
- Continuous, stutter-free playback.
- Minimal impact on the source application and server.
- Graceful behavior under packet jitter, packet loss, CPU scheduling delays, GC pauses, Wi-Fi variability, and clock drift.
- Predictable behavior across long-running sessions.
- Automatic recovery from temporary disturbances.
- Diagnostics detailed enough to identify the actual cause of audio defects before tuning parameters.
- Accessible operation for blind users. Do not introduce workflows that require visual-only diagnostics or controls.

This file is primarily written for OpenAI Codex and other coding agents working in this repository.

The most important principle is:

> Do not guess at real-time audio problems. Measure them.

A change that lowers nominal latency but causes occasional underruns, crackles, growing delay, server stalls, or unstable long-running behavior is a regression.

---

# 1. Engineering Priorities

Use this priority order when tradeoffs are necessary:

1. No source/server stalls caused by the streaming pipeline.
2. Continuous playback without stutter, crackle, or repeated underruns.
3. Bounded latency that does not grow indefinitely.
4. Automatic recovery after temporary stalls or network disturbances.
5. Low end-to-end latency.
6. Audio quality.
7. Bandwidth efficiency.
8. CPU efficiency, provided real-time reliability is preserved.

Do not sacrifice items 1-4 merely to report a smaller latency number.

A stable 40-60 ms stream is better than a nominal 10 ms stream that glitches.

---

# 2. Do Not Treat This as a Generic Networking Application

Real-time audio has deadlines.

Bytes arriving eventually is not sufficient. Audio must be available before the playback device requests it.

Never design the audio path as:

```
capture -> send socket -> receive socket -> playback
```

with direct blocking dependencies between stages.

The pipeline must instead be decoupled:

```
source application
    ->
OS audio capture
    ->
capture-side bounded buffer
    ->
encoder / packetizer
    ->
network sender
    ->
UDP / real-time transport
    ->
network receiver
    ->
decoder
    ->
per-stream playout buffer
    ->
drift/depth correction
    ->
audio render callback
    ->
output device
```

No network operation may directly block an audio callback.

No slow encoder, logger, file writer, UI callback, DNS operation, HTTP request, database operation, or other non-real-time work may execute synchronously in an audio callback.

---

# 3. Establish a Measured Baseline Before Optimizing

Before making latency-related changes, establish the current measured state.

At minimum collect:

- Audio capture callback interval.
- Maximum capture callback gap.
- Capture callback execution time.
- Encoder execution time.
- Packet frame duration.
- Packets sent per second.
- UDP send errors.
- UDP receive inter-packet gaps.
- Packet sequence gaps.
- Packet loss percentage.
- Reordered packets.
- Duplicate packets.
- Network jitter estimate.
- Decoder execution time.
- Playout buffer depth in milliseconds.
- Minimum/average/maximum playout depth over a measurement window.
- Render callback interval.
- Maximum render callback gap.
- Render callback execution time.
- Audio underrun count.
- Buffer overflow/drop count.
- Stale-audio drop count.
- Concealment/PLC activation count.
- Current clock-drift estimate in ppm.
- Current resampling correction ratio.
- Current depth-correction bias.
- Estimated end-to-end latency if practical.
- CPU usage of capture, encode, network receive, decode, and render paths.
- Allocation rate / GC pauses if using a managed runtime.

When diagnosing stutter, capture diagnostics covering the exact time at which the stutter occurred.

Do not tune based only on average values. Long-tail scheduling delays matter.

---

# 4. No Blind Tuning

Do not change any of the following merely because audio stutters:

- Buffer size.
- Jitter buffer size.
- Opus frame duration.
- Socket buffer size.
- Thread priority.
- Timer resolution.
- Audio-device period.
- Sample rate.
- Encoder bitrate.
- Packet pacing.
- Resampling ratio.

First identify which measured stage is failing.

Examples:

If:\
packet arrival is smooth\
but render underruns occur

investigate the receiver/playout/render path.

If:\
sender capture callbacks show large gaps

investigate source capture scheduling before touching the network.

If:\
sender timing is stable\
but receiver inter-packet gaps spike

investigate network/OS delivery/jitter buffering.

If:\
playout depth continuously grows

investigate clock-rate mismatch or inadequate depth control.

If:\
playout depth continuously shrinks

investigate clock-rate mismatch, packet loss, sender starvation, or an over-aggressive correction rate.

If:\
latency jumps after a temporary stall and never returns

investigate depth correction. Do not merely decrease the fixed buffer size.

Every tuning change should have:

1. A hypothesis.
2. A metric that can prove or disprove the hypothesis.
3. A before measurement.
4. The change.
5. An after measurement.
6. A rollback if the result is worse.

---

# 5. Audio Thread Rules

Treat capture and render callbacks as real-time code.

Inside an audio callback, avoid:

- Blocking locks.
- Waiting on another thread.
- Socket I/O.
- File I/O.
- Console output.
- Normal verbose logging.
- Memory allocation.
- Unbounded loops.
- Task creation.
- Thread creation.
- Process launching.
- Synchronous IPC.
- DNS.
- HTTP.
- UI calls.

Prefer:

- Preallocated buffers.
- Reusable scratch arrays.
- SPSC ring buffers.
- Lock-free or wait-free producer/consumer handoff where practical.
- Atomic counters for diagnostics.
- Fixed and predictable workloads.

Do not introduce a mutex into a proven lock-free hot path unless correctness absolutely requires it and measurements show the lock cannot violate audio deadlines.

Never hold a lock while invoking unknown/user callbacks.

---

# 6. Use Bounded Queues

All real-time audio queues must be bounded.

Do not permit audio to accumulate indefinitely.

For live audio:

> Old audio is usually less valuable than current audio.

If the producer outruns the consumer and a queue exceeds its safe maximum, prefer dropping the oldest stale audio and recovering toward the live playout point rather than allowing latency to grow without limit.

Record every forced drop in diagnostics.

The system must distinguish at least:

- Packet loss.
- Ring-buffer overflow.
- Intentional stale-audio trimming.
- User-requested latency drain.
- Catastrophic safety trimming.

Do not combine unrelated drop causes into one opaque "drops" counter.

---

# 7. Underrun Behavior

The audio render callback must always receive the number of samples it requested.

If insufficient real audio is available:

1. Never block waiting for network data.
2. Return silence or appropriate concealment for the missing samples.
3. Increment a diagnostic underrun/concealment counter.
4. Smooth discontinuities when practical.

Do not abruptly jump from a non-zero sample to zero if a short fade or codec PLC can avoid an audible click.

For brief packet loss with Opus, use Opus PLC/FEC where appropriate.

For render-side starvation unrelated to a missing encoded packet, use short edge smoothing/concealment.

Do not synthesize concealment indefinitely after the source has clearly stopped. After a bounded interval, settle to silence.

---

# 8. Transport

Prefer UDP or an established real-time media transport.

Do not switch to TCP simply to make packet delivery reliable unless the product requirements explicitly favor reliability over real-time latency.

TCP retransmission and head-of-line blocking can turn one lost packet into a latency spike.

If using raw UDP, the application protocol must include enough information to detect:

- Sequence.
- Stream/session identity.
- Audio format.
- Codec.
- Frame duration or samples per frame.
- Timestamp/sample position where useful.
- Lost packets.
- Reordered packets.
- Duplicate packets.
- Stream restarts.

Packets must be independently parseable enough that one missing packet does not corrupt all following audio.

Keep media packet sizes comfortably below the path MTU. Avoid IP fragmentation.

A typical safe target is to keep UDP datagrams below roughly 1200 bytes unless measurements and environment guarantees justify another value.

---

# 9. Opus Guidance

For compressed interactive audio, Opus is the preferred default unless project requirements dictate otherwise.

Preferred baseline:

- 48 kHz.
- Stereo when source material is stereo.
- OPUS\_APPLICATION\_RESTRICTED\_LOWDELAY for minimum codec delay where available and appropriate.
- Start with 10 ms frames.
- Consider 5 ms on clean low-jitter networks.
- Consider 20 ms on difficult WAN links.
- Do not begin by chasing 2.5 ms frames.

Enable in-band FEC when appropriate for the expected network.

Use packet-loss estimates to configure FEC behavior if the codec API supports it.

Do not assume lower frame duration is always better. Smaller frames increase:

- Packets per second.
- Scheduling pressure.
- Per-packet overhead.
- Encryption overhead.
- Kernel/socket activity.
- Sensitivity to scheduler jitter.

Measure.

---

# 10. Jitter Handling

Do not play packets according to their raw network arrival timing.

Network arrival timing is not the playback clock.

Use a playout buffer or jitter-buffer mechanism.

The receiver should absorb ordinary packet timing variation while maintaining a bounded target latency.

The target may be adaptive.

A useful conceptual controller is:

```
measured jitter increases
    -> cautiously increase target playout depth

network remains stable for a sustained period
    -> slowly reduce target depth
```

Do not rapidly oscillate the target latency.

When decreasing latency, drain excess depth smoothly when possible rather than repeatedly deleting chunks that produce audible discontinuities.

Sudden catastrophic queue bloat may justify trimming stale audio.

---

# 11. Clock Drift Is Mandatory to Handle

Never assume:

```
sender 48000 Hz == receiver 48000 Hz
```

Two free-running audio devices have independent physical clocks.

Even if both report 48 kHz, their real rates can differ by tens or hundreds of ppm.

Without correction the receiver buffer will eventually either:

- Overflow, or
- Underrun.

This may take seconds or minutes and may falsely appear to be random instability.

The architecture must support long-term clock-drift compensation.

Preferred approach:

1. Measure effective sender rate relative to receiver consumption over a multi-second window.
2. Reject implausible/outlier estimates.
3. Smooth the estimate.
4. Use continuous resampling to correct the small rate difference.
5. Update the resampling ratio slowly.

Do not perform frequent single-frame drops/repeats as the primary steady-state clock correction strategy. Those can become audible, especially on tonal content.

A small continuous resampling correction of a few hundred ppm is preferable.

Expose the estimated drift in ppm in diagnostics.

---

# 12. Clock Matching and Buffer Depth Are Different Problems

Do not confuse rate matching with latency recovery.

Clock-rate correction answers:

> Are sender and receiver producing/consuming audio at the same long-term rate?

Depth correction answers:

> Is the current receive buffer sitting near the desired latency target?

A stall can increase buffer depth even after clock rates are perfectly matched.

If:

```
target depth = 30 ms
temporary stall occurs
current depth = 120 ms
sender rate == receiver rate
```

then the system can remain at 120 ms forever unless a depth controller exists.

Implement a slow depth-feedback term that gently biases the resampling/playout rate toward the target.

The depth controller must:

- Be gradual during ordinary correction.
- Avoid audible pitch modulation.
- Be clamped to a safe maximum.
- Have a catastrophic stale-audio trim as a final safety net.

Do not use catastrophic trimming as the normal depth-control mechanism.

---

# 13. Separate Control Time Scales

Do not make every controller react at the same speed.

Typical conceptual time scales:

Fast:

- Audio render deadline.
- Packet parsing.
- PLC/concealment.
- Ring-buffer reads/writes.

Medium:

- Jitter-buffer adaptation.
- Recovery from temporary queue depth changes.

Slow:

- Clock-drift estimation.
- Long-term latency reduction after stability.
- Rate-ratio smoothing.

Very slow:

- Long-run auto-tuning policies.

A controller that reacts to instantaneous buffer changes as if they were clock drift will likely oscillate.

Network jitter must not directly drive rapid sample-rate modulation.

---

# 14. Capture-Side Isolation

The source application must not be stalled by networking.

Never design:

```
app audio
  -> encode
  -> blocking send
  -> app continues
```

Prefer:

```
OS audio callback
  -> bounded SPSC ring
  -> encoder thread
  -> network sender
```

If the network cannot keep up, discard stale queued audio according to policy rather than applying backpressure into the source application.

For Linux/Wine, prefer PipeWire-native capture/routing where possible.

For Windows, prefer WASAPI or ASIO according to requirements.

For per-application capture, use the platform-supported process/session capture mechanism rather than unnecessary global loopback when available.

---

# 15. PipeWire / Linux Guidance

For Linux servers, especially Wine workloads:

- Prefer PipeWire for audio graph routing and capture.
- Keep capture paths native to the host where practical rather than attempting to capture inside Wine.
- Use appropriate PipeWire real-time scheduling support.
- Avoid shelling out to command-line audio utilities on the real-time path.
- Do not parse `pw-*` command output for every audio block.
- Resolve nodes/routes during setup and keep the streaming hot path direct.

Suggested conceptual path:

```
Wine application
    ->
PipeWire node / virtual sink
    ->
native capture
    ->
bounded audio ring
    ->
encoder
    ->
network
```

The server should remain responsive even if the client disconnects or has a poor connection.

---

# 16. Windows Guidance

On Windows:

- Use WASAPI or ASIO according to the selected backend.
- Use event-driven audio APIs where practical.
- Consider MMCSS for dedicated capture/render/network-audio threads.
- Do not use REALTIME\_PRIORITY\_CLASS as a casual optimization.
- Preserve system responsiveness and screen-reader operation.
- If a pacing loop depends on Windows timer waits, verify timer resolution and measured wake intervals.
- Do not assume a requested 5 or 10 ms wait actually wakes at that interval.

If the project uses timeBeginPeriod/timeEndPeriod or equivalent, scope and reference-count it correctly.

Measure render/capture callback gaps rather than assuming Windows scheduling behaves ideally.

---

# 17. Networking Threads

Network receive should have its own dedicated long-lived thread or equivalent real-time-safe mechanism.

Avoid relying solely on a congested generic thread pool for packet receive/dispatch.

The network thread should:

1. Receive packet.
2. Record lightweight timing diagnostics.
3. Validate and parse packet.
4. Decode or hand off to a decoder stage.
5. Write decoded samples into the destination session's bounded ring.

Do not perform UI work, disk writes, HTTP operations, or heavy logging in the receive loop.

Use a sufficiently large OS socket receive buffer to survive short application scheduling stalls.

The kernel socket buffer is not the same thing as the audio playout target.

A large kernel receive buffer does not inherently require high playback latency.

---

# 18. Per-Stream State

Each independent remote sender/stream should own independent timing state.

Do not allow two senders to fight over one clock-drift estimator or one reset-prone buffer.

Per-stream state should normally include:

- Stream/session ID.
- Decoder.
- Sequence tracking.
- Packet-loss/reorder tracking.
- Ring buffer.
- Drift estimator.
- Resampler.
- Depth controller.
- Concealment state.
- Diagnostics.

Mix streams only after each stream has been independently stabilized to the receiver render clock.

---

# 19. Packet Loss and Reordering

UDP loss is expected and must not be treated as an exceptional crash condition.

On a missing packet:

- Detect it by sequence.
- Use codec PLC/FEC if available.
- Otherwise conceal/silence the gap.
- Continue with newer packets.

Do not wait indefinitely for a missing packet.

Late audio that has already missed its playout deadline is stale. Do not insert it into already-played time.

Reordered packets may be accepted only while they remain useful before their playout deadline.

Track late drops separately from network packet loss if practical.

---

# 20. Garbage Collection and Allocation

If using .NET, Java, Go, JavaScript, Python, or another garbage-collected runtime, allocation behavior matters.

The steady-state real-time audio path should allocate as little as practical.

Prefer:

- Array pools.
- Reusable byte arrays.
- Reusable float buffers.
- Long-lived codec state.
- Long-lived packet scratch buffers.
- Preallocated diagnostic structures.

Do not allocate one new audio buffer for every packet if avoidable.

Do not force garbage collections during streaming.

Measure GC pauses and allocation rate if unexplained periodic glitches occur.

Do not blindly change GC mode without measuring.

---

# 21. Logging

Logging must never be allowed to cause the problem it is diagnosing.

Hot-path diagnostics should use:

- Atomic counters.
- Timestamps.
- Min/max accumulators.
- Preallocated event rings where necessary.

A slower background diagnostics thread may periodically snapshot the counters and write logs.

Do not write a log line for every audio callback or packet in normal operation.

Detailed packet tracing may exist as an explicit temporary diagnostic mode, but its performance overhead must be documented.

---

# 22. Required Diagnostic Snapshot

Provide a concise machine-readable or plain-text snapshot similar to:

```
capture:
  callback_max_gap_ms=...
  callback_max_work_ms=...
  frames=...

sender:
  opus_frame_ms=...
  encode_max_ms=...
  packets_per_sec=...
  send_errors=...

network:
  rx_max_gap_ms=...
  jitter_ms=...
  loss_pct=...
  reordered=...
  late=...

receiver:
  decode_max_ms=...
  buffer_current_ms=...
  buffer_min_ms=...
  buffer_max_ms=...
  underruns=...
  overflow_drops=...
  stale_drops=...
  concealments=...

sync:
  drift_ppm=...
  resample_ratio=...
  depth_error_ms=...
  depth_bias=...

render:
  callback_max_gap_ms=...
  callback_max_work_ms=...

process:
  cpu_pct=...
  alloc_bytes_per_sec=...
  gc_pause_max_ms=...
```

Exact field names may differ, but equivalent observability is required.

Diagnostics should be usable with a screen reader and exportable as plain text.

---

# 23. Accessibility Requirements

This application is expected to be usable by blind users.

Do not make important audio diagnostics accessible only through:

- Graphs.
- Color.
- Visual meters.
- Hover tooltips.
- Waveform displays.

All important state must have a textual representation.

Controls must have accessible names and meaningful keyboard navigation.

If adding a graph or visual diagnostic, also provide a textual summary and copy/export option.

Error messages must identify the failing subsystem in text.

Do not remove existing accessibility behavior while optimizing audio.

---

# 24. Configuration Philosophy

Avoid exposing raw expert parameters unless necessary.

Prefer named operating modes such as:

- Ultra Low Latency
- Balanced
- Reliable

Internally, each mode should configure a coherent set of parameters.

Example starting points only — measure before adopting:

Ultra Low Latency:\
Opus frame: 5 ms\
target playout: 15-20 ms\
intended for: clean LAN / excellent connection

Balanced:\
Opus frame: 10 ms\
target playout: 25-40 ms\
intended for: good Wi-Fi / WAN

Reliable:\
Opus frame: 20 ms\
target playout: 50-80+ ms\
intended for: unstable WAN

These numbers are not universal truths.

Do not hard-code them as "optimal" without testing.

Automatic tuning should be preferred over requiring users to understand jitter, ppm, callback periods, and ring-buffer sizing.

---

# 25. Automatic Tuning

Auto-tuning should optimize for the smallest stable latency, not the smallest possible buffer.

A reasonable policy:

1. Begin at a conservative safe target.
2. Observe underruns, packet jitter, render gaps, and buffer depth.
3. If instability occurs, raise the target quickly enough to recover.
4. If the system remains stable for a sustained period, lower the target slowly.
5. Never react to one isolated event by making large permanent changes.
6. Clamp target latency to safe configured minimum/maximum values.
7. Expose why auto-tuning changed the target.

Examples of useful explanations:

```
target increased from 25 ms to 35 ms:
repeated receive gaps exceeded available playout margin

target decreased from 40 ms to 35 ms:
120 seconds stable with zero underruns
```

This makes behavior debuggable.

---

# 26. Latency Changes at Runtime

When the user lowers target latency, do not necessarily wait passively for a large buffer to drain over minutes.

Use a controlled fast-approach mechanism.

Possible strategies include:

- Slight temporary resampling/rate bias.
- Controlled stale-audio trimming for large excess depth.
- A combination.

Avoid repeated chunk drops that cause a click every time the target changes.

When increasing target latency, allow the queue to build toward the new target instead of inserting artificial discontinuities.

---

# 27. Do Not Over-Optimize the Wrong Layer

Before rewriting codec/network logic, identify where latency is actually spent.

End-to-end latency may contain:

```
source audio engine period
+ capture period
+ capture buffering
+ codec frame duration
+ encoder delay
+ packet scheduling
+ network transit
+ jitter/playout target
+ decoder delay
+ render buffering
+ output-device period
```

Measure these where possible.

Reducing a 10 ms Opus frame to 5 ms provides little value if the output device itself buffers 60 ms.

---

# 28. Do Not Use Sleep-Based Precision Without Verification

Thread.Sleep, Task.Delay, timers, and generic event-loop timers are not assumed to be precise enough for audio pacing.

If a pacing mechanism is required:

- Prefer hardware/audio callbacks or high-resolution waitable mechanisms.
- Measure actual wake intervals.
- Record maximum scheduling gap.
- Account for OS timer resolution.

Do not claim a 5 ms scheduler because the code contains `Sleep(5)`.

---

# 29. Testing Requirements

Audio changes require more than unit tests.

Maintain tests for:

- Packet serialization/parsing.
- Sequence wraparound.
- Packet loss.
- Packet reordering.
- Duplicate packets.
- Stream restart.
- Ring-buffer overflow behavior.
- Ring-buffer underrun behavior.
- Stale-audio trimming.
- Codec frame changes.
- Decoder resets.
- Clock-drift estimator.
- Depth controller.
- Resampling ratio clamps.
- Long-running no-drift stability.
- Long-running positive drift.
- Long-running negative drift.
- Temporary sender stall.
- Temporary receiver stall.
- Burst packet delivery.
- Jitter patterns.
- Output device change.
- Source device change.
- Client disconnect/reconnect.

Include deterministic simulated-network tests where possible.

Simulate:

- 0% packet loss.
- 1% loss.
- 5% loss.
- Burst loss.
- ±2 ms jitter.
- ±10 ms jitter.
- 20-50 ms occasional scheduling stalls.
- Reordering.
- Sender clock +50 ppm.
- Sender clock +200 ppm.
- Sender clock -200 ppm.

A test should verify not only "no crash" but also:

- Latency remains bounded.
- Buffer returns toward target after disruption.
- Underrun behavior is bounded.
- No unbounded memory growth occurs.

---

# 30. Long-Running Tests Matter

Many clock and latency bugs do not appear immediately.

Run soak tests long enough to expose:

- Clock drift.
- GC patterns.
- Buffer ratcheting.
- Leaks.
- Timer/scheduler anomalies.
- Codec-state leaks.
- Reconnect leaks.

A system that works for 30 seconds but accumulates 300 ms of latency after 3 hours is not correct.

---

# 31. Performance Changes Must Preserve Correctness

Do not "simplify" or delete a component merely because it looks redundant.

Before removing code related to:

- Ring buffering.
- Jitter handling.
- Clock correction.
- Depth correction.
- FEC.
- PLC/concealment.
- Thread priority.
- Timer resolution.
- Packet sequencing.
- Stale-data trimming.
- Diagnostics.

first determine why it exists and which failure mode it protects against.

If the reason is unclear, research the relevant code/history and measure before changing it.

Real-time audio code often contains non-obvious safeguards added after real field failures.

---

# 32. Codex-Specific Working Rules

When working on an audio bug, Codex must begin by answering internally:

1. What exact audible symptom is reported?
2. Which subsystem(s) could produce it?
3. Which existing diagnostics distinguish those causes?
4. What evidence is currently available?
5. What additional instrumentation is needed before changing behavior?

Do not begin with a random implementation change.

When diagnostics are insufficient, the preferred first patch is often instrumentation.

For every proposed audio fix, summarize:

```
Hypothesis:
Evidence:
Change:
Expected metric change:
Risks:
Verification:
```

Do not report success merely because the project compiles.

Do not report "stutter fixed" without evidence from tests or runtime measurements that exercise the failure.

If only static analysis was possible, state that explicitly.

---

# 33. Research Before Reimplementing Known Problems

Before implementing a custom solution for:

- Jitter buffering.
- Packet loss concealment.
- Clock recovery.
- Drift resampling.
- Low-latency codec behavior.
- NAT traversal.
- Congestion control.

research established implementations and standards.

Relevant technologies/projects may include:

- Opus / libopus.
- WebRTC audio / NetEQ.
- RTP.
- PipeWire.
- JACK / NetJACK.
- Jamulus.
- SonoBus.
- NAudio.
- WASAPI.
- ASIO.

Do not blindly copy code.

Use research to understand established algorithms, failure modes, and terminology.

Prefer mature libraries for codec and DSP fundamentals where possible.

---

# 34. WebRTC Versus Custom UDP

If the project requirements are compatible with WebRTC, strongly consider using it rather than recreating all real-time network behavior.

WebRTC can provide mature implementations of:

- RTP media transport.
- Jitter handling.
- Packet-loss behavior.
- Congestion control.
- NAT traversal.
- Timing logic.
- Opus integration.

A custom UDP protocol is reasonable when:

- The environment is controlled.
- Extremely specific latency behavior is required.
- The product does not need browser/WebRTC interoperability.
- The team is prepared to own jitter, sequencing, timing, and recovery.

Do not reject WebRTC merely because "UDP is faster." WebRTC commonly uses UDP.

---

# 35. Security Must Not Block the Audio Thread

Authentication, encryption setup, key exchange, and authorization belong outside the real-time callback.

Per-packet authenticated encryption may be used, but:

- Reuse state appropriately.
- Avoid expensive allocation.
- Measure encryption/decryption cost.
- Keep packet processing bounded.

Never disable transport security merely to solve a latency bug without evidence that cryptographic processing is actually the bottleneck.

---

# 36. Server Load and Backpressure

The server/source machine must remain responsive.

If client processing/network performance degrades:

- Do not allow infinite queues.
- Do not allow a slow client to stall capture.
- Do not let one client delay all other clients.
- Prefer per-client bounded send queues if multiple clients are supported.
- Drop stale media for an overloaded client.
- Disconnect persistently unhealthy clients if necessary.

Track client-specific queue depth and drops.

---

# 37. Multiple Clients

For multiple listeners, avoid doing redundant expensive work unless necessary.

A reasonable design can be:

```
capture once
  ->
encode once per codec/profile
  ->
distribute packets to clients
```

If different clients require different codecs/bitrates, isolate those encoder lanes.

A slow client must never backpressure another client or the capture path.

---

# 38. Failure Recovery

The system should recover automatically from:

- Client disconnect.
- Server/source restart.
- Audio device removal.
- Audio device reappearance.
- Network interface change.
- Temporary packet outage.
- Stream/session ID change.
- Codec reinitialization.
- Sleep/resume where relevant.

On reconnect, reset stale timing and drift state appropriately.

Do not carry an old stream's clock estimate into an unrelated new audio clock unless continuity is known.

---

# 39. Useful Failure Signatures

Use these as diagnostic heuristics, not absolute rules.

Repeated short clicks at a regular cadence:

- Possible clock correction implemented as discrete drops/repeats.
- Possible periodic GC.
- Possible periodic timer/scheduler wake issue.

Stutter only under CPU load:

- Thread priority/scheduling issue.
- Hot-path allocation/GC.
- Blocking lock.
- Encoder overload.

Stutter with low CPU but large receive gaps:

- Network jitter.
- Wi-Fi power saving/interference.
- OS socket delivery scheduling.
- Sender pacing problem.

Latency slowly grows:

- Sender/receiver clock mismatch.
- Queue lacks depth feedback.
- Slow-client backpressure.
- Stale packets accepted too late.

Latency jumps after a stall and remains high:

- Missing depth correction.

Audio runs fine briefly then underruns:

- Clock mismatch.
- Buffer target too close to jitter margin.
- Rate controller wrong sign.

Audio is crackly when latency auto-tuning changes:

- Abrupt trimming.
- Aggressive rate modulation.
- Target controller oscillation.

---

# 40. Definition of Done for a Low-Latency Audio Change

A low-latency audio change is not complete until:

- It builds.
- Relevant automated tests pass.
- Hot-path allocation behavior has not regressed materially.
- There is no new blocking operation on capture/render threads.
- Latency remains bounded.
- The system recovers after an induced stall.
- A drift test does not accumulate unbounded buffer error.
- Diagnostics can explain the current state.
- Accessibility is preserved.
- The change's expected latency/reliability effect is documented.

If runtime testing cannot be performed in the current environment, clearly state what remains unverified.

---

# 41. Recommended Initial Architecture

When starting this repository from scratch, prefer this baseline:

Server/source:

```
PipeWire/WASAPI/ASIO capture
    ->
lock-free bounded SPSC PCM ring
    ->
dedicated encoder thread
    ->
Opus 48 kHz stereo, 10 ms frames
    ->
packet sequence + stream ID + format metadata
    ->
dedicated UDP sender
```

Client:

```
dedicated UDP receive thread
    ->
sequence/loss tracking
    ->
Opus decoder with PLC/FEC
    ->
per-stream bounded SPSC float ring
    ->
slow clock-drift estimator
    ->
continuous drift resampler
    ->
slow buffer-depth controller
    ->
render callback
    ->
WASAPI/ASIO/CoreAudio/PipeWire output
```

Start conservatively.

Do not optimize below a stable 25-40 ms playout target until diagnostics demonstrate sufficient scheduling and network margin.

---

# 42. First Milestone

The first milestone is NOT "lowest possible latency."

The first milestone is:

- One source.
- One client.
- 48 kHz stereo.
- Opus 10 ms.
- UDP.
- Stable bounded buffering.
- No source backpressure.
- No unbounded latency.
- Diagnostic snapshot.
- Correct clock-drift handling.
- Correct recovery after a 50 ms artificial network/receiver stall.
- One-hour soak without increasing latency.

Only after this works should lower-latency modes be introduced.

---

# 43. Optimization Sequence

Optimize in this order:

Phase 1: Correctness

- Stable capture.
- Stable packetization.
- Correct decode.
- Bounded buffers.
- No blocking callbacks.

Phase 2: Observability

- Packet gaps.
- Callback gaps.
- Buffer depth.
- Underruns.
- Drift.
- CPU.
- GC.

Phase 3: Long-term stability

- Clock recovery.
- Depth feedback.
- Reconnect behavior.
- Soak testing.

Phase 4: Loss/jitter robustness

- FEC/PLC.
- Adaptive target.
- Burst-loss behavior.

Phase 5: Latency

- Reduce capture/output periods where possible.
- Reduce frame duration when justified.
- Lower stable playout target gradually.

Phase 6: Efficiency

- Reduce allocations.
- Reduce copies.
- Reduce codec/network overhead.

Do not jump directly to Phase 5.

---

# 44. Code Review Checklist

For every change touching the real-time path, verify:

- Does this allocate?
- Can this block?
- Can this lock?
- Can this perform I/O?
- Can this trigger arbitrary callbacks?
- Can this make queue length unbounded?
- Can this retain stale audio?
- Can this change the playback clock?
- Can this alter sample count?
- Can this reset drift state?
- Can this introduce a discontinuity?
- Can this run slower than the audio period?
- How is failure measured?
- How does the system recover?

If any answer is unclear, investigate before merging.

---

# 45. Final Principle

The goal is not to discover one perfect set of audio constants.

The goal is to build a system that remains stable when reality is imperfect.

Networks jitter.\
Schedulers wake late.\
Audio clocks disagree.\
Packets disappear.\
CPU load changes.\
Devices behave differently.\
Clients disconnect.

A robust low-latency audio engine observes these conditions and continuously keeps itself near a safe operating point.

Do not build a fragile system that requires the user to tune every number correctly.

Build the control system that does the tuning for them.
