"""Checks how the web client shows device visibility, in headless Chrome
against a server:

1. A temporary device connects (as the apps do: a device WebSocket with its
   token) with visibility off. The web client does not list it under
   "Visible devices", but "Manage devices" does.
2. The device turns visibility on: "… is now visible." is announced and it is
   listed with its sounds; Listen and Send my microphone are available.
3. It turns visibility off: "… is no longer visible." is announced and it is
   removed from the list.
4. Manage devices renames it (a labelled form, announced) and removes it
   (focus returns to the heading).

Environment: AUDIONET_URL, AUDIONET_USER, AUDIONET_PASSWORD (never printed).
Requires: pip install selenium requests websocket-client; Chrome.
"""
import json, os, sys, tempfile, threading, time
import requests
import websocket
from selenium import webdriver
from selenium.webdriver.common.by import By
from selenium.webdriver.support.ui import WebDriverWait

BASE = os.environ["AUDIONET_URL"].rstrip("/")
USER, PASSWORD = os.environ["AUDIONET_USER"], os.environ["AUDIONET_PASSWORD"]
NAME = "Web visibility test"
RENAMED = "Web visibility test renamed"
results = []


def check(name, ok, detail=""):
    results.append(ok)
    print(f"{name}: {'PASS' if ok else 'FAIL'}{'' if ok or not detail else '  (' + detail + ')'}", flush=True)


r = requests.post(f"{BASE}/api/v1/nodes/sign-in",
                  json={"username": USER, "password": PASSWORD, "name": NAME, "platform": "windows"})
r.raise_for_status()
dev = r.json()
ws_url = BASE.replace("https://", "wss://").replace("http://", "ws://") + "/api/v1/ws"
ws = websocket.create_connection(ws_url, header=[f"Authorization: Bearer {dev['token']}"])
ws.send(json.dumps({"type": "hello", "protocol_version": {"major": 0, "minor": 1},
                    "client": {"kind": "node", "software": "web visibility test", "platform": "windows",
                               "visible": False}}))
ws.recv()  # welcome
ws.send(json.dumps({"type": "endpoints",
                    "sources": [{"id": "input:mic", "name": "Test microphone", "source_type": "input", "is_default": True}],
                    "destinations": [{"id": "output:speakers", "name": "Test speakers", "is_default": True}]}))
stop = threading.Event()


def keep_reading():
    ws.settimeout(1)
    while not stop.is_set():
        try:
            ws.recv()
        except websocket.WebSocketTimeoutException:
            pass
        except Exception:
            return


threading.Thread(target=keep_reading, daemon=True).start()

opts = webdriver.ChromeOptions()
for a in ["--headless=new", f"--user-data-dir={tempfile.mkdtemp(prefix='audionet-visibility-')}"]:
    opts.add_argument(a)
drv = webdriver.Chrome(options=opts)
removed = False
try:
    w = WebDriverWait(drv, 30)
    drv.get(BASE)
    w.until(lambda d: d.find_element(By.ID, "sign-in-section").is_displayed() or d.find_element(By.ID, "app-section").is_displayed())
    drv.find_element(By.ID, "username").send_keys(USER)
    drv.find_element(By.ID, "password").send_keys(PASSWORD)
    drv.find_element(By.CSS_SELECTOR, "#sign-in-form button[type=submit]").click()
    w.until(lambda d: d.find_element(By.ID, "app-section").is_displayed())
    card = f"device-{dev['node_id']}"
    log = lambda: drv.execute_script(
        "return [...document.querySelectorAll('#status, #event-log, #events')].map(e => e.textContent).join(' ')") or ""
    managed = lambda: drv.find_element(By.ID, "managed-devices").get_attribute("textContent")

    # 1. Not visible: not listed; Manage devices has it.
    w.until(lambda d: NAME in managed())
    time.sleep(1)
    check("heading says Visible devices", drv.find_element(By.ID, "devices-heading").text == "Visible devices")
    check("a device with visibility off is not listed", not drv.find_elements(By.ID, card))
    check("Manage devices lists it", NAME in managed(), managed())
    check("no online/offline or sharing words anywhere",
          not any(word in drv.find_element(By.ID, "app-section").text.lower()
                  for word in ["online", "offline", "sharing"]))

    # 2. Visible: announced and listed with its sounds.
    ws.send(json.dumps({"type": "visibility", "visible": True}))
    w.until(lambda d: d.find_elements(By.ID, card))
    el = drv.find_element(By.ID, card)
    summary = el.find_element(By.CSS_SELECTOR, ".summary").get_attribute("textContent")
    check("listed with its sounds, no status", summary == ": Windows, 1 sound, 1 output", summary)
    w.until(lambda d: f"{NAME} is now visible." in log())
    check("announced as now visible", f"{NAME} is now visible." in log())
    check("Listen available", el.find_element(By.CSS_SELECTOR, ".listen button").is_enabled())
    check("Send my microphone available", el.find_element(By.CSS_SELECTOR, ".speak button").is_enabled())
    check("no Remove button on the device itself", not el.find_elements(By.CSS_SELECTOR, ".remove"))

    # 3. Hidden again: announced and removed.
    ws.send(json.dumps({"type": "visibility", "visible": False}))
    w.until(lambda d: not d.find_elements(By.ID, card))
    w.until(lambda d: f"{NAME} is no longer visible." in log())
    check("removed when no longer visible", not drv.find_elements(By.ID, card))
    check("announced as no longer visible", f"{NAME} is no longer visible." in log())

    # 4. Manage devices (its own tab): rename, then remove.
    drv.find_element(By.ID, "tab-manage").click()
    item = next(li for li in drv.find_elements(By.CSS_SELECTOR, "#managed-devices li") if NAME in li.text)
    rename = item.find_element(By.XPATH, ".//button[text()='Rename']")
    check("Rename has the device's name", rename.get_attribute("aria-label") == f"Rename {NAME}")
    rename.click()
    field = item.find_element(By.CSS_SELECTOR, "input")
    label = item.find_element(By.CSS_SELECTOR, "label").text
    check("the rename field is labelled and focused",
          label == f"New name for {NAME}" and drv.switch_to.active_element == field, label)
    field.clear()
    field.send_keys(RENAMED)
    item.find_element(By.XPATH, ".//button[text()='Save']").click()
    w.until(lambda d: RENAMED in managed())
    check("renamed and announced", f"{NAME} is now called {RENAMED}." in log())
    item = next(li for li in drv.find_elements(By.CSS_SELECTOR, "#managed-devices li") if RENAMED in li.text)
    drv.execute_script("window.confirm = () => true;")
    item.find_element(By.XPATH, ".//button[text()='Remove']").click()
    w.until(lambda d: RENAMED not in managed())
    removed = True
    check("removed from the account", RENAMED not in managed())
    check("focus back on the Manage devices heading",
          drv.switch_to.active_element.get_attribute("id") == "manage-heading")
finally:
    stop.set()
    try:
        ws.close()
    except Exception:
        pass
    drv.quit()
    if not removed:
        s = requests.Session()
        s.headers["Authorization"] = "Bearer " + s.post(f"{BASE}/api/v1/login", json={"username": USER, "password": PASSWORD}).json()["token"]
        s.delete(f"{BASE}/api/v1/nodes/{dev['node_id']}")
print(f"{sum(results)} of {len(results)} checks passed")
sys.exit(0 if results and all(results) else 1)
