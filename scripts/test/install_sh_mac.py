"""Tests deploy/official/install.sh on a Mac, driven through a terminal
(install_sh.py) as a person would answer it, against the real GitHub
release, in sandbox folders (AUDIONET_APP_FOLDERS) so the AudioNet in
/Applications and its settings are never touched; the app is never opened
by the installer (AUDIONET_NO_OPEN), only started directly with a test
profile where a running copy is needed.

* A fresh install piped into sh: the download and Apple code signature
  (AudioNet's team) checked; running again: up to date, Open, Reinstall;
  a running copy is quit first (asked); an update from 1.2.5; uninstall
  (declined, then done, with the sign-in and settings: the files under a
  sandbox home, and `defaults delete` checked through a stand-in).
* A folder that may not be written: asked again.
* Refused, nothing installed: a download that does not match the
  manifest; an app whose signature is broken; an app signed by someone
  else (ad hoc).

Run on the Mac, from the repository: python3 scripts/test/install_sh_mac.py
"""
import json, os, plistlib, shutil, subprocess, sys, tempfile, threading, time, urllib.request
import http.server, socketserver

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
sys.path.insert(0, HERE)
from install_sh import Failed, run  # noqa: E402

SCRIPT = os.path.join(ROOT, "deploy", "official", "install.sh")
RELEASE = "https://github.com/mad-gamer26/AudioNet/releases"
MIRROR_PORT = 18754
results = []
sandbox = tempfile.mkdtemp(prefix="audionet-install-mac-")
A, B = os.path.join(sandbox, "A"), os.path.join(sandbox, "B")
os.makedirs(A)
os.makedirs(B)


def check(name, ok, detail=""):
    results.append((name, ok))
    print(f"\n### {name}: {'PASS' if ok else 'FAIL'}{'' if ok or not detail else '  (' + str(detail)[-1500:] + ')'}", flush=True)


def sh(*args, **kw):
    return subprocess.run(args, capture_output=True, text=True, **kw)


def env(**extra):
    e = dict(os.environ, AUDIONET_APP_FOLDERS=f"{A}:{B}", AUDIONET_NO_OPEN="1")
    e.update(extra)
    return e


def installer(steps, piped=True, **extra):
    cmd = ["sh", "-c", f"cat '{SCRIPT}' | sh" if piped else f"sh '{SCRIPT}'"]
    return run(cmd, steps, timeout=300, env=env(**extra), echo=False)


def version(app):
    try:
        return plistlib.load(open(os.path.join(app, "Contents", "Info.plist"), "rb"))["CFBundleShortVersionString"]
    except OSError:
        return None


def running(app):
    exe = os.path.join(app, "Contents", "MacOS", "AudioNet")
    return [l.split()[0] for l in sh("ps", "-axo", "pid=,comm=").stdout.splitlines() if l.split(None, 1)[1:] == [exe]]


latest = json.load(urllib.request.urlopen(f"{RELEASE}/latest/download/latest-macos.json"))
VERSION = latest["version"]
APP_A = os.path.join(A, "AudioNet.app")
APP_B = os.path.join(B, "AudioNet.app")
httpd = None
try:
    # ── fresh, then again ──
    code, out = installer([("Install folder", "")])
    check("fresh install (piped into sh) finishes", code == 0, out)
    check("...the download is checked", "Download checked: size and SHA-256 match the release." in out, out)
    check("...the app's signature is checked", "App checked: signed by AudioNet's Apple developer team." in out, out)
    check(f"...{VERSION} in the first folder", version(APP_A) == VERSION, version(APP_A))
    check("...validly signed in place", sh("codesign", "--verify", "--deep", "--strict", APP_A).returncode == 0)
    check("...no colour or cursor codes", "\x1b" not in out)

    code, out = installer([("Choice, 1 to 4", "")], piped=False)
    check("run again: up to date, Open", code == 0 and f"Installed: AudioNet {VERSION} in {A}" in out
          and "It is up to date" in out and "not opened: testing" in out, out)
    code, out = installer([("Choice, 1 to 4", "2")])
    check("Reinstall", code == 0 and f"AudioNet {VERSION} is installed in {A}" in out and version(APP_A) == VERSION, out)

    # A running copy (a test profile, so no real account is used).
    app = subprocess.Popen([os.path.join(APP_A, "Contents", "MacOS", "AudioNet"), "-AudioNetProfile", "installtest",
                            "-AudioNetResetProfile", "YES"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(4)
    check("(set up: a copy running)", len(running(APP_A)) == 1)
    code, out = installer([("Choice, 1 to 4", "2"), ("Quit AudioNet now?", "n")])
    check("a running copy: declining to quit changes nothing", "Nothing was changed." in out and len(running(APP_A)) == 1, out)
    code, out = installer([("Choice, 1 to 4", "2"), ("Quit AudioNet now?", "")])
    check("...agreeing quits it, then reinstalls", code == 0 and "AudioNet quit." in out and not running(APP_A)
          and version(APP_A) == VERSION, out)
    app.wait(10)

    # ── an update from 1.2.5, in the second folder ──
    shutil.rmtree(APP_A)
    z = os.path.join(sandbox, "old.zip")
    urllib.request.urlretrieve(f"{RELEASE}/download/v1.2.5/AudioNet-macOS-1.2.5.zip", z)
    sh("ditto", "-x", "-k", z, B, check=True)
    check("(set up: 1.2.5 in the second folder)", version(APP_B) == "1.2.5")
    code, out = installer([("Choice, 1 to 3", "")])
    check("an older copy: offers the update first, updates it", code == 0 and "Installed: AudioNet 1.2.5" in out
          and f"1. Update to AudioNet {VERSION}" in out and version(APP_B) == VERSION, out)
    check("...no copy left aside", sorted(os.listdir(B)) == ["AudioNet.app"], os.listdir(B))

    # ── uninstall ──
    code, out = installer([("Choice, 1 to 4", "3"), ("Uninstall AudioNet?", "")])
    check("uninstall declined: nothing changed", "Nothing was changed." in out and version(APP_B) == VERSION, out)
    home = os.path.join(sandbox, "home")
    support = os.path.join(home, "Library", "Application Support", "AudioNet")
    os.makedirs(support)
    open(os.path.join(support, "accounts.json"), "w").write("{}")
    fakebin = os.path.join(sandbox, "bin")
    os.makedirs(fakebin)
    log = os.path.join(sandbox, "defaults.log")
    open(os.path.join(fakebin, "defaults"), "w").write(f'#!/bin/sh\necho "$@" >> "{log}"\n')
    os.chmod(os.path.join(fakebin, "defaults"), 0o755)
    code, out = installer([("Choice, 1 to 4", "3"), ("Uninstall AudioNet?", "y"), ("Also remove", "y")],
                          HOME=home, PATH=f"{fakebin}:{os.environ['PATH']}")
    check("uninstall with the sign-in and settings", code == 0 and "AudioNet was uninstalled." in out
          and not os.path.exists(APP_B) and not os.path.exists(support), out)
    check("...its preferences deleted", open(log).read().strip() == "delete org.audionet.AudioNet")
    check("...says how to remove it from the account", "Manage devices" in out)

    # ── a folder that may not be written ──
    code, out = installer([("Install folder", "/System/AudioNetTest"), ("Install folder", A)])
    check("a folder that may not be written: asked again", code == 0 and "You may not write to /System/AudioNetTest" in out
          and version(APP_A) == VERSION, out)
    shutil.rmtree(APP_A)

    # ── tampered downloads, from a mirror on this Mac ──
    mirror = os.path.join(sandbox, "mirror")
    good_zip = os.path.join(sandbox, latest["file"])
    urllib.request.urlretrieve(f"{RELEASE}/latest/download/{latest['file']}", good_zip)

    def case(name, zip_path):
        d = os.path.join(mirror, name)
        os.makedirs(d)
        shutil.copy(zip_path, os.path.join(d, latest["file"]))
        data = open(zip_path, "rb").read()
        import hashlib
        m = dict(latest, sha256=hashlib.sha256(data).hexdigest(), size=len(data))
        json.dump(m, open(os.path.join(d, "latest-macos.json"), "w"), indent=2)
        return d

    d = case("badzip", good_zip)
    with open(os.path.join(d, latest["file"]), "r+b") as f:  # changed after the manifest
        f.seek(5000)
        b = f.read(1)
        f.seek(5000)
        f.write(bytes([b[0] ^ 0xFF]))

    def repack(name, change):
        t = os.path.join(sandbox, name)
        sh("ditto", "-x", "-k", good_zip, t, check=True)
        change(os.path.join(t, "AudioNet.app"))
        z = os.path.join(sandbox, f"{name}.zip")
        sh("ditto", "-c", "-k", "--keepParent", os.path.join(t, "AudioNet.app"), z, check=True)
        return z

    case("broken", repack("broken", lambda app: open(os.path.join(app, "Contents", "extra.txt"), "w").write("x")))
    case("adhoc", repack("adhoc", lambda app: sh("codesign", "-f", "--deep", "-s", "-", app, check=True)))

    class Quiet(http.server.SimpleHTTPRequestHandler):
        def __init__(self, *a, **kw):
            super().__init__(*a, directory=mirror, **kw)

        def log_message(self, *a):
            pass

    socketserver.TCPServer.allow_reuse_address = True
    httpd = socketserver.TCPServer(("127.0.0.1", MIRROR_PORT), Quiet)
    threading.Thread(target=httpd.serve_forever, daemon=True).start()
    for name, message in [("badzip", "does not match the release manifest (size or SHA-256); nothing was installed"),
                          ("broken", "is not validly code-signed; nothing was installed"),
                          ("adhoc", "is not signed by AudioNet's developer; nothing was installed")]:
        code, out = installer([("Install folder", "")], AUDIONET_RELEASE_URL=f"http://127.0.0.1:{MIRROR_PORT}/{name}")
        check(f"{name}: refused, nothing installed", code == 1 and message in out and not os.path.exists(APP_A), out)

    # An update refused the same way keeps the copy that was there.
    sh("ditto", "-x", "-k", z, B, check=True)  # 1.2.5 again
    code, out = installer([("Choice, 1 to 3", "")], AUDIONET_RELEASE_URL=f"http://127.0.0.1:{MIRROR_PORT}/broken")
    check("a refused update keeps the old copy", code == 1 and version(APP_B) == "1.2.5", out)
except Failed as e:
    check("the installer asked what was expected", False, str(e))
finally:
    if httpd:
        httpd.shutdown()
    for app in (APP_A, APP_B):
        for pid in running(app):
            os.kill(int(pid), 9)
    sh("defaults", "delete", "org.audionet.AudioNet.test.installtest")
    shutil.rmtree(sandbox, ignore_errors=True)

failed = [n for n, ok in results if not ok]
print(f"\n{len(results) - len(failed)} of {len(results)} checks passed")
for n in failed:
    print("FAILED:", n)
sys.exit(1 if failed else 0)
