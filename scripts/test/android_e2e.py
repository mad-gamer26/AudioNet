"""End to end on an Android emulator: a throwaway AudioNet server and
account on this computer, the audionet command-line agent as a device
sharing its sounds, and the Android app (already installed: gradlew
installDebug) signing in, listening to one of the device's sounds, and
stopping. Nothing is heard: it listens to a virtual cable ("Line 1") that
nothing plays into. The microphone is never sent.

The emulator reaches the server through `adb reverse` (the app sees it at
http://127.0.0.1, so plain http is allowed); the audio itself goes over the
emulator's network path, directly or through the relay as ICE decides.

Passes when the stream connects and, after 30 seconds, is still connected
with packets arriving and no losses reported as failures. Prints the
stream's measurements (device glitches included). TalkBack and other
accessibility services are paused while it taps and types, and put back.

Needs: debug builds of the server and the agent (cargo build -p
audionet-server -p audionet-cli), the app installed (its data is cleared
first: use a test emulator), and
"Line 1 (Virtual Audio Cable)" on this computer.
"""
import os, re, subprocess, sys, tempfile, time, urllib.request

from android_ui import adb, find, nodes, paused_accessibility, scroll_to, tap, texts, type_into

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
SERVER = os.path.join(ROOT, "target", "debug", "audionet-server.exe")
CLI = os.path.join(ROOT, "target", "debug", "audionet.exe")
PORT = 18741
PASSWORD = "android-test-pass"  # a throwaway account on a throwaway server
APP = "com.matthewdovi.audionet"

work = tempfile.mkdtemp(prefix="audionet-android-")
config = os.path.join(work, "config.toml")
with open(config, "w", encoding="utf-8") as f:
    f.write(f'public_url = "http://127.0.0.1:{PORT}"\nbind = "127.0.0.1:{PORT}"\n'
            f'database = "{os.path.join(work, "test.db").replace(os.sep, "/")}"\n'
            f'web_root = "{os.path.join(ROOT, "web").replace(os.sep, "/")}"\n')
subprocess.run([SERVER, "-c", config, "user", "add", "tester"], input=PASSWORD + "\n", text=True, check=True, capture_output=True)
server = subprocess.Popen([SERVER, "-c", config, "serve"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
node = None
try:
    for _ in range(50):
        try:
            urllib.request.urlopen(f"http://127.0.0.1:{PORT}/api/v1/info", timeout=1)
            break
        except OSError:
            time.sleep(0.2)
    node_config = os.path.join(work, "node.toml")
    subprocess.run([CLI, "node", "sign-in", "--server", f"http://127.0.0.1:{PORT}", "--user", "tester", "--password-stdin",
                    "--name", "PC test device", "--config", node_config], input=PASSWORD + "\n", text=True, check=True, capture_output=True)
    node = subprocess.Popen([CLI, "node", "run", "--config", node_config], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    adb("reverse", f"tcp:{PORT}", f"tcp:{PORT}")
    # A fresh start: no sign-in, no permissions (test emulators only).
    adb("shell", "pm", "clear", APP)
    adb("shell", "am", "start", "-n", f"{APP}/.MainActivity")
    time.sleep(5)
    with paused_accessibility():
        type_into("Server address", f"http://127.0.0.1:{PORT}")
        type_into("Account name", "tester")
        type_into("Password", PASSWORD)
        type_into("Name for this phone", "Android emulator")
        tap("Sign In", exact=True)
        time.sleep(6)
        # Permission prompts: no microphone (never sent here); notifications yes.
        for _ in range(3):
            prompt = [n for n in nodes() if n["text"].startswith("Don")]
            mic = any("record audio" in t for t in texts())
            if not prompt:
                break
            target = prompt[0] if mic else find("Allow", exact=True, timeout=3)
            adb("shell", "input", "tap", str(target["x"]), str(target["y"]))
            time.sleep(1.5)
        find("PC test device: online", timeout=30)
        tap("Settings", exact=True)
        tap("Show Measurements", exact=True)
        adb("shell", "input", "keyevent", "4")
        time.sleep(1)
        tap("PC test device: online")
        tap("Sound playing on Line 1")
        n = scroll_to("Listen to PC test device")
        adb("shell", "input", "tap", str(n["x"]), str(n["y"]))
        time.sleep(30)
        scroll_to("Streams started here")
        adb("shell", "input", "swipe", "540", "1700", "540", "900", "300")
        time.sleep(1)
        screen = texts()
        stream = next((t for t in screen if t.startswith("Listening to Sound playing on Line 1") and ": " in t), "")
        measured = next((t for t in screen if t.startswith("Receiving:")), "")
        print("Stream:", stream)
        print("Measurements:", measured)
        stop = find("Stop Listening to Sound playing on Line 1")
        adb("shell", "input", "tap", str(stop["x"]), str(stop["y"]))
        time.sleep(3)
        stopped = any("You stopped it" in t for t in texts())
        # Sign out, so the next run starts from the sign-in screen again.
        tap("Settings", exact=True)
        tap("Sign Out of tester", exact=False)
        tap("Sign Out", exact=True)
        time.sleep(4)
    packets = int(m[1]) if (m := re.search(r"Receiving: (\d+) packets", measured)) else 0
    ok = stream.endswith(": connected") and packets > 1000 and stopped
    print("PASS" if ok else "FAIL")
    sys.exit(0 if ok else 1)
finally:
    adb("reverse", "--remove", f"tcp:{PORT}", check=False)
    if node:
        node.terminate()
    server.terminate()
