"""Checks the desktop app's system tray behavior through UI Automation and
Win32, the way a keyboard or screen-reader user would reach it:

1. Before signing in, the tray menu's visibility item and Settings… are
   unavailable (Settings is a tab of the signed-in window).
2. Signed in, the Settings tab has the tray options, named, with the
   defaults (keep running in the tray on, start minimized off) and no
   sharing options.
3. Closing the window keeps AudioNet running in the tray (icon present);
   starting it again shows the existing window instead of a second copy;
   Enter on the tray icon (NIN_KEYSELECT) opens the window.
4. The tray menu has named items (Open AudioNet, Hide this computer from
   my other devices, Settings…, Exit); Exit asks first while this computer
   is visible, then quits.
5. --background and "Start minimized to the system tray" start hidden,
   without taking the focus from the program the user is in.
6. After a tray menu choice that does not open the window, the focus goes
   back to where it was (a hidden window must never hold the focus: screen
   readers would read it).
7. Settings… in the tray menu opens the window on its Settings tab, focus
   on the first option.
8. With "keep running in the system tray" off, closing the window exits
   (after asking, since this computer is visible).

The app runs under AUDIONET_TEST_PROFILE, with its own window class and
preferences key (HKCU\\Software\\AudioNet-Test-autotest, removed
afterwards), and APPDATA pointed at a temporary folder, so a copy of
AudioNet the user is running and their settings are never touched. It
signs in to a server as a temporary device, removed afterwards.
Environment: AUDIONET_URL, AUDIONET_USER, AUDIONET_PASSWORD,
AUDIONET_DESKTOP (path to audionet-desktop.exe).
Requires: pip install comtypes requests
"""
import ctypes, os, subprocess, sys, tempfile, time, winreg
from ctypes import wintypes
import requests
import comtypes.client

comtypes.client.GetModule("UIAutomationCore.dll")
from comtypes.gen import UIAutomationClient as UIA  # noqa: E402

BASE = os.environ["AUDIONET_URL"].rstrip("/")
EXE = os.environ.get("AUDIONET_DESKTOP", os.path.join(os.path.dirname(__file__), "..", "..", "target", "release", "audionet-desktop.exe"))
NAME = "Desktop tray test"
uia = comtypes.client.CreateObject(UIA.CUIAutomation, interface=UIA.IUIAutomation)
user32 = ctypes.WinDLL("user32", use_last_error=True)
shell32 = ctypes.WinDLL("shell32")
user32.FindWindowW.restype = wintypes.HWND
user32.FindWindowW.argtypes = [wintypes.LPCWSTR, wintypes.LPCWSTR]
user32.PostMessageW.argtypes = [wintypes.HWND, wintypes.UINT, wintypes.WPARAM, wintypes.LPARAM]
user32.IsWindowVisible.argtypes = [wintypes.HWND]

WM_CLOSE = 0x0010
WM_APP_TRAY = 0x8000 + 10
NIN_KEYSELECT = 0x0401
WM_CONTEXTMENU = 0x007B
PROFILE = "autotest"
CLASS = f"AudioNetDesktop-{PROFILE}"
KEY = rf"Software\AudioNet-Test-{PROFILE}"
MENU = ("Open AudioNet", "Hide this computer from my other devices", "Make this computer visible to my other devices",
        "Settings…", "Exit")
APPDATA = tempfile.mkdtemp(prefix="audionet-tray-")
results = []
user32.GetForegroundWindow.restype = wintypes.HWND
user32.GetWindowThreadProcessId.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.DWORD)]

s = requests.Session()
s.post(f"{BASE}/api/v1/login", json={"username": os.environ["AUDIONET_USER"], "password": os.environ["AUDIONET_PASSWORD"]},
       headers={"Origin": BASE}).raise_for_status()


def remove_test_devices():
    for n in s.get(f"{BASE}/api/v1/nodes", headers={"Origin": BASE}).json()["nodes"]:
        if n["name"] == NAME:
            s.delete(f"{BASE}/api/v1/nodes/{n['node_id']}", headers={"Origin": BASE})


def app_has_focus(pid):
    owner = wintypes.DWORD()
    user32.GetWindowThreadProcessId(user32.GetForegroundWindow(), ctypes.byref(owner))
    return owner.value == pid


class NOTIFYICONIDENTIFIER(ctypes.Structure):
    _fields_ = [("cbSize", wintypes.DWORD), ("hWnd", wintypes.HWND), ("uID", wintypes.UINT),
                ("guidItem", ctypes.c_byte * 16)]


def check(name, ok, detail=""):
    results.append((name, ok))
    print(f"{name}: {'PASS' if ok else 'FAIL'}{'' if ok or not detail else '  (' + detail + ')'}", flush=True)


def app_window():
    return user32.FindWindowW(CLASS, None)


def wait_for(pred, timeout=10):
    end = time.time() + timeout
    while time.time() < end:
        try:
            if pred():
                return True
        except Exception:
            pass
        time.sleep(0.2)
    return False


def tray_icon_present(hwnd):
    ident = NOTIFYICONIDENTIFIER(ctypes.sizeof(NOTIFYICONIDENTIFIER), hwnd, 1)
    rect = wintypes.RECT()
    return shell32.Shell_NotifyIconGetRect(ctypes.byref(ident), ctypes.byref(rect)) == 0


def launch(*args):
    env = dict(os.environ, APPDATA=APPDATA, AUDIONET_TEST_PROFILE=PROFILE)
    return subprocess.Popen([EXE, *args], env=env)


def children(el):
    arr = el.FindAll(UIA.TreeScope_Descendants, uia.CreateTrueCondition())
    return [arr.GetElement(i) for i in range(arr.Length)]


def plain(el):
    return (el.CurrentName or "").replace("&", "")


def find(win, prefix, kind):
    for c in children(win):
        if c.CurrentControlType == kind and plain(c).startswith(prefix):
            return c
    raise KeyError(prefix)


def toggle_state(el):
    return el.GetCurrentPattern(UIA.UIA_TogglePatternId).QueryInterface(UIA.IUIAutomationTogglePattern).CurrentToggleState


def toggle(el):
    el.GetCurrentPattern(UIA.UIA_TogglePatternId).QueryInterface(UIA.IUIAutomationTogglePattern).Toggle()


def invoke(el):
    el.GetCurrentPattern(UIA.UIA_InvokePatternId).QueryInterface(UIA.IUIAutomationInvokePattern).Invoke()


def win():
    return uia.ElementFromHandle(app_window())


def selected_tab():
    for t in children(win()):
        if t.CurrentControlType == UIA.UIA_TabItemControlTypeId:
            p = t.GetCurrentPattern(UIA.UIA_SelectionItemPatternId).QueryInterface(UIA.IUIAutomationSelectionItemPattern)
            if p.CurrentIsSelected:
                return plain(t)
    return None


def choose_tab(name):
    t = find(win(), name, UIA.UIA_TabItemControlTypeId)
    t.GetCurrentPattern(UIA.UIA_SelectionItemPatternId).QueryInterface(UIA.IUIAutomationSelectionItemPattern).Select()
    time.sleep(0.4)


def menu_items():
    cond = uia.CreatePropertyCondition(UIA.UIA_ControlTypePropertyId, UIA.UIA_MenuItemControlTypeId)
    arr = uia.GetRootElement().FindAll(UIA.TreeScope_Descendants, cond)
    items = [arr.GetElement(i) for i in range(arr.Length)]
    return [i for i in items if plain(i) in MENU]


def open_menu(hwnd):
    user32.PostMessageW(hwnd, WM_APP_TRAY, (200 << 16) | 200, WM_CONTEXTMENU | (1 << 16))
    wait_for(lambda: len(menu_items()) >= 4)
    return {plain(i): i for i in menu_items()}


def close_menu():
    ctypes.windll.user32.keybd_event(0x1B, 0, 0, 0)
    ctypes.windll.user32.keybd_event(0x1B, 0, 2, 0)
    time.sleep(1)


def answer(pid, button, timeout=10):
    cond = uia.CreateAndCondition(uia.CreatePropertyCondition(UIA.UIA_ProcessIdPropertyId, pid),
                                  uia.CreatePropertyCondition(UIA.UIA_ClassNamePropertyId, "#32770"))
    end = time.time() + timeout
    while time.time() < end:
        # An owned dialog is listed under its owner, or at the top level.
        box = uia.GetRootElement().FindFirst(UIA.TreeScope_Children, cond)
        if not box and app_window():
            box = win().FindFirst(UIA.TreeScope_Children, cond)
        if box:
            invoke(find(box, button, UIA.UIA_ButtonControlTypeId))
            return True
        time.sleep(0.2)
    return False


def read_prefs():
    try:
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER, KEY) as k:
            return {n: winreg.QueryValueEx(k, n)[0] for n in ("CloseToTray", "StartInTray") if _has(k, n)}
    except FileNotFoundError:
        return None


def _has(k, name):
    try:
        winreg.QueryValueEx(k, name)
        return True
    except FileNotFoundError:
        return False


def clear_prefs():
    try:
        winreg.DeleteKey(winreg.HKEY_CURRENT_USER, KEY)
    except FileNotFoundError:
        pass


if app_window():
    raise SystemExit("A previous test instance is still running; exit it first.")
procs = []
try:
    remove_test_devices()
    clear_prefs()  # defaults for the test
    p = launch()
    procs.append(p)
    check("window opens", wait_for(lambda: app_window() and user32.IsWindowVisible(app_window())))
    hwnd = app_window()
    check("tray icon present", wait_for(lambda: tray_icon_present(hwnd)))

    # 1. Not signed in yet.
    items = open_menu(hwnd)
    check("before signing in: the visibility item and Settings… are unavailable",
          "Make this computer visible to my other devices" in items and not items["Make this computer visible to my other devices"].CurrentIsEnabled
          and "Settings…" in items and not items["Settings…"].CurrentIsEnabled, str(list(items)))
    close_menu()

    # 2. Sign in; the Settings tab.
    for label, value in [("Server address", BASE), ("Account user name", os.environ["AUDIONET_USER"]),
                         ("Password", os.environ["AUDIONET_PASSWORD"]), ("Name for this computer", NAME)]:
        find(win(), label, UIA.UIA_EditControlTypeId).GetCurrentPattern(UIA.UIA_ValuePatternId) \
            .QueryInterface(UIA.IUIAutomationValuePattern).SetValue(value)
    invoke(find(win(), "Sign in", UIA.UIA_ButtonControlTypeId))
    check("signs in (the tabs appear)", wait_for(lambda: selected_tab() == "Devices", 30), str(selected_tab()))
    choose_tab("Settings")
    keep = find(win(), "When I close the window, keep AudioNet running in the system tray", UIA.UIA_CheckBoxControlTypeId)
    start_min = find(win(), "Start minimized to the system tray", UIA.UIA_CheckBoxControlTypeId)
    check("Settings tab: tray options named, keep-running on, start-minimized off",
          toggle_state(keep) == 1 and toggle_state(start_min) == 0)
    names = [plain(c) for c in children(win())]
    check("no sharing options", not any("shar" in n.lower() for n in names))
    choose_tab("Devices")

    # 3. Closing keeps it in the tray; a second start shows it; Enter on the icon.
    user32.PostMessageW(hwnd, WM_CLOSE, 0, 0)
    check("closing hides the window", wait_for(lambda: not user32.IsWindowVisible(hwnd)))
    time.sleep(1)
    check("still running in the tray after close", p.poll() is None and tray_icon_present(hwnd))
    second = launch()
    check("second start exits", wait_for(lambda: second.poll() is not None))
    check("second start shows the existing window", wait_for(lambda: user32.IsWindowVisible(hwnd)))
    user32.PostMessageW(hwnd, WM_CLOSE, 0, 0)
    wait_for(lambda: not user32.IsWindowVisible(hwnd))
    user32.PostMessageW(hwnd, WM_APP_TRAY, 0, NIN_KEYSELECT | (1 << 16))
    check("Enter on the tray icon opens the window", wait_for(lambda: user32.IsWindowVisible(hwnd)))

    # 4. The tray menu, signed in and visible; Exit asks first.
    items = open_menu(hwnd)
    check("tray menu items are named",
          {"Open AudioNet", "Hide this computer from my other devices", "Settings…", "Exit"} <= set(items), str(list(items)))
    check("...and available", all(i.CurrentIsEnabled for i in items.values()))
    invoke(items["Exit"])
    check("Exit while visible asks first", answer(p.pid, "Yes"))
    check("...then quits cleanly (exit code 0)", wait_for(lambda: p.poll() is not None) and p.returncode == 0)
    check("tray icon removed on exit", wait_for(lambda: not tray_icon_present(hwnd), 5))

    # 5. In the tray at sign-in; the focus stays where it was.
    p = launch("--background")
    procs.append(p)
    check("--background starts in the tray",
          wait_for(lambda: app_window() and tray_icon_present(app_window())) and not user32.IsWindowVisible(app_window()))
    time.sleep(1)
    check("starting in the tray does not take the focus", not app_has_focus(p.pid))
    # 6. A tray menu choice that does not open the window gives the focus back.
    open_menu(app_window())
    close_menu()
    check("closing the tray menu gives the focus back", not app_has_focus(p.pid) and not user32.IsWindowVisible(app_window()))
    # 7. Settings… from the tray menu: the window, on its Settings tab.
    items = open_menu(app_window())
    invoke(items["Settings…"])
    check("Settings… opens the window on the Settings tab",
          wait_for(lambda: user32.IsWindowVisible(app_window()) and selected_tab() == "Settings"), str(selected_tab()))
    time.sleep(0.5)
    check("...focus on the first option", plain(uia.GetFocusedElement()).startswith("Start AudioNet automatically"),
          plain(uia.GetFocusedElement()))
    toggle(find(win(), "Start minimized to the system tray", UIA.UIA_CheckBoxControlTypeId))
    toggle(find(win(), "When I close the window, keep AudioNet running in the system tray", UIA.UIA_CheckBoxControlTypeId))
    saved_ok = lambda: (read_prefs() or {}) == {"StartInTray": 1, "CloseToTray": 0}
    check("preferences saved", wait_for(saved_ok, 5), str(read_prefs()))
    # 8. Keep-running off: closing exits (asking first: it is visible).
    user32.PostMessageW(app_window(), WM_CLOSE, 0, 0)
    check("with keep-running off, closing asks first", answer(p.pid, "Yes"))
    check("...then exits cleanly", wait_for(lambda: p.poll() is not None) and p.returncode == 0)

    p = launch()
    procs.append(p)
    check("Start minimized to the system tray starts hidden",
          wait_for(lambda: app_window() and tray_icon_present(app_window())) and not user32.IsWindowVisible(app_window()))
    time.sleep(1)
    check("...and does not take the focus", not app_has_focus(p.pid))
finally:
    for q in procs:
        if q.poll() is None:
            q.terminate()
            q.wait(10)
    clear_prefs()
    remove_test_devices()
failed = [n for n, ok in results if not ok]
print(f"{len(results) - len(failed)} of {len(results)} checks passed")
sys.exit(1 if failed else 0)
