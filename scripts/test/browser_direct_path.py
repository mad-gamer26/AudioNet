"""Checks that a browser that hides its local address (mDNS candidates)
still gets a direct connection to a device on the same network, instead of
going through the TURN relay. Run on the Windows machine.

This PC runs a temporary source device from a copy of audionet.exe that has
no Windows Firewall rule, like an installed desktop app: unsolicited
packets from the browser are dropped, so a direct path exists only if this
device resolves the browser's .local name and sends to it first. The Mac
(another machine on the same network, over SSH) runs headless Chrome that
only listens, so it hides its address, like a phone.

With OLD_CLI set to an earlier audionet.exe, that build is run first for
comparison (expected: relay).

Environment: AUDIONET_URL, AUDIONET_USER, AUDIONET_PASSWORD, MAC (ssh
host, required), OLD_CLI (optional).
Requires Chrome and selenium on the Mac.
"""
import os, shutil, subprocess, sys, tempfile, time
import testenv
import requests

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
BASE = os.environ["AUDIONET_URL"].rstrip("/")
USER, PASSWORD = os.environ["AUDIONET_USER"], os.environ["AUDIONET_PASSWORD"]
MAC = testenv.need("MAC", "user@mac.local")
SOURCE_NAME, SOURCE = "Direct path test source", "UniMic Output"
work = tempfile.mkdtemp(prefix="audionet-direct-")

def api():
    s = requests.Session()
    s.headers["Authorization"] = "Bearer " + s.post(f"{BASE}/api/v1/login", json={"username": USER, "password": PASSWORD}).json()["token"]
    return s

def remove_test_devices():
    s = api()
    for n in s.get(f"{BASE}/api/v1/nodes").json()["nodes"]:
        if n["name"] == SOURCE_NAME:
            s.delete(f"{BASE}/api/v1/nodes/{n['node_id']}")

def run(label, cli_src):
    # A path no firewall rule names.
    cli = os.path.join(work, f"audionet-{label}-{os.getpid()}.exe")
    shutil.copy(cli_src, cli)
    cfg = os.path.join(work, f"{label}.toml")
    remove_test_devices()
    for attempt in (1, 2):
        r = subprocess.run([cli, "node", "sign-in", "--server", BASE, "--user", USER, "--password-stdin",
                            "--name", SOURCE_NAME, "--config", cfg], input=PASSWORD + "\n", text=True, capture_output=True)
        if r.returncode == 0:
            break
        print(f"[{label}] sign-in failed (attempt {attempt}): {r.stderr.strip()[-200:]}")
        time.sleep(3)
    else:
        raise SystemExit("could not sign in the test source device")
    node_log = os.path.join(work, f"{label}-node.log")
    with open(node_log, "w", encoding="utf-8") as f:
        node = subprocess.Popen([cli, "node", "run", "--config", cfg], stdout=f, stderr=subprocess.STDOUT)
    try:
        time.sleep(4)
        settings = "\n".join([f"URL={BASE}", f"USER={USER}", f"PASSWORD={PASSWORD}", f"DEVICE={SOURCE_NAME}",
                              f"SOURCE={SOURCE}", "SECONDS=8"]) + "\n"
        subprocess.run(["ssh", MAC, "umask 077; cat > /tmp/audionet-browser.cfg"], input=settings.encode(), check=True)
        out = subprocess.run(["ssh", MAC, "python3 /tmp/mac_browser_listen.py /tmp/audionet-browser.cfg"],
                             capture_output=True, text=True, encoding="utf-8", errors="replace")
        diag = out.stdout.replace(PASSWORD, "(password)")
    finally:
        node.terminate()
        node.wait()
        subprocess.run(["ssh", MAC, "rm -f /tmp/audionet-browser.cfg"])
    route = next((l.strip() for l in diag.splitlines() if l.strip().startswith("Route:")), "")
    log = open(node_log, encoding="utf-8", errors="replace").read()
    found = [l.strip() for l in log.splitlines() if "hidden local address" in l]
    print(f"[{label}] {route or 'no route reported: ' + ' / '.join(l.strip() for l in out.stderr.splitlines() if l.strip().startswith(('File', 'selenium', 'w.until', 'card', 'print', 'pairs')))[-900:]}")
    for l in found:
        print(f"[{label}] source device: {l[-160:]}")
    for l in diag.splitlines():
        if l.startswith(("pair ", "selected ", "pc ", "STREAM")):
            print(f"[{label}] browser {l}")
    selected = next((l.split()[1] for l in diag.splitlines() if l.startswith("selected ")), "")
    kinds = next((l.split()[2] for l in diag.splitlines() if l.startswith(f"pair {selected} ")), "?/?")
    return route, kinds

try:
    subprocess.run(["scp", "-q", os.path.join(HERE, "mac_browser_listen.py"), f"{MAC}:/tmp/mac_browser_listen.py"], check=True)
    tone = subprocess.Popen([sys.executable, os.path.join(HERE, "tone_stream.py"), SOURCE, "300"],
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    ok = True
    if os.environ.get("OLD_CLI"):
        _, old = run("before", os.environ["OLD_CLI"])
        print(f"before the fix: the browser used {old} (local/remote)")
    route, new = run("after", os.path.join(ROOT, "target", "release", "audionet.exe"))
    print(f"after the fix: the browser used {new} (local/remote)")
    direct = new == "host/host"
    print("the web client's Route line agrees:", "yes" if "local host, remote host" in route else f"no ({route})")
    print("a browser that hides its address connects directly on the same network:", "PASS" if direct else "FAIL")
finally:
    try:
        tone.terminate()
    except NameError:
        pass
    remove_test_devices()
    subprocess.run(["ssh", MAC, "rm -f /tmp/mac_browser_listen.py /tmp/audionet-browser.cfg"])
