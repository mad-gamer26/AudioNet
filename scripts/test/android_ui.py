"""Drives the AudioNet Android app on an emulator or device through adb:
reads the screen's accessibility tree (what TalkBack sees: text, names,
roles, states), finds controls by text or name, taps them and types.

Used by android_e2e.py; also handy by hand:
    python scripts/test/android_ui.py dump
    python scripts/test/android_ui.py tap "Sign In"

TalkBack turns single taps into "move focus", so automation pauses the
device's accessibility services and puts them back exactly as they were
(paused_accessibility()).

Environment: ADB (default: the Android SDK's adb), ANDROID_SERIAL.
"""
import contextlib, os, re, subprocess, sys, time
import xml.etree.ElementTree as ET

ADB = os.environ.get("ADB") or os.path.join(os.environ.get("LOCALAPPDATA", ""), "Android", "Sdk", "platform-tools", "adb.exe")


def adb(*args, check=True):
    return subprocess.run([ADB, *args], capture_output=True, text=True, encoding="utf-8", errors="replace", check=check).stdout


def nodes():
    """Every node on screen: (text, content-desc, class, clickable, checked, bounds center)."""
    adb("shell", "uiautomator", "dump", "/sdcard/audionet-ui.xml")
    xml = adb("exec-out", "cat", "/sdcard/audionet-ui.xml")
    out = []
    for n in ET.fromstring(xml).iter("node"):
        b = re.findall(r"\d+", n.get("bounds", "[0,0][0,0]"))
        x1, y1, x2, y2 = map(int, b)
        out.append({
            "text": n.get("text", ""), "desc": n.get("content-desc", ""),
            "cls": n.get("class", "").split(".")[-1], "clickable": n.get("clickable") == "true",
            "checked": n.get("checked") == "true", "checkable": n.get("checkable") == "true",
            "selected": n.get("selected") == "true", "enabled": n.get("enabled") == "true",
            "focusable": n.get("focusable") == "true",
            "x": (x1 + x2) // 2, "y": (y1 + y2) // 2,
        })
    return out


def texts():
    return [n["text"] or n["desc"] for n in nodes() if n["text"] or n["desc"]]


def find(label, exact=False, timeout=15):
    """The first node whose text or name is (or contains) `label`, waiting for it."""
    end = time.time() + timeout
    while True:
        for n in nodes():
            for v in (n["text"], n["desc"]):
                if v and (v == label if exact else label in v):
                    return n
        if time.time() > end:
            raise SystemExit(f"not on screen: {label!r}\nscreen: {texts()}")
        time.sleep(0.7)


def tap(label, exact=False, timeout=15):
    n = find(label, exact, timeout)
    adb("shell", "input", "tap", str(n["x"]), str(n["y"]))
    time.sleep(0.6)
    return n


def keyboard_shown():
    return "mInputShown=true" in adb("shell", "dumpsys", "input_method")


def hide_keyboard():
    """Closes the on-screen keyboard if it is showing (Back, only then:
    with no keyboard, Back would leave the screen)."""
    for _ in range(3):
        if not keyboard_shown():
            return
        adb("shell", "input", "keyevent", "4")
        time.sleep(0.6)


def type_into(label, text):
    """Focuses the field labelled `label`, clears it and types `text`. The
    keyboard is closed first: it can be a full-screen one (a braille
    keyboard) that covers the form.

    The field itself is tapped (the one nearest its label): a field with
    text has its label on its top border, where a tap can miss it."""
    n = find(label)
    fields = [f for f in nodes() if f["cls"] == "EditText" and abs(f["y"] - n["y"]) <= 120]
    target = min(fields, key=lambda f: abs(f["y"] - n["y"])) if fields else n
    adb("shell", "input", "tap", str(target["x"]), str(target["y"]))
    time.sleep(0.6)
    hide_keyboard()
    adb("shell", "input", "keycombination", "113", "29")  # Ctrl+A
    adb("shell", "input", "keyevent", "67")  # Delete
    adb("shell", "input", "text", text.replace(" ", "%s"))
    time.sleep(0.4)
    hide_keyboard()


def scroll_to(label, tries=8):
    for _ in range(tries):
        for n in nodes():
            if label in n["text"] or label in n["desc"]:
                return n
        adb("shell", "input", "swipe", "540", "1700", "540", "800", "300")
        time.sleep(0.8)
    raise SystemExit(f"not found after scrolling: {label!r}\nscreen: {texts()}")


def scroll_to_top(swipes=8):
    """Scrolls the screen back to its top."""
    for _ in range(swipes):
        adb("shell", "input", "swipe", "540", "700", "540", "1800", "200")
    time.sleep(0.8)


@contextlib.contextmanager
def paused_accessibility():
    """Accessibility services (TalkBack) off while automating, then back exactly as they were."""
    services = adb("shell", "settings", "get", "secure", "enabled_accessibility_services").strip()
    enabled = adb("shell", "settings", "get", "secure", "accessibility_enabled").strip()
    adb("shell", "settings", "put", "secure", "enabled_accessibility_services", '""')
    # TalkBack takes a moment to let go; until then it swallows taps.
    for _ in range(20):
        if "Bound services:{}" in adb("shell", "dumpsys", "accessibility"):
            break
        time.sleep(0.5)
    time.sleep(0.5)
    try:
        yield
    finally:
        if services and services != "null":
            adb("shell", "settings", "put", "secure", "enabled_accessibility_services", services)
        if enabled and enabled != "null":
            adb("shell", "settings", "put", "secure", "accessibility_enabled", enabled)


if __name__ == "__main__":
    cmd = sys.argv[1] if len(sys.argv) > 1 else "dump"
    if cmd == "dump":
        for n in nodes():
            if n["text"] or n["desc"]:
                state = " checked" if n["checked"] else ""
                print(f'{n["cls"]}: {n["text"]!r} {n["desc"]!r}{state}{" clickable" if n["clickable"] else ""}')
    elif cmd == "tap":
        tap(sys.argv[2])
