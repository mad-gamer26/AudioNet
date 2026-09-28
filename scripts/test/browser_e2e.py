"""Real-browser end-to-end test of AudioNet (web client + server + node).

Drives headless Chrome with Selenium against an AudioNet server, signs this
Windows PC as a temporary device, streams a known tone from it to the
browser, checks frequency and level inside the page with Web Audio, and
removes the temporary device afterwards.

Environment:
  AUDIONET_URL        server URL, e.g. https://audionet.example.com
  AUDIONET_USER       account name
  AUDIONET_PASSWORD   account password (never printed)
  AUDIONET_CLI        path to audionet.exe (default: target/release/audionet.exe)
  AUDIONET_SOURCE     name of the output device whose sound is streamed
                      (a virtual cable is ideal), default "UniMic Output"
  AUDIONET_RELAY=1    also test relay-only ICE (?ice=relay) through TURN
  AUDIONET_LISTENERS=2  also test two browsers listening to the same
                      source at the same time

Requires: pip install selenium numpy sounddevice; Google Chrome.
"""
import os, subprocess, sys, time, json, tempfile
from selenium import webdriver
from selenium.webdriver.common.by import By
from selenium.webdriver.support.ui import WebDriverWait, Select

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
BASE = os.environ["AUDIONET_URL"].rstrip("/")
USER = os.environ["AUDIONET_USER"]
PASSWORD = os.environ["AUDIONET_PASSWORD"]
CLI = os.environ.get("AUDIONET_CLI", os.path.join(ROOT, "target", "release", "audionet.exe"))
SOURCE = os.environ.get("AUDIONET_SOURCE", "UniMic Output")
DEVICE_NAME = "AudioNet browser test"

def log(*a):
    print(*a, flush=True)

def new_browser():
    opts = webdriver.ChromeOptions()
    for a in ["--headless=new", "--use-fake-ui-for-media-stream", "--use-fake-device-for-media-stream",
              "--autoplay-policy=no-user-gesture-required", f"--user-data-dir={tempfile.mkdtemp(prefix='audionet-chrome-')}"]:
        opts.add_argument(a)
    return webdriver.Chrome(options=opts)

driver = new_browser()
wait = WebDriverWait(driver, 40)
extra_browsers = []
work = tempfile.mkdtemp(prefix="audionet-e2e-")
node_cfg = os.path.join(work, "node.toml")
procs = []
results = []

def sign_in(url, drv=None):
    drv = drv or driver
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
    # Devices are collapsed disclosures: expand this one (as Enter would).
    if not card.get_attribute("open"):
        card.find_element(By.TAG_NAME, "summary").click()
    src = Select(card.find_element(By.CSS_SELECTOR, "select[id$='-source']"))
    src.select_by_visible_text(next(o.text for o in src.options if SOURCE in o.text))
    card.find_element(By.CSS_SELECTOR, "form.listen button").click()
    w.until(lambda d: "Connected" in d.find_element(By.CSS_SELECTOR, "#streams li .state").text)
    drv.execute_script(open(os.path.join(HERE, "levels_setup.js")).read())

def measure(drv, label):
    """Frequency and level of what this browser is hearing, plus its diagnostics."""
    levels = drv.execute_script(open(os.path.join(HERE, "levels_read.js")).read())
    freq = drv.execute_script("""
        const s = [...state.sessions.values()][0];
        const ctx = new AudioContext(); const src = ctx.createMediaStreamSource(s.audio.srcObject);
        const an = ctx.createAnalyser(); an.fftSize = 32768; src.connect(an);
        return new Promise(r => setTimeout(() => { const f = new Float32Array(an.frequencyBinCount); an.getFloatFrequencyData(f);
          let b = 0; for (let i = 1; i < f.length; i++) if (f[i] > f[b]) b = i; r(b * ctx.sampleRate / an.fftSize); }, 1500));""")
    time.sleep(2.5)
    diag = drv.find_element(By.ID, "diagnostics").get_attribute("textContent")
    log(f"[{label}] {freq:.1f} Hz, left {levels['left_peak_dbfs']:.1f} dBFS, right {levels['right_peak_dbfs']:.1f} dBFS")
    log(diag)
    return abs(freq - 997) < 3 and levels["left_peak_dbfs"] > -20, diag

def stop_listening(drv):
    drv.find_element(By.CSS_SELECTOR, "#streams li button.stop").click()
    time.sleep(1)

def listen(label):
    start_listening(driver)
    time.sleep(5)
    result = measure(driver, label)
    stop_listening(driver)
    return result

try:
    sign_in(BASE)
    r = subprocess.run([CLI, "node", "sign-in", "--server", BASE, "--user", USER, "--password-stdin",
                        "--name", DEVICE_NAME, "--config", node_cfg],
                       input=PASSWORD + "\n", capture_output=True, text=True)
    log((r.stdout + r.stderr).strip().splitlines()[0])
    procs.append(subprocess.Popen([CLI, "node", "run", "--config", node_cfg], stdout=open(os.path.join(work, "node.log"), "w"),
                                  stderr=subprocess.STDOUT))
    procs.append(subprocess.Popen([sys.executable, os.path.join(HERE, "tone_stream.py"), SOURCE, "90"]))
    time.sleep(1.5)
    ok, _ = listen("direct")
    results.append(("listen, direct ICE", ok))
    if os.environ.get("AUDIONET_RELAY") == "1":
        sign_in(BASE + "/?ice=relay")
        ok, diag = listen("relay")
        results.append(("listen, relay-only ICE", ok and "through the TURN relay" in diag))
    if os.environ.get("AUDIONET_LISTENERS") == "2":
        # Two browsers on the same source at once: two independent sessions
        # on the device, each must hear the tone.
        second = new_browser()
        extra_browsers.append(second)
        sign_in(BASE, driver)
        sign_in(BASE, second)
        start_listening(driver)
        start_listening(second)
        time.sleep(5)
        ok_a, _ = measure(driver, "listener 1 of 2")
        ok_b, _ = measure(second, "listener 2 of 2")
        results.append(("two simultaneous listeners", ok_a and ok_b))
        stop_listening(second)
        stop_listening(driver)
finally:
    try:
        node_id = [l.split('"')[1] for l in open(node_cfg) if l.startswith("node_id")][0]
        driver.execute_script(f"return fetch('/api/v1/nodes/{node_id}', {{method: 'DELETE', credentials: 'same-origin'}});")
    except Exception as e:
        log("could not remove the test device:", e)
    for p in procs:
        p.terminate()
    for b in extra_browsers:
        b.quit()
    driver.quit()
for name, ok in results:
    log(f"{name}: {'PASS' if ok else 'FAIL'}")
sys.exit(0 if results and all(ok for _, ok in results) else 1)
