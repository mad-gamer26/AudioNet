"""Longer listening test over a direct path and through the TURN relay.

Streams a steady tone from a temporary device on this PC (the audionet
command-line agent) to headless Chrome, first with normal ICE, then with
relay-only ICE (?ice=relay), and reports every 10 seconds what the
browser's receiver saw: packets per second (100 for the device's 10 ms
packets arriving in real time), loss, concealed audio, jitter and jitter-buffer
delay, and the time-stretching the browser did to keep its buffer (speed-up
to shed excess, slow-down to stretch what is there). Audio arriving slower
than real time shows as fewer packets per second, rising slow-down and
concealment.

Both ends are on this PC, so this checks the relay server and the path to
it from here, not another person's home connection.

Environment: AUDIONET_URL, AUDIONET_USER, AUDIONET_PASSWORD (never
printed), AUDIONET_CLI (default target/release/audionet.exe),
AUDIONET_SOURCE (output device the tone plays on, default "UniMic Output"),
DIRECT_S and RELAY_S (seconds per phase, default 60 and 120).

Requires: pip install selenium numpy sounddevice; Google Chrome.
"""
import os, subprocess, sys, tempfile, time
from selenium import webdriver
from selenium.webdriver.common.by import By
from selenium.webdriver.support.ui import WebDriverWait, Select

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
BASE = os.environ["AUDIONET_URL"].rstrip("/")
USER, PASSWORD = os.environ["AUDIONET_USER"], os.environ["AUDIONET_PASSWORD"]
CLI = os.environ.get("AUDIONET_CLI", os.path.join(ROOT, "target", "release", "audionet.exe"))
SOURCE = os.environ.get("AUDIONET_SOURCE", "UniMic Output")
DIRECT_S = int(os.environ.get("DIRECT_S", "60"))
RELAY_S = int(os.environ.get("RELAY_S", "120"))
DEVICE_NAME = "AudioNet relay test"
EVERY_S = 10

STATS_JS = """
const s = [...state.sessions.values()][0];
const stats = await s.pc.getStats();
const byId = new Map(); stats.forEach(r => byId.set(r.id, r));
let out = {};
stats.forEach(r => {
  if (r.type === "transport" && r.selectedCandidatePairId) {
    const p = byId.get(r.selectedCandidatePairId);
    if (p) {
      const l = byId.get(p.localCandidateId), rm = byId.get(p.remoteCandidateId);
      out.route = (l ? l.candidateType : "?") + " to " + (rm ? rm.candidateType : "?");
      out.rtt_ms = (p.currentRoundTripTime || 0) * 1000;
    }
  }
  if (r.type === "inbound-rtp" && r.kind === "audio") {
    Object.assign(out, {packets: r.packetsReceived, lost: r.packetsLost, samples: r.totalSamplesReceived,
      concealed: r.concealedSamples, jitter_ms: (r.jitter || 0) * 1000,
      jb_delay: r.jitterBufferDelay, jb_count: r.jitterBufferEmittedCount,
      slowed: r.insertedSamplesForDeceleration || 0, sped: r.removedSamplesForAcceleration || 0});
  }
});
return out;
"""


def log(*a):
    print(*a, flush=True)


def new_browser():
    opts = webdriver.ChromeOptions()
    for a in ["--headless=new", "--autoplay-policy=no-user-gesture-required",
              f"--user-data-dir={tempfile.mkdtemp(prefix='audionet-chrome-')}"]:
        opts.add_argument(a)
    return webdriver.Chrome(options=opts)


def sign_in(drv, url):
    w = WebDriverWait(drv, 40)
    drv.get(url)
    w.until(lambda d: d.find_element(By.ID, "sign-in-section").is_displayed() or d.find_element(By.ID, "app-section").is_displayed())
    if drv.find_element(By.ID, "sign-in-section").is_displayed():
        drv.find_element(By.ID, "username").send_keys(USER)
        drv.find_element(By.ID, "password").send_keys(PASSWORD)
        drv.find_element(By.CSS_SELECTOR, "#sign-in-form button[type=submit]").click()
    w.until(lambda d: "Connected" in d.find_element(By.ID, "connection-state").text)


def start_listening(drv):
    w = WebDriverWait(drv, 40)
    card = w.until(lambda d: next((c for c in d.find_elements(By.CSS_SELECTOR, ".device")
                                   if DEVICE_NAME in c.text and ": online" in c.text), None))
    if not card.get_attribute("open"):
        card.find_element(By.TAG_NAME, "summary").click()
    src = Select(card.find_element(By.CSS_SELECTOR, "select[id$='-source']"))
    src.select_by_visible_text(next(o.text for o in src.options if SOURCE in o.text))
    card.find_element(By.CSS_SELECTOR, "form.listen button").click()
    w.until(lambda d: "Connected" in d.find_element(By.CSS_SELECTOR, "#streams li .state").text)


def stop_listening(drv):
    drv.find_element(By.CSS_SELECTOR, "#streams li button.stop").click()
    time.sleep(1)


def phase(drv, label, seconds):
    """Listens for `seconds`, reporting every EVERY_S; returns totals."""
    start_listening(drv)
    time.sleep(3)  # let the jitter buffer settle
    first = prev = drv.execute_script(STATS_JS)
    t0 = tprev = time.monotonic()
    log(f"{label}: route {first.get('route', 'unknown')}, round trip {first.get('rtt_ms', 0):.0f} ms")
    while time.monotonic() - t0 < seconds:
        time.sleep(EVERY_S)
        now, cur = time.monotonic(), drv.execute_script(STATS_JS)
        dt = now - tprev
        d = lambda k: cur.get(k, 0) - prev.get(k, 0)
        jb = d("jb_delay") / d("jb_count") * 1000 if d("jb_count") else float("nan")
        samples = d("samples")
        log(f"{label} at {now - t0:.0f} s: packets per second {d('packets') / dt:.2f}; lost {d('lost')}; "
            f"concealed {d('concealed') / samples * 100 if samples else 0:.2f} %; "
            f"slowed down {d('slowed') / samples * 100 if samples else 0:.2f} %, sped up {d('sped') / samples * 100 if samples else 0:.2f} %; "
            f"jitter {cur.get('jitter_ms', 0):.1f} ms; jitter buffer {jb:.0f} ms; route {cur.get('route', '?')}")
        prev, tprev = cur, now
    stop_listening(drv)
    dt = tprev - t0
    samples = prev.get("samples", 0) - first.get("samples", 0)
    total = {
        "route": prev.get("route", "?"),
        "packets_per_s": (prev.get("packets", 0) - first.get("packets", 0)) / dt,
        "lost": prev.get("lost", 0) - first.get("lost", 0),
        "concealed_pct": (prev.get("concealed", 0) - first.get("concealed", 0)) / samples * 100 if samples else 0,
        "slowed_pct": (prev.get("slowed", 0) - first.get("slowed", 0)) / samples * 100 if samples else 0,
    }
    log(f"{label} overall: route {total['route']}; packets per second {total['packets_per_s']:.2f}; lost {total['lost']}; "
        f"concealed {total['concealed_pct']:.2f} %; slowed down {total['slowed_pct']:.2f} %")
    return total


work = tempfile.mkdtemp(prefix="audionet-relay-")
node_cfg = os.path.join(work, "node.toml")
procs = []
driver = new_browser()
results = []
try:
    r = subprocess.run([CLI, "node", "sign-in", "--server", BASE, "--user", USER, "--password-stdin",
                        "--name", DEVICE_NAME, "--config", node_cfg],
                       input=PASSWORD + "\n", capture_output=True, text=True, check=True)
    procs.append(subprocess.Popen([CLI, "node", "run", "--config", node_cfg],
                                  stdout=open(os.path.join(work, "node.log"), "w"), stderr=subprocess.STDOUT))
    procs.append(subprocess.Popen([sys.executable, os.path.join(HERE, "tone_stream.py"), SOURCE,
                                   str(DIRECT_S + RELAY_S + 90)]))
    time.sleep(2)
    sign_in(driver, BASE)
    direct = phase(driver, "Direct", DIRECT_S)
    sign_in(driver, BASE + "/?ice=relay")
    relay = phase(driver, "Relay", RELAY_S)
    for label, t in (("direct", direct), ("relay", relay)):
        # The device sends 10 ms packets: 100 per second in real time;
        # allow 1 % for clocks.
        results.append((f"{label}: packets arrive in real time", abs(t["packets_per_s"] - 100) < 1.0))
        results.append((f"{label}: concealed under 1 %", t["concealed_pct"] < 1.0))
    results.append(("relay phase went through the relay", "relay" in relay["route"]))
finally:
    try:
        node_id = [l.split('"')[1] for l in open(node_cfg) if l.startswith("node_id")][0]
        driver.execute_script(f"return fetch('/api/v1/nodes/{node_id}', {{method: 'DELETE', credentials: 'same-origin'}});")
    except Exception as e:
        log("could not remove the test device:", e)
    for p in procs:
        p.terminate()
    driver.quit()
for name, ok in results:
    log(f"{name}: {'PASS' if ok else 'FAIL'}")
sys.exit(0 if results and all(ok for _, ok in results) else 1)
