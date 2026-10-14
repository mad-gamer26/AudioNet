# AudioNet for NVGT

AudioNet in [NVGT](https://github.com/samtupy/nvgt) games and programs: sign
in to an AudioNet account, see its devices, listen to them through NVGT's
own sound system (3D position, HRTF, pitch, effects, mixers) and send audio
from NVGT (a microphone, a tone, a game's sound) to a device's output.

The program joins the account as a **visitor**, like the web client and
the NVDA add-on: nothing is added to the account, and the password is used
once to sign in and never kept.

## What is here

| Path | What it is |
| --- | --- |
| `plugin/audionet_plugin.cpp` | The NVGT plugin (`audionet.dll`): AudioNet's engine linked in, registering `audionet_*` functions. |
| `include/audionet.nvgt` | Script classes over the plugin: `audionet_client`, `audionet_device`, `audionet_event`, `audionet_stream`. |
| `demo/audionet_demo.nvgt` | A complete audio_form demonstration (see below). |
| `test/roundtrip_test.nvgt` | The round-trip test run by `scripts/test/nvgt_roundtrip.py`. |
| `build-plugin.ps1` | Builds the plugin on Windows (and installs it with `-Install`). |

The plugin is built on `crates/audionet-c`, AudioNet's C interface
(`crates/audionet-c/include/audionet.h`), which other C and C++ programs can
use too.

## Building and installing (Windows)

Needs Rust, Git and Visual Studio's C++ build tools.

```
powershell -ExecutionPolicy Bypass -File nvgt\build-plugin.ps1 -Install
```

This builds `nvgt\release\audionet.dll` and copies it to `C:\nvgt\lib`
(`-NvgtDir` for another NVGT folder). It is a single DLL: the engine is
inside. NVGT bundles it with a compiled game like its other plugins.

A plugin must match NVGT's plugin interface (version 5) and AngelScript
version (2.39.0 WIP) exactly; the script pins the NVGT and AngelScript
commits it builds against. If NVGT reports a version mismatch after an NVGT
update, update the two commits in the script and rebuild. The plugin also
refuses to load into an NVGT whose AngelScript version differs.

## Using it

```angelscript
#include "audionet.nvgt"

audionet_client an;
void main() {
	if (!an.sign_in("https://audionet.example.com", "name", "password")) {
		alert("AudioNet", an.error);
		return;
	}
	audionet_stream radio;
	while (true) {
		wait(5);
		audionet_event@ e = an.poll();         // devices, stream states, problems
		if (@e != null and e.type == "devices" and !radio.active and an.devices.length() > 0) {
			audionet_device@ d = an.devices[0];
			if (d.sharing and d.sources.length() > 0) radio.start_listening(an, d, d.sources[0]);
			radio.snd.set_position_3d(5, 0, 0);  // to your right
		}
		radio.update();                         // every time round the loop
	}
}
```

* `audionet_event.text` is always a sentence to speak or log.
* Listening: `audionet_stream.snd` is an ordinary NVGT `sound`. Place it,
  turn on `hrtf`, change `pitch`, add nodes to its `effects_chain`, give it
  a mixer. Call `update()` every time round the loop; it keeps the sound's
  buffer `depth_ms` full (80 by default). NVGT's playback sets the pace, and
  AudioNet's drift control follows it, so the buffer neither runs dry nor
  overflows. `buffered_ms` and `level` say how it is doing.
* Sending: `start_sending(client, device, output, name, rate, channels)`,
  then `write(float[])` with the audio as it is made (a `microphone`'s
  `read()`, a generated tone). Writes never wait: a block that does not fit
  is dropped.
* NVGT's listener faces +Y with +Z up: +X is to the right.

## The demonstration

`demo/audionet_demo.nvgt` (run it with NVGT from the `demo` folder):

1. Sign in: server address, account name, password. The server and
   account name are remembered (in `audionet_demo_settings.json` next to the
   script); the password never is.
2. The main form, all standard audio_form controls:
   * **Devices**, **Sounds on this device** and **Listen**: listens to the
     chosen sound in 3D, a little further round you for each new stream.
   * **Outputs on this device**, **Send My Microphone** and **Send a Test
     Tone**.
   * **Streams**: every stream with its state. The controls below act on the
     stream selected here:
     - Volume and Mute (AudioNet's own).
     - Placement: left or right, behind or in front, below or above, and
       "Circle around me" (once round every 8 seconds).
     - Binaural sound (HRTF) and Pitch.
     - Reverb (room size, amount), a Low-pass filter (cutoff), a High-pass
       filter (cutoff) and an Echo (delay, decay).
   * **Stop This Stream**, the **Status log**, **Sign Out** and **Quit**.

Everything that happens is spoken and kept in the status log.

## Tested

* The round trip (`python scripts/test/nvgt_roundtrip.py`): a throwaway
  server and account on this computer, the command-line agent as the device
  (a virtual audio cable's output and its loopback), and NVGT sending a
  440 Hz tone and listening to it back through the plugin. The stream
  connects within about 140 ms, the sound's buffer stays 70 to 80 ms
  against its 80 ms target over 12 seconds, and the tone comes back at its
  level (RMS 0.211 of 0.212). No game-loop pass took over 40 ms.
* The demo compiles in NVGT with no warnings. It has not yet been used with
  a screen reader.
