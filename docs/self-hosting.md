# Running your own AudioNet server

This guide sets up a complete AudioNet server on a Linux machine, for
example at `https://audionet.example.com`. Nothing in AudioNet depends on
the project's official instance: devices and the web client talk to
whatever server URL you give them.

## What you are installing

| Component | Role | Listens on |
| --- | --- | --- |
| `audionet-server` | Accounts, device sign-in, presence, signaling, web client | `127.0.0.1:8740` (behind the proxy) |
| nginx (or another TLS reverse proxy) | HTTPS and WebSocket termination | TCP 80, 443 |
| coturn | STUN (public address discovery) and TURN (relay when direct audio is impossible) | UDP+TCP 3478, TCP 5349 (TLS), UDP 49160–49359 (relays) |

Audio never passes through `audionet-server`. It flows peer to peer
(DTLS-SRTP encrypted) or, when networks prevent that, through coturn, which
forwards encrypted packets it cannot read.

## Requirements

* A Linux server with a public IP address (examples use Ubuntu 22.04+).
* A DNS name pointing to it, e.g. `audionet.example.com`.
* A TLS certificate for that name (for example from Let's Encrypt).
* Rust 1.85+ to build (`rustup` is fine, installed as a normal user), gcc.
* Firewall openings: TCP 80, 443, 3478, 5349; UDP 3478, 5349 and the relay
  range (default 49160–49359). If you use a cloud provider firewall, open
  them there too.

The server needs little: tens of MB of RAM and almost no CPU, because it
never processes audio. coturn uses bandwidth only for relayed sessions
(about 150 kbit/s per stereo stream direction).

## 1. Build

```sh
git clone <repository URL> audionet
cd audionet
cargo build --release -p audionet-server
```

The binary is `target/release/audionet-server`. The web client is the
`web/` directory (static files, no build step).

## 2. Install

```sh
sudo useradd --system --home-dir /var/lib/audionet --no-create-home --shell /usr/sbin/nologin audionet
sudo install -d -m 0755 /opt/audionet/bin
sudo install -m 0755 target/release/audionet-server /opt/audionet/bin/
sudo cp -r web /opt/audionet/web
sudo install -d -m 0750 -o root -g audionet /etc/audionet
sudo install -m 0640 -o root -g audionet deploy/config.example.toml /etc/audionet/config.toml
sudo install -m 0640 -o root -g audionet deploy/audionet.env.example /etc/audionet/audionet.env
sudo install -m 0644 deploy/systemd/audionet.service /etc/systemd/system/
```

Edit `/etc/audionet/config.toml` (replace every `example.com` value) and
set a real secret in `/etc/audionet/audionet.env`:

```sh
openssl rand -hex 32    # use the output as AUDIONET_TURN_SECRET
```

Start it:

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now audionet
curl http://127.0.0.1:8740/api/v1/info
```

## 3. Configuration reference

`/etc/audionet/config.toml`:

| Key | Default | Meaning |
| --- | --- | --- |
| `public_url` | required | URL users and devices use, e.g. `https://audionet.example.com`. Must be HTTPS in production (cookies are marked `Secure` only for HTTPS). |
| `bind` | `127.0.0.1:8740` | Listening address. Keep it on localhost behind the proxy. |
| `database` | `audionet.db` | SQLite file. With the systemd unit use `/var/lib/audionet/audionet.db`. |
| `web_root` | none | Directory of the web client; omit to serve only the API. |
| `allowed_origins` | origin of `public_url` | Origins allowed to use browser sessions and the WebSocket. |
| `allow_registration` | `false` | Let anyone create an account in the web client ("Create an account"). The apps and the command line only sign in to an existing account. |
| `sign_ups_per_address_per_hour` | `3` | With open sign-up: new accounts one client address may create per hour. |
| `sign_ups_per_hour` | `30` | With open sign-up: new accounts the whole server accepts per hour. |
| `client_address_header` | none | Header the reverse proxy sets to the client's address (nginx: `X-Real-IP` from `$remote_addr`, as in the example site). Without it, every request seems to come from the proxy, so the per-address limit acts server-wide. Only name a header the proxy always overwrites. |
| `session_days` | `30` | Browser sign-in lifetime. |
| `[ice] stun_urls` | `[]` | STUN servers given to clients. |
| `[ice] turn_urls` | `[]` | TURN servers given to clients (require a secret). |
| `[ice] turn_secret` | none | Shared secret with coturn. Prefer the environment variable. |
| `[ice] turn_credential_ttl_s` | `3600` | Lifetime of generated TURN credentials. |

Environment variables (override the file; put secrets here):
`AUDIONET_CONFIG` (config path), `AUDIONET_PUBLIC_URL`, `AUDIONET_BIND`,
`AUDIONET_DATABASE`, `AUDIONET_WEB_ROOT`, `AUDIONET_ALLOWED_ORIGINS`
(comma-separated), `AUDIONET_TURN_SECRET`, `RUST_LOG` (log filter, e.g.
`audionet_server=debug`).

Check a configuration without starting: `audionet-server --config
/etc/audionet/config.toml check-config`.

## 4. Accounts

Registration is closed by default. Create accounts on the server:

```sh
sudo -u audionet sh -c 'echo "a long password here" | /opt/audionet/bin/audionet-server --config /etc/audionet/config.toml user add alice'
sudo -u audionet /opt/audionet/bin/audionet-server --config /etc/audionet/config.toml user list
sudo -u audionet /opt/audionet/bin/audionet-server --config /etc/audionet/config.toml user passwd alice   # reads the new password from standard input
sudo -u audionet /opt/audionet/bin/audionet-server --config /etc/audionet/config.toml user delete alice
```

Passwords must be at least 10 characters. Changing a password signs the
user out of all browsers.

## 5. Reverse proxy (nginx)

Use `deploy/nginx/audionet.example.conf`: replace the host name and
certificate paths, put it in `/etc/nginx/conf.d/` (or your
sites-available/sites-enabled layout), then:

```sh
sudo nginx -t && sudo systemctl reload nginx
```

Requirements for any proxy:

* **HTTPS** with a valid certificate.
* **WebSocket upgrade** on `/api/v1/ws`: pass `Upgrade` and `Connection`
  headers (nginx: the `map $http_upgrade $connection_upgrade` block, once
  per nginx instance). The server sends a WebSocket ping every 20 seconds,
  so any read timeout above ~30 seconds keeps idle connections open.
* Pass `Host` and `X-Forwarded-For` (standard proxy headers; sign-in
  throttling is per account name).
* Do not strip the server's security headers
  (`Content-Security-Policy`, `X-Frame-Options`, ...). If your nginx has
  global `proxy_hide_header` rules, add `proxy_pass_header` for those
  headers in the AudioNet server block (see
  `deploy/official/audionet.mad-gamer.com.conf` for an example).

The web client can live under any host name. It uses only relative,
same-origin URLs.

## 6. TURN and STUN (coturn)

Without TURN, devices behind strict firewalls or symmetric NATs cannot
connect. Install coturn (`sudo apt install coturn`) and start from
`deploy/coturn/turnserver.example.conf`:

* Set `listening-ip`, `relay-ip` and `external-ip` to your public address.
  Behind NAT use `external-ip=PUBLIC/PRIVATE`.
* Set `realm` to your host name and `static-auth-secret` to the **same**
  value as `AUDIONET_TURN_SECRET`.
* Keep the `denied-peer-ip` lines: they stop anyone from using your TURN
  server to reach private networks.
* For TURN over TLS (port 5349, helps on networks that block UDP), coturn
  needs a readable copy of your certificate.
  `deploy/coturn/certbot-deploy-hook.sh` copies it after every certbot
  renewal without changing `/etc/letsencrypt` permissions: install it in
  `/etc/letsencrypt/renewal-hooks/deploy/`, set the certificate name in it,
  and run it once.

Then list coturn in `config.toml`:

```toml
[ice]
stun_urls = ["stun:audionet.example.com:3478"]
turn_urls = [
  "turn:audionet.example.com:3478?transport=udp",
  "turn:audionet.example.com:3478?transport=tcp",
  "turns:audionet.example.com:5349?transport=tcp",
]
```

and restart both services. Credentials handed to clients expire after
`turn_credential_ttl_s` seconds.

## 7. Verify

1. `https://audionet.example.com/api/v1/info` returns JSON with `"name":"AudioNet"`.
2. Sign in to `https://audionet.example.com/`; the page says "Connected to the server."
3. Sign a Windows PC in: in the AudioNet desktop app enter the server
   address, your account name and password and press **Sign in**, then
   **Start**. Or from a terminal:
   `audionet node sign-in --server https://audionet.example.com --user YOUR-NAME`
   (it asks for the password), then `audionet node run`. The PC appears as
   online.
4. Choose a sound source and press **Listen**.
5. Test the relay: open `https://audionet.example.com/?ice=relay` and
   listen again. The diagnostics should say "through the TURN relay".

## Operations

* **Logs:** `journalctl -u audionet` and `journalctl -u coturn`. The
  server logs connections, device sign-ins and errors, never audio or
  secrets.
* **Health:** `GET /api/v1/health` returns status and the number of
  signaling connections.
* **Backup:** the database is the only state. Back up
  `/var/lib/audionet/audionet.db` with `sqlite3 audionet.db ".backup
  backup.db"` (safe while running), plus `/etc/audionet/`.
* **Upgrade:** build the new version, stop the service, replace
  `/opt/audionet/bin/audionet-server` and `/opt/audionet/web/`, start the
  service. The database schema upgrades automatically; back it up first.
  Clients and server must speak the same protocol major version (and, while
  the version is 0.x, the same minor version); mismatches are reported in
  plain language.

## Troubleshooting

| Symptom | Likely cause |
| --- | --- |
| Web client says "Not connected to the server" repeatedly | WebSocket upgrade not proxied, or `allowed_origins` does not include the page's origin |
| Sign-in returns "This page is not allowed to use this server" | The browser's origin is not in `allowed_origins` (for example `www.` vs bare host) |
| Stream shows "Could not connect to the other side" | UDP blocked and no working TURN; check coturn, firewall ports and the shared secret; try `?ice=relay` |
| `audionet node run` says the device is no longer recognized | The device was removed from the account; sign it in again |
| Server refuses to start: "turn_urls are set but no TURN secret" | Set `AUDIONET_TURN_SECRET` in the environment file |

## Security recommendations

* HTTPS only; keep `allow_registration = false` unless you run a public service.
  With open sign-up, set `client_address_header` so the per-address limit
  works, and keep the limits low: every account can use your TURN relay's
  bandwidth. People cannot pick names such as "admin" or "support" for
  themselves, or a password containing their username. There is no email
  and so no self-service password reset: `audionet-server user passwd
  NAME` sets a new one.
* Keep the environment file `0640 root:audionet` and the database directory `0700`.
* Keep coturn's private-network denials.
* Update the server, coturn and the OS regularly.

See [SECURITY.md](../SECURITY.md) for the full security model.
