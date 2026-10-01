"""Checks what the web client's "Send my microphone" really delivers, in
headless Chrome, measured where a device plays it:

1. Voice processing is off by default, and "My microphone" lists the
   default microphone and, once allowed, the browser's microphones by name.
2. Voice processing off sends stereo: Chrome's fake microphone plays a
   stereo file (440 Hz left, 1000 Hz right); a temporary device on this PC
   (the audionet command-line agent) plays what it receives on
   AUDIONET_OUTPUT, which is recorded (loopback) and each channel measured.
3. Voice processing on sends mono: both channels carry the same sound.
4. A chosen microphone is the one opened, and is chosen again after a
   reload.

AUDIONET_OUTPUT must be an output nobody hears and nothing else plays on (a
virtual cable), default "UniMic Output". Environment: AUDIONET_URL, AUDIONET_USER,
AUDIONET_PASSWORD (a server that serves this checkout's web/ folder),
AUDIONET_CLI. The temporary device is removed afterwards.
Requires: pip install selenium numpy; Chrome.
"""
import math, os, re, struct, subprocess, sys, tempfile, time, wave
import numpy as np
from selenium import webdriver
from selenium.webdriver.common.by import By
from selenium.webdriver.support.ui import WebDriverWait, Select

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
BASE = os.environ["AUDIONET_URL"].rstrip("/")
USER, PASSWORD = os.environ["AUDIONET_USER"], os.environ["AUDIONET_PASSWORD"]
CLI = os.environ.get("AUDIONET_CLI", os.path.join(ROOT, "target", "release", "audionet.exe"))
OUTPUT = os.environ.get("AUDIONET_OUTPUT", "UniMic Output")
DEVICE_NAME = "AudioNet stereo test"
LEFT_HZ, RIGHT_HZ = 440.0, 1000.0
results = []


def check(name, ok, detail=""):
    results.append(ok)
    print(f"{name}: {'PASS' if ok else 'FAIL'}{'  (' + detail + ')' if detail else ''}", flush=True)


def stereo_file(path, seconds=30, rate=48000):
    with wave.open(path, "wb") as w:
        w.setnchannels(2)
        w.setsampwidth(2)
        w.setframerate(rate)
        frames = bytearray()
        for i in range(seconds * rate):
            t = i / rate
            frames += struct.pack("<hh", int(8000 * math.sin(2 * math.pi * LEFT_HZ * t)),
                                  int(8000 * math.sin(2 * math.pi * RIGHT_HZ * t)))
        w.writeframes(bytes(frames))


def output_number():
    listing = subprocess.run([CLI, "list"], capture_output=True, text=True).stdout
    m = re.search(rf"Output device (\d+) of \d+: {re.escape(OUTPUT)}", listing)
    if not m:
        sys.exit(f"No output named {OUTPUT}")
    return m.group(1)


def record(path, seconds=3):
    subprocess.run([CLI, "capture-test", "--loopback", output_number(), "--seconds", str(seconds), "--wav", path],
                   capture_output=True, check=True)
    # The recording is 32-bit float (WAVE_FORMAT_EXTENSIBLE), which the
    # wave module cannot read: find the fmt and data chunks by hand.
    raw = open(path, "rb").read()
    pos, channels, rate, bits, data = 12, 0, 0, 0, b""
    while pos + 8 <= len(raw):
        cid, size = raw[pos:pos + 4], struct.unpack("<I", raw[pos + 4:pos + 8])[0]
        body = raw[pos + 8:pos + 8 + size]
        if cid == b"fmt ":
            channels, rate = struct.unpack("<HI", body[2:8])
            bits = struct.unpack("<H", body[14:16])[0]
        elif cid == b"data":
            data = body
        pos += 8 + size + (size & 1)
    samples = np.frombuffer(data, dtype=np.float32 if bits == 32 else np.int16).astype(np.float64)
    if bits != 32:
        samples /= 32768.0
    return samples.reshape(-1, channels), rate


def level_at(x, rate, hz):
    """Level (dB) of the `hz` component of `x`."""
    spectrum = np.abs(np.fft.rfft(x * np.hanning(len(x))))
    freqs = np.fft.rfftfreq(len(x), 1 / rate)
    band = (freqs > hz - 15) & (freqs < hz + 15)
    return 20 * math.log10(max(spectrum[band].max(), 1e-12))


def send(drv, w, card, voice):
    box = card.find_element(By.CSS_SELECTOR, "form.speak .voice")
    if box.is_selected() != voice:
        box.click()
    dest = Select(card.find_element(By.CSS_SELECTOR, "select[id$='-dest']"))
    dest.select_by_visible_text(next(o.text for o in dest.options if OUTPUT in o.text))
    card.find_element(By.CSS_SELECTOR, "form.speak button").click()
    w.until(lambda d: "Connected" in d.find_element(By.CSS_SELECTOR, "#streams li .state").text)
    time.sleep(2)


def stop(drv, w):
    drv.find_element(By.CSS_SELECTOR, "#streams li button.stop").click()
    w.until(lambda d: not d.find_elements(By.CSS_SELECTOR, "#streams li"))


work = tempfile.mkdtemp(prefix="audionet-stereo-")
mic_file = os.path.join(work, "stereo.wav")
stereo_file(mic_file)
opts = webdriver.ChromeOptions()
for a in ["--headless=new", "--use-fake-ui-for-media-stream", "--use-fake-device-for-media-stream",
          f"--use-file-for-fake-audio-capture={mic_file}", "--autoplay-policy=no-user-gesture-required",
          f"--user-data-dir={tempfile.mkdtemp(prefix='audionet-chrome-')}"]:
    opts.add_argument(a)
drv = webdriver.Chrome(options=opts)
w = WebDriverWait(drv, 40)
node_cfg = os.path.join(work, "node.toml")
procs = []
try:
    drv.get(BASE)
    w.until(lambda d: d.find_element(By.ID, "sign-in-section").is_displayed())
    drv.find_element(By.ID, "username").send_keys(USER)
    drv.find_element(By.ID, "password").send_keys(PASSWORD)
    drv.find_element(By.CSS_SELECTOR, "#sign-in-form button[type=submit]").click()
    w.until(lambda d: "Connected" in d.find_element(By.ID, "connection-state").text)
    subprocess.run([CLI, "node", "sign-in", "--server", BASE, "--user", USER, "--password-stdin",
                    "--name", DEVICE_NAME, "--config", node_cfg], input=PASSWORD + "\n", capture_output=True,
                   text=True, check=True)
    procs.append(subprocess.Popen([CLI, "node", "run", "--config", node_cfg], stdout=subprocess.DEVNULL,
                                  stderr=subprocess.DEVNULL))
    card = w.until(lambda d: next((c for c in d.find_elements(By.CSS_SELECTOR, ".device")
                                   if DEVICE_NAME in c.text and ": online" in c.text), None))
    if not card.get_attribute("open"):
        card.find_element(By.TAG_NAME, "summary").click()

    # 1. The form.
    check("voice processing is off by default", not card.find_element(By.CSS_SELECTOR, "form.speak .voice").is_selected())
    mic = card.find_element(By.CSS_SELECTOR, "select.mic")
    label = drv.execute_script("return document.querySelector(`label[for='${arguments[0]}']`).textContent", mic.get_attribute("id"))
    check("the microphone list is labelled", label == "My microphone", repr(label))
    check("the default microphone comes first", Select(mic).options[0].text == "Default microphone")

    # 2. Voice processing off: stereo.
    send(drv, w, card, voice=False)
    x, rate = record(os.path.join(work, "off.wav"))
    left_l, left_r = level_at(x[:, 0], rate, LEFT_HZ), level_at(x[:, 0], rate, RIGHT_HZ)
    right_l, right_r = level_at(x[:, 1], rate, LEFT_HZ), level_at(x[:, 1], rate, RIGHT_HZ)
    check("without voice processing the left channel carries the left sound",
          left_l - left_r > 20, f"440 Hz {left_l:.0f} dB, 1000 Hz {left_r:.0f} dB")
    check("without voice processing the right channel carries the right sound",
          right_r - right_l > 20, f"1000 Hz {right_r:.0f} dB, 440 Hz {right_l:.0f} dB")
    log = drv.find_element(By.ID, "event-log").get_attribute("textContent")
    check("the status log says stereo", "stereo, voice processing off" in log,
          next((l for l in log.splitlines() if "Microphone:" in l), "no microphone line"))
    stop(drv, w)

    # 3. Voice processing on: mono.
    send(drv, w, card, voice=True)
    x, rate = record(os.path.join(work, "on.wav"))
    # The same sound in both channels: the best match between them, at any
    # small offset in time, leaves almost nothing over.
    seg = slice(rate, rate + rate // 2)
    lags = range(-200, 201)
    def leftover(lag):
        l, r = x[seg, 0], np.roll(x[:, 1], lag)[seg]
        return np.sqrt(np.mean((l - r) ** 2)) / max(np.sqrt(np.mean(l ** 2)), 1e-12)
    best = min(lags, key=leftover)
    check("with voice processing both channels are the same (mono)", leftover(best) < 0.05,
          f"leftover {leftover(best):.3f} of the level at an offset of {best} samples; " +
          ", ".join(f"ch{c} {hz:.0f} Hz {level_at(x[:, c], rate, hz):.0f} dB" for c in (0, 1) for hz in (LEFT_HZ, RIGHT_HZ)))
    check("with voice processing sound arrives", np.sqrt(np.mean(x[:, 0] ** 2)) > 1e-3)
    stop(drv, w)

    # 4. Choosing a microphone.
    names = [o.text for o in Select(mic).options]
    check("once allowed, the microphones are listed by name", len(names) > 1 and not any(re.fullmatch(r"Microphone \d+", n) for n in names),
          ", ".join(names))
    chosen = names[-1]
    Select(mic).select_by_visible_text(chosen)
    chosen_id = Select(mic).first_selected_option.get_attribute("value")
    send(drv, w, card, voice=False)
    opened = drv.execute_script("return [...state.sessions.values()][0].localStream.getAudioTracks()[0].getSettings().deviceId")
    check("the chosen microphone is the one opened", opened == chosen_id, f"{chosen}")
    stop(drv, w)
    drv.refresh()
    w.until(lambda d: "Connected" in d.find_element(By.ID, "connection-state").text)
    card = w.until(lambda d: next((c for c in d.find_elements(By.CSS_SELECTOR, ".device")
                                   if DEVICE_NAME in c.text and ": online" in c.text), None))
    w.until(lambda d: len(Select(card.find_element(By.CSS_SELECTOR, "select.mic")).options) > 1)
    again = card.find_element(By.CSS_SELECTOR, "select.mic").get_attribute("value")
    check("the chosen microphone is chosen again after a reload", again == chosen_id)
finally:
    try:
        node_id = [l.split('"')[1] for l in open(node_cfg) if l.startswith("node_id")][0]
        drv.execute_script(f"return fetch('/api/v1/nodes/{node_id}', {{method: 'DELETE', credentials: 'same-origin'}});")
    except Exception as e:
        print("could not remove the test device:", e)
    for p in procs:
        p.terminate()
    drv.quit()
sys.exit(0 if results and all(results) else 1)
