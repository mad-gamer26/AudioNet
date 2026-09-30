# AudioNet protocol types

Status: protocol version **0.1** (unstable). Types live in
`crates/audionet-protocol`. The real-time media packet format is **not yet
defined**; see `docs/architecture.md` §10.

## 1. Serialization conventions

These apply to every AudioNet JSON document and message.

- Field names and enum values are `snake_case`.
- **Every field is always present.** An absent value is an explicit `null`,
  never an omitted key. Consumers can rely on a fixed key set.
- Collections are always present; empty is `[]`, never `null`.
- Tagged unions use a `"type"` field inside the object, e.g.
  `{"type": "endpoint_loopback", "endpoint": {...}}`.
- Units are in the field name where not obvious: `sample_rate_hz`,
  `frame_duration_us`.
- Identifiers are validated on input (see §3); invalid documents are rejected
  at the boundary.
- Consumers must ignore unknown fields. Producers add fields without a
  version bump, and remove, rename or change the meaning of a field only
  with one.

## 2. Versioning

`ProtocolVersion { major, minor }`, currently `0.1`.

- `major == 0`: unstable. Peers interoperate only if `major` and `minor`
  both match.
- `major >= 1`: peers with the same `major` interoperate; `minor` adds
  optional behavior only.

Standalone documents (such as CLI JSON output) carry their own `schema` name
and integer `schema_version`, independent of the protocol version.

## 3. Identifiers

| Type | Form | Rules |
| --- | --- | --- |
| `NodeId`, `RouteId`, `SessionId` | JSON string | 1–128 bytes, no control characters |
| `EndpointId` | `{"backend": ..., "native_id": "..."}` | `native_id` 1–1024 bytes, no control characters |

`backend` is one of `wasapi`, `core_audio`, `pipe_wire`, `av_audio_session`,
`a_audio`, `web_audio`.

**Endpoint IDs are not permanent.** They may be saved, but driver updates,
device reinstallation, hardware changes and virtual-cable updates can
remove or recreate them. `audionet_audio::resolve_saved_endpoint` resolves a
saved reference against the current inventory:

| Result | Meaning |
| --- | --- |
| `Available` | ID found and active. |
| `Unavailable { state }` | ID found but disabled, unplugged or not present. |
| `MissingWithCandidate` | ID gone; exactly one active endpoint has the same backend, direction and name. A suggestion only; the user must confirm it. |
| `Missing` | ID gone; no unambiguous replacement. |

A new `SessionId` is issued whenever a stream restarts with a new audio
clock. Receivers reset drift and timing state on a session change.

## 4. Nodes

```json
{ "id": "studio-pc", "display_name": "Studio PC", "platform": "windows",
  "protocol_version": { "major": 0, "minor": 1 } }
```

`platform`: `windows`, `mac_os`, `linux`, `ios`, `android`, `browser`.

## 5. Endpoints

```json
{
  "id": { "backend": "wasapi", "native_id": "{0.0.0.00000000}.{...}" },
  "direction": "output",
  "name": "Speakers (Realtek Audio)",
  "description": "Speakers",
  "adapter": "Realtek Audio",
  "state": "active",
  "default_roles": ["console", "multimedia"],
  "format": {
    "sample_rate_hz": 48000,
    "channels": 2,
    "sample_format": { "encoding": "float", "container_bits": 32, "valid_bits": 32 },
    "channel_mask": 3
  },
  "loopback": "expected"
}
```

| Field | Values |
| --- | --- |
| `direction` | `output` (playback), `input` (recording) |
| `state` | `active`, `disabled`, `unplugged`, `not_present` |
| `default_roles` | subset of `console`, `multimedia`, `communications`, in that order |
| `format` | shared-mode engine format, or `null` if unreadable |
| `sample_format.encoding` | `float`, `integer`, `other` |
| `channel_mask` | WAVE speaker mask or `null` |
| `loopback` | see below |

### Loopback support

Separate facts, weakest to strongest evidence:

| Value | Meaning | Produced by |
| --- | --- | --- |
| `not_applicable` | input endpoint | enumeration |
| `endpoint_not_active` | output endpoint that is not active | enumeration |
| `expected` | backend documents loopback for this endpoint kind; untested | enumeration |
| `client_initialized` | a loopback client initialized | explicit probe / stream open |
| `capture_verified` | loopback delivered buffers | explicit probe / stream open |
| `failed` | initialization or capture failed | explicit probe / stream open |

Routine enumeration never produces the last three (see `docs/windows-audio.md`).

## 6. Sources, destinations and routes

```json
{
  "id": "living-room",
  "source": {
    "node": "studio-pc",
    "kind": { "type": "endpoint_loopback",
              "endpoint": { "backend": "wasapi", "native_id": "..." } }
  },
  "destinations": [
    { "node": "phone", "kind": { "type": "node_default_output" } }
  ]
}
```

Source kinds:

| `type` | Fields | Meaning |
| --- | --- | --- |
| `endpoint_capture` | `endpoint` | record an input endpoint |
| `endpoint_loopback` | `endpoint` | capture what an output endpoint plays |
| `process_capture` | `executable`, `include_process_tree` | one application's audio (resolved to a process at session start) |
| `node_default_input` | — | platform-chosen microphone (browser, phone) |

Destination kinds: `endpoint_render` (`endpoint`), `node_default_output`.

Validation (`AudioRoute::validate`): 1–32 destinations; no duplicate
destinations; no destination that renders into the endpoint the route
captures by loopback on the same node (feedback loop); non-empty executable
name. Existence of nodes and endpoints is checked separately at session start.

## 7. Stream sessions

```json
{
  "session_id": "s-01",
  "route_id": "living-room",
  "source_node": "studio-pc",
  "format": { "codec": "opus", "sample_rate_hz": 48000, "channels": 2,
              "frame_duration_us": 10000 },
  "protocol_version": { "major": 0, "minor": 1 }
}
```

`frame_duration_us` accepts 2500, 5000, 10000, 20000, 40000 or 60000. Opus
streams must be 48 kHz with 1 or 2 channels. The baseline
(`StreamFormat::BASELINE`) is Opus, 48 kHz, stereo, 10 ms (480 samples per
channel per frame), the AGENTS.md first-milestone format, not a tuned optimum.

## 8. Media packets

### LAN mode (pre-shared key)

Each UDP datagram is one RTP packet (RFC 3550) carrying one Opus frame
(RFC 7587), encrypted:

```text
| RTP header, 12 bytes | nonce, 24 bytes | ciphertext of the Opus payload | tag, 16 bytes |
```

* RTP: version 2, no CSRC/extension/padding, payload type 111, marker on
  the first packet, sequence and timestamp start at random values, SSRC
  random per stream. Timestamps count 48 kHz samples.
* Cipher: XChaCha20-Poly1305. Key: HKDF-SHA256(salt `"AudioNet PSK v1"`,
  IKM = 32-byte pre-shared key, info `"media packet key"`). Nonce: random
  per packet. The RTP header is the associated data (authenticated, not
  encrypted).
* Datagrams stay under 1200 bytes.
* Key files hold 64 hexadecimal characters (`audionet keygen`).

Receivers drop packets that fail authentication, duplicates (1024-packet
window) and packets older than what has already been played. A new SSRC is
accepted only after the current sender has been silent for one second.

### Server-coordinated mode

WebRTC: RTP/SRTP with Opus negotiated by SDP (payload type from the
offer, usually 111; `stereo=1` requested by the web client). Devices send
10 ms frames; browsers usually send 20 ms frames, which receivers handle.

## 9. CLI documents

### `audionet list --json`

```json
{
  "schema": "audionet.endpoint_list",
  "schema_version": 1,
  "backend": "wasapi",
  "output_count": 2,
  "input_count": 1,
  "endpoints": [ /* endpoint objects (§5), same order as the text output */ ],
  "warnings": [ { "native_id": "..." , "message": "..." } ]
}
```

Endpoint order: outputs, then inputs; within each, default endpoints first,
then by state (active, disabled, unplugged, not present), then by name
(case-insensitive), then by ID. `warnings` lists per-endpoint problems that
did not stop enumeration; `native_id` is `null` if the endpoint could not be
identified. Exit codes: `0` success (even with warnings), `1` backend
failure (message on stderr naming the subsystem), `2` usage error.

### `audionet capture-test --json`

A single document printed when capture ends (progress text goes to
stderr): `schema` `"audionet.capture_report"`, `schema_version` 1,
`stream` (endpoint, `mode` `input` or `loopback`, device `format`,
`device_period_ns`, `os_buffer_frames`, `ring_capacity_frames`, `mmcss`
`not_requested`, `registered` or `failed`), `elapsed_ms`, `diagnostics`
(`packets`, `frames_captured`, `device_discontinuities`,
`startup_discontinuities`, `silent_packets`, `timestamp_errors`, `timing`
with nanosecond interval and work statistics, `ring` with depth, overflow
and stale-trim counts), `consumer` (`frames_read`, `discontinuities_seen`,
`levels` with `peak_dbfs` and `rms_dbfs`, `null` for digital silence),
`end_reason` (`duration`, `interrupted` or `error`), and `error` (text or
`null`). Durations are integer nanoseconds; an unmeasured value is `null`.
Exit codes: `0` when capture ran to the end or was interrupted, `1` on a
capture or device-selection error, `2` on a usage error.

## 10. Signaling (WebSocket)

Endpoint: `GET /api/v1/ws` on the server's public URL, upgraded to a
WebSocket. Messages are JSON text frames with a `"type"` field
(`audionet_protocol::signal`).

Authentication happens at the upgrade:

* Browsers: the `audionet_session` cookie, and an `Origin` header that is
  in the server's `allowed_origins`.
* Devices: `Authorization: Bearer ann_…` with the token from device sign-in.

The client's first message must be `hello` within 10 seconds. The server
pings every 20 seconds.

### Client → server

| `type` | Fields | Sent by |
| --- | --- | --- |
| `hello` | `protocol_version`, `client { kind: "node" \| "browser", software, platform }` | everyone, first |
| `sharing` | `sharing: bool` | devices, right after `hello` and whenever it changes: whether the device shares its audio. Until a device says, the server takes it as sharing (0.7 devices connected only to share) |
| `endpoints` | `sources: [SourceInfo]`, `destinations: [DestinationInfo]` | devices, whenever they change |
| `list_nodes` | none | browsers |
| `session_offer` | `session_id`, `node_id`, `media`, `sdp` | the side starting a session: a browser, or a device (native apps listen to and talk to other devices of the same account) |
| `session_answer` | `session_id`, `sdp` | the device |
| `session_status` | `session_id`, `state`, `detail` | either side |
| `session_end` | `session_id`, `reason` | either side |
| `ping` | `nonce` | anyone |

### Server → client

| `type` | Fields |
| --- | --- |
| `welcome` | `protocol_version`, `connection_id`, `username`, `node_id` (devices; otherwise `null`), `ice_servers: [{urls, username, credential}]` |
| `nodes` | `nodes: [NodeSummary]` |
| `node_update` | `node: NodeSummary` (a device came online or offline, started or stopped sharing, or its endpoints changed); sent to browsers and to the account's other devices |
| `session_offer` | `session_id`, `from` (offerer connection id), `media`, `sdp` |
| `session_answer`, `session_status`, `session_end` | as sent by the other side |
| `error` | `code`, `message` (plain language, suitable for users), `session_id` (the session a refused `session_offer` would have started, so the offerer can end it with the reason; otherwise `null`) |
| `pong` | `nonce` |

`session_status.state` is `starting`, `active`, `ended` or `failed`, and
`detail` is a plain-language sentence for people. A session can go from
`active` back to `starting` and return, for example "Audio capture
stopped: the device was disconnected. Waiting up to 30 seconds for the
device to come back." followed by "The audio device is back. …", or a
network interruption followed by "Network connection restored. Audio is
flowing." Clients announce those changes once; only `failed` or
`session_end` ends a session. A `detail` beginning with "Warning: " (for
example "Warning: the microphone is sending only digital silence. …") is
announced once, as is the next `active` detail after it (such as "Sound is
now arriving from the microphone.").

**Sharing.** `NodeSummary.sharing` says whether an online device shares
its audio (always false offline; older servers leave it out, meaning
"sharing whenever online"). A device that does not share stays online:
it can be sent to (`speak`) and can listen to others, but the server
refuses `listen` to it and refuses its own `speak` offers to others
(`not_sharing`, in words, for example `"Laptop" is not sharing its audio.`), and
the device refuses them too. When a device stops sharing, the server
ends every session in which it sends, telling both sides (`"Laptop"
stopped sharing its audio.`); what it receives goes on. Progress or an
end for a session that is already over is ignored.

Types:

```json
{ "id": "loopback:{0.0.0.00000000}.{…}", "name": "Sound playing on Speakers",
  "source_type": "loopback", "is_default": true }                    // SourceInfo
{ "id": "output:{0.0.0.00000000}.{…}", "name": "Speakers", "is_default": true }   // DestinationInfo
{ "node_id": "node_…", "name": "Studio PC", "platform": "windows", "online": true,
  "sources": [ … ], "destinations": [ … ] }                           // NodeSummary
{ "kind": "listen", "source_id": "…" }                                // media: device sends audio
{ "kind": "speak", "destination_id": "…" }                            // media: device plays received audio
```

`state` is `starting`, `active`, `ended` or `failed`. Source and
destination ids are opaque; Windows devices use `input:`, `loopback:` and
`output:` prefixes on WASAPI endpoint ids.

Rules enforced by the server: a session can only be offered to an online
device owned by the same account; only the device answers; at most 16
sessions per offering connection; a client whose outgoing queue (128
messages) fills is disconnected. When a party disconnects, the other side
receives `session_end`.

Error codes include `protocol_version`, `wrong_client_kind`,
`bad_message`, `node_offline`, `no_session`, `duplicate_session`,
`too_many_sessions`, `invalid`.

## 11. HTTP API

All bodies are JSON. Errors: HTTP status plus
`{"error": {"code": "...", "message": "..."}}` with a plain-language message.

| Method and path | Auth | Purpose |
| --- | --- | --- |
| `GET /api/v1/health` | none | `{"status":"ok","connections":N}` |
| `GET /api/v1/info` | none | `{"name":"AudioNet","version","protocol_version","allow_registration","email_required","password_reset"}`; `password_reset` is whether the server can send email; clients use it to check a server URL |
| `POST /api/v1/login` | none (Origin checked if present) | `{username, password}` → sets the session cookie; returns `{username, token}` |
| `POST /api/v1/register` | none (Origin checked if present) | `{username, password, email}`: creates an account and signs in, like login, and emails a link to confirm the address. Only if `allow_registration`; within the sign-up limits (`429 sign_up_limit`); reserved names and passwords containing the username are refused (`400`); a missing or malformed address is `400 bad_email`; a taken name is `409 username_taken`, an address another account has confirmed `409 email_taken` (an address only waiting for confirmation elsewhere is accepted) |
| `POST /api/v1/logout` | session | clears the session |
| `GET /api/v1/me` | session or device | `{username, node_id, email, pending_email}`: `email` is the confirmed address (`null` if none), `pending_email` one waiting for confirmation (`null` if none) |
| `POST /api/v1/account/email` | session or device | `{email, password}` (the account password, `401 bad_password` if wrong): the address waits for confirmation and a link is emailed to it; a confirmed address stays in use until then and is emailed a notice of the change. Giving the confirmed address again cancels a change. `{email, pending_email, link_sent}`; `409 email_taken` if another account has confirmed it |
| `POST /api/v1/account/email/send-link` | session or device | emails a new confirmation link to the waiting address (earlier ones stop working) and gives it 7 more days; `{email, pending_email, link_sent}`; `400 no_email`, `503 email_unavailable`, `429 email_limit` |
| `POST /api/v1/email/verify` | none (the link's token) | `{token}` → `{username, email}`: the waiting address becomes the confirmed one, and other accounts waiting for it stop; `400 link_invalid` when used, expired or no longer waiting; `409 email_taken` if another account confirmed it first |
| `POST /api/v1/password/forgot` | none (Origin checked if present) | `{account}` (username or email address) → always `{"requested": true}`, so it does not reveal whether the account exists; emails a reset link only to a confirmed address. `503 email_unavailable` without email settings; `429 reset_limit` after 10 requests from one client address in an hour |
| `POST /api/v1/password/reset` | none (the link's token) | `{token, password}`: sets the password (the link works once, for 1 hour, and only while the address it went to is still the confirmed one), signs out every browser session, and signs this browser in like login. Device tokens stay valid. A rejected password (`400 bad_password`) does not use up the link |
| `POST /api/v1/nodes/sign-in` | account password (throttled like sign-in) | `{username, password, name, platform}` → `{node_id, token, username}`; the only way to add a device to an account; the password is not kept on the device |
| `GET /api/v1/nodes` | session | `{nodes: [NodeSummary]}` |
| `PATCH /api/v1/nodes/{id}` | session | `{name}` renames a device |
| `DELETE /api/v1/nodes/{id}` | session | removes a device and revokes its token |

Failed sign-ins (web and device) are throttled per account name with
exponential backoff.

### Push notifications

| Method and path | Auth | Purpose |
| --- | --- | --- |
| `PUT /api/v1/push/pusher` | device | `{gateway, handle, key, presence, sharing}`: this device (a phone) wants notifications about its account's other devices: `presence` (online, offline), `sharing` (started, stopped sharing). `gateway` must be this server's `push_gateway` (from `/api/v1/info`); `handle` comes from that gateway; `key` is 32 bytes, base64. Both false removes it |
| `DELETE /api/v1/push/pusher` | device | no more notifications for this device |
| `POST /push/v1/register` | none (rate limited) | on a push gateway: `{apns_token, sandbox}` → `{handle}` |
| `POST /push/v1/send` | the handle | on a push gateway: `{handle, payload, collapse_id}`; `410` when the device no longer receives notifications (the server then drops the pusher) |
| `POST /push/v1/unregister` | the handle | on a push gateway: forget the handle |

A notification's title is the account name and its body one of "NAME is
online.", "NAME is offline.", "NAME started sharing its audio.", "NAME
stopped sharing its audio." The server seals `{"title", "body"}` with
ChaCha20-Poly1305 and the device's key (base64 of nonce, ciphertext and
tag), so the gateway and Apple see only a placeholder and ciphertext; the
app's notification extension opens it. "Offline" is sent only after a device
has stayed away 30 seconds, "online" only after an "offline" or a longer
absence, never for devices reconnecting just after the server starts;
sharing only for changes while online.

An account has at most a confirmed address (unique) and one waiting for
confirmation, which does not keep anyone else from using the address and
is dropped when its 7 days end (on a server without email settings, where
it cannot be confirmed, it is kept). Emailed links are
`{public_url}/?verify=TOKEN` (confirm an address, valid 7 days) and `{public_url}/?reset=TOKEN` (choose a new password, valid 1
hour); the web client takes the token out of the address bar at once.
Tokens are stored only as SHA-256 hashes, and only the newest link of each
kind works. An account is sent at most 3 emails an hour, the server at
most 100. `{public_url}/?forgot` opens the web client's "Reset your
password" page (the apps' "Forgot password" opens it).

### `audionet send --json` / `audionet receive --json`

Final reports with `schema` `audionet.send_report` or
`audionet.receive_report`, `schema_version` 1, `end_reason`, `error`, and a
`report` object containing the sender (capture and encoder counters) or
receiver (network, sequence, playout, controller and render timing)
snapshots. Durations are nanoseconds; unmeasured values are `null`.
