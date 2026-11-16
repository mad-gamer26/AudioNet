"""Another device's sound settings in the web client, in headless Chrome
against a server (a private one: it signs in a temporary device):

1. A stand-in device connects allowing control, with speakers (default),
   headphones, a microphone and a disabled output, and answers control
   requests as a real device does (changes its state, reports it at once).
2. Its "Sound settings" opens a dialog listing them; moving the speakers'
   volume slider with the keyboard and muting reach the device, and the
   dialog follows its answers.
3. "Make Headphones the default" makes it the default (the button goes;
   focus stays in the dialog, never falls to the page).
4. A change made at the device itself shows at once (slider value).
5. A refused change (Disable) is announced with the device's reason.
6. "Enable" on the disabled output brings it back as an output.
7. The device turns the switch off: the dialog says so, with no controls.

Environment: AUDIONET_URL, AUDIONET_USER, AUDIONET_PASSWORD (never printed).
Requires: pip install selenium requests websocket-client; Chrome.
"""
import json, os, sys, threading, time
import requests
import websocket
from selenium import webdriver
from selenium.webdriver.common.by import By
from selenium.webdriver.common.keys import Keys
from selenium.webdriver.support.ui import WebDriverWait

BASE = os.environ["AUDIONET_URL"].rstrip("/")
USER, PASSWORD = os.environ["AUDIONET_USER"], os.environ["AUDIONET_PASSWORD"]
NAME = "Sound settings test"
results = []


def check(name, ok, detail=""):
    results.append(ok)
    print(f"{name}: {'PASS' if ok else 'FAIL'}{'' if ok or not detail else '  (' + detail + ')'}", flush=True)


def control(volume, muted, can_enable=True):
    return {"volume": volume, "muted": muted, "enabled": True, "is_default_communications": False,
            "can_set_volume": True, "can_mute": True, "can_set_default": True,
            "can_set_default_communications": False, "can_enable": can_enable}


# The stand-in device's state.
dev_state = {"speakers": [0.5, False], "headphones": [0.8, False], "default": "speakers",
             "phones_disabled": True, "asked": []}
lock = threading.Lock()

r = requests.post(f"{BASE}/api/v1/nodes/sign-in",
                  json={"username": USER, "password": PASSWORD, "name": NAME, "platform": "windows"})
r.raise_for_status()
dev = r.json()
ws_url = BASE.replace("https://", "wss://").replace("http://", "ws://") + "/api/v1/ws"
ws = websocket.create_connection(ws_url, header=[f"Authorization: Bearer {dev['token']}"])
ws.send(json.dumps({"type": "hello", "protocol_version": {"major": 0, "minor": 1},
                    "client": {"kind": "node", "software": "web sound settings test 1.4.0", "platform": "windows",
                               "visible": True, "allow_control": True}}))
ws.recv()  # welcome


def endpoints():
    with lock:
        s = dev_state
        dests = [{"id": "output:speakers", "name": "Speakers", "is_default": s["default"] == "speakers",
                  "control": control(*s["speakers"])},
                 {"id": "output:headphones", "name": "Headphones", "is_default": s["default"] == "headphones",
                  "control": control(*s["headphones"], can_enable=False)}]
        disabled = []
        if s["phones_disabled"]:
            disabled.append({"id": "output:usb", "name": "USB Speaker", "kind": "output",
                             "control": {**control(None, None), "enabled": False, "can_set_volume": False,
                                         "can_mute": False, "can_set_default": False, "can_enable": True}})
        else:
            dests.append({"id": "output:usb", "name": "USB Speaker", "is_default": False, "control": control(1.0, False)})
    ws.send(json.dumps({"type": "endpoints",
                        "sources": [{"id": "input:mic", "name": "Test microphone", "source_type": "input",
                                     "is_default": True, "control": control(0.7, False, can_enable=False)}],
                        "destinations": dests, "disabled": disabled, "volumes": []}))


endpoints()
stop = threading.Event()


def serve():
    ws.settimeout(1)
    while not stop.is_set():
        try:
            m = json.loads(ws.recv())
        except websocket.WebSocketTimeoutException:
            continue
        except Exception:
            return
        if m.get("type") != "control":
            continue
        a = m["action"]
        error = None
        with lock:
            dev_state["asked"].append(a)
            name = a["target"].split(":", 1)[1]
            if a["kind"] == "set_volume" and name in ("speakers", "headphones"):
                dev_state[name][0] = round(a["volume"], 2)
            elif a["kind"] == "set_muted" and name in ("speakers", "headphones"):
                dev_state[name][1] = a["muted"]
            elif a["kind"] == "set_default":
                dev_state["default"] = name
            elif a["kind"] == "set_enabled" and name == "usb" and a["enabled"]:
                dev_state["phones_disabled"] = False
            else:
                error = "Windows didn't disable it (test refusal)."
        ws.send(json.dumps({"type": "control_result", "request_id": m["request_id"], "to": m["from"], "error": error}))
        endpoints()


threading.Thread(target=serve, daemon=True).start()

opts = webdriver.ChromeOptions()
opts.add_argument("--headless=new")
driver = webdriver.Chrome(options=opts)
wait = WebDriverWait(driver, 15)
try:
    driver.get(BASE + "/")
    wait.until(lambda d: d.find_element(By.ID, "sign-in-section").is_displayed() or d.find_element(By.ID, "app-section").is_displayed())
    driver.find_element(By.ID, "username").send_keys(USER)
    driver.find_element(By.ID, "password").send_keys(PASSWORD)
    driver.find_element(By.CSS_SELECTOR, "#sign-in-form button[type=submit]").click()
    wait.until(lambda d: d.find_element(By.ID, "app-section").is_displayed())
    summary = wait.until(lambda d: next((e for e in d.find_elements(By.CSS_SELECTOR, "details.device summary")
                                         if NAME in e.text), None))
    summary.click()
    details = summary.find_element(By.XPATH, "..")
    button = details.find_element(By.CSS_SELECTOR, "button.sound-settings")
    check("the button is named for the device", button.get_attribute("aria-label") == f"Sound settings of {NAME}")
    button.click()
    dialog = wait.until(lambda d: d.find_element(By.ID, "sound-settings") if d.find_element(By.ID, "sound-settings").get_attribute("open") is not None else None)
    title = driver.find_element(By.ID, "sound-settings-title").text
    check("the dialog names the device", title == f"Sound settings of {NAME}", title)
    body = driver.find_element(By.ID, "sound-settings-body").text
    check("outputs, microphones and disabled listed",
          all(t in body for t in ["Outputs", "Speakers (default)", "Headphones", "Microphones", "Test microphone",
                                  "Disabled", "USB Speaker (output, disabled)"]), body)

    def el(key):
        return driver.find_element(By.CSS_SELECTOR, f'#sound-settings-body [data-key="{key}"]')

    slider = el("v:output:speakers")
    check("the speakers' slider shows 50%", slider.get_attribute("value") == "50" and slider.get_attribute("aria-valuetext") == "50%")
    label = driver.find_element(By.CSS_SELECTOR, f'label[for="{slider.get_attribute("id")}"]').text
    check("the slider is labelled", label == "Volume of Speakers", label)
    slider.send_keys(Keys.ARROW_RIGHT * 10)
    WebDriverWait(driver, 5).until(lambda d: abs(dev_state["speakers"][0] - 0.6) < 0.005)
    check("moving the slider changed the device's volume", abs(dev_state["speakers"][0] - 0.6) < 0.005, str(dev_state["speakers"]))
    el("m:output:speakers").click()
    WebDriverWait(driver, 5).until(lambda d: dev_state["speakers"][1] is True)
    check("Mute reached the device", dev_state["speakers"][1] is True)
    time.sleep(0.5)
    check("the mute checkbox follows the device", el("m:output:speakers").is_selected())

    el("d:output:headphones").click()
    WebDriverWait(driver, 5).until(lambda d: "Headphones (default)" in d.find_element(By.ID, "sound-settings-body").text)
    check("Headphones became the default", dev_state["default"] == "headphones")
    gone = not driver.find_elements(By.CSS_SELECTOR, '#sound-settings-body [data-key="d:output:headphones"]')
    check("its Make default button went", gone)
    active = driver.execute_script("return document.activeElement && document.activeElement.closest('#sound-settings') !== null")
    check("focus stayed in the dialog", active)

    # Changed at the device itself.
    time.sleep(1.2)  # past the slider's own recent move
    with lock:
        dev_state["speakers"][0] = 0.25
    endpoints()
    WebDriverWait(driver, 5).until(lambda d: el("v:output:speakers").get_attribute("value") == "25")
    check("a change at the device shows at once", el("v:output:speakers").get_attribute("aria-valuetext") == "25%")

    el("x:output:speakers").click()
    WebDriverWait(driver, 5).until(lambda d: "test refusal" in d.find_element(By.ID, "status").text)
    check("a refusal is announced with the reason", True)

    el("e:output:usb").click()
    WebDriverWait(driver, 5).until(lambda d: "USB Speaker (output, disabled)" not in d.find_element(By.ID, "sound-settings-body").text)
    body = driver.find_element(By.ID, "sound-settings-body").text
    check("Enable brought the output back", "USB Speaker" in body and "Disabled" not in body, body)

    ws.send(json.dumps({"type": "allow_control", "allowed": False}))
    WebDriverWait(driver, 5).until(lambda d: "doesn't let other devices" in d.find_element(By.ID, "sound-settings-body").text)
    no_controls = not driver.find_elements(By.CSS_SELECTOR, "#sound-settings-body input, #sound-settings-body button")
    check("switched off: one note, no controls", no_controls)

    driver.find_element(By.ID, "close-sound-settings").click()
    refocus = driver.execute_script("return document.activeElement === arguments[0]", button)
    check("closing returns focus to the button", refocus)
finally:
    stop.set()
    driver.quit()
    ws.close()
    # The test device goes (signed in as the account, like the web's Manage devices).
    with requests.Session() as sess:
        sess.headers["Authorization"] = "Bearer " + sess.post(
            f"{BASE}/api/v1/login", json={"username": USER, "password": PASSWORD}).json()["token"]
        sess.delete(f"{BASE}/api/v1/nodes/{dev['node_id']}")

print(f"{sum(results)} of {len(results)} checks passed")
sys.exit(0 if all(results) else 1)
