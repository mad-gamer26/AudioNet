"""The Windows app's devices and another device's Sound Settings, through
UI Automation (run in the desktop session), against a server with a
stand-in device that allows control and answers as a real one does:

1. Devices lists the stand-in as one disclosure button, reported collapsed;
   pressing it reports expanded and opens its own group (Sound to listen
   to, Play it on, Listen to, Send from this computer, To its output, Send
   to, Sound Settings of …); pressing it again closes the group.
2. Sound Settings of the device opens its window: its outputs and
   microphone with volume sliders (at the device's values) and Mute.
3. The arrow key on a volume slider changes the device's volume; Mute
   mutes it; "Make Headphones the default" makes it the default (the
   button goes).
4. A change made at the device itself shows at once (the slider moves).
5. A refused change is said (the status log has the device's reason).
6. The settings switch is in Settings, on by default.

Environment: AUDIONET_URL, AUDIONET_USER, AUDIONET_PASSWORD.
Requires: pip install comtypes requests websocket-client
"""
import ctypes, json, os, subprocess, sys, tempfile, threading, time, winreg
from ctypes import wintypes
import requests
import websocket
import comtypes.client

comtypes.client.GetModule("UIAutomationCore.dll")
from comtypes.gen import UIAutomationClient as UIA  # noqa: E402
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import desktop_log  # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
BASE = os.environ["AUDIONET_URL"].rstrip("/")
USER, PASSWORD = os.environ["AUDIONET_USER"], os.environ["AUDIONET_PASSWORD"]
EXE = os.path.join(ROOT, "target", "release", "audionet-desktop.exe")
PROFILE = "controltest"
APP_NAME = "Device control test PC"
DEVICE = "Sound test device"
COLLAPSED, EXPANDED = 0x400, 0x200

uia = comtypes.client.CreateObject(UIA.CUIAutomation, interface=UIA.IUIAutomation)
u = ctypes.WinDLL("user32")
u.FindWindowW.restype = wintypes.HWND
u.FindWindowW.argtypes = [wintypes.LPCWSTR, wintypes.LPCWSTR]
u.SendMessageW.argtypes = [wintypes.HWND, wintypes.UINT, wintypes.WPARAM, wintypes.LPARAM]
u.SendMessageW.restype = ctypes.c_ssize_t
work = tempfile.mkdtemp(prefix="audionet-control-")
results, procs = [], []


def check(name, ok, detail=""):
    results.append((name, ok))
    print(f"{name}: {'PASS' if ok else 'FAIL'}{'  (' + str(detail) + ')' if detail and not ok else ''}", flush=True)


def wait_for(pred, timeout=15):
    end = time.time() + timeout
    while time.time() < end:
        try:
            if pred():
                return True
        except Exception:
            pass
        time.sleep(0.2)
    return False


def main_window():
    return uia.ElementFromHandle(u.FindWindowW(f"AudioNetDesktop-{PROFILE}", None))


def sound_window():
    h = u.FindWindowW(f"AudioNetDesktop-{PROFILE}-SoundSettings", None)
    if not h:
        raise KeyError("no Sound Settings window")
    return uia.ElementFromHandle(h)


def named(root, prefix, ctype=None):
    arr = root.FindAll(UIA.TreeScope_Descendants, uia.CreateTrueCondition())
    for i in range(arr.Length):
        e = arr.GetElement(i)
        if (e.CurrentName or "").replace("&", "").startswith(prefix) and (ctype is None or e.CurrentControlType == ctype):
            return e
    raise KeyError(prefix)


def exists(root, prefix, ctype=None):
    try:
        named(root, prefix, ctype)
        return True
    except KeyError:
        return False


def invoke(e):
    e.GetCurrentPattern(UIA.UIA_InvokePatternId).QueryInterface(UIA.IUIAutomationInvokePattern).Invoke()


def press(root, name):
    invoke(named(root, name, UIA.UIA_ButtonControlTypeId))


def set_edit(label, text):
    named(main_window(), label, UIA.UIA_EditControlTypeId).GetCurrentPattern(UIA.UIA_ValuePatternId) \
        .QueryInterface(UIA.IUIAutomationValuePattern).SetValue(text)


def msaa_state(e):
    return e.GetCurrentPattern(UIA.UIA_LegacyIAccessiblePatternId) \
        .QueryInterface(UIA.IUIAutomationLegacyIAccessiblePattern).CurrentState


def slider_value(root, name):
    return named(root, name, UIA.UIA_SliderControlTypeId).GetCurrentPattern(UIA.UIA_RangeValuePatternId) \
        .QueryInterface(UIA.IUIAutomationRangeValuePattern).CurrentValue


def status_log():
    return desktop_log.read(uia, UIA, main_window())


# ─── the stand-in device ────────────────────────────────────────────────

def control(volume, muted, **kw):
    c = {"volume": volume, "muted": muted, "enabled": True, "is_default_communications": False,
         "can_set_volume": True, "can_mute": True, "can_set_default": True,
         "can_set_default_communications": False, "can_enable": False}
    c.update(kw)
    return c


state = {"speakers": [0.5, False], "headphones": [0.8, False], "default": "speakers"}
lock = threading.Lock()
r = requests.post(f"{BASE}/api/v1/nodes/sign-in",
                  json={"username": USER, "password": PASSWORD, "name": DEVICE, "platform": "linux"})
r.raise_for_status()
dev = r.json()
ws = websocket.create_connection(BASE.replace("http", "ws", 1) + "/api/v1/ws",
                                 header=[f"Authorization: Bearer {dev['token']}"])
ws.send(json.dumps({"type": "hello", "protocol_version": {"major": 0, "minor": 1},
                    "client": {"kind": "node", "software": "device control test 1.4.0", "platform": "linux",
                               "visible": True, "allow_control": True}}))
ws.recv()


def endpoints():
    with lock:
        d = [{"id": "output:speakers", "name": "Speakers", "is_default": state["default"] == "speakers",
              "control": control(*state["speakers"])},
             {"id": "output:headphones", "name": "Headphones", "is_default": state["default"] == "headphones",
              "control": control(*state["headphones"], can_enable=True)}]
    ws.send(json.dumps({"type": "endpoints",
                        "sources": [{"id": "input:mic", "name": "Test microphone", "source_type": "input",
                                     "is_default": True, "control": control(0.7, False)}],
                        "destinations": d, "disabled": [], "volumes": []}))


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
        a, error = m["action"], None
        with lock:
            name = a["target"].split(":", 1)[1]
            if a["kind"] == "set_volume":
                state[name][0] = round(a["volume"], 2)
            elif a["kind"] == "set_muted":
                state[name][1] = a["muted"]
            elif a["kind"] == "set_default":
                state["default"] = name
            else:
                error = "The test device refused to disable it."
        ws.send(json.dumps({"type": "control_result", "request_id": m["request_id"], "to": m["from"], "error": error}))
        endpoints()


threading.Thread(target=serve, daemon=True).start()

try:
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
    press(main_window(), "Sign in")
    check("connects", wait_for(lambda: "Connected to" in status_log(), 30), status_log()[-300:])

    label = f"{DEVICE}, Linux"
    ok = wait_for(lambda: exists(main_window(), label, UIA.UIA_ButtonControlTypeId), 15)
    check("the device is a button", ok)
    button = named(main_window(), label, UIA.UIA_ButtonControlTypeId)
    check("reported collapsed", msaa_state(button) & COLLAPSED, hex(msaa_state(button)))
    check("no group yet", not exists(main_window(), f"Listen to {DEVICE}"))
    invoke(button)
    check("pressed: reported expanded", wait_for(lambda: msaa_state(named(main_window(), label, UIA.UIA_ButtonControlTypeId)) & EXPANDED, 5))
    for part in ["Sound to listen to", "Play it on", f"Listen to {DEVICE}", "Send from this computer",
                 "To its output", f"Send to {DEVICE}", f"Sound Settings of {DEVICE}"]:
        check(f"its group has {part}", exists(main_window(), part))
    check("Play it on is a labelled drop-down list", exists(main_window(), "Play it on", UIA.UIA_ComboBoxControlTypeId))

    press(main_window(), f"Sound Settings of {DEVICE}")
    check("Sound Settings opens", wait_for(lambda: sound_window() is not None, 5))
    win = sound_window()
    check("titled for the device", win.CurrentName == f"Sound Settings of {DEVICE}", win.CurrentName)
    check("speakers at 50", slider_value(win, "Volume of Speakers") == 50, slider_value(win, "Volume of Speakers"))
    check("headphones at 80", slider_value(win, "Volume of Headphones") == 80)
    check("the microphone at 70", slider_value(win, "Volume of Test microphone") == 70)

    s = named(win, "Volume of Speakers", UIA.UIA_SliderControlTypeId)
    s.SetFocus()
    time.sleep(0.3)
    h = s.CurrentNativeWindowHandle
    u.SendMessageW(h, 0x0100, 0x27, 0)  # Right: one step (5%)
    u.SendMessageW(h, 0x0101, 0x27, 0)
    check("the arrow key changed the device's volume", wait_for(lambda: abs(state["speakers"][0] - 0.55) < 0.005, 5), state["speakers"])
    mute = named(win, "Mute Speakers", UIA.UIA_CheckBoxControlTypeId)
    mute.GetCurrentPattern(UIA.UIA_TogglePatternId).QueryInterface(UIA.IUIAutomationTogglePattern).Toggle()
    check("Mute muted it", wait_for(lambda: state["speakers"][1] is True, 5), state["speakers"])
    press(win, "Make Headphones the default")
    check("Headphones became the default", wait_for(lambda: state["default"] == "headphones", 5))
    check("...its button went", wait_for(lambda: not exists(sound_window(), "Make Headphones the default"), 5))
    check("...and Speakers can be made the default", exists(sound_window(), "Make Speakers the default"))

    time.sleep(1.2)
    with lock:
        state["headphones"][0] = 0.25
    endpoints()
    check("a change at the device shows at once", wait_for(lambda: slider_value(sound_window(), "Volume of Headphones") == 25, 5))

    press(sound_window(), "Disable Headphones")
    check("a refusal is said", wait_for(lambda: "refused to disable" in status_log(), 5), status_log()[-300:])

    press(sound_window(), "Close")
    check("Close closes it", wait_for(lambda: not u.FindWindowW(f"AudioNetDesktop-{PROFILE}-SoundSettings", None), 5))

    invoke(named(main_window(), label, UIA.UIA_ButtonControlTypeId))
    check("pressed again: collapsed, group gone", wait_for(
        lambda: msaa_state(named(main_window(), label, UIA.UIA_ButtonControlTypeId)) & COLLAPSED
        and not exists(main_window(), f"Listen to {DEVICE}"), 5))

    # Settings: the switch, on by default.
    tabs = named(main_window(), "Settings", UIA.UIA_TabItemControlTypeId)
    tabs.GetCurrentPattern(UIA.UIA_SelectionItemPatternId).QueryInterface(UIA.IUIAutomationSelectionItemPattern).Select()
    box = None
    if wait_for(lambda: exists(main_window(), "Let my other devices change my sound settings", UIA.UIA_CheckBoxControlTypeId), 5):
        box = named(main_window(), "Let my other devices change my sound settings", UIA.UIA_CheckBoxControlTypeId)
    check("Settings has the switch, on", box is not None and box.GetCurrentPattern(UIA.UIA_TogglePatternId)
          .QueryInterface(UIA.IUIAutomationTogglePattern).CurrentToggleState == 1)
finally:
    stop.set()
    for p in procs:
        p.kill()
    ws.close()
    s = requests.Session()
    s.headers["Authorization"] = "Bearer " + s.post(f"{BASE}/api/v1/login", json={"username": USER, "password": PASSWORD}).json()["token"]
    for n in s.get(f"{BASE}/api/v1/nodes").json()["nodes"]:
        if n["name"] in (APP_NAME, DEVICE):
            s.delete(f"{BASE}/api/v1/nodes/{n['node_id']}")

passed = sum(1 for _, ok in results if ok)
print(f"{passed} of {len(results)} checks passed")
sys.exit(0 if passed == len(results) else 1)
