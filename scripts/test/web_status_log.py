"""Checks the web client's status log dialog as a screen reader sees it
(Chrome's accessibility tree), in headless Chrome:

1. The main page has a "Status log" button, not the log itself.
2. The button opens a modal dialog named "Status log"; focus moves to the
   events, which hold every announcement so far (the sign-in).
3. Measurements are hidden unless "Show measurements" (a check box, off)
   is checked; then they are there, named. Copy and Close buttons.
4. Escape closes it and focus returns to the Status log button; Close does
   the same.

Environment: AUDIONET_URL, AUDIONET_USER, AUDIONET_PASSWORD (a server
serving this checkout's web/ folder). Requires: pip install selenium; Chrome.
"""
import os, sys, tempfile, time
from selenium import webdriver
from selenium.webdriver.common.by import By
from selenium.webdriver.common.keys import Keys
from selenium.webdriver.support.ui import WebDriverWait

BASE = os.environ["AUDIONET_URL"].rstrip("/")
USER, PASSWORD = os.environ["AUDIONET_USER"], os.environ["AUDIONET_PASSWORD"]
results = []


def check(name, ok, detail=""):
    results.append(ok)
    print(f"{name}: {'PASS' if ok else 'FAIL'}{'' if ok or not detail else '  (' + detail + ')'}")


def ax(drv, selector):
    obj = drv.execute_cdp_cmd("Runtime.evaluate", {"expression": f"document.querySelector({selector!r})"})["result"]
    n = drv.execute_cdp_cmd("Accessibility.getPartialAXTree", {"objectId": obj["objectId"], "fetchRelatives": False})["nodes"][0]
    props = {p["name"]: p["value"].get("value") for p in n.get("properties", [])}
    return n["role"]["value"], n.get("name", {}).get("value", ""), props, n.get("ignored", False)


def focused_id(drv):
    return drv.execute_script("return document.activeElement && document.activeElement.id")


opts = webdriver.ChromeOptions()
for a in ["--headless=new", f"--user-data-dir={tempfile.mkdtemp(prefix='audionet-log-')}"]:
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
    w.until(lambda d: d.find_element(By.ID, "app-section").is_displayed())

    button = drv.find_element(By.ID, "open-status-log")
    role, name, _, _ = ax(drv, "#open-status-log")
    check("the page has a Status log button", role == "button" and name == "Status log", f"{role} {name!r}")
    check("...and the log is not on the page", not drv.find_element(By.ID, "event-log").is_displayed())

    button.click()
    time.sleep(0.3)
    role, name, props, _ = ax(drv, "#status-log")
    check("it opens a modal dialog named Status log", role == "dialog" and name == "Status log" and props.get("modal") is True,
          f"{role} {name!r} {props}")
    check("focus moves to the events", focused_id(drv) == "event-log", str(focused_id(drv)))
    events = drv.find_element(By.ID, "event-log").text
    check("the events include the sign-in announcement", "Signed in as" in events, events[-200:])
    role, name, _, _ = ax(drv, "#event-log")
    check("the events are named", name == "Events", f"{role} {name!r}")
    role, name, props, _ = ax(drv, "#show-measurements")
    check("Show measurements is a check box, off", role == "checkbox" and name.startswith("Show measurements")
          and props.get("checked") in (False, "false"), f"{role} {name!r} {props}")
    check("...and no measurements are shown", not drv.find_element(By.ID, "diagnostics").is_displayed())
    drv.find_element(By.ID, "show-measurements").click()
    time.sleep(0.3)
    role, name, _, _ = ax(drv, "#diagnostics")
    check("checked, the measurements are shown, named", name == "Measurements" and drv.find_element(By.ID, "diagnostics").is_displayed(),
          f"{role} {name!r}")
    drv.find_element(By.ID, "show-measurements").click()
    drv.find_element(By.ID, "event-log").click()
    check("Copy and Close are there", drv.find_element(By.ID, "copy-status-log").is_displayed()
          and drv.find_element(By.ID, "close-status-log").is_displayed())

    drv.switch_to.active_element.send_keys(Keys.ESCAPE)
    time.sleep(0.3)
    check("Escape closes it", not drv.find_element(By.ID, "status-log").is_displayed())
    check("...and focus returns to the Status log button", focused_id(drv) == "open-status-log", str(focused_id(drv)))

    button.click()
    time.sleep(0.3)
    drv.find_element(By.ID, "close-status-log").click()
    time.sleep(0.3)
    check("Close closes it and focus returns to the button",
          not drv.find_element(By.ID, "status-log").is_displayed() and focused_id(drv) == "open-status-log",
          str(focused_id(drv)))
finally:
    drv.quit()
print("web status log:", "PASS" if all(results) else "FAIL")
sys.exit(0 if all(results) else 1)
