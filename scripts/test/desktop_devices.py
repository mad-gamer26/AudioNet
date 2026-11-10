"""The desktop app's own lists follow this computer's audio devices: an
output turned off disappears from "Play it on" (and its sound from "Send
from this computer") within seconds, and comes back when it is turned on
again, without restarting or reconnecting; what the user chose there stays
chosen while it is still listed.

The output switched off and on (as the Sound control panel's Disable
does, endpoint_visibility.ps1) must be active and one nothing plays to
(default: the PC's spare "Line 1 (Virtual Audio Cable)" output). Needs an
administrator (to disable the endpoint) and the desktop session (UI
Automation).

Environment: AUDIONET_URL, AUDIONET_USER, AUDIONET_PASSWORD, optionally
TOGGLE_ENDPOINT (its endpoint id, as `audionet list` shows it),
TOGGLE_NAME (its name), KEEP (another output to keep chosen; default
"Speakers (Yeti Classic)").
Requires: pip install comtypes requests
"""
import ctypes, os, subprocess, sys, tempfile, time, winreg
from ctypes import wintypes
import requests
import comtypes.client

comtypes.client.GetModule("UIAutomationCore.dll")
from comtypes.gen import UIAutomationClient as UIA  # noqa: E402
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import desktop_log  # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
BASE = os.environ["AUDIONET_URL"].rstrip("/")
USER, PASSWORD = os.environ["AUDIONET_USER"], os.environ["AUDIONET_PASSWORD"]
TOGGLE_ENDPOINT = os.environ.get("TOGGLE_ENDPOINT", "{0.0.0.00000000}.{b06e2793-a428-46b3-811a-fa374e21d48f}")
TOGGLE_NAME = os.environ.get("TOGGLE_NAME", "Line 1 (Virtual Audio Cable)")
KEEP = os.environ.get("KEEP", "Speakers (Yeti Classic)")
EXE = os.path.join(ROOT, "target", "release", "audionet-desktop.exe")
PROFILE = "devicestest"
APP_NAME = "Desktop devices test"

uia = comtypes.client.CreateObject(UIA.CUIAutomation, interface=UIA.IUIAutomation)
u = ctypes.WinDLL("user32")
u.FindWindowW.restype = wintypes.HWND
u.FindWindowW.argtypes = [wintypes.LPCWSTR, wintypes.LPCWSTR]
u.SendMessageW.argtypes = [wintypes.HWND, wintypes.UINT, wintypes.WPARAM, wintypes.LPARAM]
u.SendMessageW.restype = ctypes.c_ssize_t
work = tempfile.mkdtemp(prefix="audionet-devices-")
results, procs = [], []
CB_GETCOUNT, CB_GETLBTEXTLEN, CB_GETLBTEXT = 0x0146, 0x0149, 0x0148


def check(name, ok, detail=""):
    results.append((name, ok))
    print(f"{name}: {'PASS' if ok else 'FAIL'}{'  (' + detail + ')' if detail and not ok else ''}", flush=True)


def wait_for(pred, timeout=30):
    end = time.time() + timeout
    while time.time() < end:
        try:
            if pred():
                return True
        except Exception:
            pass
        time.sleep(0.2)
    return False


def win():
    return uia.ElementFromHandle(u.FindWindowW(f"AudioNetDesktop-{PROFILE}", None))


def named(prefix, ctype=None):
    arr = win().FindAll(UIA.TreeScope_Descendants, uia.CreateTrueCondition())
    for i in range(arr.Length):
        e = arr.GetElement(i)
        if (e.CurrentName or "").replace("&", "").startswith(prefix) and (ctype is None or e.CurrentControlType == ctype):
            return e
    raise KeyError(prefix)


def set_edit(label, text):
    named(label, UIA.UIA_EditControlTypeId).GetCurrentPattern(UIA.UIA_ValuePatternId) \
        .QueryInterface(UIA.IUIAutomationValuePattern).SetValue(text)


def press(name):
    named(name, UIA.UIA_ButtonControlTypeId).GetCurrentPattern(UIA.UIA_InvokePatternId) \
        .QueryInterface(UIA.IUIAutomationInvokePattern).Invoke()


def status_log():
    return desktop_log.read(uia, UIA, win())


def combo_value(label):
    return named(label, UIA.UIA_ComboBoxControlTypeId).GetCurrentPattern(UIA.UIA_ValuePatternId) \
        .QueryInterface(UIA.IUIAutomationValuePattern).CurrentValue


def combo_rows(label):
    """Every row of a drop-down list (read from the control itself, so
    the list need not be open)."""
    h = named(label, UIA.UIA_ComboBoxControlTypeId).CurrentNativeWindowHandle
    rows = []
    for i in range(u.SendMessageW(h, CB_GETCOUNT, 0, 0)):
        buf = ctypes.create_unicode_buffer(u.SendMessageW(h, CB_GETLBTEXTLEN, i, 0) + 1)
        u.SendMessageW(h, CB_GETLBTEXT, i, ctypes.addressof(buf))
        rows.append(buf.value)
    return rows


def choose_by_keyboard(label, text, tries=40):
    box = named(label, UIA.UIA_ComboBoxControlTypeId)
    hwnd = box.CurrentNativeWindowHandle
    box.SetFocus()
    time.sleep(0.3)
    u.SendMessageW(hwnd, 0x0100, 0x24, 0)  # Home
    for _ in range(tries):
        if text in combo_value(label):
            return True
        u.SendMessageW(hwnd, 0x0100, 0x28, 0)  # Down
        time.sleep(0.05)
    return text in combo_value(label)


def driver(enable):
    subprocess.run(["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File",
                    os.path.join(HERE, "endpoint_visibility.ps1"), "-Id", TOGGLE_ENDPOINT, "-Visible", str(int(enable))],
                   check=True, capture_output=True)


def listed(label, text):
    # The output's own row ("Name" or "Name (default)"), not a sound row.
    return any(r == text or r.startswith(text + " (") for r in combo_rows(label))


def api():
    s = requests.Session()
    token = s.post(f"{BASE}/api/v1/login", json={"username": USER, "password": PASSWORD}).json()["token"]
    s.headers["Authorization"] = f"Bearer {token}"
    return s


def remove_test_device():
    s = api()
    for n in s.get(f"{BASE}/api/v1/nodes").json()["nodes"]:
        if n["name"] == APP_NAME:
            s.delete(f"{BASE}/api/v1/nodes/{n['node_id']}")


disabled = False
try:
    remove_test_device()
    try:
        winreg.DeleteKey(winreg.HKEY_CURRENT_USER, rf"Software\AudioNet-Test-{PROFILE}")
    except FileNotFoundError:
        pass
    env = dict(os.environ, APPDATA=os.path.join(work, "appdata"), AUDIONET_TEST_PROFILE=PROFILE)
    procs.append(subprocess.Popen([EXE], env=env))
    check("app window opens", wait_for(lambda: bool(u.FindWindowW(f"AudioNetDesktop-{PROFILE}", None)), 15))
    set_edit("Server address", BASE)
    set_edit("Account user name", USER)
    set_edit("Password", PASSWORD)
    set_edit("Name for this computer", APP_NAME)
    press("Sign in")
    check("connects", wait_for(lambda: "Connected to" in status_log(), 30), status_log()[-300:])
    check(f"{TOGGLE_NAME} is in Play it on", wait_for(lambda: listed("Play it on", TOGGLE_NAME), 10), str(combo_rows("Play it on")))
    check(f"...and its sound in Send from this computer",
          listed("Send from this computer", f"Sound playing on {TOGGLE_NAME}"), str(combo_rows("Send from this computer")))
    check(f"choose {KEEP} in Play it on", choose_by_keyboard("Play it on", KEEP), combo_value("Play it on"))
    send_before = combo_value("Send from this computer")

    t = time.time()
    driver(False)
    disabled = True
    gone = wait_for(lambda: not listed("Play it on", TOGGLE_NAME), 10)
    check(f"turned off: {TOGGLE_NAME} leaves Play it on (no restart)", gone, str(combo_rows("Play it on")))
    print(f"  ...after {time.time() - t:.1f} s")
    check("...and its sound leaves Send from this computer",
          not listed("Send from this computer", f"Sound playing on {TOGGLE_NAME}"), str(combo_rows("Send from this computer")))
    check(f"...{KEEP} stays chosen", KEEP in combo_value("Play it on"), combo_value("Play it on"))
    check("...the chosen sound to send stays chosen", combo_value("Send from this computer") == send_before,
          combo_value("Send from this computer"))

    t = time.time()
    driver(True)
    disabled = False
    back = wait_for(lambda: listed("Play it on", TOGGLE_NAME), 10)
    check(f"turned on: {TOGGLE_NAME} is back in Play it on", back, str(combo_rows("Play it on")))
    print(f"  ...after {time.time() - t:.1f} s")
    check("...and its sound in Send from this computer", wait_for(
        lambda: listed("Send from this computer", f"Sound playing on {TOGGLE_NAME}"), 5))
    check(f"...{KEEP} still chosen", KEEP in combo_value("Play it on"), combo_value("Play it on"))

    # The chosen output itself goes away: the default is chosen instead.
    check(f"choose {TOGGLE_NAME}", choose_by_keyboard("Play it on", TOGGLE_NAME), combo_value("Play it on"))
    driver(False)
    disabled = True
    check("the chosen output turned off: another one is chosen, not nothing",
          wait_for(lambda: not listed("Play it on", TOGGLE_NAME) and combo_value("Play it on") not in ("", None)
                   and TOGGLE_NAME not in combo_value("Play it on"), 10), combo_value("Play it on"))
    driver(True)
    disabled = False
    wait_for(lambda: listed("Play it on", TOGGLE_NAME), 10)
    check("no device-watch error in the status log", "will not be followed" not in status_log())
    check("the app is still running", procs[0].poll() is None)
finally:
    if disabled:
        driver(True)
    for p in procs:
        if p.poll() is None:
            p.terminate()
    try:
        remove_test_device()
    except Exception as e:
        print("clean-up: could not remove the test device:", e)
    try:
        winreg.DeleteKey(winreg.HKEY_CURRENT_USER, rf"Software\AudioNet-Test-{PROFILE}")
    except FileNotFoundError:
        pass
failed = [n for n, ok in results if not ok]
print(f"{len(results) - len(failed)} of {len(results)} checks passed")
sys.exit(1 if failed or not results else 0)
