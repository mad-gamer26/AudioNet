"""Checks the web client's per-stream volume and mute in headless Chrome:

1. Listening to a temporary device on this PC (the audionet command-line
   agent, playing a tone on AUDIONET_SOURCE): the stream has a Mute check
   box and a Volume slider named after the stream (Chrome's accessibility
   tree); half volume plays at a quarter of the amplitude (the square
   curve), mute mutes.
2. Sending Chrome's fake microphone to that device: what is sent is
   measured inside the page; half volume is about 12 dB quieter, muted is
   silence, and full volume comes back.

Environment: AUDIONET_URL, AUDIONET_USER, AUDIONET_PASSWORD (a server that
serves this checkout's web/ folder), AUDIONET_CLI, AUDIONET_SOURCE (default
"UniMic Output"). The temporary device is removed afterwards.
Requires: pip install selenium; Chrome.
"""
import math, os, subprocess, sys, tempfile, time
from selenium import webdriver
from selenium.webdriver.common.by import By
from selenium.webdriver.support.ui import WebDriverWait, Select

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
BASE = os.environ["AUDIONET_URL"].rstrip("/")
USER, PASSWORD = os.environ["AUDIONET_USER"], os.environ["AUDIONET_PASSWORD"]
CLI = os.environ.get("AUDIONET_CLI", os.path.join(ROOT, "target", "release", "audionet.exe"))
SOURCE = os.environ.get("AUDIONET_SOURCE", "UniMic Output")
DEVICE_NAME = "AudioNet volume test"
results = []


def check(name, ok, detail=""):
    results.append(ok)
    print(f"{name}: {'PASS' if ok else 'FAIL'}{'' if ok or not detail else '  (' + detail + ')'}", flush=True)


def ax(drv, selector):
    obj = drv.execute_cdp_cmd("Runtime.evaluate", {"expression": f"document.querySelector({selector!r})"})["result"]
    n = drv.execute_cdp_cmd("Accessibility.getPartialAXTree", {"objectId": obj["objectId"], "fetchRelatives": False})["nodes"][0]
    props = {p["name"]: p["value"].get("value") for p in n.get("properties", [])}
    return n["role"]["value"], n.get("name", {}).get("value", ""), props


def set_volume(drv, li, percent):
    drv.execute_script("const r = arguments[0].querySelector('.volume'); r.value = arguments[1];"
                       "r.dispatchEvent(new Event('input', {bubbles: true}));", li, percent)


def session(drv, kind):
    return drv.execute_script(
        "return [...state.sessions.values()].map(s => ({kind: s.media.kind, volume: s.volume, muted: s.muted,"
        " audioVolume: s.audio ? s.audio.volume : null, audioMuted: s.audio ? s.audio.muted : null,"
        " gain: s.gain ? s.gain.gain.value : null})).find(s => s.kind === arguments[0]) || null", kind)


def sent_peak(drv, seconds=1.5):
    """The loudest sample sent in the next `seconds` (what the gain node
    passes on; Chrome's fake microphone beeps, so peaks are compared)."""
    return drv.execute_async_script("""
        const done = arguments[arguments.length - 1];
        const s = [...state.sessions.values()].find(s => s.gain);
        const an = s.audioContext.createAnalyser(); an.fftSize = 2048;
        s.gain.connect(an);
        const buf = new Float32Array(an.fftSize); let peak = 0;
        const t = setInterval(() => { an.getFloatTimeDomainData(buf);
            for (const v of buf) peak = Math.max(peak, Math.abs(v)); }, 20);
        setTimeout(() => { clearInterval(t); s.gain.disconnect(an); done(peak); }, arguments[0] * 1000);
    """, seconds)


def db(x):
    return 20 * math.log10(max(x, 1e-9))


opts = webdriver.ChromeOptions()
for a in ["--headless=new", "--use-fake-ui-for-media-stream", "--use-fake-device-for-media-stream",
          "--autoplay-policy=no-user-gesture-required", f"--user-data-dir={tempfile.mkdtemp(prefix='audionet-vol-')}"]:
    opts.add_argument(a)
drv = webdriver.Chrome(options=opts)
w = WebDriverWait(drv, 40)
work = tempfile.mkdtemp(prefix="audionet-vol-")
node_cfg = os.path.join(work, "node.toml")
procs = []
try:
    drv.execute_cdp_cmd("Accessibility.enable", {})
    drv.get(BASE)
    w.until(lambda d: d.find_element(By.ID, "sign-in-section").is_displayed())
    drv.find_element(By.ID, "username").send_keys(USER)
    drv.find_element(By.ID, "password").send_keys(PASSWORD)
    drv.find_element(By.CSS_SELECTOR, "#sign-in-form button[type=submit]").click()
    w.until(lambda d: "Connected" in d.find_element(By.ID, "connection-state").text)
    subprocess.run([CLI, "node", "sign-in", "--server", BASE, "--user", USER, "--password-stdin",
                    "--name", DEVICE_NAME, "--config", node_cfg], input=PASSWORD + "\n", capture_output=True, text=True,
                   check=True)
    procs.append(subprocess.Popen([CLI, "node", "run", "--config", node_cfg], stdout=subprocess.DEVNULL,
                                  stderr=subprocess.DEVNULL))
    procs.append(subprocess.Popen([sys.executable, os.path.join(HERE, "tone_stream.py"), SOURCE, "90"],
                                  stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))
    card = w.until(lambda d: next((c for c in d.find_elements(By.CSS_SELECTOR, ".device")
                                   if DEVICE_NAME in c.text and ": online" in c.text), None))
    if not card.get_attribute("open"):
        card.find_element(By.TAG_NAME, "summary").click()

    # 1. Listening.
    src = Select(card.find_element(By.CSS_SELECTOR, "select[id$='-source']"))
    src.select_by_visible_text(next(o.text for o in src.options if SOURCE in o.text))
    card.find_element(By.CSS_SELECTOR, "form.listen button").click()
    w.until(lambda d: "Connected" in d.find_element(By.CSS_SELECTOR, "#streams li .state").text)
    li = drv.find_element(By.CSS_SELECTOR, "#streams li")
    role, name, props = ax(drv, "#streams li .volume")
    # Chrome's DevTools accessibility tree does not report aria-valuetext
    # (it shows the number), so the attribute screen readers read is checked.
    valuetext = lambda: drv.execute_script("return document.querySelector('#streams li .volume').getAttribute('aria-valuetext')")
    check("the volume slider is named after the stream", role == "slider" and name.startswith("Volume for Listening to")
          and valuetext() == "100 percent", f"{role} {name!r} {valuetext()!r}")
    role, name, props = ax(drv, "#streams li .mute")
    check("the mute check box is named after the stream", role == "checkbox" and name.startswith("Mute Listening to"),
          f"{role} {name!r}")
    set_volume(drv, li, 50)
    s = session(drv, "listen")
    check("half volume plays at a quarter of the amplitude (-12 dB)", abs(s["audioVolume"] - 0.25) < 1e-6, str(s))
    check("the slider says 50 percent", valuetext() == "50 percent", str(valuetext()))
    li.find_element(By.CSS_SELECTOR, ".mute").click()
    s = session(drv, "listen")
    check("mute mutes the stream", s["audioMuted"] is True, str(s))
    check("the slider says muted", valuetext() == "50 percent, muted", str(valuetext()))
    li.find_element(By.CSS_SELECTOR, "button.stop").click()
    w.until(lambda d: not d.find_elements(By.CSS_SELECTOR, "#streams li"))

    # 2. Sending the (fake) microphone.
    out = Select(card.find_element(By.CSS_SELECTOR, "form.speak select"))
    out.select_by_index(0)
    card.find_element(By.CSS_SELECTOR, "form.speak button").click()
    w.until(lambda d: "Connected" in d.find_element(By.CSS_SELECTOR, "#streams li .state").text)
    li = drv.find_element(By.CSS_SELECTOR, "#streams li")
    full = sent_peak(drv)
    set_volume(drv, li, 50)
    time.sleep(0.2)
    half = sent_peak(drv)
    check("sending at half volume is about 12 dB quieter", abs(db(full) - db(half) - 12.0) < 1.0,
          f"{db(full):.1f} vs {db(half):.1f} dBFS")
    li.find_element(By.CSS_SELECTOR, ".mute").click()
    time.sleep(0.2)
    muted = sent_peak(drv)
    check("sending muted is silence", muted < 1e-4, f"{db(muted):.1f} dBFS")
    li.find_element(By.CSS_SELECTOR, ".mute").click()
    set_volume(drv, li, 100)
    time.sleep(0.2)
    back = sent_peak(drv)
    check("full volume comes back", abs(db(back) - db(full)) < 1.0, f"{db(back):.1f} vs {db(full):.1f} dBFS")
    li.find_element(By.CSS_SELECTOR, "button.stop").click()
finally:
    try:
        node_id = [l.split('"')[1] for l in open(node_cfg) if l.startswith("node_id")][0]
        drv.execute_script(f"return fetch('/api/v1/nodes/{node_id}', {{method: 'DELETE', credentials: 'same-origin'}});")
    except Exception as e:
        print("could not remove the test device:", e)
    for p in procs:
        p.terminate()
    drv.quit()
print("web stream volume:", "PASS" if results and all(results) else "FAIL")
sys.exit(0 if results and all(results) else 1)
