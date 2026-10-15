# AudioNet on Linux

A Linux computer joins your account as a device with the `audionet`
command-line program: your other devices (the apps, the web client, the
NVDA add-on) can then listen to its microphones and to the sound each of
its outputs plays, and send audio to its outputs. There is no Linux
desktop app yet.

## What it offers

`audionet list` shows exactly what the device shares, one line each:

```
Source 1 of 3: Built-in Audio Analog Stereo (input, default)
Source 2 of 3: Sound playing on Built-in Audio Analog Stereo (sound playing, default)
Source 3 of 3: Sound playing on HDMI Audio (sound playing)
Output 1 of 2: Built-in Audio Analog Stereo (default)
Output 2 of 2: HDMI Audio
```

* **Microphones and other inputs**, by the names your sound system gives
  them.
* **Sound playing on each output**: what that output plays (system
  audio), from its monitor. It includes what AudioNet itself plays there.
* **Outputs**, to play what other devices send.

AudioNet talks to PulseAudio, or to PipeWire through its PulseAudio
server (`pipewire-pulse`, standard on current desktops). With neither
running it falls back to ALSA, where outputs' sound cannot be shared.

## Setting it up

1. Get the program: the `audionet-linux-x64-<version>.tar.gz` download
   (64-bit Intel or AMD; glibc 2.34 or later: Ubuntu 22.04, Debian 12,
   Fedora 35 and newer),
   or build it (see [developer setup](developer-setup.md#linux)). It needs
   `libasound2` (ALSA), which every desktop has.
2. Sign in once, as the user whose sound you want to share (not root):

   ```
   audionet node sign-in --server https://audionet.example.com --user alice --name "Studio Linux"
   ```

   It asks for the password without showing it; the password is used
   once, and the computer keeps a device token in
   `~/.config/audionet/node.toml` (readable only by you).
3. Run it: `audionet node run` (Ctrl+C stops it), or `audionet node run -b`
   to keep it running in the background. `audionet node status` says which
   server and account it is signed in to.

To start it with your session, a systemd user service
(`~/.config/systemd/user/audionet.service`):

```
[Unit]
Description=AudioNet device
After=pipewire-pulse.service pulseaudio.service

[Service]
ExecStart=%h/.local/bin/audionet node run
Restart=on-failure

[Install]
WantedBy=default.target
```

then `systemctl --user enable --now audionet`.

## Tested

On Ubuntu 22.04 (`python3 scripts/test/linux_roundtrip.py`), with a
private PulseAudio and its virtual outputs, a throwaway server and
account: the device shares "Sound playing on" an output into which a
997 Hz tone plays; a listener on the same computer receives it on
another output. Received: 996.9 Hz at -12.0 dBFS, as sent; no packets
lost or concealed, no underruns, the clocks locked. Three runs.

Not yet: real sound hardware (microphones, speakers), PipeWire, ALSA
without a sound server, a desktop session with a screen reader, and
`audionet node run` started by systemd.
