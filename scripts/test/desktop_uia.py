"""Checks the Windows desktop app through UI Automation, the interface
screen readers use: every control's role, accessible name, keyboard
focusability, then a real flow against a server: signing in puts the
computer online, not sharing; Start sharing and Stop sharing, as the
server sees them; the choice kept across a restart; Sign out removing
the device from the account. It
runs under AUDIONET_TEST_PROFILE (its own window class and preferences key,
removed afterwards), so a copy of AudioNet the user runs is never touched.

Environment: AUDIONET_URL, AUDIONET_USER, AUDIONET_PASSWORD (an account on
the server), AUDIONET_DESKTOP (path to audionet-desktop.exe). The app runs
with APPDATA pointed at a temporary folder, so the real profile is untouched.
Requires: pip install comtypes requests
"""
import os, subprocess, sys, tempfile, time, winreg
import requests
import comtypes.client

comtypes.client.GetModule("UIAutomationCore.dll")
from comtypes.gen import UIAutomationClient as UIA  # noqa: E402
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import desktop_log  # noqa: E402

BASE = os.environ["AUDIONET_URL"].rstrip("/")
EXE = os.environ.get("AUDIONET_DESKTOP", os.path.join(os.path.dirname(__file__), "..", "..", "target", "release", "audionet-desktop.exe"))
uia = comtypes.client.CreateObject(UIA.CUIAutomation, interface=UIA.IUIAutomation)
TYPES = {v: k for k, v in vars(UIA).items() if k.startswith("UIA_") and k.endswith("ControlTypeId")}

def find_window(pid, timeout=10):
    cond = uia.CreatePropertyCondition(UIA.UIA_ProcessIdPropertyId, pid)
    end = time.time() + timeout
    while time.time() < end:
        w = uia.GetRootElement().FindFirst(UIA.TreeScope_Children, cond)
        if w:
            return w
        time.sleep(0.2)
    raise SystemExit("window not found")

def children(el):
    arr = el.FindAll(UIA.TreeScope_Descendants, uia.CreateTrueCondition())
    return [arr.GetElement(i) for i in range(arr.Length)]

def by_name(win, prefix):
    for c in children(win):
        if (c.CurrentName or "").replace("&", "").startswith(prefix):
            return c
    raise KeyError(prefix)

def set_value(win, label_prefix, text):
    # Edits are named by their preceding label.
    el = next(c for c in children(win) if c.CurrentControlType == UIA.UIA_EditControlTypeId and (c.CurrentName or "").startswith(label_prefix))
    el.GetCurrentPattern(UIA.UIA_ValuePatternId).QueryInterface(UIA.IUIAutomationValuePattern).SetValue(text)

def press(win, name_prefix):
    el = by_name(win, name_prefix)
    el.GetCurrentPattern(UIA.UIA_InvokePatternId).QueryInterface(UIA.IUIAutomationInvokePattern).Invoke()

def status_text(win):
    return desktop_log.read(uia, UIA, win)

# An API session (to check and remove the test device afterwards).
s = requests.Session()
s.post(f"{BASE}/api/v1/login", json={"username": os.environ["AUDIONET_USER"], "password": os.environ["AUDIONET_PASSWORD"]},
       headers={"Origin": BASE}).raise_for_status()

TEST_KEY = r"Software\AudioNet-Test-autotest"

def clear_test_prefs():
    try:
        winreg.DeleteKey(winreg.HKEY_CURRENT_USER, TEST_KEY)
    except FileNotFoundError:
        pass

def remove_test_devices():
    for n in s.get(f"{BASE}/api/v1/nodes", headers={"Origin": BASE}).json()["nodes"]:
        if n["name"] == "Desktop UIA test":
            s.delete(f"{BASE}/api/v1/nodes/{n['node_id']}", headers={"Origin": BASE})

# Devices left by an earlier, interrupted run would be found instead.
remove_test_devices()
clear_test_prefs()
env = dict(os.environ, APPDATA=tempfile.mkdtemp(prefix="audionet-desktop-"), AUDIONET_TEST_PROFILE="autotest")
proc = subprocess.Popen([EXE], env=env)
ok = True
try:
    win = find_window(proc.pid)
    print(f"Window: {win.CurrentName!r}")
    print("Accessibility tree (role | name | keyboard focusable | enabled):")
    for c in children(win):
        role = TYPES.get(c.CurrentControlType, c.CurrentControlType).replace("UIA_", "").replace("ControlTypeId", "")
        print(f"  {role:10} | {c.CurrentName!r:70} | {bool(c.CurrentIsKeyboardFocusable)!s:5} | {bool(c.CurrentIsEnabled)}")
    system = (UIA.UIA_TitleBarControlTypeId, UIA.UIA_MenuBarControlTypeId, UIA.UIA_MenuItemControlTypeId)
    unnamed = [c for c in children(win) if c.CurrentIsKeyboardFocusable and c.CurrentControlType not in system
               and not (c.CurrentName or "").strip()]
    if unnamed:
        ok = False
        print(f"FAIL: {len(unnamed)} focusable controls without an accessible name")

    def server_view():
        nodes = s.get(f"{BASE}/api/v1/nodes", headers={"Origin": BASE}).json()["nodes"]
        return next((n for n in nodes if n["name"] == "Desktop UIA test"), None)

    def wait_server(pred, timeout=20):
        end = time.time() + timeout
        while time.time() < end:
            n = server_view()
            if pred(n):
                return n
            time.sleep(0.3)
        return server_view()

    def account_row():
        rows = [c.CurrentName or "" for c in children(win) if c.CurrentControlType == UIA.UIA_ListItemControlTypeId
                and "Desktop UIA test" in (c.CurrentName or "")]
        return rows[0] if rows else ""

    def check(name, good, detail=""):
        global ok
        ok = ok and good
        print(f"{name}: {'PASS' if good else 'FAIL'}{'' if good or not detail else '  (' + detail + ')'}")

    def answer(button, timeout=10):
        cond = uia.CreateAndCondition(uia.CreatePropertyCondition(UIA.UIA_ProcessIdPropertyId, proc.pid),
                                      uia.CreatePropertyCondition(UIA.UIA_ClassNamePropertyId, "#32770"))
        end = time.time() + timeout
        while time.time() < end:
            box = uia.GetRootElement().FindFirst(UIA.TreeScope_Children, cond) or win.FindFirst(UIA.TreeScope_Children, cond)
            if box:
                press(box, button)
                return True
            time.sleep(0.2)
        return False

    # 1. Signing in puts this computer online at once, not sharing.
    set_value(win, "Server address", BASE)
    set_value(win, "Account user name", os.environ["AUDIONET_USER"])
    set_value(win, "Password", os.environ["AUDIONET_PASSWORD"])
    set_value(win, "Device name", "Desktop UIA test")
    press(win, "Sign in")
    n = wait_server(lambda n: n and n["online"])
    check("signed in: online at once, without pressing anything", bool(n and n["online"]), str(n))
    check("...and not sharing", bool(n) and n.get("sharing") is False, str(n and n.get("sharing")))
    time.sleep(0.5)
    check("the Accounts row says online, not sharing", account_row().endswith("online, not sharing"), account_row())
    button = by_name(win, "Start sharing")
    check("the button offers Start sharing", button.CurrentName.replace("&", "") == "Start sharing", button.CurrentName)

    # 2. Start sharing: the server sees it sharing, with its sources.
    press(win, "Start sharing")
    n = wait_server(lambda n: n and n.get("sharing"))
    check("Start sharing: the server sees it sharing", bool(n and n.get("sharing") and n["sources"]), str(n and n.get("sharing")))
    time.sleep(0.5)
    check("the Accounts row says online, sharing", account_row().endswith("online, sharing"), account_row())
    check("the button now offers Stop sharing", by_name(win, "Stop sharing").CurrentName.replace("&", "") == "Stop sharing")
    print("Status log:\n  " + status_text(win).strip().replace("\r\n", "\n  "))

    # The log window: focus lands in the log, and returns to the button.
    logw = desktop_log.open_log(uia, UIA, win)
    time.sleep(0.5)
    f = uia.GetFocusedElement()
    log_focus = f.CurrentControlType == UIA.UIA_EditControlTypeId and (f.CurrentName or "").startswith("Status log")
    check("Status log window: focus in the log", log_focus, repr(f.CurrentName))
    # Keyboard: Tab leaves the read-only log (it does not keep Tab as a
    # character), goes on to Copy and Close and back round; Escape in the
    # log closes the window.
    import ctypes
    WM_KEYDOWN, VK_TAB, VK_ESCAPE = 0x0100, 0x09, 0x1B
    def key(vk):
        ctypes.windll.user32.PostMessageW(uia.GetFocusedElement().CurrentNativeWindowHandle, WM_KEYDOWN, vk, 0)
        time.sleep(0.4)
        return (uia.GetFocusedElement().CurrentName or "").replace("&", "")
    before = desktop_log.text_of(uia, UIA, logw)
    check("Tab from the log moves to Copy", key(VK_TAB) == "Copy")
    check("...then to Close", key(VK_TAB) == "Close")
    check("...and back to the log", key(VK_TAB).startswith("Status log"))
    check("...without changing the log", desktop_log.text_of(uia, UIA, logw) == before)
    key(VK_ESCAPE)
    check("Escape in the log closes the window", not desktop_log.window(uia, UIA, proc.pid, win))
    time.sleep(0.5)
    f = uia.GetFocusedElement()
    check("...and focus returns to its button", (f.CurrentName or "").replace("&", "").startswith("Status log"), repr(f.CurrentName))
    logw = desktop_log.open_log(uia, UIA, win)
    time.sleep(0.5)
    desktop_log.close(uia, UIA, logw)
    time.sleep(0.5)
    f = uia.GetFocusedElement()
    check("Status log window closed: focus back on its button", (f.CurrentName or "").replace("&", "").startswith("Status log"), repr(f.CurrentName))
    check("...and it is gone", not desktop_log.window(uia, UIA, proc.pid, win))

    # 3. Settings no longer has "Start sharing automatically" (each account
    #    remembers its own choice instead).
    press(win, "Settings")
    name = uia.CreatePropertyCondition(UIA.UIA_NamePropertyId, "AudioNet Settings")
    settings = None
    for _ in range(50):
        settings = uia.GetRootElement().FindFirst(UIA.TreeScope_Children, name) or win.FindFirst(UIA.TreeScope_Descendants, name)
        if settings:
            break
        time.sleep(0.2)
    boxes = [(c.CurrentName or "").replace("&", "") for c in children(settings) if c.CurrentControlType == UIA.UIA_CheckBoxControlTypeId]
    check("Settings has no Start sharing automatically", not any(b.startswith("Start sharing") for b in boxes), str(boxes))
    press(settings, "Close")
    time.sleep(0.5)

    # 4. Exit while sharing asks first; the computer goes offline.
    press(win, "Exit")
    check("exiting while sharing asks first", answer("Yes"))
    proc.wait(10)
    n = wait_server(lambda n: n and not n["online"])
    check("after exiting: offline", bool(n) and not n["online"], str(n))

    # 5. Opening again: online at once, sharing as it was.
    proc = subprocess.Popen([EXE], env=env)
    win = find_window(proc.pid)
    n = wait_server(lambda n: n and n["online"] and n.get("sharing"), 30)
    check("opened again: online and sharing, as it was", bool(n and n["online"] and n.get("sharing")), str(n))

    # 6. Stop sharing: still online.
    press(win, "Stop sharing")
    n = wait_server(lambda n: n and n["online"] and n.get("sharing") is False)
    check("Stop sharing: still online, not sharing", bool(n and n["online"] and n.get("sharing") is False), str(n))

    # 7. Sign out (while online): asks, then this computer is gone from the account.
    press(win, "Start sharing")
    wait_server(lambda n: n and n.get("sharing"))
    press(win, "Sign out of the selected account")
    check("sign-out asks first", answer("Yes"))
    n = wait_server(lambda n: n is None)
    check("signed out while sharing: the device is removed from the account", n is None, str(n))
    time.sleep(0.5)
    check("the log says so", "was removed from that account" in status_text(win), status_text(win)[-200:])
finally:
    if proc.poll() is None:
        proc.terminate()
        proc.wait(10)
    clear_test_prefs()
    remove_test_devices()
print("DESKTOP UIA:", "PASS" if ok else "FAIL")
sys.exit(0 if ok else 1)
