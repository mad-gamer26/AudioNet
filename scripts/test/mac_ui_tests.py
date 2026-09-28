"""Runs the macOS app's UI tests (apps/macos/AudioNetUITests: Xcode's
accessibility audit and keyboard flows) on the Mac, including the
signed-in window. Run on the Windows machine; the Mac is reached over SSH.

1. A temporary source device on this PC (the audionet command-line agent)
   is online, so the Mac app has a device to choose and listen to (its
   "UniMic Output" loopback, silent: nothing plays there).
2. A temporary "Mac UI test" device is created through the server API; its
   device token goes to the Mac in a file only the Mac user can read, which
   the app (in its separate test profile) deletes as soon as it reads it.
   The account password never goes to the Mac.
3. xcodebuild test runs in ~/audionet-src/apps/macos (synced and built
   beforehand, see docs/developer-setup.md). UI automation must be allowed
   on the Mac (automationmodetool).
4. Both temporary devices are removed, which revokes the token.

Environment: AUDIONET_URL, AUDIONET_USER, AUDIONET_PASSWORD, MAC (ssh
host, required).
"""
import os, subprocess, tempfile, time
import testenv
import requests
import second_account

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
BASE = os.environ["AUDIONET_URL"].rstrip("/")
USER, PASSWORD = os.environ["AUDIONET_USER"], os.environ["AUDIONET_PASSWORD"]
MAC = testenv.need("MAC", "user@mac.local")
CLI = os.path.join(ROOT, "target", "release", "audionet.exe")
SOURCE_NAME, MAC_NAME = "Remote test source", "Mac UI test"
ACCOUNT_FILE = "/tmp/audionet-uitest-account"
work = tempfile.mkdtemp(prefix="audionet-mac-ui-")

def api():
    s = requests.Session()
    s.headers["Authorization"] = "Bearer " + s.post(f"{BASE}/api/v1/login", json={"username": USER, "password": PASSWORD}).json()["token"]
    return s

def remove_test_devices():
    s = api()
    for n in s.get(f"{BASE}/api/v1/nodes").json()["nodes"]:
        if n["name"] in (SOURCE_NAME, MAC_NAME):
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
                      json={"username": USER, "password": PASSWORD, "name": MAC_NAME, "platform": "mac_os"})
    r.raise_for_status()
    dev = r.json()
    # A temporary second account: the app is signed in to both.
    second = second_account.create()
    r2 = requests.post(f"{BASE}/api/v1/nodes/sign-in",
                       json={"username": second[0], "password": second[1], "name": MAC_NAME, "platform": "mac_os"})
    r2.raise_for_status()
    dev2 = r2.json()
    account = "\n".join([f"server={BASE}", f"node_id={dev['node_id']}", f"token={dev['token']}",
                         f"device_name={MAC_NAME}", f"username={dev['username']}", "---",
                         f"server={BASE}", f"node_id={dev2['node_id']}", f"token={dev2['token']}",
                         f"device_name={MAC_NAME}", f"username={dev2['username']}"]) + "\n"
    subprocess.run(["ssh", MAC, f"umask 077; cat > {ACCOUNT_FILE}"], input=account.encode(), check=True)
    time.sleep(2)
    subprocess.run(["ssh", MAC, "rm -rf /tmp/audionet-uitests.xcresult"])
    test = subprocess.run(["ssh", MAC, "cd ~/audionet-src/apps/macos && xcodebuild test -project AudioNet.xcodeproj "
                           "-scheme AudioNet -destination platform=macOS -derivedDataPath /tmp/audionet-mac-build "
                           # Its own bundle identifier: never shares macOS permissions
                           # with the AudioNet installed on the Mac.
                           "AUDIONET_BUNDLE_SUFFIX=.uitest "
                           "-resultBundlePath /tmp/audionet-uitests.xcresult 2>&1"],
                          capture_output=True, text=True, encoding="utf-8", errors="replace")
    for line in test.stdout.splitlines():
        if any(k in line for k in ("Test Case", "error:", "issues:", "Skipped", "** TEST", "Executed", "STATUS ", "LISTEN:")):
            print(line.strip())
    ok = "** TEST SUCCEEDED **" in test.stdout
    # Kept on failure (screenshots of failed audits), for a closer look.
    if ok:
        subprocess.run(["ssh", MAC, "rm -rf /tmp/audionet-uitests.xcresult"])
    else:
        print("result bundle kept on the Mac: /tmp/audionet-uitests.xcresult")
finally:
    for p in procs:
        if p.poll() is None:
            p.terminate()
    subprocess.run(["ssh", MAC, f"rm -f {ACCOUNT_FILE}"])
    try:
        remove_test_devices()
    except Exception as e:
        print("clean-up: could not remove the test devices:", e)
    if second:
        second_account.delete(second[0])
print("macOS UI tests:", "PASS" if ok else "FAIL")
