"""Checks the Windows desktop app through UI Automation, the interface
screen readers use: every control's role, accessible name and keyboard
focusability on each page, then a real flow against a server:

* before signing in, only the sign-in page shows (no tabs);
* signing in opens the Devices tab, and the computer is visible to the
  account's other devices at once;
* the tabs (Devices, Streams, Accounts, Settings) show one page at a time,
  and Ctrl+Tab / Ctrl+Shift+Tab move between them;
* Accounts: the Visible checkbox hides the computer from the account (the
  server stops listing it) and shows it again; the account row says which;
* the status log window's keyboard behavior;
* Settings has no sharing options;
* Exit asks while visible; the computer is no longer visible after it;
  the visibility choice is kept across a restart;
* Sign out removes the device from the account and returns to sign-in.

It runs under AUDIONET_TEST_PROFILE (its own window class and preferences
key, removed afterwards), so a copy of AudioNet the user runs is never
touched.

Environment: AUDIONET_URL, AUDIONET_USER, AUDIONET_PASSWORD (an account on
the server), AUDIONET_DESKTOP (path to audionet-desktop.exe). The app runs
with APPDATA pointed at a temporary folder, so the real profile is untouched.
It must run in the signed-in desktop session (keyboard input is sent to it).
Requires: pip install comtypes requests websocket-client
"""
import ctypes, os, subprocess, sys, tempfile, time, winreg
import requests
import comtypes.client

comtypes.client.GetModule("UIAutomationCore.dll")
from comtypes.gen import UIAutomationClient as UIA  # noqa: E402
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import desktop_log  # noqa: E402
from visibility import registered_devices, visible_devices  # noqa: E402

BASE = os.environ["AUDIONET_URL"].rstrip("/")
EXE = os.environ.get("AUDIONET_DESKTOP", os.path.join(os.path.dirname(__file__), "..", "..", "target", "release", "audionet-desktop.exe"))
NAME = "Desktop UIA test"
uia = comtypes.client.CreateObject(UIA.CUIAutomation, interface=UIA.IUIAutomation)
TYPES = {v: k for k, v in vars(UIA).items() if k.startswith("UIA_") and k.endswith("ControlTypeId")}
user32 = ctypes.windll.user32


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


def plain(el):
    return (el.CurrentName or "").replace("&", "")


def by_name(win, prefix, kind=None):
    for c in children(win):
        if plain(c).startswith(prefix) and (kind is None or c.CurrentControlType == kind):
            return c
    raise KeyError(prefix)


def has(win, prefix, kind=None):
    try:
        by_name(win, prefix, kind)
        return True
    except KeyError:
        return False


def set_value(win, label_prefix, text):
    # Edits are named by their preceding label.
    el = next(c for c in children(win) if c.CurrentControlType == UIA.UIA_EditControlTypeId and plain(c).startswith(label_prefix))
    el.GetCurrentPattern(UIA.UIA_ValuePatternId).QueryInterface(UIA.IUIAutomationValuePattern).SetValue(text)


def press(win, name_prefix):
    el = by_name(win, name_prefix, UIA.UIA_ButtonControlTypeId)
    el.GetCurrentPattern(UIA.UIA_InvokePatternId).QueryInterface(UIA.IUIAutomationInvokePattern).Invoke()


def toggle(win, name_prefix):
    el = by_name(win, name_prefix, UIA.UIA_CheckBoxControlTypeId)
    el.GetCurrentPattern(UIA.UIA_TogglePatternId).QueryInterface(UIA.IUIAutomationTogglePattern).Toggle()


def checked(win, name_prefix):
    el = by_name(win, name_prefix, UIA.UIA_CheckBoxControlTypeId)
    return el.GetCurrentPattern(UIA.UIA_TogglePatternId).QueryInterface(UIA.IUIAutomationTogglePattern).CurrentToggleState == 1


def tabs(win):
    return [c for c in children(win) if c.CurrentControlType == UIA.UIA_TabItemControlTypeId]


def selected_tab(win):
    for t in tabs(win):
        p = t.GetCurrentPattern(UIA.UIA_SelectionItemPatternId).QueryInterface(UIA.IUIAutomationSelectionItemPattern)
        if p.CurrentIsSelected:
            return plain(t)
    return None


def choose_tab(win, name):
    t = next(t for t in tabs(win) if plain(t).startswith(name))
    t.GetCurrentPattern(UIA.UIA_SelectionItemPatternId).QueryInterface(UIA.IUIAutomationSelectionItemPattern).Select()
    time.sleep(0.4)


def keys(*vks):
    """Presses `vks` together (as a person would), then releases them."""
    for vk in vks:
        user32.keybd_event(vk, 0, 0, 0)
    for vk in reversed(vks):
        user32.keybd_event(vk, 0, 2, 0)
    time.sleep(0.5)


def answer(pid, win, button, timeout=10):
    """Presses `button` in the app's message box (when one opens)."""
    cond = uia.CreateAndCondition(uia.CreatePropertyCondition(UIA.UIA_ProcessIdPropertyId, pid),
                                  uia.CreatePropertyCondition(UIA.UIA_ClassNamePropertyId, "#32770"))
    end = time.time() + timeout
    while time.time() < end:
        box = uia.GetRootElement().FindFirst(UIA.TreeScope_Children, cond) or win.FindFirst(UIA.TreeScope_Children, cond)
        if box:
            press(box, button)
            return True
        time.sleep(0.2)
    return False


def bring_to_front(win):
    """Puts the app's window in front, as clicking it would, and waits for
    it. (Windows lets a background program hand over the foreground only
    right after input, so Shift is pressed first; Alt would open the
    window's menu mode.)"""
    h = win.CurrentNativeWindowHandle
    end = time.time() + 5
    while user32.GetForegroundWindow() != h and time.time() < end:
        user32.keybd_event(0x10, 0, 0, 0)
        user32.keybd_event(0x10, 0, 2, 0)
        user32.SetForegroundWindow(h)
        time.sleep(0.3)
    time.sleep(0.5)


def focused():
    return plain(uia.GetFocusedElement())


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
        if n["name"] == NAME:
            s.delete(f"{BASE}/api/v1/nodes/{n['node_id']}", headers={"Origin": BASE})


def wait(pred, timeout=20):
    end = time.time() + timeout
    while time.time() < end:
        if pred():
            return True
        time.sleep(0.3)
    return pred()


def is_visible():
    return NAME in visible_devices(BASE, s)


def is_registered():
    return NAME in registered_devices(BASE, s)


ok = True


def check(name, good, detail=""):
    global ok
    ok = ok and good
    print(f"{name}: {'PASS' if good else 'FAIL'}{'' if good or not detail else '  (' + detail + ')'}", flush=True)


def audit(win, page):
    """Prints the page's tree and fails on focusable controls without names."""
    print(f"Accessibility tree, {page} (role | name | keyboard focusable | enabled):")
    for c in children(win):
        role = TYPES.get(c.CurrentControlType, c.CurrentControlType).replace("UIA_", "").replace("ControlTypeId", "")
        print(f"  {role:10} | {c.CurrentName!r:70} | {bool(c.CurrentIsKeyboardFocusable)!s:5} | {bool(c.CurrentIsEnabled)}")
    system = (UIA.UIA_TitleBarControlTypeId, UIA.UIA_MenuBarControlTypeId, UIA.UIA_MenuItemControlTypeId)
    unnamed = [c for c in children(win) if c.CurrentIsKeyboardFocusable and c.CurrentControlType not in system
               and not (c.CurrentName or "").strip()]
    check(f"{page}: every focusable control has a name", not unnamed, f"{len(unnamed)} unnamed")


# Devices left by an earlier, interrupted run would be found instead.
remove_test_devices()
clear_test_prefs()
env = dict(os.environ, APPDATA=tempfile.mkdtemp(prefix="audionet-desktop-"), AUDIONET_TEST_PROFILE="autotest")
proc = subprocess.Popen([EXE], env=env)
try:
    win = find_window(proc.pid)
    print(f"Window: {win.CurrentName!r}")
    bring_to_front(win)

    # 1. Before signing in: the sign-in page only.
    audit(win, "sign-in page")
    check("before signing in: no tabs", not tabs(win), str([plain(t) for t in tabs(win)]))
    check("...and no device or account controls", not has(win, "Devices") and not has(win, "Accounts this computer"))
    check("...focus in Server address", focused().startswith("Server address"), focused())
    check("...no Cancel (nothing to go back to)", not has(win, "Cancel", UIA.UIA_ButtonControlTypeId))

    # 2. Signing in: visible at once, on the Devices tab.
    set_value(win, "Server address", BASE)
    set_value(win, "Account user name", os.environ["AUDIONET_USER"])
    set_value(win, "Password", os.environ["AUDIONET_PASSWORD"])
    set_value(win, "Name for this computer", NAME)
    press(win, "Sign in")
    check("signed in: visible to the account's other devices at once", wait(is_visible))
    time.sleep(0.5)
    check("the tabs are Devices, Streams, Accounts, Settings",
          [plain(t) for t in tabs(win)] == ["Devices", "Streams", "Accounts", "Settings"], str([plain(t) for t in tabs(win)]))
    check("...on the Devices tab", selected_tab(win) == "Devices", str(selected_tab(win)))
    bring_to_front(win)
    check("...focus in the device tree", uia.GetFocusedElement().CurrentControlType == UIA.UIA_TreeControlTypeId, focused())
    check("...the sign-in fields are gone", not has(win, "Server address"))
    audit(win, "Devices tab")

    # 3. Moving between tabs with the keyboard.
    VK_CONTROL, VK_SHIFT, VK_TAB, VK_NEXT, VK_PRIOR, VK_ESCAPE = 0x11, 0x10, 0x09, 0x22, 0x21, 0x1B
    keys(VK_CONTROL, VK_TAB)
    check("Ctrl+Tab: Streams", selected_tab(win) == "Streams", str(selected_tab(win)))
    check("...only the Streams page shows", has(win, "Streams this computer") and not has(win, "Devices (", UIA.UIA_TextControlTypeId))
    audit(win, "Streams tab")
    keys(VK_CONTROL, VK_TAB)
    check("Ctrl+Tab: Accounts", selected_tab(win) == "Accounts", str(selected_tab(win)))
    check("...focus in the account list (on its selected row)",
          uia.GetFocusedElement().CurrentControlType in (UIA.UIA_ListControlTypeId, UIA.UIA_ListItemControlTypeId)
          and NAME in focused(), focused())
    audit(win, "Accounts tab")
    keys(VK_CONTROL, VK_NEXT)
    check("Ctrl+Page Down: Settings", selected_tab(win) == "Settings", str(selected_tab(win)))
    audit(win, "Settings tab")
    boxes = [plain(c) for c in children(win) if c.CurrentControlType == UIA.UIA_CheckBoxControlTypeId]
    check("Settings has no sharing options", not any("shar" in b.lower() for b in boxes), str(boxes))
    keys(VK_CONTROL, VK_TAB)
    check("Ctrl+Tab wraps round to Devices", selected_tab(win) == "Devices", str(selected_tab(win)))
    keys(VK_CONTROL, VK_SHIFT, VK_TAB)
    check("Ctrl+Shift+Tab: back to Settings", selected_tab(win) == "Settings", str(selected_tab(win)))
    keys(VK_CONTROL, VK_PRIOR)
    check("Ctrl+Page Up: Accounts", selected_tab(win) == "Accounts", str(selected_tab(win)))

    # 4. Visibility, from the Accounts tab.
    row = lambda: next((plain(c) for c in children(win) if c.CurrentControlType == UIA.UIA_ListItemControlTypeId and NAME in plain(c)), "")
    check("the account row says visible", row().endswith(": visible"), row())
    check("the Visible checkbox is on", checked(win, "Visible to my other devices"))
    toggle(win, "Visible to my other devices")
    check("Visible off: the account's other devices no longer see it", wait(lambda: not is_visible()))
    check("...but it is still in the account", is_registered())
    time.sleep(0.5)
    check("...the row says not visible", row().endswith(": not visible"), row())
    check("...the checkbox is off", not checked(win, "Visible to my other devices"))
    toggle(win, "Visible to my other devices")
    check("Visible on again: seen again", wait(is_visible))
    time.sleep(0.5)
    check("...the row says visible", row().endswith(": visible"), row())

    # 5. The status log window: focus lands in the log, and returns.
    print("Status log:\n  " + status_text(win).strip().replace("\r\n", "\n  "))
    logw = desktop_log.open_log(uia, UIA, win)
    time.sleep(0.5)
    f = uia.GetFocusedElement()
    check("Status log window: focus in the log",
          f.CurrentControlType == UIA.UIA_EditControlTypeId and plain(f).startswith("Status log"), repr(f.CurrentName))
    WM_KEYDOWN = 0x0100

    def key(vk):
        user32.PostMessageW(uia.GetFocusedElement().CurrentNativeWindowHandle, WM_KEYDOWN, vk, 0)
        time.sleep(0.4)
        return focused()
    before = desktop_log.text_of(uia, UIA, logw)
    check("Tab from the log moves to Copy", key(VK_TAB) == "Copy")
    check("...then to Close", key(VK_TAB) == "Close")
    check("...and back to the log", key(VK_TAB).startswith("Status log"))
    check("...without changing the log", desktop_log.text_of(uia, UIA, logw) == before)
    key(VK_ESCAPE)
    check("Escape in the log closes the window", not desktop_log.window(uia, UIA, proc.pid, win))
    time.sleep(0.5)
    check("...and focus returns to its button", focused().startswith("Status log"), focused())
    check("the log says when the computer was hidden",
          "no longer visible in" in status_text(win), status_text(win)[-300:])

    # 6. Exit while visible asks first; then the computer is not visible.
    press(win, "Exit")
    check("exiting while visible asks first", answer(proc.pid, win, "Yes"))
    proc.wait(10)
    check("after exiting: not visible", wait(lambda: not is_visible()))
    check("...still in the account", is_registered())

    # 7. Opening again: visible again, as it was. Hide it, restart: hidden.
    proc = subprocess.Popen([EXE], env=env)
    win = find_window(proc.pid)
    check("opened again: visible, as it was", wait(is_visible, 30))
    check("...on the Devices tab", selected_tab(win) == "Devices", str(selected_tab(win)))
    choose_tab(win, "Accounts")
    toggle(win, "Visible to my other devices")
    wait(lambda: not is_visible())
    press(win, "Exit")  # not visible: no question
    proc.wait(10)
    proc = subprocess.Popen([EXE], env=env)
    win = find_window(proc.pid)
    time.sleep(3)
    check("opened again after hiding: still hidden", not is_visible())
    choose_tab(win, "Accounts")
    check("...the checkbox is off", not checked(win, "Visible to my other devices"))

    # 8. Sign out: asks, the device leaves the account, sign-in page again.
    press(win, "Sign out of the selected account")
    check("sign-out asks first", answer(proc.pid, win, "Yes"))
    check("signed out: the device is removed from the account", wait(lambda: not is_registered()))
    time.sleep(0.5)
    check("...back on the sign-in page (no tabs)", not tabs(win) and has(win, "Server address"))
    check("the log says so", "was removed from that account" in status_text(win), status_text(win)[-200:])
finally:
    if proc.poll() is None:
        proc.terminate()
        proc.wait(10)
    clear_test_prefs()
    remove_test_devices()
print("DESKTOP UIA:", "PASS" if ok else "FAIL")
sys.exit(0 if ok else 1)
