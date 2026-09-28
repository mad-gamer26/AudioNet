"""Checks the web client's device list as a screen reader sees it (Chrome's
accessibility tree), in headless Chrome:

1. Each device is one collapsed native disclosure (<details>/<summary>),
   named with the device's name and state, with its controls hidden.
2. Enter on it expands it (expanded state, controls reachable).
3. When the device list refreshes (another device changes), the expanded
   device stays expanded and keeps the keyboard focus.

Creates two temporary devices through the API (they stay offline) and
removes them afterwards. Environment: AUDIONET_URL, AUDIONET_USER,
AUDIONET_PASSWORD. Requires: pip install selenium requests; Chrome.
"""
import os, sys, tempfile, time
import requests
from selenium import webdriver
from selenium.webdriver.common.by import By
from selenium.webdriver.common.keys import Keys
from selenium.webdriver.support.ui import WebDriverWait

BASE = os.environ["AUDIONET_URL"].rstrip("/")
USER, PASSWORD = os.environ["AUDIONET_USER"], os.environ["AUDIONET_PASSWORD"]
NAMES = ["A11y test device one", "A11y test device two"]
results = []

def check(name, ok, detail=""):
    results.append(ok)
    print(f"{name}: {'PASS' if ok else 'FAIL'}{'' if ok or not detail else '  (' + detail + ')'}")

def api():
    s = requests.Session()
    s.headers["Authorization"] = "Bearer " + s.post(f"{BASE}/api/v1/login", json={"username": USER, "password": PASSWORD}).json()["token"]
    return s

def remove_devices():
    s = api()
    for n in s.get(f"{BASE}/api/v1/nodes").json()["nodes"]:
        if n["name"] in NAMES or n["name"].startswith("A11y test device"):
            s.delete(f"{BASE}/api/v1/nodes/{n['node_id']}")

def ax_node(drv, element):
    """The accessibility node Chrome exposes for a DOM element."""
    backend = drv.execute_cdp_cmd("DOM.describeNode", {"objectId": drv.execute_cdp_cmd(
        "Runtime.evaluate", {"expression": f"document.getElementById('{element.get_attribute('id')}')"})["result"]["objectId"]})
    return backend

def ax_for_selector(drv, selector):
    obj = drv.execute_cdp_cmd("Runtime.evaluate", {"expression": f"document.querySelector({selector!r})"})["result"]
    nodes = drv.execute_cdp_cmd("Accessibility.getPartialAXTree", {"objectId": obj["objectId"], "fetchRelatives": False})["nodes"]
    n = nodes[0]
    props = {p["name"]: p["value"].get("value") for p in n.get("properties", [])}
    return n["role"]["value"], n.get("name", {}).get("value", ""), props, n.get("ignored", False)

remove_devices()
ids = []
for name in NAMES:
    r = requests.post(f"{BASE}/api/v1/nodes/sign-in", json={"username": USER, "password": PASSWORD, "name": name, "platform": "windows"})
    r.raise_for_status()
    ids.append(r.json()["node_id"])

opts = webdriver.ChromeOptions()
for a in ["--headless=new", f"--user-data-dir={tempfile.mkdtemp(prefix='audionet-a11y-')}"]:
    opts.add_argument(a)
drv = webdriver.Chrome(options=opts)
try:
    drv.execute_cdp_cmd("Accessibility.enable", {})
    w = WebDriverWait(drv, 30)
    drv.get(BASE)
    w.until(lambda d: d.find_element(By.ID, "sign-in-section").is_displayed())
    drv.find_element(By.ID, "username").send_keys(USER)
    drv.find_element(By.ID, "password").send_keys(PASSWORD)
    drv.find_element(By.CSS_SELECTOR, "#sign-in-form button[type=submit]").click()
    w.until(lambda d: d.find_element(By.ID, f"device-{ids[0]}"))
    sel = f"#device-{ids[0]} > summary"
    role, name, props, _ = ax_for_selector(drv, sel)
    check("a device is one disclosure line named with its name and state",
          role == "DisclosureTriangle" and NAMES[0] in name and "offline" in name, f"{role} {name!r}")
    check("...collapsed at first", props.get("expanded") is False, str(props))
    _, _, _, hidden = ax_for_selector(drv, f"#device-{ids[0]}-source")
    check("...with its controls hidden from the screen reader", hidden)

    summary = drv.find_element(By.CSS_SELECTOR, sel)
    drv.execute_script("arguments[0].focus()", summary)
    summary.send_keys(Keys.ENTER)
    time.sleep(0.3)
    _, _, props, _ = ax_for_selector(drv, sel)
    _, _, _, hidden = ax_for_selector(drv, f"#device-{ids[0]}-source")
    check("Enter expands it and its controls become reachable", props.get("expanded") is True and not hidden, str(props))

    # Another device changes: the list refreshes (rename through the API).
    api().patch(f"{BASE}/api/v1/nodes/{ids[1]}", json={"name": "A11y test device renamed"})
    w.until(lambda d: "A11y test device renamed" in d.find_element(By.ID, "devices").text)
    time.sleep(0.5)
    open_still = drv.find_element(By.ID, f"device-{ids[0]}").get_attribute("open") is not None
    focused = drv.execute_script("return document.activeElement === document.querySelector(arguments[0])", sel)
    check("after the list refreshes, the device stays expanded and keeps the focus", open_still and focused,
          f"open={open_still} focused={focused}")
finally:
    drv.quit()
    remove_devices()
print("web device list accessibility:", "PASS" if all(results) else "FAIL")
sys.exit(0 if all(results) else 1)
