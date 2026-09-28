"""Times how a Mac starts listening, per output: runs apps/macos/EngineCheck
headless on the Mac (over SSH, no window) with REPEAT, against a temporary
silent source device on this PC, and prints for each attempt how long the
Mac took to open its output and to connect.

Environment: AUDIONET_URL, AUDIONET_USER, AUDIONET_PASSWORD, MAC (ssh host,
required), OUTPUTS (Mac outputs by name,
"|"-separated; default "Jump Desktop Audio|MacBook Air Speakers"), REPEAT
(attempts per output, default 5). The source is silent: nothing is heard.
The password reaches the Mac in a file only its user can read, deleted as
soon as it is read. Temporary devices are removed afterwards.
"""
import os, subprocess, tempfile
import testenv
import requests

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
BASE = os.environ["AUDIONET_URL"].rstrip("/")
USER, PASSWORD = os.environ["AUDIONET_USER"], os.environ["AUDIONET_PASSWORD"]
MAC = testenv.need("MAC", "user@mac.local")
OUTPUTS = os.environ.get("OUTPUTS", "Jump Desktop Audio|MacBook Air Speakers")
REPEAT = os.environ.get("REPEAT", "5")
CLI = os.path.join(ROOT, "target", "release", "audionet.exe")
SOURCE_NAME, MAC_NAME = "Remote test source", "Mac timing test"
work = tempfile.mkdtemp(prefix="audionet-listen-timing-")

def api():
    s = requests.Session()
    s.headers["Authorization"] = "Bearer " + s.post(f"{BASE}/api/v1/login", json={"username": USER, "password": PASSWORD}).json()["token"]
    return s

def remove_test_devices():
    s = api()
    for n in s.get(f"{BASE}/api/v1/nodes").json()["nodes"]:
        if n["name"] in (SOURCE_NAME, MAC_NAME):
            s.delete(f"{BASE}/api/v1/nodes/{n['node_id']}")

node = None
try:
    remove_test_devices()
    cfg = os.path.join(work, "source.toml")
    subprocess.run([CLI, "node", "sign-in", "--server", BASE, "--user", USER, "--password-stdin", "--name", SOURCE_NAME,
                    "--config", cfg], input=PASSWORD + "\n", text=True, check=True, capture_output=True)
    log = open(os.path.join(work, "source-node.log"), "w", encoding="utf-8")
    node = subprocess.Popen([CLI, "node", "run", "--config", cfg], stdout=log, stderr=subprocess.STDOUT)
    settings = "\n".join([f"URL={BASE}", f"USER={USER}", f"PASSWORD={PASSWORD}", f"DEVICE_NAME={MAC_NAME}",
                          f"SOURCE_DEVICE={SOURCE_NAME}", "SOURCE_MATCH=UniMic Output", f"REPEAT={REPEAT}",
                          f"REPEAT_OUTPUTS={OUTPUTS}"]) + "\n"
    subprocess.run(["ssh", MAC, "umask 077; cat > /tmp/audionet-timing.cfg"], input=settings.encode(), check=True)
    out = subprocess.run(["ssh", MAC, "/tmp/audionet-engine-check /tmp/audionet-timing.cfg 2>&1 | grep -E 'REPEAT|FAIL|DONE'"],
                         capture_output=True, text=True, encoding="utf-8", errors="replace").stdout
    print(out.replace(PASSWORD, "(password)").rstrip())
    print("source device log:", log.name)
finally:
    if node and node.poll() is None:
        node.terminate()
    subprocess.run(["ssh", MAC, "rm -f /tmp/audionet-timing.cfg ~/Library/Logs/audionet-engine-check.log"])
    try:
        remove_test_devices()
    except Exception as e:
        print("clean-up: could not remove the test devices:", e)
