"""Checks the desktop app's system tray behavior through UI Automation and
Win32, the way a keyboard or screen-reader user would reach it:

1. Closing the window keeps AudioNet running in the tray (icon present).
2. Starting it again shows the existing window instead of a second copy.
3. Enter on the tray icon (NIN_KEYSELECT) opens the window.
4. The tray menu has named items (Open AudioNet, Start sharing, Exit) and
   Exit quits.
5. --background and "Start minimized to the system tray" start hidden,
   without taking the focus from the program the user is in.
6. After a tray menu choice that does not open the window, the focus goes
   back to where it was (a hidden window must never hold the focus: screen
   readers would read it).
7. With "keep running in the system tray" off, closing the window exits.
8. The options live in a Settings window (from the main window's
   Settings button or the tray menu): named, focus on the first option,
   Escape closes it and focus returns to the Settings button.

No server is needed. The app runs under AUDIONET_TEST_PROFILE, with its own
window class and preferences key (HKCU\\Software\\AudioNet-Test-autotest,
removed afterwards), and APPDATA pointed at a temporary folder, so a copy
of AudioNet the user is running and their settings are never touched.
Environment: AUDIONET_DESKTOP (path to audionet-desktop.exe).
Requires: pip install comtypes
"""
import ctypes, os, subprocess, sys, tempfile, time, winreg
from ctypes import wintypes
import comtypes.client

comtypes.client.GetModule("UIAutomationCore.dll")
from comtypes.gen import UIAutomationClient as UIA  # noqa: E402

EXE = os.environ.get("AUDIONET_DESKTOP", os.path.join(os.path.dirname(__file__), "..", "..", "target", "release", "audionet-desktop.exe"))
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
SETTINGS_CLASS = f"{CLASS}-Settings"
WM_KEYDOWN = 0x0100
VK_ESCAPE = 0x1B
KEY = rf"Software\AudioNet-Test-{PROFILE}"
results = []
user32.GetForegroundWindow.restype = wintypes.HWND
user32.GetWindowThreadProcessId.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.DWORD)]

def app_has_focus(pid):
    owner = wintypes.DWORD()
    user32.GetWindowThreadProcessId(user32.GetForegroundWindow(), ctypes.byref(owner))
    return owner.value == pid

class NOTIFYICONIDENTIFIER(ctypes.Structure):
    _fields_ = [("cbSize", wintypes.DWORD), ("hWnd", wintypes.HWND), ("uID", wintypes.UINT),
                ("guidItem", ctypes.c_byte * 16)]

def check(name, ok):
    results.append((name, ok))
    print(f"{name}: {'PASS' if ok else 'FAIL'}", flush=True)

def app_window():
    return user32.FindWindowW(CLASS, None)

def wait_for(pred, timeout=10):
    end = time.time() + timeout
    while time.time() < end:
        if pred():
            return True
        time.sleep(0.2)
    return False

def tray_icon_present(hwnd):
    ident = NOTIFYICONIDENTIFIER(ctypes.sizeof(NOTIFYICONIDENTIFIER), hwnd, 1)
    rect = wintypes.RECT()
    return shell32.Shell_NotifyIconGetRect(ctypes.byref(ident), ctypes.byref(rect)) == 0

def launch(*args):
    env = dict(os.environ, APPDATA=tempfile.mkdtemp(prefix="audionet-tray-"), AUDIONET_TEST_PROFILE=PROFILE)
    return subprocess.Popen([EXE, *args], env=env)

def children(el):
    arr = el.FindAll(UIA.TreeScope_Descendants, uia.CreateTrueCondition())
    return [arr.GetElement(i) for i in range(arr.Length)]

def checkbox(win, prefix):
    for c in children(win):
        if c.CurrentControlType == UIA.UIA_CheckBoxControlTypeId and (c.CurrentName or "").replace("&", "").startswith(prefix):
            return c
    raise KeyError(prefix)

def toggle_state(el):
    return el.GetCurrentPattern(UIA.UIA_TogglePatternId).QueryInterface(UIA.IUIAutomationTogglePattern).CurrentToggleState

def toggle(el):
    el.GetCurrentPattern(UIA.UIA_TogglePatternId).QueryInterface(UIA.IUIAutomationTogglePattern).Toggle()

def uia_window(hwnd):
    return uia.ElementFromHandle(hwnd)

def menu_items():
    cond = uia.CreatePropertyCondition(UIA.UIA_ControlTypePropertyId, UIA.UIA_MenuItemControlTypeId)
    arr = uia.GetRootElement().FindAll(UIA.TreeScope_Descendants, cond)
    items = [arr.GetElement(i) for i in range(arr.Length)]
    return [i for i in items if (i.CurrentName or "").replace("&", "") in ("Open AudioNet", "Start sharing", "Stop sharing", "Settings…", "Exit")]

def settings_window():
    return user32.FindWindowW(SETTINGS_CLASS, None)

def invoke(el):
    el.GetCurrentPattern(UIA.UIA_InvokePatternId).QueryInterface(UIA.IUIAutomationInvokePattern).Invoke()

def button(win, name):
    for c in children(win):
        if c.CurrentControlType == UIA.UIA_ButtonControlTypeId and (c.CurrentName or "").replace("&", "") == name:
            return c
    raise KeyError(name)

def open_settings(main_hwnd):
    """Opens Settings with the main window's button; returns its UIA element."""
    invoke(button(uia_window(main_hwnd), "Settings…"))
    wait_for(lambda: settings_window() and user32.IsWindowVisible(settings_window()))
    return uia_window(settings_window())

def close_settings_with_escape(settings):
    """Escape on the focused control, as a keyboard user would press it."""
    focused = uia.GetFocusedElement()
    user32.PostMessageW(focused.CurrentNativeWindowHandle, WM_KEYDOWN, VK_ESCAPE, 0)
    return wait_for(lambda: not settings_window(), 5)

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
    clear_prefs()  # defaults for the test
    p = launch()
    procs.append(p)
    check("window opens", wait_for(lambda: app_window() and user32.IsWindowVisible(app_window())))
    hwnd = app_window()
    settings = open_settings(hwnd)
    check("the Settings button opens a window named AudioNet Settings",
          settings_window() and settings.CurrentName == "AudioNet Settings")
    focused = uia.GetFocusedElement()
    check("focus starts on the first option",
          (focused.CurrentName or "").replace("&", "").startswith("Start AudioNet automatically"))
    keep = checkbox(settings, "When I close the window, keep AudioNet running in the system tray")
    start_min = checkbox(settings, "Start minimized to the system tray")
    check("tray checkboxes are named, keep-running on, start-minimized off",
          toggle_state(keep) == 1 and toggle_state(start_min) == 0)
    everything = settings.FindAll(UIA.TreeScope_Descendants, uia.CreateTrueCondition())
    names = [(everything.GetElement(i).CurrentName or "").replace("&", "") for i in range(everything.Length)]
    check("no Start sharing automatically (each account keeps its own choice)",
          not any(n.startswith("Start sharing") for n in names))
    check("Escape closes Settings", close_settings_with_escape(settings))
    time.sleep(0.5)
    check("focus returns to the Settings button",
          (uia.GetFocusedElement().CurrentName or "").replace("&", "") == "Settings…")
    check("tray icon present", wait_for(lambda: tray_icon_present(hwnd)))

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

    # The tray menu: find it through UI Automation, check names, choose Exit.
    user32.PostMessageW(hwnd, WM_APP_TRAY, (200 << 16) | 200, WM_CONTEXTMENU | (1 << 16))
    wait_for(lambda: len(menu_items()) >= 3)
    items = {(i.CurrentName or "").replace("&", ""): i for i in menu_items()}
    check("tray menu items are named", {"Open AudioNet", "Start sharing", "Settings…", "Exit"} <= set(items))
    check("Start sharing is unavailable while not signed in",
          "Start sharing" in items and not items["Start sharing"].CurrentIsEnabled)
    items["Exit"].GetCurrentPattern(UIA.UIA_InvokePatternId).QueryInterface(UIA.IUIAutomationInvokePattern).Invoke()
    check("Exit from the tray menu quits cleanly (exit code 0)", wait_for(lambda: p.poll() is not None) and p.returncode == 0)
    check("tray icon removed on exit", wait_for(lambda: not tray_icon_present(hwnd), 5))

    p = launch("--background")
    procs.append(p)
    check("--background starts in the tray",
          wait_for(lambda: app_window() and tray_icon_present(app_window())) and not user32.IsWindowVisible(app_window()))
    time.sleep(1)
    check("starting in the tray does not take the focus", not app_has_focus(p.pid))
    # A tray menu choice that does not open the window gives the focus back.
    user32.PostMessageW(app_window(), WM_APP_TRAY, (200 << 16) | 200, WM_CONTEXTMENU | (1 << 16))
    wait_for(lambda: len(menu_items()) >= 3)
    ctypes.windll.user32.keybd_event(0x1B, 0, 0, 0)
    ctypes.windll.user32.keybd_event(0x1B, 0, 2, 0)
    time.sleep(1)
    check("closing the tray menu gives the focus back", not app_has_focus(p.pid) and not user32.IsWindowVisible(app_window()))
    # Settings from the tray menu, while the window is hidden.
    user32.PostMessageW(app_window(), WM_APP_TRAY, (200 << 16) | 200, WM_CONTEXTMENU | (1 << 16))
    wait_for(lambda: len(menu_items()) >= 4)
    items = {(i.CurrentName or "").replace("&", ""): i for i in menu_items()}
    invoke(items["Settings…"])
    check("Settings opens from the tray menu while the window is hidden",
          wait_for(lambda: settings_window() and user32.IsWindowVisible(settings_window()))
          and not user32.IsWindowVisible(app_window()))
    settings = uia_window(settings_window())
    toggle(checkbox(settings, "Start minimized to the system tray"))
    toggle(checkbox(settings, "When I close the window, keep AudioNet running in the system tray"))
    close_settings_with_escape(settings)
    user32.PostMessageW(app_window(), WM_APP_TRAY, 0, NIN_KEYSELECT | (1 << 16))
    wait_for(lambda: user32.IsWindowVisible(app_window()))
    saved_ok = lambda: (read_prefs() or {}) == {"StartInTray": 1, "CloseToTray": 0}
    check("preferences saved", wait_for(saved_ok, 5))
    if not saved_ok():
        print("  preferences read back:", read_prefs())
    hwnd = app_window()
    user32.PostMessageW(hwnd, WM_CLOSE, 0, 0)
    check("with keep-running off, closing the window exits cleanly", wait_for(lambda: p.poll() is not None) and p.returncode == 0)

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
failed = [n for n, ok in results if not ok]
print(f"{len(results) - len(failed)} of {len(results)} checks passed")
sys.exit(1 if failed else 0)
