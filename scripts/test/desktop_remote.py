"""End-to-end test of the desktop app as a remote, through UI Automation
(the way a keyboard or screen-reader user drives it) and a live server:

1. A temporary source device (the audionet command-line agent) shares
   what plays on SOURCE_OUTPUT, where a 997 Hz tone plays.
2. The desktop app signs in with the account password and starts. In the
   "Your devices" tree that device is one collapsed item; the test expands
   it, then "Sounds to listen to", selects the sound, chooses a local
   output (PLAYBACK_OUTPUT) and presses Enter on the tree (listen).
3. What arrives on PLAYBACK_OUTPUT is recorded (WASAPI loopback) and must
   be the tone at the right level; then "Stop the selected stream" must
   stop it.

Use outputs that do not feed each other (check with a baseline first).
The app runs under AUDIONET_TEST_PROFILE (never touches a running
AudioNet or its settings); both temporary devices are removed afterwards.

Environment: AUDIONET_URL, AUDIONET_USER, AUDIONET_PASSWORD, optionally
SOURCE_OUTPUT (default "UniMic Output"), PLAYBACK_OUTPUT (default
"Speakers (Yeti Classic)"). Requires: pip install comtypes requests numpy
"""
import ctypes, os, subprocess, sys, tempfile, time, winreg
from ctypes import wintypes
import requests
import comtypes.client

comtypes.client.GetModule("UIAutomationCore.dll")
from comtypes.gen import UIAutomationClient as UIA  # noqa: E402
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import desktop_log  # noqa: E402
import second_account  # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
BASE = os.environ["AUDIONET_URL"].rstrip("/")
USER, PASSWORD = os.environ["AUDIONET_USER"], os.environ["AUDIONET_PASSWORD"]
SOURCE_OUTPUT = os.environ.get("SOURCE_OUTPUT", "UniMic Output")
PLAYBACK_OUTPUT = os.environ.get("PLAYBACK_OUTPUT", "Speakers (Yeti Classic)")
CLI = os.path.join(ROOT, "target", "release", "audionet.exe")
EXE = os.path.join(ROOT, "target", "release", "audionet-desktop.exe")
PROFILE = "remotetest"
SOURCE_NAME, APP_NAME = "Remote test source", "Desktop remote test"

uia = comtypes.client.CreateObject(UIA.CUIAutomation, interface=UIA.IUIAutomation)
u = ctypes.WinDLL("user32")
u.FindWindowW.restype = wintypes.HWND
u.FindWindowW.argtypes = [wintypes.LPCWSTR, wintypes.LPCWSTR]
u.SendMessageW.argtypes = [wintypes.HWND, wintypes.UINT, wintypes.WPARAM, wintypes.LPARAM]
u.SendMessageW.restype = ctypes.c_ssize_t
u.PostMessageW.argtypes = [wintypes.HWND, wintypes.UINT, wintypes.WPARAM, wintypes.LPARAM]
work = tempfile.mkdtemp(prefix="audionet-remote-")
results, procs = [], []

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
        time.sleep(0.3)
    return False

def win():
    return uia.ElementFromHandle(u.FindWindowW(f"AudioNetDesktop-{PROFILE}", None))

def all_elements():
    arr = win().FindAll(UIA.TreeScope_Descendants, uia.CreateTrueCondition())
    return [arr.GetElement(i) for i in range(arr.Length)]

def named(prefix, ctype=None):
    for e in all_elements():
        if (e.CurrentName or "").replace("&", "").startswith(prefix) and (ctype is None or e.CurrentControlType == ctype):
            return e
    raise KeyError(prefix)

def set_edit(label, text):
    e = named(label, UIA.UIA_EditControlTypeId)
    e.GetCurrentPattern(UIA.UIA_ValuePatternId).QueryInterface(UIA.IUIAutomationValuePattern).SetValue(text)

def press(name):
    named(name, UIA.UIA_ButtonControlTypeId).GetCurrentPattern(UIA.UIA_InvokePatternId).QueryInterface(UIA.IUIAutomationInvokePattern).Invoke()

def status_log():
    return desktop_log.read(uia, UIA, win())

def items(container_label, ctype):
    box = named(container_label, ctype)
    arr = box.FindAll(UIA.TreeScope_Descendants, uia.CreatePropertyCondition(UIA.UIA_ControlTypePropertyId, UIA.UIA_ListItemControlTypeId))
    return [arr.GetElement(i) for i in range(arr.Length)]

def choose(container_label, ctype, text):
    """Selects the item containing `text` the way UI Automation clients do
    (a drop-down list is opened first: its items exist only while open)."""
    box = named(container_label, ctype)
    combo = ctype == UIA.UIA_ComboBoxControlTypeId
    if combo:
        box.GetCurrentPattern(UIA.UIA_ExpandCollapsePatternId).QueryInterface(UIA.IUIAutomationExpandCollapsePattern).Expand()
        time.sleep(0.3)
    try:
        for it in items(container_label, ctype):
            if text in (it.CurrentName or ""):
                it.GetCurrentPattern(UIA.UIA_SelectionItemPatternId).QueryInterface(UIA.IUIAutomationSelectionItemPattern).Select()
                return True
        return False
    finally:
        if combo:
            box.GetCurrentPattern(UIA.UIA_ExpandCollapsePatternId).QueryInterface(UIA.IUIAutomationExpandCollapsePattern).Collapse()

def choose_by_keyboard(label, text, tries=40):
    """Presses Home, then Down, in a drop-down list until its value contains
    `text`, the way a keyboard or screen-reader user chooses. The keys go to
    the control's own window (never to whatever window is in front)."""
    box = named(label, UIA.UIA_ComboBoxControlTypeId)
    hwnd = box.CurrentNativeWindowHandle
    WM_KEYDOWN, VK_HOME, VK_DOWN = 0x0100, 0x24, 0x28
    box.SetFocus()  # lets the app bring the list in line with the device
    time.sleep(0.3)
    u.SendMessageW(hwnd, WM_KEYDOWN, VK_HOME, 0)
    for _ in range(tries):
        if text in combo_value(label):
            return True
        u.SendMessageW(hwnd, WM_KEYDOWN, VK_DOWN, 0)
        time.sleep(0.05)
    return text in combo_value(label)

def tree_items(parent):
    arr = parent.FindAll(UIA.TreeScope_Descendants, uia.CreatePropertyCondition(UIA.UIA_ControlTypePropertyId, UIA.UIA_TreeItemControlTypeId))
    return [arr.GetElement(i) for i in range(arr.Length)]

def tree_item(parent, prefix):
    for it in tree_items(parent):
        if (it.CurrentName or "").startswith(prefix):
            return it
    raise KeyError(prefix)

def expand_state(el):
    """0 collapsed, 1 expanded, 3 no children (UI Automation's values)."""
    return el.GetCurrentPattern(UIA.UIA_ExpandCollapsePatternId).QueryInterface(UIA.IUIAutomationExpandCollapsePattern).CurrentExpandCollapseState

def expand(el):
    el.GetCurrentPattern(UIA.UIA_ExpandCollapsePatternId).QueryInterface(UIA.IUIAutomationExpandCollapsePattern).Expand()
    time.sleep(0.3)

def select(el):
    el.GetCurrentPattern(UIA.UIA_SelectionItemPatternId).QueryInterface(UIA.IUIAutomationSelectionItemPattern).Select()
    time.sleep(0.2)

def tree_top():
    """The top-level items of the device tree."""
    tree = named("Your devices", UIA.UIA_TreeControlTypeId)
    arr = tree.FindAll(UIA.TreeScope_Children, uia.CreatePropertyCondition(UIA.UIA_ControlTypePropertyId,
                                                                          UIA.UIA_TreeItemControlTypeId))
    return [arr.GetElement(i).CurrentName or "" for i in range(arr.Length)]

def answer_dialog(button, timeout=10):
    """Presses `button` in the app's message box (a standard dialog)."""
    pid = win().CurrentProcessId
    cond = uia.CreateAndCondition(uia.CreatePropertyCondition(UIA.UIA_ProcessIdPropertyId, pid),
                                  uia.CreatePropertyCondition(UIA.UIA_ClassNamePropertyId, "#32770"))
    end = time.time() + timeout
    while time.time() < end:
        box = uia.GetRootElement().FindFirst(UIA.TreeScope_Children, cond) or win().FindFirst(UIA.TreeScope_Children, cond)
        if box:
            arr = box.FindAll(UIA.TreeScope_Descendants, uia.CreatePropertyCondition(UIA.UIA_ControlTypePropertyId,
                                                                                    UIA.UIA_ButtonControlTypeId))
            for i in range(arr.Length):
                b = arr.GetElement(i)
                if (b.CurrentName or "").replace("&", "") == button:
                    b.GetCurrentPattern(UIA.UIA_InvokePatternId).QueryInterface(UIA.IUIAutomationInvokePattern).Invoke()
                    return True
        time.sleep(0.2)
    return False

def combo_value(label):
    box = named(label, UIA.UIA_ComboBoxControlTypeId)
    return box.GetCurrentPattern(UIA.UIA_ValuePatternId).QueryInterface(UIA.IUIAutomationValuePattern).CurrentValue

def output_number(name):
    out = subprocess.run([CLI, "list"], capture_output=True, text=True, encoding="utf-8").stdout
    for line in out.splitlines():
        if line.startswith("Output device") and name in line:
            return line.split()[2]
    raise SystemExit(f"no output device named {name}")

def api():
    s = requests.Session()
    token = s.post(f"{BASE}/api/v1/login", json={"username": USER, "password": PASSWORD}).json()["token"]
    s.headers["Authorization"] = f"Bearer {token}"
    return s

try:
    s = api()
    for n in s.get(f"{BASE}/api/v1/nodes").json()["nodes"]:
        if n["name"] in (SOURCE_NAME, APP_NAME):
            s.delete(f"{BASE}/api/v1/nodes/{n['node_id']}")
    try:
        winreg.DeleteKey(winreg.HKEY_CURRENT_USER, rf"Software\AudioNet-Test-{PROFILE}")
    except FileNotFoundError:
        pass

    # 0. Baseline: the same -15.05 dBFS tone played locally on the playback
    #    output (devices may change the level, e.g. a volume setting).
    out_no = output_number(PLAYBACK_OUTPUT)
    tone = subprocess.Popen([CLI, "tone-test", "--output", out_no, "--seconds", "5"], stdout=subprocess.DEVNULL)
    time.sleep(1.5)
    base_wav = os.path.join(work, "baseline.wav")
    subprocess.run([CLI, "capture-test", "--loopback", out_no, "--seconds", "2", "--wav", base_wav], capture_output=True)
    tone.wait()
    base = subprocess.run([sys.executable, os.path.join(HERE, "verify_wav.py"), base_wav], capture_output=True, text=True).stdout
    baseline_rms = next((float(l.split("rms=")[1].split()[0]) for l in base.splitlines() if "rms=" in l), -200.0)
    print(f"  baseline on {PLAYBACK_OUTPUT}: {baseline_rms} dBFS")

    # 1. The source device and its tone.
    node_cfg = os.path.join(work, "source.toml")
    subprocess.run([CLI, "node", "sign-in", "--server", BASE, "--user", USER, "--password-stdin",
                    "--name", SOURCE_NAME, "--config", node_cfg],
                   input=PASSWORD + "\n", text=True, check=True, capture_output=True)
    procs.append(subprocess.Popen([CLI, "node", "run", "--config", node_cfg], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))
    procs.append(subprocess.Popen([sys.executable, os.path.join(HERE, "tone_stream.py"), SOURCE_OUTPUT, "120"],
                                  stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))

    # 2. The desktop app as a remote.
    env = dict(os.environ, APPDATA=os.path.join(work, "appdata"), AUDIONET_TEST_PROFILE=PROFILE)
    procs.append(subprocess.Popen([EXE], env=env))
    check("app window opens", wait_for(lambda: bool(u.FindWindowW(f"AudioNetDesktop-{PROFILE}", None)), 15))
    set_edit("Server address", BASE)
    set_edit("Account user name", USER)
    set_edit("Password", PASSWORD)
    set_edit("Device name", APP_NAME)
    press("Sign in")
    check("signs in with the password", wait_for(lambda: "Signed in as" in status_log(), 30), status_log()[-300:])
    check("password field is cleared", named("Password", UIA.UIA_EditControlTypeId).GetCurrentPattern(UIA.UIA_ValuePatternId)
          .QueryInterface(UIA.IUIAutomationValuePattern).CurrentValue == "")

    # A second account (a temporary one): added with the same fields and the
    # button, which now says "Add account".
    second = second_account.create()
    set_edit("Account user name", second[0])
    set_edit("Password", second[1])
    press("Add account")
    accounts = lambda: [i.CurrentName or "" for i in items("Accounts", UIA.UIA_ListControlTypeId)]
    check("adds a second account", wait_for(lambda: len(accounts()) == 2, 30), str(accounts()))
    check("the accounts are listed in words", any(a.startswith(f"{USER} on ") for a in accounts())
          and any(a.startswith(f"{second[0]} on ") for a in accounts()), str(accounts()))
    # Online at once after signing in; sending from this computer needs
    # sharing in that account: select it (the new one is selected after
    # adding it), then Start sharing.
    choose("Accounts", UIA.UIA_ListControlTypeId, f"{USER} on ")
    press("Start sharing")
    check("sharing in the first account only", wait_for(lambda: any(a.startswith(f"{USER} on ") and a.endswith("online, sharing") for a in accounts())
          and any(a.startswith(f"{second[0]} on ") and a.endswith("online, not sharing") for a in accounts()), 15), str(accounts()))
    check("connects", wait_for(lambda: "Connected to" in status_log(), 30))
    check("both accounts connect", wait_for(lambda: status_log().count("Connected to") >= 2, 30))
    check("the tree has an item per account", wait_for(lambda: any(t.startswith(f"Devices in {USER} on") for t in tree_top())
          and any(t.startswith(f"Devices in {second[0]} on") for t in tree_top()), 20), str(tree_top()))
    devices = lambda: named("Your devices", UIA.UIA_TreeControlTypeId)
    check("lists the source device online, as one collapsed tree item",
          wait_for(lambda: expand_state(tree_item(devices(), f"{SOURCE_NAME}, online")) == 0, 30))
    source = tree_item(devices(), f"{SOURCE_NAME}, online")
    select(source)
    expand(source)
    check("expanding it shows Sounds to listen to and Outputs to send to",
          expand_state(source) == 1 and bool(tree_item(source, "Sounds to listen to"))
          and bool(tree_item(source, "Outputs to send to")))
    sounds = tree_item(source, "Sounds to listen to")
    expand(sounds)
    sound = tree_item(sounds, f"Sound playing on {SOURCE_OUTPUT}")
    select(sound)
    check("its sound can be chosen", sound.GetCurrentPattern(UIA.UIA_SelectionItemPatternId)
          .QueryInterface(UIA.IUIAutomationSelectionItemPattern).CurrentIsSelected == 1)
    check("this computer's outputs are offered",
          choose_by_keyboard("Play it on", PLAYBACK_OUTPUT), combo_value("Play it on"))
    # Enter on the chosen sound listens (as a keyboard user presses it).
    tree_hwnd = devices().CurrentNativeWindowHandle
    u.PostMessageW(tree_hwnd, 0x0100, 0x0D, 0)  # WM_KEYDOWN, VK_RETURN
    streams = lambda: [i.CurrentName or "" for i in items("Streams this computer started", UIA.UIA_ListControlTypeId)]
    check("stream connects", wait_for(lambda: any("connected" in r for r in streams()), 30), str(streams()))

    # 3. What plays here.
    time.sleep(3)
    wav = os.path.join(work, "heard.wav")
    subprocess.run([CLI, "capture-test", "--loopback", output_number(PLAYBACK_OUTPUT), "--seconds", "4", "--wav", wav],
                   capture_output=True)
    report = subprocess.run([sys.executable, os.path.join(HERE, "verify_wav.py"), wav], capture_output=True, text=True).stdout
    print("  " + report.strip().replace("\n", "\n  "))
    freq = next((float(l.split(":")[1].split()[0]) for l in report.splitlines() if l.startswith("dominant frequency")), 0.0)
    rms = next((float(l.split("rms=")[1].split()[0]) for l in report.splitlines() if "rms=" in l), -200.0)
    check("the tone arrives (997 Hz)", abs(freq - 997) < 3, f"{freq} Hz")
    discontinuities = next((int(l.rsplit(":", 1)[1].split()[0]) for l in report.splitlines()
                            if "discontinuities" in l), -1)
    check("without discontinuities", discontinuities == 0, str(discontinuities))
    check("at the level of a local tone on the same output (within 1 dB)",
          abs(rms - baseline_rms) < 1.0, f"{rms} vs {baseline_rms} dBFS")

    # 3b. The stream's volume and mute, through UI Automation as a screen
    #     reader user would set them: half volume is about 12 dB quieter,
    #     muted is silence, and the list line says so.
    choose("Streams this computer started", UIA.UIA_ListControlTypeId, "Listening to")
    slider = named("Volume of the selected stream", UIA.UIA_SliderControlTypeId)
    rv = slider.GetCurrentPattern(UIA.UIA_RangeValuePatternId).QueryInterface(UIA.IUIAutomationRangeValuePattern)
    check("the volume slider is named and at 100", rv.CurrentValue == 100, str(rv.CurrentValue))
    rv.SetValue(50)

    def level(seconds=2):
        w = os.path.join(work, f"level{time.time()}.wav")
        subprocess.run([CLI, "capture-test", "--loopback", output_number(PLAYBACK_OUTPUT), "--seconds", str(seconds),
                        "--wav", w], capture_output=True)
        r = subprocess.run([sys.executable, os.path.join(HERE, "verify_wav.py"), w], capture_output=True, text=True).stdout
        return next((float(l.split("rms=")[1].split()[0]) for l in r.splitlines() if "rms=" in l), -200.0)

    time.sleep(1)
    half = level()
    check("half volume plays about 12 dB quieter", abs(rms - half - 12.0) < 1.5, f"{half} vs {rms} dBFS")
    check("the list says the volume", any("volume 50 percent" in r for r in streams()), str(streams()))
    mute = named("Mute the selected stream", UIA.UIA_CheckBoxControlTypeId)
    mute.GetCurrentPattern(UIA.UIA_TogglePatternId).QueryInterface(UIA.IUIAutomationTogglePattern).Toggle()
    time.sleep(1)
    muted = level()
    check("muted plays silence", muted < -80.0, f"{muted} dBFS")
    check("the list says muted", any(r.endswith(", muted") for r in streams()), str(streams()))
    mute.GetCurrentPattern(UIA.UIA_TogglePatternId).QueryInterface(UIA.IUIAutomationTogglePattern).Toggle()
    rv.SetValue(100)
    time.sleep(1)
    check("back to full volume", abs(level() - rms) < 1.0)

    choose("Streams this computer started", UIA.UIA_ListControlTypeId, "Listening to")
    press("Stop the selected stream")
    check("stopping removes the stream", wait_for(lambda: not streams(), 15), str(streams()))
    check("the stop is logged in words", "Stopped: Listening to" in status_log())

    # 4. Send: this computer's copy of the tone to the other device's output.
    check("this computer's sounds are offered",
          choose_by_keyboard("Send from this computer", f"Sound playing on {SOURCE_OUTPUT}"), combo_value("Send from this computer"))
    source = tree_item(devices(), f"{SOURCE_NAME}, online")
    expand(source)
    outputs = tree_item(source, "Outputs to send to")
    expand(outputs)
    select(tree_item(outputs, PLAYBACK_OUTPUT))
    check("the tree kept the device expanded after the listen", expand_state(source) == 1)
    press("Send to the chosen output")
    check("send stream connects", wait_for(lambda: any("Sending" in r and "connected" in r for r in streams()), 30), str(streams()))
    time.sleep(3)
    wav2 = os.path.join(work, "sent.wav")
    subprocess.run([CLI, "capture-test", "--loopback", out_no, "--seconds", "4", "--wav", wav2], capture_output=True)
    report = subprocess.run([sys.executable, os.path.join(HERE, "verify_wav.py"), wav2], capture_output=True, text=True).stdout
    print("  " + report.strip().replace("\n", "\n  "))
    freq = next((float(l.split(":")[1].split()[0]) for l in report.splitlines() if l.startswith("dominant frequency")), 0.0)
    rms = next((float(l.split("rms=")[1].split()[0]) for l in report.splitlines() if "rms=" in l), -200.0)
    check("the other device plays what this computer sends", abs(freq - 997) < 3 and abs(rms - baseline_rms) < 1.0,
          f"{freq} Hz, {rms} dBFS")
    choose("Streams this computer started", UIA.UIA_ListControlTypeId, "Sending")
    press("Stop the selected stream")
    check("stopping the send removes it", wait_for(lambda: not streams(), 15), str(streams()))

    # Signing out of the second account (the app asks first) leaves one.
    choose("Accounts", UIA.UIA_ListControlTypeId, second[0])
    press("Sign out of the selected account")
    check("sign-out asks first", answer_dialog("Yes"))
    check("one account is left", wait_for(lambda: len(accounts()) == 1, 15), str(accounts()))
    check("the tree has no account items with one account",
          wait_for(lambda: not any(t.startswith("Devices in") for t in tree_top()), 10), str(tree_top()))
finally:
    if 'second' in globals() and second:
        second_account.delete(second[0])
    for p in procs:
        if p.poll() is None:
            p.terminate()
    try:
        s = api()
        for n in s.get(f"{BASE}/api/v1/nodes").json()["nodes"]:
            if n["name"] in (SOURCE_NAME, APP_NAME):
                s.delete(f"{BASE}/api/v1/nodes/{n['node_id']}")
    except Exception as e:
        print("clean-up: could not remove the test devices:", e)
    try:
        winreg.DeleteKey(winreg.HKEY_CURRENT_USER, rf"Software\AudioNet-Test-{PROFILE}")
    except FileNotFoundError:
        pass
failed = [n for n, ok in results if not ok]
print(f"{len(results) - len(failed)} of {len(results)} checks passed")
sys.exit(1 if failed or not results else 0)
