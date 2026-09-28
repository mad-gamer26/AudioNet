"""Mac <-> Windows end-to-end check of the macOS app's engine, through a
live server. Run on the Windows machine; the Mac is reached over SSH.

1. A temporary source device on this PC (the audionet command-line agent)
   shares SOURCE_OUTPUT, where a 997 Hz tone plays.
2. On the Mac, apps/macos/EngineCheck (the app's own Rust core and Swift
   bindings, no UI) signs in, listens to that tone on PLAY_ON (a silent
   virtual output), then sends the Mac's microphone to this PC's
   PLAYBACK_OUTPUT. It runs from Terminal so the microphone permission
   granted to Terminal applies.
3. This PC records PLAYBACK_OUTPUT meanwhile and reports what arrived.

The password reaches the Mac in a file only its user can read, deleted as
soon as the check starts. Temporary devices are removed afterwards.
Environment: AUDIONET_URL, AUDIONET_USER, AUDIONET_PASSWORD, MAC (ssh
host, required), SEND_FROM (optional: the Mac input
to send, by name; e.g. a loopback device when the MacBook lid is closed).
"""
import os, subprocess, sys, tempfile, time
import testenv
import numpy as np
import requests

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
BASE = os.environ["AUDIONET_URL"].rstrip("/")
USER, PASSWORD = os.environ["AUDIONET_USER"], os.environ["AUDIONET_PASSWORD"]
MAC = testenv.need("MAC", "user@mac.local")
CLI = os.path.join(ROOT, "target", "release", "audionet.exe")
SOURCE_OUTPUT = "UniMic Output"
PLAYBACK_OUTPUT = "Speakers (Yeti Classic)"
SOURCE_NAME, MAC_NAME = "Remote test source", "Mac engine test"
work = tempfile.mkdtemp(prefix="audionet-mac-check-")

def api():
    s = requests.Session()
    s.headers["Authorization"] = "Bearer " + s.post(f"{BASE}/api/v1/login", json={"username": USER, "password": PASSWORD}).json()["token"]
    return s

def remove_test_devices():
    s = api()
    for n in s.get(f"{BASE}/api/v1/nodes").json()["nodes"]:
        if n["name"] in (SOURCE_NAME, MAC_NAME):
            s.delete(f"{BASE}/api/v1/nodes/{n['node_id']}")

def output_number(name):
    out = subprocess.run([CLI, "list"], capture_output=True, text=True, encoding="utf-8").stdout
    return next(l.split()[2] for l in out.splitlines() if l.startswith("Output device") and name in l)

def read_wav(path):
    data = open(path, "rb").read()
    i = data.find(b"data"); n = int.from_bytes(data[i+4:i+8], "little")
    f = data.find(b"fmt "); ch = int.from_bytes(data[f+10:f+12], "little"); sr = int.from_bytes(data[f+12:f+16], "little")
    return np.frombuffer(data[i+8:i+8+n], dtype="<f4").reshape(-1, ch)[:, 0].astype(np.float64), sr

procs = []
try:
    remove_test_devices()
    cfg = os.path.join(work, "source.toml")
    subprocess.run([CLI, "node", "sign-in", "--server", BASE, "--user", USER, "--password-stdin", "--name", SOURCE_NAME,
                    "--config", cfg], input=PASSWORD + "\n", text=True, check=True, capture_output=True)
    source_log = open(os.path.join(work, "source-node.log"), "w", encoding="utf-8")
    print("source device log:", source_log.name)
    procs.append(subprocess.Popen([CLI, "node", "run", "--config", cfg], stdout=source_log, stderr=subprocess.STDOUT))
    procs.append(subprocess.Popen([sys.executable, os.path.join(HERE, "tone_stream.py"), SOURCE_OUTPUT, "120"],
                                  stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))
    time.sleep(3)
    wav = os.path.join(work, "yeti.wav")
    rec = subprocess.Popen([CLI, "capture-test", "--loopback", output_number(PLAYBACK_OUTPUT), "--seconds", "75", "--wav", wav],
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    rec_start = time.time()
    settings = "\n".join([f"URL={BASE}", f"USER={USER}", f"PASSWORD={PASSWORD}", f"DEVICE_NAME={MAC_NAME}",
                          f"SOURCE_DEVICE={SOURCE_NAME}", f"SOURCE_MATCH={SOURCE_OUTPUT}", "PLAY_ON=Jump Desktop Audio",
                          f"SEND_TO={PLAYBACK_OUTPUT}", "SEND_SECONDS=12", "SEND_FROM=" + os.environ.get("SEND_FROM", "")]) + "\n"
    subprocess.run(["ssh", MAC, "umask 077; cat > /tmp/audionet-engine-check.cfg; rm -f ~/Library/Logs/audionet-engine-check.log; "
                    "open -a Terminal ~/audionet/tools/engine-check.command"],
                   input=settings.encode(), check=True)  # bytes: Windows adds no \r
    log = ""
    for _ in range(120):
        time.sleep(1)
        log = subprocess.run(["ssh", MAC, "cat ~/Library/Logs/audionet-engine-check.log 2>/dev/null"],
                             capture_output=True, text=True).stdout
        if "DONE" in log or "RESULT" in log:
            break
    print(log.rstrip())
    rec.wait()
    x, sr = read_wav(wav)
    # Where the Mac's audio arrived, found in the recording itself (the two
    # machines' clocks can disagree by seconds): the stretch of 250 ms blocks
    # louder than -80 dBFS. With SEND_FROM set to a loopback device the Mac
    # sends back the 997 Hz tone it receives, so that stretch must be the tone.
    blk = sr // 4
    blocks = [x[i:i + blk] for i in range(0, len(x) - blk + 1, blk)]
    lvl = lambda v: 20 * np.log10(max(np.sqrt(np.mean(v ** 2)), 1e-12))
    loud = [i for i, v in enumerate(blocks) if lvl(v) > -80]
    send_seconds = 12
    if not loud:
        print("the Mac's audio arrives on this PC: FAIL (the recording is silent)")
    else:
        seg = x[loud[0] * blk:(loud[-1] + 1) * blk]
        inner = seg[blk:-blk] if len(seg) > 3 * blk else seg
        gaps = np.sum(np.diff(np.flatnonzero(np.abs(inner) > 0)) > sr // 100)
        spec = np.abs(np.fft.rfft(inner * np.hanning(len(inner))))
        peak_hz = np.fft.rfftfreq(len(inner), 1 / sr)[np.argmax(spec[5:]) + 5]
        dur = len(seg) / sr
        print(f"Mac audio on this PC: {dur:.2f} s at {lvl(inner):.1f} dBFS, strongest frequency {peak_hz:.0f} Hz")
        print(f"gaps of 10 ms or more of exact silence inside it: {gaps}")
        ok = abs(dur - send_seconds) < 2 and gaps == 0
        if os.environ.get("SEND_FROM"):
            ok = ok and abs(peak_hz - 997) < 5
        print(f"the Mac's audio arrives on this PC: {'PASS' if ok else 'FAIL'}")
finally:
    for p in procs:
        if p.poll() is None:
            p.terminate()
    try:
        remove_test_devices()
    except Exception as e:
        print("clean-up: could not remove the test devices:", e)
