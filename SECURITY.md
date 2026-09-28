# Security

## Reporting a vulnerability

Please report security problems privately, not in public issues: use
GitHub's private vulnerability reporting at
https://github.com/mad-gamer26/AudioNet/security/advisories/new, which only the maintainers can read. Include steps to reproduce and the version you
tested. We aim to acknowledge reports within a week.

## Security model

**Media is always encrypted.**

* Server-coordinated sessions use WebRTC: DTLS-SRTP between devices and
  browsers, with keys negotiated per session. The server relays signaling
  only; it never sees audio. TURN relays forward encrypted packets without
  being able to decrypt them.
* Direct LAN streaming (`audionet send` / `receive`) uses a 256-bit
  pre-shared key. Each packet is encrypted and authenticated with
  XChaCha20-Poly1305 (key derived with HKDF-SHA256), and the RTP header is
  authenticated. Anyone holding the key file can listen and inject audio,
  so treat it like a password. Known limitation: replays of packets from an
  *earlier* stream (different sender identity) are not detected in this
  mode; use the server-coordinated mode where that matters.

There is no unencrypted mode.

**Accounts and devices.**

* Passwords are hashed with Argon2id. Sign-in failures are throttled with
  exponential backoff.
* Browser sessions use an `HttpOnly`, `SameSite=Strict` cookie (`Secure`
  over HTTPS). Every cookie-authenticated request and WebSocket must come
  from an allowed origin.
* A device joins an account by signing in with the account name and
  password (throttled like any sign-in). It gets its own 256-bit token and
  does not keep the password; removing the
  device from the account revokes it immediately.
* Tokens are stored only as SHA-256 hashes.
* Each user can see and reach only their own devices.

**TURN.** Credentials are short-lived (default one hour) and derived from
a shared secret (coturn's TURN REST scheme). The example coturn
configuration forbids relaying into private and loopback networks.

**Automatic updates (Windows app).** Official builds install updates only
from a manifest signed with the project's Ed25519 release key, whose
private half never leaves the release machine and is never on the web
server. The manifest pins the package's SHA-256 and size; unsigned or
altered manifests, older versions and non-HTTPS sources are refused, and
package entries must be plain file names. An update that does not start
is rolled back. Source builds never update themselves. See
[docs/releasing.md](docs/releasing.md).

**Server hardening.** The example systemd unit runs the server as an
unprivileged user with a read-only system, no capabilities, and write
access only to its data directory. The web client uses a strict Content
Security Policy (same-origin scripts and connections only).

## Operator responsibilities

* Serve AudioNet only over HTTPS. Devices refuse plain-HTTP servers except
  on `localhost`.
* Keep `AUDIONET_TURN_SECRET` and the database private; never commit them.
* Leave open registration off unless you intend to run a public service.
* Keep the server, coturn and the operating system updated.
