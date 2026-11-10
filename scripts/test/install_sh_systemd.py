"""Tests deploy/official/install.sh's systemd user service on this Linux
computer, as a throwaway user (`antest`, with lingering so it has its own
systemd user manager; deleted afterwards), against the real GitHub release
and a throwaway AudioNet server on 127.0.0.1:

* a fresh install piped into sh, with "Start AudioNet when you log in" and
  signing in: the service is written, enabled and running, and the device
  is visible;
* running it again (reinstall): the service is stopped and started again;
* uninstalling: the service is stopped, disabled and removed, and the
  device is no longer visible.

Needs passwordless sudo (to create and delete the user) and a release
build of the server.
"""
import os, pwd, shutil, subprocess, sys, tempfile, time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
sys.path.insert(0, HERE)
from install_sh import Failed, run  # noqa: E402
from visibility import visible_devices  # noqa: E402
import requests  # noqa: E402

USER = "antest"
SERVER_BIN = os.path.join(ROOT, "target", "release", "audionet-server")
PORT = 18753
SERVER = f"http://127.0.0.1:{PORT}"
PASSWORD = "install-test-password"
results = []


def check(name, ok, detail=""):
    results.append((name, ok))
    print(f"\n### {name}: {'PASS' if ok else 'FAIL'}{'' if ok or not detail else '  (' + str(detail)[-1500:] + ')'}", flush=True)


def sh(*args, **kw):
    return subprocess.run(args, capture_output=True, text=True, **kw)


try:
    pwd.getpwnam(USER)
    raise SystemExit(f"the user {USER} exists already; remove it first (sudo userdel -r {USER})")
except KeyError:
    pass

work = tempfile.mkdtemp(prefix="audionet-install-systemd-")
os.chmod(work, 0o755)
script = os.path.join(work, "install.sh")
shutil.copy(os.path.join(ROOT, "deploy", "official", "install.sh"), script)
os.chmod(script, 0o644)
cfg = os.path.join(work, "server.toml")
open(cfg, "w").write(f'public_url = "{SERVER}"\nbind = "127.0.0.1:{PORT}"\ndatabase = "{work}/test.db"\nweb_root = "{ROOT}/web"\n')
sh(SERVER_BIN, "-c", cfg, "user", "add", "tester", input=PASSWORD + "\n", check=True)
server = subprocess.Popen([SERVER_BIN, "-c", cfg, "serve"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

sh("sudo", "-n", "useradd", "-m", "-s", "/bin/bash", USER, check=True)
uid = pwd.getpwnam(USER).pw_uid
sh("sudo", "-n", "loginctl", "enable-linger", USER, check=True)
for _ in range(50):
    if os.path.exists(f"/run/user/{uid}/bus"):
        break
    time.sleep(0.2)
ENV = ["env", f"HOME=/home/{USER}", f"XDG_RUNTIME_DIR=/run/user/{uid}",
       f"DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/{uid}/bus", "PATH=/usr/local/bin:/usr/bin:/bin",
       f"AUDIONET_SERVER={SERVER}"]


def as_user(script_text):
    return sh("sudo", "-n", "-u", USER, *ENV, "sh", "-c", script_text).stdout.strip()


def installer(steps):
    return run(["sudo", "-n", "-u", USER, *ENV, "sh", "-c", f"cd && cat {script} | sh"], steps, timeout=240, echo=False)


def svc(what):
    return as_user(f"systemctl --user {what} audionet.service")


BIN = f"/home/{USER}/.local/bin/audionet"
UNIT = f"/home/{USER}/.config/systemd/user/audionet.service"
try:
    s = requests.Session()
    s.post(f"{SERVER}/api/v1/login", json={"username": "tester", "password": PASSWORD}, headers={"Origin": SERVER}).raise_for_status()

    code, out = installer([
        ("Install folder for the audionet program", ""),
        ("Start AudioNet when you log in", ""),
        ("Add the audionet command to your PATH", ""),
        ("Sign in to your AudioNet account now?", ""),
        ("Server address", ""),
        ("Account name:", "tester"),
        ("Name for this computer", "Systemd test"),
        ("Password for tester", PASSWORD),
    ])
    check("fresh install with the service finishes", code == 0 and "AudioNet will start when you log in." in out
          and "AudioNet is running: this computer is visible" in out, out)
    check("...the service file runs the installed program", f"ExecStart={BIN} node run" in as_user(f"cat {UNIT}"))
    check("...enabled", svc("is-enabled") == "enabled", svc("is-enabled"))
    check("...running", svc("is-active") == "active", svc("is-active"))
    time.sleep(3)
    check("...the device is visible", "Systemd test" in visible_devices(SERVER, s))
    pid1 = svc("show -p MainPID --value")

    code, out = installer([("Choice, 1 to 3", "2"), ("Stop AudioNet now?", "")])
    pid2 = svc("show -p MainPID --value")
    check("reinstall: the service is stopped and started again", code == 0 and "AudioNet stopped." in out
          and svc("is-active") == "active" and pid1 != pid2, out)
    check("...enabled still, and still one service file", svc("is-enabled") == "enabled")
    time.sleep(3)
    check("...visible again", "Systemd test" in visible_devices(SERVER, s))

    code, out = installer([("Choice, 1 to 3", "3"), ("Uninstall AudioNet?", "y"), ("Stop AudioNet now?", ""),
                           ("Also remove", "y")])
    check("uninstall finishes", code == 0 and "AudioNet was uninstalled." in out, out)
    check("...the service stopped and removed", svc("is-active") != "active" and as_user(f"ls {UNIT} 2>/dev/null") == "")
    check("...the program removed", as_user(f"ls {BIN} 2>/dev/null") == "")
    time.sleep(1)
    check("...no longer visible", "Systemd test" not in visible_devices(SERVER, s))
except Failed as e:
    check("the installer asked what was expected", False, str(e))
finally:
    sh("sudo", "-n", "loginctl", "disable-linger", USER)
    sh("sudo", "-n", "systemctl", "stop", f"user@{uid}.service")
    sh("sudo", "-n", "pkill", "-u", USER)
    time.sleep(1)
    sh("sudo", "-n", "userdel", "-r", USER)
    server.terminate()
    shutil.rmtree(work, ignore_errors=True)

failed = [n for n, ok in results if not ok]
print(f"\n{len(results) - len(failed)} of {len(results)} checks passed")
for n in failed:
    print("FAILED:", n)
sys.exit(1 if failed else 0)
