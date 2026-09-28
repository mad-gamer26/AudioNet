"""End-to-end test of the macOS app's automatic update, on the Mac, with a
throwaway signing key (never the release key) and a local update server.
Run on the Windows machine (the manifests are signed here, like real
releases); the Mac is reached over SSH and ~/audionet-src must be synced.

Builds (for the Mac's own processor) version 1.0.0 and 1.0.1 of the app
that trust the test key and look for updates at http://127.0.0.1:PORT, and
a 1.0.2 whose program exits at once (validly signed, but it never starts).
Then, in a test profile, with the app in ~/AudioNetUpdateTest:

1. A manifest with a bad signature is ignored: 1.0.0 stays.
2. A correctly signed 1.0.1 is downloaded, checked, installed, and started;
   it reports "updated from version 1.0.0 to 1.0.1", nothing is left over.
3. The broken 1.0.2 is installed, does not start, and is rolled back:
   1.0.1 runs again, reports it, and 1.0.2 is never offered again.

Environment: MAC (ssh host, required).
"""
import os, subprocess, sys, tempfile, time
import testenv

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
MAC = testenv.need("MAC", "user@mac.local")
PORT = 8765
REMOTE = "/tmp/audionet-update-e2e"      # builds and served files, on the Mac
APP_DIR = "~/AudioNetUpdateTest"
APP = f"{APP_DIR}/AudioNet.app"
PROFILE = "updtest"
SUITE = f"org.audionet.AudioNet.test.{PROFILE}"
SIGN = [sys.executable, os.path.join(ROOT, "scripts", "release_sign.py")]
work = tempfile.mkdtemp(prefix="audionet-mac-update-")

def mac(cmd, check=True):
    r = subprocess.run(["ssh", MAC, cmd], capture_output=True, text=True, encoding="utf-8", errors="replace")
    if check and r.returncode != 0:
        raise SystemExit(f"on the Mac: {cmd}\n{r.stdout}{r.stderr}")
    return r.stdout.strip()

def installed_version():
    return mac(f"defaults read {APP}/Contents/Info.plist CFBundleShortVersionString 2>/dev/null", check=False)

def status():
    return mac(f"defaults read {SUITE} updateStatus 2>/dev/null", check=False)

def running():
    return mac(f"pgrep -f {APP_DIR.replace('~', '$HOME')}/AudioNet.app/Contents/MacOS/AudioNet", check=False) != ""

def wait_for(what, cond, timeout):
    end = time.time() + timeout
    while time.time() < end:
        if cond():
            return True
        time.sleep(2)
    print(f"  timed out waiting for {what}; status: {status()!r}, installed {installed_version()!r}")
    return False

def serve(version, tamper=False):
    """Signs the manifest for `version` here and publishes it on the Mac."""
    zip_name = f"AudioNet-macOS-{version}.zip"
    local_zip = os.path.join(work, zip_name)
    if not os.path.exists(local_zip):
        subprocess.run(["scp", "-q", f"{MAC}:{REMOTE}/www/{zip_name}", local_zip], check=True)
    subprocess.run(SIGN + ["manifest", key, local_zip, version, work, "--product", "audionet-macos-universal",
                           "--name", "latest-macos.json"], check=True, capture_output=True)
    if tamper:
        sig = open(os.path.join(work, "latest-macos.json.sig")).read().strip()
        flipped = ("0" if sig[0] != "0" else "1") + sig[1:]
        open(os.path.join(work, "latest-macos.json.sig"), "w", newline="\n").write(flipped + "\n")
    subprocess.run(["scp", "-q", os.path.join(work, "latest-macos.json"), os.path.join(work, "latest-macos.json.sig"),
                    f"{MAC}:{REMOTE}/www/"], check=True)

def account_file_private():
    """The test profile's account file exists and only this user may read it."""
    mode = mac(f"stat -f %Lp \"$HOME/Library/Application Support/AudioNet/account-test-{PROFILE}.json\" 2>/dev/null", check=False)
    return mode == "600"

def launch(with_account=False):
    extra = f" -AudioNetTestAccountFile {REMOTE}/account" if with_account else ""
    mac(f"open -n {APP} --args -AudioNetProfile {PROFILE}{extra}")

def quit_app():
    mac(f"pkill -f {APP_DIR.replace('~', '$HOME')}/AudioNet.app/Contents/MacOS/AudioNet", check=False)
    time.sleep(2)

results = []
def check(name, ok):
    results.append(ok)
    print(f"{name}: {'PASS' if ok else 'FAIL'}")

try:
    key = os.path.join(work, "test-key.pem")
    pub = os.path.join(work, "test-key.pub")
    subprocess.run(SIGN + ["keygen", key, pub], check=True, capture_output=True)
    mac(f"rm -rf {REMOTE} {APP_DIR}; mkdir -p {REMOTE}/www {APP_DIR}; defaults delete {SUITE} 2>/dev/null; "
        f"security delete-generic-password -s org.audionet.AudioNet.device.test.{PROFILE} >/dev/null 2>&1; "
        f"rm -f \"$HOME/Library/Application Support/AudioNet/account-test-{PROFILE}.json\"; true")
    subprocess.run(["scp", "-q", pub, f"{MAC}:{REMOTE}/test-key.pub"], check=True)

    print("building 1.0.0, 1.0.1 and a broken 1.0.2 on the Mac")
    for v in ("1.0.0", "1.0.1", "1.0.2"):
        mac(f"cd ~/audionet-src && . ~/.cargo/env && export PATH=/opt/homebrew/bin:$PATH && "
            f"VERSION={v} sh apps/macos/package.sh --this-mac-only "
            f"--update-url http://127.0.0.1:{PORT}/latest.json --public-key-file {REMOTE}/test-key.pub >/dev/null 2>&1 && "
            f"mv dist/AudioNet-macOS-{v}.zip dist/AudioNet-macOS-{v}.zip.sha256 {REMOTE}/www/")
    # 1.0.2: a validly signed app whose program exits at once.
    mac(f"cd {REMOTE} && mkdir broken && ditto -x -k www/AudioNet-macOS-1.0.2.zip broken && "
        f"echo 'int main(void) {{ return 0; }}' | clang -x c - -o broken/AudioNet.app/Contents/MacOS/AudioNet && "
        f"codesign --force --deep -s - broken/AudioNet.app 2>/dev/null && rm www/AudioNet-macOS-1.0.2.zip && "
        f"ditto -c -k --keepParent broken/AudioNet.app www/AudioNet-macOS-1.0.2.zip && rm -rf broken")
    mac(f"ditto -x -k {REMOTE}/www/AudioNet-macOS-1.0.0.zip {APP_DIR}")
    mac(f"cd {REMOTE}/www && (nohup python3 -m http.server {PORT} --bind 127.0.0.1 >/dev/null 2>&1 &) ; sleep 1")

    # A signed-in copy, like a real one (a fake token, never sent anywhere
    # since the copy is not put online).
    mac(f"umask 077; printf 'server=http://127.0.0.1:9\nnode_id=node_updtest\ntoken=not-a-real-token\n"
        f"device_name=Update test\nusername=updtest\n' > {REMOTE}/account")
    print("1. tampered manifest")
    serve("1.0.1", tamper=True)
    launch(with_account=True)
    saved = wait_for("the sign-in", lambda: mac(f"defaults read {SUITE} accountStatus 2>/dev/null", check=False) == "signed in", 30)
    check("the sign-in is kept in a file only this user can read", saved and account_file_private())
    ok = wait_for("the check", lambda: "not signed by the AudioNet release key" in status(), 90)
    check("a manifest with a bad signature is ignored", ok and installed_version() == "1.0.0" and running())
    quit_app()

    print("2. update to 1.0.1")
    serve("1.0.1")
    mac(f"defaults delete {SUITE} updateStatus 2>/dev/null; defaults delete {SUITE} accountStatus 2>/dev/null; true")
    launch()
    ok = wait_for("1.0.1 to report", lambda: "updated from version 1.0.0 to 1.0.1" in status(), 150)
    left = mac(f"ls -A {APP_DIR}; ls {REMOTE}/www | wc -l", check=False)
    check("1.0.1 was installed and started", ok and installed_version() == "1.0.1" and running())
    signed_in = wait_for("the account", lambda: mac(f"defaults read {SUITE} accountStatus 2>/dev/null", check=False) == "signed in", 30)
    check("1.0.1 is still signed in, from a file only this user can read", signed_in and account_file_private())
    check("nothing was left over (staging, backup)", left.splitlines()[0] == "AudioNet.app" and ".AudioNet" not in left)

    print("3. broken 1.0.2 is rolled back")
    serve("1.0.2")
    quit_app()
    launch()
    ok = wait_for("the rollback", lambda: "1.0.2 did not start" in status(), 180)
    check("the broken update was rolled back and 1.0.1 runs again", ok and installed_version() == "1.0.1" and running())
    skip = mac(f"defaults read {SUITE} skipUpdateVersion 2>/dev/null", check=False)
    check("1.0.2 will not be offered again", skip == "1.0.2")
    left = mac(f"ls -A {APP_DIR}", check=False)
    check("nothing was left over after the rollback", left == "AudioNet.app")
finally:
    quit_app()
    mac(f"pkill -f 'http.server {PORT}'; rm -rf {REMOTE} {APP_DIR}; defaults delete {SUITE} 2>/dev/null; "
        f"security delete-generic-password -s org.audionet.AudioNet.device.test.{PROFILE} >/dev/null 2>&1; "
        f"rm -f \"$HOME/Library/Application Support/AudioNet/account-test-{PROFILE}.json\"; true", check=False)
print("macOS update test:", "PASS" if results and all(results) else "FAIL")
