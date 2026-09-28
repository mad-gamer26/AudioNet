"""Checks how the web client shows a device that is online but does not
share its audio, in headless Chrome against a server:

1. A temporary device connects (as the apps do: a device WebSocket with its
   token) and says it does not share.
2. The web client lists it as "online, not sharing its audio"; it offers no
   sounds to listen to (the list says "Not sharing its audio" and Listen is
   unavailable), while Send my microphone stays available.
3. The device starts sharing: "… started sharing its audio." is announced,
   and Listen becomes available with its sounds.
4. The temporary device is removed afterwards.

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
NAME = "Web sharing test"
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
                    "client": {"kind": "node", "software": "web sharing test", "platform": "windows"}}))
ws.recv()  # welcome
ws.send(json.dumps({"type": "sharing", "sharing": False}))
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
for a in ["--headless=new", f"--user-data-dir={tempfile.mkdtemp(prefix='audionet-sharing-')}"]:
    opts.add_argument(a)
drv = webdriver.Chrome(options=opts)
try:
    w = WebDriverWait(drv, 30)
    drv.get(BASE)
    w.until(lambda d: d.find_element(By.ID, "sign-in-section").is_displayed() or d.find_element(By.ID, "app-section").is_displayed())
    drv.find_element(By.ID, "username").send_keys(USER)
    drv.find_element(By.ID, "password").send_keys(PASSWORD)
    drv.find_element(By.CSS_SELECTOR, "#sign-in-form button[type=submit]").click()
    w.until(lambda d: d.find_element(By.ID, "app-section").is_displayed())
    card = f"device-{dev['node_id']}"
    w.until(lambda d: d.find_elements(By.ID, card))
    el = drv.find_element(By.ID, card)
    summary = lambda: el.find_element(By.CSS_SELECTOR, ".summary").get_attribute("textContent")
    w.until(lambda d: "online" in summary())
    check("listed as online, not sharing", summary().startswith(": online, not sharing its audio"), summary())
    source = el.find_element(By.CSS_SELECTOR, f"select[id$='-source']")
    options = [o.get_attribute("textContent") for o in source.find_elements(By.TAG_NAME, "option")]
    check("no sounds offered; the list says why", options == ["Not sharing its audio"], str(options))
    check("Listen unavailable", not el.find_element(By.CSS_SELECTOR, ".listen button").is_enabled())
    check("Send my microphone available", el.find_element(By.CSS_SELECTOR, ".speak button").is_enabled())

    ws.send(json.dumps({"type": "sharing", "sharing": True}))
    w.until(lambda d: "not sharing" not in summary())
    time.sleep(0.5)
    check("sharing: listed as online with its sounds", summary().startswith(": online, Windows, 1 sounds"), summary())
    log = drv.execute_script("return document.getElementById('status').textContent + ' ' + (window.state ? '' : '')")
    events = drv.execute_script("return [...document.querySelectorAll('#event-log, #events')].map(e => e.textContent).join(' ')") or ""
    check("the change is announced", "started sharing its audio" in (log + events), (log + events)[-200:])
    check("Listen available", el.find_element(By.CSS_SELECTOR, ".listen button").is_enabled())
finally:
    stop.set()
    try:
        ws.close()
    except Exception:
        pass
    drv.quit()
    s = requests.Session()
    s.headers["Authorization"] = "Bearer " + s.post(f"{BASE}/api/v1/login", json={"username": USER, "password": PASSWORD}).json()["token"]
    s.delete(f"{BASE}/api/v1/nodes/{dev['node_id']}")
print(f"{sum(results)} of {len(results)} checks passed")
sys.exit(0 if results and all(results) else 1)
