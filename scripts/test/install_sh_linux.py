"""Tests deploy/official/install.sh on Linux, in Docker containers, driven
through a terminal (install_sh.py) as a person would answer it, against
the real GitHub release and a throwaway AudioNet server on this computer.

* Ubuntu 22.04, the whole life: a fresh install piped into sh (`cat
  install.sh | sh`, like `curl … | sh`) with the release signature and
  download checked, the PATH line, signing in (the password typed at the
  program's own prompt), starting in the background, and the device
  visible on the server; running it again (up to date: reinstall, the
  device stopped and started again); an update from 1.2.5; uninstalling
  with the sign-in (nothing left: program, guide, PATH line, settings,
  process; the device no longer visible).
* Refused before anything is installed: a manifest that is not signed
  with the release key, a download that does not match the manifest, a
  manifest for another product, a missing manifest; no terminal; root.
* Refused with a reason: Ubuntu 20.04 (glibc 2.31), Alpine (musl),
  Ubuntu 22.04 without the ALSA library.
* Debian 12 and Fedora: a fresh install, then uninstall.

Needs Docker, the release files of the current version in REL_DIR (for
the tampering mirror; default ~/.claude/jobs/…/tmp/rel), and a release
build of the server (cargo build --release -p audionet-server).
The systemd user service is tested by install_sh_systemd.py.
"""
import json, os, shutil, subprocess, sys, tempfile, threading, time, urllib.request
import http.server, socketserver

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
sys.path.insert(0, HERE)
from install_sh import Failed, run  # noqa: E402

SCRIPT = os.path.join(ROOT, "deploy", "official", "install.sh")
SERVER_BIN = os.path.join(ROOT, "target", "release", "audionet-server")
REL = os.environ["REL_DIR"]
SERVER_PORT, MIRROR_PORT = 18751, 18752
SERVER = f"http://127.0.0.1:{SERVER_PORT}"
PASSWORD = "install-test-password"
VERSION = json.load(open(os.path.join(REL, "latest-linux.json")))["version"]
results = []


def check(name, ok, detail=""):
    results.append((name, ok))
    print(f"\n### {name}: {'PASS' if ok else 'FAIL'}{'' if ok or not detail else '  (' + str(detail)[-1500:] + ')'}", flush=True)


def sh(*args, **kw):
    return subprocess.run(args, capture_output=True, text=True, **kw)


# ─── a throwaway server and a release mirror on this computer ─────────────

work = tempfile.mkdtemp(prefix="audionet-install-test-")
cfg = os.path.join(work, "server.toml")
open(cfg, "w").write(f'public_url = "{SERVER}"\nbind = "127.0.0.1:{SERVER_PORT}"\n'
                     f'database = "{work}/test.db"\nweb_root = "{ROOT}/web"\n')
sh(SERVER_BIN, "-c", cfg, "user", "add", "tester", input=PASSWORD + "\n", check=True)
server = subprocess.Popen([SERVER_BIN, "-c", cfg, "serve"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

mirror = os.path.join(work, "mirror")
tar = f"audionet-linux-x64-{VERSION}.tar.gz"
for case in ("good", "badsig", "badtar", "wrongproduct"):
    d = os.path.join(mirror, case)
    os.makedirs(d)
    for f in ("latest-linux.json", "latest-linux.json.sig", tar):
        shutil.copy(os.path.join(REL, f), d)
m = json.load(open(os.path.join(mirror, "badsig", "latest-linux.json")))
m["sha256"] = "0" * 64  # changed after signing
json.dump(m, open(os.path.join(mirror, "badsig", "latest-linux.json"), "w"), indent=2)
with open(os.path.join(mirror, "badtar", tar), "r+b") as f:  # one byte changed
    f.seek(1000)
    b = f.read(1)
    f.seek(1000)
    f.write(bytes([b[0] ^ 0xFF]))
m = json.load(open(os.path.join(mirror, "wrongproduct", "latest-linux.json")))
m["product"] = "audionet-windows-x64"
json.dump(m, open(os.path.join(mirror, "wrongproduct", "latest-linux.json"), "w"), indent=2)
os.makedirs(os.path.join(mirror, "missing"))


class Quiet(http.server.SimpleHTTPRequestHandler):
    def __init__(self, *a, **kw):
        super().__init__(*a, directory=mirror, **kw)

    def log_message(self, *a):
        pass


socketserver.TCPServer.allow_reuse_address = True
httpd = socketserver.TCPServer(("127.0.0.1", MIRROR_PORT), Quiet)
threading.Thread(target=httpd.serve_forever, daemon=True).start()


def api():
    import requests
    s = requests.Session()
    s.post(f"{SERVER}/api/v1/login", json={"username": "tester", "password": PASSWORD}, headers={"Origin": SERVER}).raise_for_status()
    return s


sys.path.insert(0, HERE)
from visibility import registered_devices, visible_devices  # noqa: E402

# ─── containers ─────────────────────────────────────────────────────────────

IMAGES = {
    "ubuntu22": ("ubuntu:22.04", "apt-get update -qq && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq curl ca-certificates libasound2 openssl >/dev/null"),
    "noalsa": ("ubuntu:22.04", "apt-get update -qq && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq curl ca-certificates openssl >/dev/null"),
    "ubuntu20": ("ubuntu:20.04", "apt-get update -qq && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq curl ca-certificates libasound2 >/dev/null"),
    "debian12": ("debian:12", "apt-get update -qq && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq curl ca-certificates libasound2 openssl >/dev/null"),
    "fedora": ("fedora:41", "dnf install -y -q alsa-lib openssl >/dev/null"),
    "alpine": ("alpine:3.20", "apk add -q curl"),
}
containers = []


def start(kind):
    image, prepare = IMAGES[kind]
    name = f"audionet-install-{kind}-{os.getpid()}"
    r = sh("docker", "run", "-d", "--name", name, "--network", "host", "-v", f"{SCRIPT}:/install.sh:ro", image, "sleep", "infinity")
    if r.returncode != 0:
        raise SystemExit(r.stderr)
    containers.append(name)
    useradd = "adduser -D tester" if kind == "alpine" else "useradd -m tester"
    r = sh("docker", "exec", name, "sh", "-c", f"{prepare} && {useradd}")
    if r.returncode != 0:
        raise SystemExit(f"{kind}: {r.stderr}")
    return name


def installer(name, steps, env=None, piped=True, user="tester", tty=True, timeout=240):
    """Runs the installer in container `name` (as `user`) with `steps`."""
    envs = []
    for k, v in (env or {}).items():
        envs += ["-e", f"{k}={v}"]
    cmd = ["docker", "exec", "-u", user, *envs] + (["-it"] if tty else ["-i"]) + \
          ["-w", "/home/tester" if user == "tester" else "/", name, "sh", "-c",
           "cat /install.sh | sh" if piped else "sh /install.sh"]
    if not tty:
        r = sh(*cmd, stdin=subprocess.DEVNULL, timeout=timeout)
        return r.returncode, r.stdout + r.stderr
    return run(cmd, steps, timeout=timeout, echo=False)


def inside(name, script, user="tester"):
    return sh("docker", "exec", "-u", user, name, "sh", "-c", script).stdout.strip()


def running_count(name):
    return int(inside(name, "n=0; for d in /proc/[0-9]*; do tr '\\0' ' ' <$d/cmdline 2>/dev/null | grep -q '^/home/tester/.local/bin/audionet node run' && n=$((n+1)); done; echo $n") or 0)


BIN = "/home/tester/.local/bin/audionet"

try:
    # ── Ubuntu 22.04: the whole life ──
    u = start("ubuntu22")
    code, out = installer(u, [
        ("Install folder for the audionet program", ""),
        ("Add the audionet command to your PATH", ""),
        ("Sign in to your AudioNet account now?", ""),
        ("Server address", SERVER),
        ("Account name:", "tester"),
        ("Name for this computer", "Install test"),
        ("Password for tester", PASSWORD),
        ("Start AudioNet now, in the background?", ""),
    ], env={"AUDIONET_SERVER": SERVER})
    check("fresh install (piped into sh) finishes", code == 0, out)
    check("...the release signature is checked", "Release signature checked: signed with the AudioNet release key." in out, out)
    check("...the download is checked", "Download checked: size and SHA-256 match the release." in out, out)
    check(f"...{VERSION} installed in ~/.local/bin", inside(u, f"{BIN} --version") == f"audionet {VERSION}")
    check("...its guide in ~/.local/share/audionet", inside(u, "ls ~/.local/share/audionet | tr '\\n' ' '") == "LICENSE README.md")
    check("...the PATH line in ~/.profile", "# Added by the AudioNet installer" in inside(u, "cat ~/.profile")
          and inside(u, ". ~/.profile; command -v audionet") == BIN)
    check("...signed in", "Server: " + SERVER in inside(u, f"{BIN} node status"))
    check("...running in the background", running_count(u) == 1, running_count(u))
    time.sleep(2)
    s = api()
    check("...and visible to the account's other devices", "Install test" in visible_devices(SERVER, s))
    check("...no colour or cursor codes in the output", "\x1b" not in out)

    code, out = installer(u, [
        ("Choice, 1 to 3", "2"),
        ("Stop AudioNet now?", ""),
    ], piped=False)
    check("run again: says it is up to date, offers reinstall", f"Installed: AudioNet {VERSION}" in out and "It is up to date" in out, out)
    check("...reinstall stops the device and starts it again", code == 0 and "AudioNet stopped." in out
          and "running in the background" in out and running_count(u) == 1, out)
    check("...the PATH line is not added twice", inside(u, "grep -c 'Added by the AudioNet installer' ~/.profile") == "1")

    # An older copy: 1.2.5 (with its device stopped first).
    inside(u, f"for d in /proc/[0-9]*; do tr '\\0' ' ' <$d/cmdline 2>/dev/null | grep -q '^{BIN} node run' && kill ${{d#/proc/}}; done; "
              f"cd /tmp && curl -fsSL -o old.tgz https://github.com/mad-gamer26/AudioNet/releases/download/v1.2.5/audionet-linux-x64-1.2.5.tar.gz "
              f"&& tar -xzf old.tgz && cp audionet-linux-x64-1.2.5/audionet {BIN} && {BIN} node run -b >/dev/null")
    check("(set up: 1.2.5 installed and running)", inside(u, f"{BIN} --version") == "audionet 1.2.5" and running_count(u) == 1)
    code, out = installer(u, [
        ("Choice, 1 to 3", ""),
        ("Stop AudioNet now?", ""),
    ])
    check("an older copy: offers the update first", "Installed: AudioNet 1.2.5" in out and f"1. Update to AudioNet {VERSION}" in out, out)
    check("...updates it and starts it again", code == 0 and inside(u, f"{BIN} --version") == f"audionet {VERSION}" and running_count(u) == 1, out)

    code, out = installer(u, [
        ("Choice, 1 to 3", "3"),
        ("Uninstall AudioNet?", "y"),
        ("Stop AudioNet now?", ""),
        ("Also remove this computer's AudioNet sign-in from it?", "y"),
    ])
    check("uninstall finishes", code == 0 and "AudioNet was uninstalled." in out, out)
    check("...the device stopped", running_count(u) == 0)
    check("...nothing left: program, guide, settings",
          inside(u, f"ls {BIN} ~/.local/share/audionet ~/.config/audionet 2>/dev/null | wc -l") == "0")
    check("...the PATH line removed", "AudioNet" not in inside(u, "cat ~/.profile"))
    time.sleep(1)
    check("...no longer visible to the account", "Install test" not in visible_devices(SERVER, s))
    check("...and it says how to remove it from the account", "Manage devices" in out)
    for d in registered_devices(SERVER, s).values():
        s.delete(f"{SERVER}/api/v1/nodes/{d['node_id']}", headers={"Origin": SERVER})

    # Uninstall when declined: nothing changes (install first, quickly).
    code, out = installer(u, [("Install folder", ""), ("PATH", "n"), ("Sign in to your AudioNet account now?", "n")])
    code, out = installer(u, [("Choice, 1 to 3", "3"), ("Uninstall AudioNet?", "")])
    check("declining uninstall changes nothing", "Nothing was changed." in out and inside(u, f"{BIN} --version") == f"audionet {VERSION}", out)
    inside(u, f"rm -rf {BIN} ~/.local/share/audionet")

    # ── refused before anything is installed ──
    for case, message in [
        ("badsig", "is not signed with the AudioNet release key"),
        ("wrongproduct", "is not one for audionet-linux-x64"),
        ("missing", "could not download the release manifest"),
    ]:
        code, out = installer(u, [], env={"AUDIONET_RELEASE_URL": f"http://127.0.0.1:{MIRROR_PORT}/{case}"})
        check(f"{case}: refused before any question", code == 1 and message in out and "Install folder" not in out, out)
    code, out = installer(u, [("Install folder", ""), ("PATH", "n")],
                          env={"AUDIONET_RELEASE_URL": f"http://127.0.0.1:{MIRROR_PORT}/badtar"})
    check("badtar: a download that does not match is refused", code == 1
          and "does not match the release manifest (size or SHA-256); nothing was installed" in out, out)
    check("...and nothing was installed", inside(u, f"ls {BIN} 2>/dev/null | wc -l") == "0")
    code, out = installer(u, [], tty=False)
    check("no terminal: says it needs one", code == 1 and "needs a terminal" in out, out)
    code, out = installer(u, [], user="root")
    check("root: refused", code == 1 and "not as root" in out, out)

    # ── refused with a reason ──
    for kind, message in [("ubuntu20", "needs glibc 2.34 or later"), ("alpine", "needs a system with glibc 2.34 or later")]:
        c = start(kind)
        code, out = installer(c, [])
        check(f"{kind}: refused with the reason", code == 1 and message in out, out)
    c = start("noalsa")
    code, out = installer(c, [("Install folder", ""), ("PATH", "n")])
    check("no ALSA library: says to install libasound2, installs nothing", code == 1 and "libasound2" in out
          and inside(c, f"ls {BIN} 2>/dev/null | wc -l") == "0", out)

    # ── other distributions ──
    for kind in ("debian12", "fedora"):
        c = start(kind)
        code, out = installer(c, [("Install folder", ""), ("PATH", ""), ("Sign in to your AudioNet account now?", "n")])
        check(f"{kind}: installs", code == 0 and inside(c, f"{BIN} --version") == f"audionet {VERSION}"
              and "Release signature checked" in out, out)
        code, out = installer(c, [("Choice, 1 to 3", "3"), ("Uninstall AudioNet?", "y"), ("Also remove", "y")])
        check(f"{kind}: uninstalls", code == 0 and inside(c, f"ls {BIN} 2>/dev/null | wc -l") == "0"
              and "AudioNet" not in inside(c, "cat ~/.profile 2>/dev/null"), out)
except Failed as e:
    check("the installer asked what was expected", False, str(e))
finally:
    for c in containers:
        sh("docker", "rm", "-f", c)
    httpd.shutdown()
    server.terminate()
    shutil.rmtree(work, ignore_errors=True)

failed = [n for n, ok in results if not ok]
print(f"\n{len(results) - len(failed)} of {len(results)} checks passed")
for n in failed:
    print("FAILED:", n)
sys.exit(1 if failed else 0)
