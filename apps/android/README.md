# AudioNet for Android

A native Android app: Kotlin and Jetpack Compose on the same Rust engine as
the Windows, Mac and iPhone apps (through UniFFI's Kotlin bindings), with
Android's own audio (AAudio, through cpal). No web view.

What it does today, like the iPhone app:

* Sign in with an account name and password (used once; the phone keeps a
  revocable device token, encrypted with a key in the Android Keystore).
  Several accounts at once; sign out removes the phone from the account.
* Online in every account while it runs; "Share This Phone's Audio" per
  account.
* The account's devices, each a row that expands ("Studio PC: online,
  collapsed"): choose a sound and Listen, choose an output and Send My
  Microphone.
* Streams started here: state in words, Mute, Volume, Stop; measurements
  when "Show Measurements" is on (Settings).
* A status log (Copy), Settings (accounts, Add Account, Show Measurements,
  versions).
* Stays online with the screen off: a foreground service with the
  notification Android requires, saying the state in words ("Online in
  alice on audionet.example.com, sharing, 1 stream."). While a stream runs
  it keeps the CPU awake and Wi-Fi in low-latency mode.

Android 8.0 (API 26) and later; built for Android 17 (API 37). Phones
(arm64-v8a) and the x86_64 emulator; 16 KB memory pages.

## Building (Windows)

Needs Rust with `aarch64-linux-android` and `x86_64-linux-android`,
`cargo-ndk`, the Android SDK with an NDK (r28 or later) and platform 37,
and JDK 17 or later.

```
powershell -ExecutionPolicy Bypass -File apps\android\build-core.ps1   # engine + Kotlin bindings
cd apps\android
gradlew assembleDebug                                                # or installDebug
```

`build-core.ps1 -EmulatorOnly` builds only the x86_64 engine (faster).
`local.properties` names the SDK (`sdk.dir=C:/Users/.../Android/Sdk`, with
forward slashes). Official builds may pre-fill the sign-in server with
`-PaudionetDefaultServer=https://...`; nothing depends on a particular
server.

## Accessibility

Built for TalkBack: headings for each section, every switch a single
control with its label, sound and output choices as radio buttons in a
group, device rows as buttons that say whether they are expanded, buttons
named for what they act on ("Listen to Studio PC", "Stop Listening to
..."), the volume slider's value in words ("80 percent, muted"), and the
latest message in a live region at the top that TalkBack speaks. The
notification says the state in words.

## Tested (emulator, Android 17, x86_64)

`python scripts/test/android_e2e.py` (throwaway server and account on this
computer; the command-line agent as a device):

* Sign in, permission prompts, the device list, expanding a device,
  choosing a sound, Listen, Stop, the notification text, sign out.
* A stream from the computer's device: connected, about 100 packets a
  second, no loss, the playout buffer at its target with no underruns,
  directly on the local network.
* The app keeps its sign-in across a restart.

Not yet: a real phone, sending the microphone (the emulator's microphone
may be the computer's), speaking with TalkBack, push notifications (they
would need Firebase), and the iPhone app's Live Activity, widget and Siri
equivalents.

The emulator's audio reports frequent device glitches (AAudio underruns,
counted under "Device glitches" in the measurements) while AudioNet's own
playout never runs dry; a real phone will show whether that count is near
zero before any buffer is changed.
