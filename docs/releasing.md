# Releasing the Windows and Mac apps (and automatic updates)

The Windows desktop app can update itself. It does so only when the build
was given an update source and a public key; plain source builds never
update themselves.

## How updates work

1. The app checks the update manifest 30 seconds after it starts and then
   every 6 hours (hourly after a failure), or when you press **Check for
   updates now** in Settings. **Keep AudioNet up to date automatically**
   (also in Settings) turns the automatic checks off.
2. The manifest (`latest.json`) names the version, the package file, its
   SHA-256 and its size. It must carry a valid Ed25519 signature
   (`latest.json.sig`) from the release key; the public key is built into
   the app. Anything unsigned, altered, older than the running copy, not
   for `audionet-windows-x64`, or not over HTTPS is ignored.
3. The package is downloaded to the temporary folder, checked against the
   signed size and SHA-256, unpacked into a staging folder next to the
   program (plain file names only), and the downloaded zip is deleted.
4. When no one is listening or talking through the computer, each program
   file is renamed aside and the new one moved into place. The new copy is
   started with the window and sharing as they were; if it does not report
   within 20 seconds that it runs, every file is put back and the old copy
   keeps running. The new copy deletes the renamed old files and says
   "AudioNet was updated from version X to Y" (a Windows notification when
   it is in the system tray).
5. If an installed release still reports an older version (a mislabeled
   build), it is not offered again, so a bad release cannot loop.

The update source and key are build-time settings (`AUDIONET_UPDATE_URL`,
`AUDIONET_UPDATE_PUBLIC_KEY`); nothing in the code names a server.

## The release key

The signing key is an Ed25519 key kept outside the repository, by default
at `%USERPROFILE%\.audionet-release\update-signing-key.pem`. Its public
half is committed as `deploy/official/update-public-key.txt` (public keys
are not secret).

* **Back it up** somewhere safe and offline. If it is lost, installed
  copies cannot be updated past it: people would have to download a build
  with a new key once, by hand.
* **Never commit it**, and never copy it to the web server. A compromised
  web server then cannot push anything the app will run.

Create a key (once):

```sh
python scripts/release_sign.py keygen %USERPROFILE%\.audionet-release\update-signing-key.pem deploy/official/update-public-key.txt
```

## Publishing a release (official instance)

1. Raise `version` in the workspace `Cargo.toml` (updates only go to a
   strictly higher version).
2. Build, package and sign:

   ```sh
   powershell -ExecutionPolicy Bypass -File scripts/package-windows.ps1 ^
     -DefaultServer https://audionet.mad-gamer.com ^
     -UpdateUrl https://audionet.mad-gamer.com/downloads/latest.json
   ```

   The script refuses to build if the signing key does not match the
   committed public key. It writes the zip, its `.sha256`, `latest.json`
   and `latest.json.sig` to `dist/`.
3. Update `deploy/official/downloads/index.html` (file name and checksum).
4. Copy the zip, `.sha256`, `latest.json`, `latest.json.sig` and
   `index.html` to `/opt/audionet/downloads/` on the server. Publish
   `latest.json` and its signature last, so no one is offered a package
   that is not there yet.

## Self-hosters and forks

To ship your own updating builds, create your own key, commit or keep
your public key, and pass `-UpdateUrl` pointing at your server with
`-PublicKeyFile` and `-SigningKey` for your key pair. Your builds then
accept only your releases.

## The Mac app

The Mac app uses the same key and the same checks. It reads
`latest-macos.json` (product `audionet-macos-universal`) next to the
configured `latest.json`. After downloading, it also requires the
unpacked app to have AudioNet's bundle identifier, the signed version and
a valid code signature. It installs when no stream is running: a helper
swaps the app bundles after AudioNet quits and starts the new copy,
online again if it was. If that copy does not report within 20 seconds,
the old one is put back and the version is not offered again. The app
must be somewhere it may replace itself, such as Applications, after
being moved there by the person who uses it.

1. Build on a Mac (Apple silicon and Intel in one app):

   ```sh
   apps/macos/package.sh --default-server https://audionet.mad-gamer.com \
     --update-url https://audionet.mad-gamer.com/downloads/latest.json \
     --public-key-file deploy/official/update-public-key.txt
   ```

   With `DEVELOPER_ID` and `TEAM_ID` set it signs with that Developer ID
   certificate. With `NOTARY_PROFILE` also set, it notarizes and staples
   the app. Without them the app is ad-hoc signed. People then confirm it
   once in System Settings (Privacy and Security, Open Anyway), and macOS
   asks for permissions again after each update, because every ad-hoc
   build looks like a new app to it.
   **Signing.** package.sh signs with the Developer ID Application
   certificate in the Mac user's login keychain when there is one (else
   ad-hoc). Signing needs that user's session, which SSH sessions do not
   have, so over SSH run it through `apps/macos/in-session.sh`: a one-off
   launchd job in the logged-in user's session, with no window and no
   password (the user must be logged in):

   ```sh
   apps/macos/in-session.sh sh apps/macos/package.sh --default-server … --update-url … --public-key-file …
   ```

   **Notarization** uses an App Store Connect API key (no keychain):
   `~/.audionet-notary` on the Mac, readable only by its user, sets
   `NOTARY_KEY` (path to the `.p8` file), `NOTARY_KEY_ID` and
   `NOTARY_ISSUER`. The app is then notarized and the ticket stapled.

   A Developer ID signed app accepts updates only from the same Apple
   team, in addition to the release key's signature, and macOS remembers
   its permissions (microphone, system audio) across updates.

   The Mac app keeps its sign-in in a file only its user can read
   (`~/Library/Application Support/AudioNet/account.json`, mode 0600), not
   in the keychain: macOS sees every ad-hoc build as a different app and
   would ask for the login password after each update. Versions up to
   0.5.1 used the keychain; the first newer version moves the sign-in into
   the file, which may make macOS ask one last time.
2. Copy `dist/AudioNet-macOS-<version>.zip` and its `.sha256` to the
   machine with the release key and sign the manifest there:

   ```sh
   python scripts/release_sign.py manifest KEY dist/AudioNet-macOS-<version>.zip <version> dist ^
     --product audionet-macos-universal --name latest-macos.json
   ```

3. Update `deploy/official/downloads/index.html`. Publish the zip,
   `.sha256` and page first, then `latest-macos.json` and its `.sig`.

## Testing

`scripts/test/update_e2e.py` builds a test copy with a throwaway key and a
local update source, installs it in a temporary folder, and checks a real
update end to end, a repeated check (no loop), a tampered manifest
(rejected) and an update that cannot start (rolled back).

`scripts/test/mac_update_e2e.py` does the same for the Mac app, on the Mac,
with a throwaway key and a local update server: a tampered manifest is
ignored; 1.0.0 updates to 1.0.1, restarts and reports; a 1.0.2 that
never starts is rolled back and not offered again.
