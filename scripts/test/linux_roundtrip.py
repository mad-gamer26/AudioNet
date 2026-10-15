"""Linux end to end, on one Linux computer, without touching its own sound.

A private PulseAudio (its own socket in a temporary folder; the computer's
own PulseAudio or PipeWire, and whatever plays or records there, is never
used) has two virtual outputs, "Share" and "Play". A throwaway AudioNet
server and account run on localhost. Then:

* `audionet node run` signs in as "Linux test device" and shares what
  "Share" plays (its monitor: "Sound playing on Share");
* a 997 Hz tone at -12 dBFS plays into "Share";
* `audionet-visitor` (the NVDA add-on's engine, here only a listener)
  listens to "Sound playing on Share" on "Play";
* what "Play" receives is recorded and checked: the tone's level and
  pitch, and the stream's measurements.

Passes when the stream connects, the recording is the tone (frequency
within 2 Hz, level within 1.5 dB), nothing was lost or concealed, and the
number of PulseAudio clients does not grow while the device runs.

Run on Linux, from the repository:
    cargo build --release -p audionet-server -p audionet-cli -p audionet-visitor
    python3 scripts/test/linux_roundtrip.py [target-dir]

Needs `pulseaudio` and `pulseaudio-utils` (pacat, parec). Nothing is heard,
and no microphone is used.
"""
import json, math, os, queue, shutil, signal, struct, subprocess, sys, tempfile, threading, time, urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
TARGET = sys.argv[1] if len(sys.argv) > 1 else os.path.join(ROOT, "target")
BIN = os.path.join(TARGET, "release")
PORT = 18760
PASSWORD = "linux-test-pass"  # a throwaway account on a throwaway server
RATE, FREQ, AMP = 48000, 997.0, 10 ** (-12 / 20)

work = tempfile.mkdtemp(prefix="audionet-linux-")
procs = []


def start(args, **kw):
    p = subprocess.Popen(args, **kw)
    procs.append(p)
    return p


def cleanup():
    for p in reversed(procs):
        if p.poll() is None:
            p.send_signal(signal.SIGTERM)
            try:
                p.wait(5)
            except subprocess.TimeoutExpired:
                p.kill()
    shutil.rmtree(work, ignore_errors=True)


try:
    # The private PulseAudio.
    rt = os.path.join(work, "pulse")
    os.makedirs(rt)
    socket_path = os.path.join(rt, "native")
    env = dict(os.environ, PULSE_RUNTIME_PATH=rt, PULSE_STATE_PATH=os.path.join(work, "state"),
               PULSE_SERVER=f"unix:{socket_path}", PULSE_COOKIE=os.path.join(work, "cookie"))
    env.pop("PULSE_SINK", None)
    env.pop("PULSE_SOURCE", None)
    start(["pulseaudio", "-n", "--daemonize=no", "--use-pid-file=no", "--exit-idle-time=-1",
           "--log-target=stderr", "--log-level=error",
           "-L", f"module-native-protocol-unix socket={socket_path} auth-anonymous=1",
           "-L", f"module-null-sink sink_name=an_share rate={RATE} sink_properties=device.description=Share",
           "-L", f"module-null-sink sink_name=an_play rate={RATE} sink_properties=device.description=Play"],
          # No session bus: the computer's own PulseAudio holds its name there.
          env=dict(env, DBUS_SESSION_BUS_ADDRESS="unix:path=/nonexistent"), stdout=subprocess.DEVNULL)
    for _ in range(50):
        if os.path.exists(socket_path):
            break
        time.sleep(0.1)
    subprocess.run(["pactl", "set-default-sink", "an_play"], env=env, check=True)

    # The throwaway server and account.
    config = os.path.join(work, "config.toml")
    with open(config, "w", encoding="utf-8") as f:
        f.write(f'public_url = "http://127.0.0.1:{PORT}"\nbind = "127.0.0.1:{PORT}"\n'
                f'database = "{os.path.join(work, "test.db")}"\nweb_root = "{os.path.join(ROOT, "web")}"\n')
    server = os.path.join(BIN, "audionet-server")
    subprocess.run([server, "-c", config, "user", "add", "tester"], input=PASSWORD + "\n", text=True,
                   check=True, capture_output=True)
    start([server, "-c", config, "serve"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    for _ in range(50):
        try:
            urllib.request.urlopen(f"http://127.0.0.1:{PORT}/api/v1/info", timeout=1)
            break
        except OSError:
            time.sleep(0.2)

    # The Linux device.
    cli = os.path.join(BIN, "audionet")
    listed = subprocess.run([cli, "list"], env=env, text=True, capture_output=True, check=True).stdout
    print("audionet list (private PulseAudio):")
    print("".join("  " + line + "\n" for line in listed.splitlines()), end="")
    node_config = os.path.join(work, "node.toml")
    subprocess.run([cli, "node", "sign-in", "--server", f"http://127.0.0.1:{PORT}", "--user", "tester",
                    "--password-stdin", "--name", "Linux test device", "--config", node_config],
                   input=PASSWORD + "\n", text=True, check=True, capture_output=True, env=env)
    start([cli, "node", "run", "--config", node_config], env=env,
          stdout=subprocess.DEVNULL, stderr=open(os.path.join(work, "node.log"), "w"))

    # The tone, into "Share" (raw stereo 16-bit, looped).
    period = int(RATE / FREQ * 1000)  # 1000 cycles: a whole number of samples
    cycle = b"".join(struct.pack("<hh", s, s) for s in
                     (int(AMP * 32767 * math.sin(2 * math.pi * FREQ * i / RATE)) for i in range(period)))
    tone = start(["pacat", "--playback", "-d", "an_share", "--format=s16le", f"--rate={RATE}",
                  "--channels=2", "--latency-msec=40"], env=env, stdin=subprocess.PIPE)

    def feed():
        try:
            while tone.poll() is None:
                tone.stdin.write(cycle)
        except (BrokenPipeError, OSError):
            pass
    threading.Thread(target=feed, daemon=True).start()

    # The listener.
    visitor = start([os.path.join(BIN, "audionet-visitor")], env=env, stdin=subprocess.PIPE,
                    stdout=subprocess.PIPE, text=True, bufsize=1)
    lines = queue.Queue()
    threading.Thread(target=lambda: [lines.put(json.loads(l)) for l in visitor.stdout], daemon=True).start()
    next_id = [0]

    def ask(cmd, **args):
        next_id[0] += 1
        visitor.stdin.write(json.dumps({"id": next_id[0], "cmd": cmd, **args}) + "\n")
        visitor.stdin.flush()
        return wait(lambda m: m.get("id") == next_id[0], f"reply to {cmd}")

    def wait(pred, what, timeout=30):
        end = time.time() + timeout
        while time.time() < end:
            try:
                m = lines.get(timeout=end - time.time())
            except queue.Empty:
                break
            events.append(m)
            if pred(m):
                return m
        raise SystemExit(f"FAIL: no {what}\nlast messages: {events[-5:]}")

    events = []
    login = ask("sign_in", server=f"http://127.0.0.1:{PORT}", username="tester", password=PASSWORD)["result"]
    ask("connect", account="a", server=f"http://127.0.0.1:{PORT}", username="tester", token=login["token"])
    device = None
    end = time.time() + 30
    while device is None and time.time() < end:
        m = wait(lambda m: m.get("event") in ("devices", "device_update"), "device list")
        found = m.get("devices") or [m.get("device")]
        device = next((d for d in found if d and d.get("name") == "Linux test device" and d.get("online")), None)
    if device is None:
        raise SystemExit("FAIL: the Linux device never came online")
    print("Its sources, as the listener sees them:", ", ".join(s["name"] for s in device["sources"]))
    source = next(s for s in device["sources"] if s["name"] == "Sound playing on Share")
    local = ask("local_audio")["result"]
    play = next(d for d in local["destinations"] if d["name"] == "Play")
    session = ask("listen", account="a", node_id=device["node_id"], source_id=source["id"],
                  destination_id=play["id"])["result"]["session_id"]
    wait(lambda m: m.get("event") == "session" and m.get("session_id") == session
         and str(m.get("state")).lower() == "active", "active session")
    print("Listening: connected")

    def clients():
        out = subprocess.run(["pactl", "list", "short", "clients"], env=env, text=True, capture_output=True).stdout
        return len(out.splitlines())
    clients_at_start = clients()
    time.sleep(8)
    if os.environ.get("AN_PULSE_DETAILS"):
        # How the private PulseAudio set up AudioNet's streams.
        for kind in ("sink-inputs", "source-outputs"):
            out = subprocess.run(["pactl", "list", kind], env=env, text=True, capture_output=True).stdout
            keep = [l.strip() for l in out.splitlines()
                    if any(k in l for k in ("#", "application.name", "Latency", "Sink:", "Source:", "Sample Spec"))]
            print(f"PulseAudio {kind}:", *keep, sep="\n  ")

    # Record what "Play" receives.
    rec = subprocess.run(["timeout", "8", "parec", "-d", "an_play.monitor", "--format=s16le", f"--rate={RATE}",
                          "--channels=1", "--raw"], env=env, capture_output=True)
    pcm = struct.unpack(f"<{len(rec.stdout) // 2}h", rec.stdout[: len(rec.stdout) // 2 * 2])
    x = [v / 32768 for v in pcm[RATE:]]  # skip the first second
    rms = math.sqrt(sum(v * v for v in x) / max(len(x), 1))
    crossings = sum(1 for a, b in zip(x, x[1:]) if a < 0 <= b)
    freq = crossings / (len(x) / RATE) if x else 0
    level = 20 * math.log10(rms * math.sqrt(2)) if rms > 0 else -200
    print(f"Received: {freq:.1f} Hz at {level:.1f} dBFS (sent {FREQ:.0f} Hz at -12.0 dBFS), {len(x) / RATE:.1f} s")
    time.sleep(3)
    diag = [m["text"] for m in events if m.get("event") == "diagnostics"]
    while not lines.empty():
        m = lines.get()
        if m.get("event") == "diagnostics":
            diag.append(m["text"])
    measured = diag[-1] if diag else ""
    print("Measurements:", measured or "(none)")
    # The device looks at its audio every 5 seconds: that must not open a
    # new connection to the sound server each time (it once filled
    # PulseAudio's limit of 64 in six minutes).
    time.sleep(12)
    clients_at_end = clients()
    print(f"PulseAudio clients: {clients_at_start} while listening, {clients_at_end} 30 seconds later")
    ask("stop", session_id=session)
    lost = "0.00 % lost" in measured and " 0 concealed" in measured
    ok = abs(freq - FREQ) <= 2 and abs(level + 12) <= 1.5 and lost and clients_at_end <= clients_at_start
    print("PASS" if ok else "FAIL")
    sys.exit(0 if ok else 1)
finally:
    cleanup()
