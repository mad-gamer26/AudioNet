# Official hosted instance

These files configure the project's official hosted AudioNet service at
`https://audionet.mad-gamer.com`. They are specific to that server (its
nginx layout, wildcard certificate and addresses) and are **not needed**
to run AudioNet elsewhere. Self-hosters should start from the generic
templates in `deploy/` and `docs/self-hosting.md`.

No secrets are stored here: the TURN secret lives only in
`/etc/audionet/audionet.env` and `/etc/turnserver.conf` on the server.

`install.ps1` is the Windows installer served at
`https://audionet.mad-gamer.com/install.ps1` (`irm ... | iex`). To publish a
change, copy it to `/var/www/audionet/install.ps1` on the server (mode
644); the nginx `location = /install.ps1` block serves it as plain text,
never cached.

`install.sh` is the macOS and Linux installer served at
`https://audionet.mad-gamer.com/install.sh` (`curl -fsSL ... | sh`), published
the same way to `/var/www/audionet/install.sh` (`location = /install.sh`).
It embeds the release public key (base64 DER of
`update-public-key.txt`): change it there too if the key ever changes.
Test it before publishing (see docs/testing.md, "Installers").
