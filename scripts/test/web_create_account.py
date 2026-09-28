"""Checks creating an account in the web client, as a screen reader sees
it (Chrome's accessibility tree), in headless Chrome, against a local
server started for the test (temporary database, sign-up allowed), so no
account is created on a real server:

1. The sign-in page offers "Create an account" (a button) only when the
   server allows it; pressing it moves focus to the "Create an account"
   heading, and the name typed on the sign-in form is carried over.
2. The fields are named, and the username and password fields carry their
   rules as descriptions.
3. Different passwords are caught before sending: an alert says so, focus
   moves to the second field and it is marked invalid.
4. Creating the account signs in and announces it; focus moves to the
   device list heading.
5. After signing out, the same name is refused as taken, with focus on the
   username field.
6. "Back to sign in" returns to the sign-in form and its heading.
7. With sign-up not allowed, the offer is not shown.

Environment: AUDIONET_SERVER (default target/release/audionet-server.exe).
Requires: pip install selenium; Chrome.
"""
import os, socket, subprocess, sys, tempfile, time
from selenium import webdriver
from selenium.webdriver.common.by import By
from selenium.webdriver.support.ui import WebDriverWait

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
SERVER = os.environ.get("AUDIONET_SERVER", os.path.join(ROOT, "target", "release", "audionet-server.exe"))
results = []


def check(name, ok, detail=""):
    results.append(ok)
    print(f"{name}: {'PASS' if ok else 'FAIL'}{'' if ok or not detail else '  (' + detail + ')'}")


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def start_server(allow):
    work = tempfile.mkdtemp(prefix="audionet-signup-")
    port = free_port()
    cfg = os.path.join(work, "config.toml")
    web = os.path.join(ROOT, "web").replace("\\", "/")
    db = os.path.join(work, "audionet.db").replace("\\", "/")
    with open(cfg, "w") as f:
        f.write(f'public_url = "http://127.0.0.1:{port}"\nbind = "127.0.0.1:{port}"\n'
                f'database = "{db}"\nweb_root = "{web}"\nallow_registration = {"true" if allow else "false"}\n')
    proc = subprocess.Popen([SERVER, "-c", cfg, "serve"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    for _ in range(100):
        try:
            socket.create_connection(("127.0.0.1", port), timeout=0.2).close()
            break
        except OSError:
            time.sleep(0.1)
    return proc, f"http://127.0.0.1:{port}"


def ax(drv, selector):
    obj = drv.execute_cdp_cmd("Runtime.evaluate", {"expression": f"document.querySelector({selector!r})"})["result"]
    n = drv.execute_cdp_cmd("Accessibility.getPartialAXTree", {"objectId": obj["objectId"], "fetchRelatives": False})["nodes"][0]
    props = {p["name"]: p["value"].get("value") for p in n.get("properties", [])}
    return n["role"]["value"], n.get("name", {}).get("value", ""), n.get("description", {}).get("value", ""), props


def focused_id(drv):
    return drv.execute_script("return document.activeElement && document.activeElement.id")


def alert_text(drv):
    WebDriverWait(drv, 5).until(lambda d: d.find_element(By.ID, "alert").text.strip())
    return drv.find_element(By.ID, "alert").text


opts = webdriver.ChromeOptions()
for a in ["--headless=new", f"--user-data-dir={tempfile.mkdtemp(prefix='audionet-signup-chrome-')}"]:
    opts.add_argument(a)
drv = webdriver.Chrome(options=opts)
servers = []
try:
    drv.execute_cdp_cmd("Accessibility.enable", {})
    w = WebDriverWait(drv, 30)
    proc, base = start_server(True)
    servers.append(proc)
    drv.get(base)
    w.until(lambda d: d.find_element(By.ID, "sign-in-section").is_displayed())
    w.until(lambda d: d.find_element(By.ID, "create-account-offer").is_displayed())
    role, name, _, _ = ax(drv, "#show-create-account")
    check("sign-in offers a Create an account button", role == "button" and name == "Create an account", f"{role} {name!r}")

    drv.find_element(By.ID, "username").send_keys("new.listener")
    drv.find_element(By.ID, "show-create-account").click()
    w.until(lambda d: d.find_element(By.ID, "create-account-section").is_displayed())
    check("focus moves to the Create an account heading", focused_id(drv) == "create-account-heading", focused_id(drv))
    check("the sign-in form is hidden", not drv.find_element(By.ID, "sign-in-section").is_displayed())
    check("the typed name is carried over", drv.find_element(By.ID, "new-username").get_attribute("value") == "new.listener")

    role, name, desc, _ = ax(drv, "#new-username")
    check("username field named, with its rules", role == "textbox" and name == "Username" and "Letters, digits" in desc, f"{role} {name!r} {desc!r}")
    role, name, desc, _ = ax(drv, "#new-password")
    check("password field named, with its rules", name == "Password" and "At least 10 characters" in desc, f"{role} {name!r} {desc!r}")
    role, name, _, _ = ax(drv, "#new-password-again")
    check("second password field named", name == "Password again", f"{role} {name!r}")

    drv.find_element(By.ID, "new-password").send_keys("correct horse battery")
    drv.find_element(By.ID, "new-password-again").send_keys("correct horse batteyr")
    drv.find_element(By.CSS_SELECTOR, "#create-account-form button[type=submit]").click()
    text = alert_text(drv)
    _, _, _, props = ax(drv, "#new-password-again")
    check("different passwords: alert", "two passwords are different" in text, text)
    check("...focus on the second field, marked invalid",
          focused_id(drv) == "new-password-again" and props.get("invalid") == "true", f"{focused_id(drv)} {props.get('invalid')}")

    field = drv.find_element(By.ID, "new-password-again")
    field.clear()
    field.send_keys("correct horse battery")
    drv.find_element(By.CSS_SELECTOR, "#create-account-form button[type=submit]").click()
    w.until(lambda d: d.find_element(By.ID, "app-section").is_displayed())
    time.sleep(0.3)
    status = drv.find_element(By.ID, "status").text
    check("creating signs in and announces it", "Account new.listener created" in status, status)
    check("...focus on the device list heading", focused_id(drv) == "devices-heading", focused_id(drv))
    check("...the account name is shown", drv.find_element(By.ID, "account-name").text == "new.listener")

    drv.find_element(By.ID, "sign-out").click()
    w.until(lambda d: d.find_element(By.ID, "sign-in-section").is_displayed())
    drv.find_element(By.ID, "show-create-account").click()
    for fid, value in (("new-password", "another good password"), ("new-password-again", "another good password")):
        drv.find_element(By.ID, fid).send_keys(value)
    drv.find_element(By.CSS_SELECTOR, "#create-account-form button[type=submit]").click()
    text = alert_text(drv)
    check("a taken name is refused", "taken" in text, text)
    check("...focus on the username field", focused_id(drv) == "new-username", focused_id(drv))

    drv.find_element(By.ID, "back-to-sign-in").click()
    check("Back to sign in shows the sign-in form",
          drv.find_element(By.ID, "sign-in-section").is_displayed() and not drv.find_element(By.ID, "create-account-section").is_displayed())
    check("...focus on its heading", focused_id(drv) == "sign-in-heading", focused_id(drv))

    proc2, base2 = start_server(False)
    servers.append(proc2)
    drv.get(base2)
    w.until(lambda d: d.find_element(By.ID, "sign-in-section").is_displayed())
    time.sleep(0.5)
    check("sign-up not allowed: no offer", not drv.find_element(By.ID, "create-account-offer").is_displayed())
finally:
    drv.quit()
    for p in servers:
        p.terminate()
print(f"{sum(results)} of {len(results)} checks passed")
sys.exit(0 if results and all(results) else 1)
