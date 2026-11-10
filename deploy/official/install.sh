#!/bin/sh
# AudioNet installer for macOS and Linux (official instance).
#
#   curl -fsSL https://audionet.mad-gamer.com/install.sh | sh
#
# Installs the latest AudioNet from the project's GitHub releases, or
# updates or uninstalls an existing copy, like install.ps1 does on Windows:
#
# * macOS 14 or later: the AudioNet app, in /Applications (or a folder you
#   choose, such as ~/Applications).
# * Linux (64-bit Intel or AMD, glibc 2.34 or later): the `audionet`
#   program, which joins your account as a device, in ~/.local/bin (or a
#   folder you choose); optionally signed in, started with your session
#   (a systemd user service) and added to your PATH.
#
# Asks a few questions; pressing Enter takes the answer in brackets. The
# questions are read from the terminal, so piping the script into sh works.
# No administrator rights: everything goes in your own folders (unless you
# choose /Applications, which administrators may write to). Served by the
# official server's nginx from /var/www/audionet/install.sh (see
# audionet.mad-gamer.com.conf).
#
# Checks: the download must match the size and SHA-256 in the release's
# manifest (latest-macos.json, latest-linux.json), fetched over HTTPS from
# GitHub. On Linux the manifest's Ed25519 signature is also checked against
# AudioNet's release key when OpenSSL 3 is available. On macOS the app must
# carry a valid Apple code signature from AudioNet's developer team.
#
# Output is plain lines for screen readers: no colours, progress bars or
# cursor movement.
#
# For testing only: AUDIONET_RELEASE_URL replaces the release address,
# AUDIONET_APP_FOLDERS (macOS, colon-separated) the folders searched for and
# offered for the app, AUDIONET_NO_OPEN=1 (macOS) leaves the app closed, and
# AUDIONET_SERVER the server offered at sign-in. A release address of
# http://127.0.0.1:PORT (a test mirror) may be plain http.

set -u

RELEASE=${AUDIONET_RELEASE_URL:-https://github.com/mad-gamer26/AudioNet/releases/latest/download}
SERVER=${AUDIONET_SERVER:-https://audionet.mad-gamer.com}
# The key the releases are signed with (deploy/official/update-public-key.txt,
# 699e9933...3f57), as an Ed25519 public key in base64 DER.
RELEASE_KEY_PEM=MCowBQYDK2VwAyEAaZ6ZM0nXw2PtMUMYC868sAxmFYkc3Rc4Y1ehCtjkP1c=
MAC_BUNDLE_ID=org.audionet.AudioNet
MAC_TEAM_ID=5NJK4F832Z
MARK='# Added by the AudioNet installer'

WORK=''
cleanup() {
    if [ -n "$WORK" ]; then rm -rf "$WORK"; fi
}
trap cleanup EXIT
trap 'say ""; say "The AudioNet installer was interrupted. Nothing more was changed."; exit 130' INT TERM

say() { printf '%s\n' "$*"; }

stop() {
    say ''
    say "The AudioNet installer stopped: $*"
    exit 1
}

# ─── questions (read from the terminal, also when piped into sh) ──────────

ask_line() {
    printf '%s ' "$1"
    REPLY=''
    IFS= read -r REPLY </dev/tty || stop 'no answer could be read from the terminal'
    # Leading and trailing spaces do not count.
    REPLY=$(printf '%s' "$REPLY" | sed 's/^[[:space:]]*//; s/[[:space:]]*$//')
}

# ask_yes_no QUESTION DEFAULT(yes|no): success for yes.
ask_yes_no() {
    if [ "$2" = yes ]; then hint='Y/n'; else hint='y/N'; fi
    while :; do
        ask_line "$1 ($hint)"
        case $(printf '%s' "$REPLY" | tr 'A-Z' 'a-z') in
            '') [ "$2" = yes ]; return ;;
            y | yes) return 0 ;;
            n | no) return 1 ;;
            *) say 'Please answer y for yes or n for no.' ;;
        esac
    done
}

# ask_choice QUESTION CHOICE...: sets CHOICE (1 when Enter is pressed).
ask_choice() {
    say "$1"
    shift
    n=0
    for c in "$@"; do
        n=$((n + 1))
        say "$n. $c"
    done
    while :; do
        ask_line "Choice, 1 to $n (1)"
        case $REPLY in
            '') CHOICE=1; return ;;
            *[!0-9]*) ;;
            *) if [ "$REPLY" -ge 1 ] && [ "$REPLY" -le "$n" ]; then CHOICE=$REPLY; return; fi ;;
        esac
        say "Please type a number from 1 to $n."
    done
}

# ask_text QUESTION DEFAULT: sets ANSWER (the default when Enter is pressed).
ask_text() {
    ask_line "$1 ($2)"
    if [ -z "$REPLY" ]; then ANSWER=$2; else ANSWER=$REPLY; fi
}

# A folder answer: ~ and $HOME expanded.
# shellcheck disable=SC2088 # a typed ~/, not one the shell expands
expand_folder() {
    case $1 in
        '~') printf '%s' "$HOME" ;;
        '~/'*) printf '%s/%s' "$HOME" "${1#\~/}" ;;
        '$HOME') printf '%s' "$HOME" ;;
        '$HOME/'*) printf '%s/%s' "$HOME" "${1#\$HOME/}" ;;
        *) printf '%s' "$1" ;;
    esac
}

# ─── downloads and checks ──────────────────────────────────────────────────

fetch() {
    # HTTPS only; plain http only for a test mirror on this computer.
    proto='=https'
    case $1 in
        http://127.0.0.1:*) proto='=http' ;;
    esac
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL --proto "$proto" --tlsv1.2 -o "$2" "$1"
    elif command -v wget >/dev/null 2>&1; then
        if [ "$proto" = '=https' ]; then wget -q --https-only -O "$2" "$1"; else wget -q -O "$2" "$1"; fi
    else
        stop 'neither curl nor wget is installed, so nothing can be downloaded'
    fi
}

sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    else
        shasum -a 256 "$1" | cut -d' ' -f1
    fi
}

size_of() {
    wc -c <"$1" | tr -d ' '
}

# json_field FILE NAME: a string or number field of the (flat) manifest.
json_field() {
    sed -n "s/^[[:space:]]*\"$2\":[[:space:]]*\"\{0,1\}\([^\",]*\)\"\{0,1\},\{0,1\}[[:space:]]*\$/\1/p" "$1" | head -n 1
}

# Hex text to base64 text (decoded by openssl: no xxd or perl needed, and
# no NUL bytes through the shell).
hex_to_base64() {
    printf '%s' "$1" | awk '
        BEGIN { b = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"; h = "0123456789abcdef" }
        {
            n = 0
            for (i = 1; i < length($0); i += 2) byte[n++] = (index(h, substr($0, i, 1)) - 1) * 16 + index(h, substr($0, i + 1, 1)) - 1
            out = ""
            for (i = 0; i < n; i += 3) {
                x = byte[i] * 65536 + (i + 1 < n ? byte[i + 1] : 0) * 256 + (i + 2 < n ? byte[i + 2] : 0)
                out = out substr(b, int(x / 262144) % 64 + 1, 1) substr(b, int(x / 4096) % 64 + 1, 1)
                out = out (i + 1 < n ? substr(b, int(x / 64) % 64 + 1, 1) : "=") (i + 2 < n ? substr(b, x % 64 + 1, 1) : "=")
            }
            print out
        }'
}

# Checks the manifest's Ed25519 signature when OpenSSL 3 can (Linux).
# Prints what happened; stops on a bad signature.
check_signature() {
    manifest=$1
    command -v openssl >/dev/null 2>&1 || { say 'Signature not checked (OpenSSL is not installed); size and SHA-256 are.'; return; }
    case $(openssl version 2>/dev/null) in
        'OpenSSL 3'*) ;;
        *) say 'Signature not checked (this OpenSSL cannot check Ed25519); size and SHA-256 are.'; return ;;
    esac
    fetch "$RELEASE/$(basename "$manifest").sig" "$manifest.sig" || stop 'the release manifest has no signature on GitHub'
    sig=$(tr -d ' \r\n' <"$manifest.sig")
    case $sig in
        *[!0-9a-f]*) stop 'the release manifest signature is malformed' ;;
    esac
    [ ${#sig} -eq 128 ] || stop 'the release manifest signature is malformed'
    hex_to_base64 "$sig" | openssl base64 -d -A >"$WORK/sig.bin" 2>/dev/null || stop 'the release manifest signature is malformed'
    printf -- '-----BEGIN PUBLIC KEY-----\n%s\n-----END PUBLIC KEY-----\n' "$RELEASE_KEY_PEM" >"$WORK/key.pem"
    if openssl pkeyutl -verify -pubin -inkey "$WORK/key.pem" -rawin -in "$manifest" \
        -sigfile "$WORK/sig.bin" >/dev/null 2>&1; then
        say 'Release signature checked: signed with the AudioNet release key.'
    else
        stop 'the release manifest is not signed with the AudioNet release key; nothing was installed'
    fi
}

# get_manifest NAME PRODUCT FILE_PATTERN: sets VERSION, PACKAGE, SHA, SIZE.
get_manifest() {
    MANIFEST="$WORK/$1"
    fetch "$RELEASE/$1" "$MANIFEST" || stop "could not download the release manifest ($1) from GitHub. Check your internet connection and try again."
    product=$(json_field "$MANIFEST" product)
    VERSION=$(json_field "$MANIFEST" version)
    PACKAGE=$(json_field "$MANIFEST" file)
    SHA=$(json_field "$MANIFEST" sha256 | tr 'A-F' 'a-f')
    SIZE=$(json_field "$MANIFEST" size)
    [ "$product" = "$2" ] || stop "the release manifest on GitHub is not one for $2"
    printf '%s' "$VERSION" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$' || stop 'the release manifest has no valid version'
    printf '%s' "$PACKAGE" | grep -Eq "$3" || stop 'the release manifest names an unexpected file'
    printf '%s' "$SHA" | grep -Eq '^[0-9a-f]{64}$' || stop 'the release manifest has no valid SHA-256'
    printf '%s' "$SIZE" | grep -Eq '^[0-9]+$' || stop 'the release manifest has no valid size'
}

download_package() {
    mb=$(awk "BEGIN { printf \"%.1f\", $SIZE / 1048576 }")
    say "Downloading AudioNet $VERSION ($mb MB) from GitHub."
    fetch "$RELEASE/$PACKAGE" "$WORK/$PACKAGE" || stop 'the download failed. Check your internet connection and try again.'
    if [ "$(size_of "$WORK/$PACKAGE")" != "$SIZE" ] || [ "$(sha256_of "$WORK/$PACKAGE")" != "$SHA" ]; then
        stop 'the download does not match the release manifest (size or SHA-256); nothing was installed'
    fi
    say 'Download checked: size and SHA-256 match the release.'
}

# version_ge A B: success when version A >= version B.
version_ge() {
    [ "$(printf '%s\n%s\n' "$1" "$2" | sort -t. -k1,1n -k2,2n -k3,3n | tail -n 1)" = "$1" ]
}

# ─── macOS ──────────────────────────────────────────────────────────────────

mac_app_folders() {
    # One per line, the last one ended too (read skips an unended line).
    printf '%s\n' "${AUDIONET_APP_FOLDERS:-/Applications:$HOME/Applications}" | tr ':' '\n'
}

mac_plist() {
    /usr/libexec/PlistBuddy -c "Print :$2" "$1/Contents/Info.plist" 2>/dev/null
}

# The installed app (the first folder holding AudioNet's own app), or empty.
mac_find_installed() {
    mac_app_folders | while IFS= read -r folder; do
        app="$folder/AudioNet.app"
        if [ -d "$app" ] && [ "$(mac_plist "$app" CFBundleIdentifier)" = "$MAC_BUNDLE_ID" ]; then
            printf '%s' "$app"
            return
        fi
    done
}

# Process ids of the app running from bundle $1.
mac_running() {
    ps -axo pid=,comm= | awk -v exe="$1/Contents/MacOS/AudioNet" '$2 == exe { print $1 }'
}

# Closes the app running from $1, after asking. Failure: it keeps running.
mac_close_running() {
    pids=$(mac_running "$1")
    [ -z "$pids" ] && return 0
    say "AudioNet is running. It must quit $2; any streams stop."
    ask_yes_no 'Quit AudioNet now?' yes || return 1
    kill $pids 2>/dev/null
    i=0
    while [ -n "$(mac_running "$1")" ] && [ $i -lt 40 ]; do
        sleep 0.25
        i=$((i + 1))
    done
    pids=$(mac_running "$1")
    if [ -n "$pids" ]; then kill -9 $pids 2>/dev/null; sleep 1; fi
    say 'AudioNet quit.'
}

mac_open() {
    if [ "${AUDIONET_NO_OPEN:-}" = 1 ]; then say 'AudioNet is ready (not opened: testing).'; return; fi
    open "$1"
    say 'AudioNet is opening.'
}

mac_install() {
    target=$1
    download_package
    mkdir "$WORK/unpacked"
    ditto -x -k "$WORK/$PACKAGE" "$WORK/unpacked" || stop 'the download could not be unpacked; nothing was installed'
    new="$WORK/unpacked/AudioNet.app"
    [ -d "$new" ] || stop 'the download has no AudioNet.app; nothing was installed'
    [ "$(mac_plist "$new" CFBundleIdentifier)" = "$MAC_BUNDLE_ID" ] || stop 'the downloaded app is not AudioNet; nothing was installed'
    codesign --verify --deep --strict "$new" 2>/dev/null || stop 'the downloaded app is not validly code-signed; nothing was installed'
    team=$(codesign -dv "$new" 2>&1 | sed -n 's/^TeamIdentifier=//p')
    [ "$team" = "$MAC_TEAM_ID" ] || stop 'the downloaded app is not signed by AudioNet'"'"'s developer; nothing was installed'
    say 'App checked: signed by AudioNet'"'"'s Apple developer team.'
    folder=$(dirname "$target")
    mkdir -p "$folder" 2>/dev/null
    [ -w "$folder" ] || stop "you may not write to $folder; choose another folder, such as $HOME/Applications"
    # The old copy is kept aside until the new one is in place.
    if [ -e "$target" ]; then
        mv "$target" "$WORK/AudioNet-previous.app" || stop "the old copy in $folder could not be moved aside; nothing was changed"
    fi
    if ! ditto "$new" "$target"; then
        rm -rf "$target"
        [ -e "$WORK/AudioNet-previous.app" ] && mv "$WORK/AudioNet-previous.app" "$target"
        stop "AudioNet could not be copied to $folder; the previous copy was kept"
    fi
}

mac_uninstall() {
    app=$1
    say "This removes AudioNet from $(dirname "$app")."
    if ! ask_yes_no 'Uninstall AudioNet?' no; then say 'Nothing was changed.'; return; fi
    if ! mac_close_running "$app" 'to be uninstalled'; then say 'Nothing was changed.'; return; fi
    remove_data=no
    if ask_yes_no 'Also remove your AudioNet sign-in and settings from this Mac?' no; then remove_data=yes; fi
    rm -rf "$app" || stop "AudioNet could not be removed from $(dirname "$app")"
    if [ $remove_data = yes ]; then
        rm -rf "$HOME/Library/Application Support/AudioNet" "$HOME/Library/Caches/$MAC_BUNDLE_ID"
        defaults delete "$MAC_BUNDLE_ID" >/dev/null 2>&1
        say 'Your AudioNet sign-in and settings were removed. The Mac is still listed in your account: remove it in the web client'"'"'s Manage devices.'
    fi
    say 'AudioNet was uninstalled. If it was set to start at login, macOS drops that entry by itself.'
    say 'To install it again, run the same command.'
}

mac_main() {
    major=$(sw_vers -productVersion | cut -d. -f1)
    if [ "$major" -lt 14 ]; then
        say 'AudioNet needs macOS 14 Sonoma or later. Nothing was installed.'
        exit 1
    fi
    say 'Checking the latest release on GitHub.'
    get_manifest latest-macos.json audionet-macos-universal '^AudioNet-macOS-[0-9]+\.[0-9]+\.[0-9]+\.zip$'
    say "Latest version: $VERSION"

    app=$(mac_find_installed)
    fresh=yes
    if [ -n "$app" ]; then
        installed=$(mac_plist "$app" CFBundleShortVersionString)
        say "Installed: AudioNet ${installed:-of an unknown version} in $(dirname "$app")"
        say ''
        if [ -n "$installed" ] && version_ge "$installed" "$VERSION"; then
            ask_choice 'It is up to date. What would you like to do?' 'Open AudioNet' "Reinstall AudioNet $VERSION" 'Uninstall AudioNet' 'Quit'
        else
            ask_choice 'What would you like to do?' "Update to AudioNet $VERSION" 'Uninstall AudioNet' 'Quit'
            CHOICE=$((CHOICE + 1))
        fi
        case $CHOICE in
            1)
                mac_open "$app"
                return
                ;;
            3) mac_uninstall "$app"; return ;;
            4) say 'Nothing was changed.'; return ;;
        esac
        fresh=no
    else
        say 'AudioNet is not installed on this Mac yet.'
        default=$(mac_app_folders | head -n 1)
        while :; do
            ask_text 'Install folder' "$default"
            folder=$(expand_folder "$ANSWER")
            case $folder in
                /*) ;;
                *) say 'Please give a full folder path, starting with / or ~.'; continue ;;
            esac
            if mkdir -p "$folder" 2>/dev/null && [ -w "$folder" ]; then break; fi
            say "You may not write to $folder. Try $HOME/Applications instead."
            default="$HOME/Applications"
        done
        app="$folder/AudioNet.app"
    fi

    if ! mac_close_running "$app" 'to be updated'; then say 'Nothing was changed.'; return; fi
    mac_install "$app"
    say ''
    say "AudioNet $VERSION is installed in $(dirname "$app")"
    mac_open "$app"
    if [ $fresh = yes ]; then
        say 'Sign in with your server address, account name and password. No account yet? Create one in your AudioNet server'"'"'s web client.'
        say 'Settings (Command-comma) has starting AudioNet when you log in.'
    fi
    say 'AudioNet keeps itself up to date. To update, reinstall or uninstall it later, run this command again.'
}

# ─── Linux ──────────────────────────────────────────────────────────────────

LINUX_DOCS="$HOME/.local/share/audionet"
UNIT_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
UNIT="$UNIT_DIR/audionet.service"
CONFIG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/audionet"

# The installed program's version ("audionet 1.3.0"), or empty.
linux_version() {
    "$1" --version 2>/dev/null | sed -n 's/^audionet \([0-9][0-9.]*\).*/\1/p' | head -n 1
}

# The installed program: the one the service runs, the one on the PATH, or
# the default place; empty when none is AudioNet's.
linux_find_installed() {
    for c in "$(unit_program)" \
        "$(command -v audionet 2>/dev/null)" "$HOME/.local/bin/audionet"; do
        if [ -n "$c" ] && [ -x "$c" ] && [ -n "$(linux_version "$c")" ]; then
            printf '%s' "$c"
            return
        fi
    done
}

# The program the user service runs (systemd's %h is the home folder), or
# empty when there is no service.
unit_program() {
    sed -n 's/^ExecStart=\([^ ]*\) .*/\1/p' "$UNIT" 2>/dev/null | head -n 1 | sed "s|^%h|$HOME|"
}

user_systemd() {
    command -v systemctl >/dev/null 2>&1 && systemctl --user show-environment >/dev/null 2>&1
}

service_active() {
    user_systemd && systemctl --user is-active --quiet audionet.service
}

service_enabled() {
    [ -f "$UNIT" ] && user_systemd && systemctl --user is-enabled --quiet audionet.service
}

# Process ids of `audionet node run` from program $1 (from the service,
# in the background or in a terminal). Through /proc: small systems may
# have no ps.
linux_running() {
    real=$(readlink -f "$1" 2>/dev/null || printf '%s' "$1")
    for d in /proc/[0-9]*; do
        [ "$(readlink "$d/exe" 2>/dev/null)" = "$real" ] || continue
        args=$(tr '\0' '\n' <"$d/cmdline" 2>/dev/null | sed -n '2,3p' | tr '\n' ' ')
        [ "$args" = 'node run ' ] && printf '%s\n' "${d#/proc/}"
    done
}

# Stops the running device, after asking. Sets WAS_RUNNING (service,
# background or empty). Failure: it keeps running.
linux_close_running() {
    WAS_RUNNING=''
    if service_active; then WAS_RUNNING=service; fi
    pids=$(linux_running "$1")
    if [ -z "$WAS_RUNNING" ] && [ -n "$pids" ]; then WAS_RUNNING=background; fi
    [ -z "$WAS_RUNNING" ] && return 0
    say "AudioNet is running. It must stop $2; any streams stop."
    ask_yes_no 'Stop AudioNet now?' yes || return 1
    if [ "$WAS_RUNNING" = service ]; then systemctl --user stop audionet.service; fi
    pids=$(linux_running "$1")
    if [ -n "$pids" ]; then
        kill $pids 2>/dev/null
        i=0
        while [ -n "$(linux_running "$1")" ] && [ $i -lt 40 ]; do
            sleep 0.25
            i=$((i + 1))
        done
        pids=$(linux_running "$1")
        if [ -n "$pids" ]; then kill -9 $pids 2>/dev/null; fi
    fi
    say 'AudioNet stopped.'
}

on_path() {
    case ":$PATH:" in
        *":$1:"*) return 0 ;;
    esac
    return 1
}

# The shell start-up file the PATH line goes in.
profile_file() {
    printf '%s' "$HOME/.profile"
}

add_to_path() {
    f=$(profile_file)
    grep -qF "$MARK" "$f" 2>/dev/null && return
    printf '\n%s\nexport PATH="%s:$PATH"\n' "$MARK" "$1" >>"$f"
}

remove_from_path() {
    f=$(profile_file)
    [ -f "$f" ] && grep -qF "$MARK" "$f" || return 0
    # The marker line and the line after it (and the blank line before).
    awk -v mark="$MARK" '
        $0 == mark { skip = 1; if (blank) { blank = 0 }; next }
        skip { skip = 0; next }
        { if (blank) print ""; blank = ($0 == ""); if (!blank) print }
        END { if (blank) print "" }' "$f" >"$f.audionet-tmp" &&
        cat "$f.audionet-tmp" >"$f" && rm -f "$f.audionet-tmp"
}

write_unit() {
    mkdir -p "$UNIT_DIR"
    cat >"$UNIT" <<EOF
# Written by the AudioNet installer: this computer as an AudioNet device,
# started with your session. To stop it for good, run the installer again
# and uninstall, or: systemctl --user disable --now audionet
[Unit]
Description=AudioNet device
After=pipewire-pulse.service pulseaudio.service

[Service]
ExecStart=$1 node run
Restart=on-failure
RestartSec=5

[Install]
WantedBy=default.target
EOF
    systemctl --user daemon-reload
}

signed_in() {
    "$1" node status 2>/dev/null | grep -q '^Server: '
}

linux_install() {
    bin=$1
    download_package
    mkdir "$WORK/unpacked"
    tar -xzf "$WORK/$PACKAGE" -C "$WORK/unpacked" || stop 'the download could not be unpacked; nothing was installed'
    dir="$WORK/unpacked/${PACKAGE%.tar.gz}"
    [ -f "$dir/audionet" ] || stop 'the download has no audionet program; nothing was installed'
    chmod 755 "$dir/audionet"
    if [ -z "$(linux_version "$dir/audionet")" ]; then
        if command -v ldd >/dev/null 2>&1 && ldd "$dir/audionet" 2>/dev/null | grep -q 'libasound.so.2 => not found'; then
            stop 'AudioNet needs the ALSA sound library: install libasound2 (on Fedora: alsa-lib), then run this again. Nothing was installed.'
        fi
        stop 'the downloaded program does not run on this computer; nothing was installed'
    fi
    mkdir -p "$(dirname "$bin")" "$LINUX_DOCS" || stop "the folder $(dirname "$bin") could not be made"
    # Replaced in one step (a running program keeps its old file).
    cp "$dir/audionet" "$bin.audionet-new" && mv -f "$bin.audionet-new" "$bin" ||
        stop "AudioNet could not be copied to $(dirname "$bin")"
    cp "$dir/README.md" "$dir/LICENSE" "$LINUX_DOCS/"
}

linux_uninstall() {
    bin=$1
    say "This removes AudioNet from $(dirname "$bin"), with its start with your session and PATH entry."
    if ! ask_yes_no 'Uninstall AudioNet?' no; then say 'Nothing was changed.'; return; fi
    if ! linux_close_running "$bin" 'to be uninstalled'; then say 'Nothing was changed.'; return; fi
    remove_data=no
    if ask_yes_no 'Also remove this computer'"'"'s AudioNet sign-in from it?' no; then remove_data=yes; fi
    if [ -f "$UNIT" ] && [ "$(unit_program)" = "$bin" ]; then
        if user_systemd; then systemctl --user disable audionet.service >/dev/null 2>&1; fi
        rm -f "$UNIT"
        if user_systemd; then systemctl --user daemon-reload; fi
    fi
    rm -f "$bin"
    rm -rf "$LINUX_DOCS"
    remove_from_path
    if [ $remove_data = yes ]; then
        rm -rf "$CONFIG_DIR"
        say 'The sign-in was removed. This computer is still listed in your account: remove it in the web client'"'"'s Manage devices.'
    fi
    say 'AudioNet was uninstalled. To install it again, run the same command.'
}

linux_sign_in() {
    bin=$1
    say 'Sign in with your AudioNet account: this computer becomes a device in it. The password is used once and not stored.'
    ask_text 'Server address' "$SERVER"
    server=$ANSWER
    while :; do
        ask_line 'Account name:'
        [ -n "$REPLY" ] && break
        say 'Please type your account name.'
    done
    user=$REPLY
    ask_text 'Name for this computer' "$(hostname 2>/dev/null || cat /etc/hostname 2>/dev/null || printf 'Linux computer')"
    name=$ANSWER
    # The program asks for the password itself, without showing it.
    if "$bin" node sign-in --server "$server" --user "$user" --name "$name" </dev/tty; then
        return 0
    fi
    say 'Signing in did not work. You can sign in later with:'
    say "  $bin node sign-in --server $server --user $user"
    return 1
}

linux_main() {
    case $(uname -m) in
        x86_64 | amd64) ;;
        *)
            say "AudioNet for Linux is built for 64-bit Intel and AMD computers; this one is $(uname -m). Nothing was installed."
            exit 1
            ;;
    esac
    libc=$(getconf GNU_LIBC_VERSION 2>/dev/null)
    case $libc in
        'glibc '*)
            if ! version_ge "${libc#glibc }.0" '2.34.0'; then
                say "AudioNet for Linux needs glibc 2.34 or later (Ubuntu 22.04, Debian 12, Fedora 35 or newer); this computer has $libc. Nothing was installed."
                exit 1
            fi
            ;;
        *)
            say 'AudioNet for Linux needs a system with glibc 2.34 or later (Ubuntu 22.04, Debian 12, Fedora 35 or newer). Nothing was installed.'
            exit 1
            ;;
    esac
    if [ "$(id -u)" -eq 0 ]; then
        say 'Run this as the user whose sound AudioNet should use, not as root. Nothing was installed.'
        exit 1
    fi

    say 'Checking the latest release on GitHub.'
    get_manifest latest-linux.json audionet-linux-x64 '^audionet-linux-x64-[0-9]+\.[0-9]+\.[0-9]+\.tar\.gz$'
    check_signature "$MANIFEST"
    say "Latest version: $VERSION"

    bin=$(linux_find_installed)
    fresh=yes
    if [ -n "$bin" ]; then
        installed=$(linux_version "$bin")
        say "Installed: AudioNet $installed in $(dirname "$bin")"
        say ''
        if version_ge "$installed" "$VERSION"; then
            ask_choice 'It is up to date. What would you like to do?' 'Leave it as it is' "Reinstall AudioNet $VERSION" 'Uninstall AudioNet'
        else
            ask_choice 'What would you like to do?' "Update to AudioNet $VERSION" 'Uninstall AudioNet' 'Quit'
            CHOICE=$((CHOICE + 1))
            [ $CHOICE -eq 4 ] && CHOICE=1
        fi
        case $CHOICE in
            1) say 'Nothing was changed.'; return ;;
            3) linux_uninstall "$bin"; return ;;
        esac
        fresh=no
    else
        say 'AudioNet is not installed on this computer yet.'
        while :; do
            ask_text 'Install folder for the audionet program' "$HOME/.local/bin"
            folder=$(expand_folder "$ANSWER")
            case $folder in
                /*) ;;
                *) say 'Please give a full folder path, starting with / or ~.'; continue ;;
            esac
            if mkdir -p "$folder" 2>/dev/null && [ -w "$folder" ]; then break; fi
            say "You may not write to $folder. Try $HOME/.local/bin instead."
        done
        bin="$folder/audionet"
    fi

    want_service=no
    want_path=no
    if [ $fresh = yes ]; then
        if user_systemd; then
            if ask_yes_no 'Start AudioNet when you log in (a systemd user service), so this computer is always visible to your other devices?' yes; then
                want_service=yes
            fi
        fi
        if ! on_path "$(dirname "$bin")"; then
            if ask_yes_no 'Add the audionet command to your PATH (in ~/.profile, for new login shells)?' yes; then want_path=yes; fi
        fi
    fi

    if ! linux_close_running "$bin" 'to be updated'; then say 'Nothing was changed.'; return; fi
    linux_install "$bin"
    say ''
    say "AudioNet $VERSION is installed in $(dirname "$bin")"
    [ $want_path = yes ] && { add_to_path "$(dirname "$bin")"; say 'The audionet command works in new login shells (or after: . ~/.profile).'; }

    if [ $fresh = yes ]; then
        if signed_in "$bin"; then
            say 'This computer is already signed in to AudioNet (from an earlier copy).'
        elif ask_yes_no 'Sign in to your AudioNet account now?' yes; then
            linux_sign_in "$bin" || true
        else
            say "To sign in later: $bin node sign-in --server $SERVER --user YOUR-ACCOUNT"
        fi
    fi

    if [ $want_service = yes ]; then
        write_unit "$bin"
        systemctl --user enable audionet.service >/dev/null 2>&1
        say 'AudioNet will start when you log in.'
    elif [ $fresh = no ] && [ -f "$UNIT" ] && [ "$(unit_program)" != "$bin" ]; then
        # The service ran a copy elsewhere: it now runs this one.
        write_unit "$bin"
    fi

    if signed_in "$bin"; then
        if service_enabled || [ "${WAS_RUNNING:-}" = service ]; then
            systemctl --user restart audionet.service && say 'AudioNet is running: this computer is visible to your other devices.'
        elif [ "${WAS_RUNNING:-}" = background ] || { [ $fresh = yes ] && ask_yes_no 'Start AudioNet now, in the background?' yes; }; then
            "$bin" node run -b >/dev/null 2>&1 </dev/null && say 'AudioNet is running in the background: this computer is visible to your other devices.'
        fi
    fi
    say "Its guide is in $LINUX_DOCS/README.md. $bin --help lists every command."
    say 'The Linux program does not update itself: to update, reinstall or uninstall it, run this command again.'
}

# ─── main ───────────────────────────────────────────────────────────────────

main() {
    # In a subshell: a failed redirection on ":" ends the whole shell in dash.
    if ! (: </dev/tty) 2>/dev/null; then
        say 'The AudioNet installer asks a few questions, so it needs a terminal. Run it in a terminal window.'
        exit 1
    fi
    case $(uname -s) in
        Darwin) say 'AudioNet installer for macOS.' ;;
        Linux) say 'AudioNet installer for Linux.' ;;
        *) say "AudioNet has no installer for $(uname -s). See https://github.com/mad-gamer26/AudioNet for what it runs on."; exit 1 ;;
    esac
    say 'Press Enter to accept the answer in brackets.'
    say ''
    WORK=$(mktemp -d "${TMPDIR:-/tmp}/audionet-install.XXXXXX") || stop 'no temporary folder could be made'
    case $(uname -s) in
        Darwin) mac_main ;;
        Linux) linux_main ;;
    esac
}

main "$@"
