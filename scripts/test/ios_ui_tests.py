"""Runs the native iPhone app's UI tests (apps/ios/AudioNetUITests: Xcode's
accessibility audit and VoiceOver-relevant flows) in the iOS simulator on
the Mac, including the signed-in screen. Run on the Windows machine; the
Mac is reached over SSH.

1. A temporary source device on this PC (the audionet command-line agent)
   is online, so the app has a device to expand and listen to (its
   "UniMic Output" loopback, silent).
2. A temporary "iPhone UI test" device is created through the server API.
   Its device token (never the password) reaches the simulator app as
   TEST_RUNNER_AUDIONET_TEST_ACCOUNT, in its separate test profile.
3. Microphone permission is granted to the simulator app beforehand, so no
   permission alert covers the screen.
4. Both temporary devices are removed afterwards, which revokes the token.

Environment: AUDIONET_URL, AUDIONET_USER, AUDIONET_PASSWORD, MAC (ssh
host, required), SIMULATOR (simulator id).
"""
import os, subprocess, tempfile
import testenv
import requests
import second_account

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
BASE = os.environ["AUDIONET_URL"].rstrip("/")
USER, PASSWORD = os.environ["AUDIONET_USER"], os.environ["AUDIONET_PASSWORD"]
MAC = testenv.need("MAC", "user@mac.local")
SIM = testenv.need("SIMULATOR", "the id from xcrun simctl list devices")
CLI = os.path.join(ROOT, "target", "release", "audionet.exe")
SOURCE_NAME, APP_NAME = "Remote test source", "iPhone UI test"
work = tempfile.mkdtemp(prefix="audionet-ios-ui-")

def api():
    s = requests.Session()
    s.headers["Authorization"] = "Bearer " + s.post(f"{BASE}/api/v1/login", json={"username": USER, "password": PASSWORD}).json()["token"]
    return s

def remove_test_devices():
    s = api()
    for n in s.get(f"{BASE}/api/v1/nodes").json()["nodes"]:
        if n["name"] in (SOURCE_NAME, APP_NAME):
            s.delete(f"{BASE}/api/v1/nodes/{n['node_id']}")

procs = []
ok = False
second = None
try:
    remove_test_devices()
    cfg = os.path.join(work, "source.toml")
    subprocess.run([CLI, "node", "sign-in", "--server", BASE, "--user", USER, "--password-stdin", "--name", SOURCE_NAME,
                    "--config", cfg], input=PASSWORD + "\n", text=True, check=True, capture_output=True)
    procs.append(subprocess.Popen([CLI, "node", "run", "--config", cfg], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))
    r = requests.post(f"{BASE}/api/v1/nodes/sign-in",
                      json={"username": USER, "password": PASSWORD, "name": APP_NAME, "platform": "ios"})
    r.raise_for_status()
    dev = r.json()
    # A temporary second account: the app is signed in to both
    # (AUDIONET_ONE_ACCOUNT=1 leaves it out, to tell problems apart).
    second = None if os.environ.get("AUDIONET_ONE_ACCOUNT") == "1" else second_account.create()
    lines = [f"server={BASE}", f"node_id={dev['node_id']}", f"token={dev['token']}",
             f"device_name={APP_NAME}", f"username={dev['username']}"]
    if second:
        r2 = requests.post(f"{BASE}/api/v1/nodes/sign-in",
                           json={"username": second[0], "password": second[1], "name": APP_NAME, "platform": "ios"})
        r2.raise_for_status()
        dev2 = r2.json()
        lines += ["---", f"server={BASE}", f"node_id={dev2['node_id']}", f"token={dev2['token']}",
                  f"device_name={APP_NAME}", f"username={dev2['username']}"]
    account = "\n".join(lines)
    subprocess.run(["ssh", MAC, "umask 077; cat > /tmp/audionet-ios-account"], input=account.encode(), check=True)
    test = subprocess.run(["ssh", MAC,
                           f"xcrun simctl boot {SIM} 2>/dev/null; xcrun simctl privacy {SIM} grant microphone com.matthewdovi.AudioNet 2>/dev/null; "
                           "export TEST_RUNNER_AUDIONET_TEST_ACCOUNT=\"$(cat /tmp/audionet-ios-account)\"; rm -f /tmp/audionet-ios-account; "
                           "cd ~/audionet-src/apps/ios && rm -rf /tmp/audionet-ios-uitests.xcresult && "
                           f"xcodebuild test -project AudioNet.xcodeproj -scheme AudioNet -destination 'platform=iOS Simulator,id={SIM}' "
                           "-derivedDataPath /tmp/audionet-ios-sim -resultBundlePath /tmp/audionet-ios-uitests.xcresult 2>&1"],
                          capture_output=True, text=True, encoding="utf-8", errors="replace")
    for line in test.stdout.splitlines():
        if any(k in line for k in ("Test Case", "error:", "** TEST", "Executed", "DEVICE ELEMENT", "StaticText", "Button", "Cell")):
            print(line.strip()[:400])
    ok = "** TEST SUCCEEDED **" in test.stdout
    if not ok:
        print("result bundle kept on the Mac: /tmp/audionet-ios-uitests.xcresult")
finally:
    for p in procs:
        if p.poll() is None:
            p.terminate()
    subprocess.run(["ssh", MAC, "rm -f /tmp/audionet-ios-account"])
    try:
        remove_test_devices()
    except Exception as e:
        print("clean-up: could not remove the test devices:", e)
    if second:
        second_account.delete(second[0])
print("iOS UI tests:", "PASS" if ok else "FAIL")
