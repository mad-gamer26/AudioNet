"""End-to-end test of the desktop app's automatic update, on real
processes and files, with a throwaway key and a local web server:

1. A test build (update source http://127.0.0.1:8751, test public key) is
   installed into a temporary folder and started.
2. A signed update (version 9.9.9, plus a marker file) is published.
   "Check for updates now" must: download, verify, unpack, replace the
   files, start the new copy (window shown, as before), exit the old copy
   cleanly, and leave no downloaded zip, staging folder or old files.
3. Checking again must not reinstall 9.9.9 (the installed program still
   reports its real version: the "mislabeled release" loop guard).
4. A manifest changed after signing is rejected; nothing is installed.
5. An update whose app cannot start is rolled back; the old copy keeps
   running with its original files.

It runs under AUDIONET_TEST_PROFILE, so a copy of AudioNet the user is
running and their settings are never touched; the real release key is
never used. Requires: pip install comtypes cryptography
"""
import ctypes, glob, hashlib, http.server, json, os, shutil, subprocess, sys, tempfile, threading, time, winreg, zipfile
from ctypes import wintypes
import comtypes.client

comtypes.client.GetModule("UIAutomationCore.dll")
from comtypes.gen import UIAutomationClient as UIA  # noqa: E402
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import desktop_log  # noqa: E402

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
PROFILE = "updtest"
CLASS = f"AudioNetDesktop-{PROFILE}"
REG_KEY = rf"Software\AudioNet-Test-{PROFILE}"
PORT = 8751
work = tempfile.mkdtemp(prefix="audionet-update-e2e-")
serve_dir = os.path.join(work, "site")
install_dir = os.path.join(work, "installed")
os.makedirs(serve_dir)

u = ctypes.WinDLL("user32")
u.FindWindowW.restype = wintypes.HWND
u.FindWindowW.argtypes = [wintypes.LPCWSTR, wintypes.LPCWSTR]
u.GetWindowThreadProcessId.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.DWORD)]
u.IsWindowVisible.argtypes = [wintypes.HWND]
uia = comtypes.client.CreateObject(UIA.CUIAutomation, interface=UIA.IUIAutomation)
results = []

def check(name, ok, detail=""):
    results.append((name, ok))
    print(f"{name}: {'PASS' if ok else 'FAIL'}{'  (' + detail + ')' if detail and not ok else ''}", flush=True)

def wait_for(pred, timeout=30):
    end = time.time() + timeout
    while time.time() < end:
        if pred():
            return True
        time.sleep(0.25)
    return False

def window_pid():
    h = u.FindWindowW(CLASS, None)
    if not h:
        return None
    pid = wintypes.DWORD()
    u.GetWindowThreadProcessId(h, ctypes.byref(pid))
    return pid.value

def children(el):
    arr = el.FindAll(UIA.TreeScope_Descendants, uia.CreateTrueCondition())
    return [arr.GetElement(i) for i in range(arr.Length)]

def ui():
    return uia.ElementFromHandle(u.FindWindowW(CLASS, None))

def press(prefix):
    el = next(c for c in children(ui()) if (c.CurrentName or "").replace("&", "").startswith(prefix))
    el.GetCurrentPattern(UIA.UIA_InvokePatternId).QueryInterface(UIA.IUIAutomationInvokePattern).Invoke()

def status_log():
    return desktop_log.read(uia, UIA, ui())

def sign_tool(*args):
    out = subprocess.run([sys.executable, os.path.join(ROOT, "scripts", "release_sign.py"), *args],
                         check=True, capture_output=True, text=True)
    return out.stdout.strip()

def publish(zip_name, files, version, tamper=False):
    """Writes a package and its signed manifest into the served folder."""
    path = os.path.join(serve_dir, zip_name)
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as z:
        for name, data in files.items():
            z.writestr(name, data)
    sign_tool("manifest", key_file, path, version, serve_dir)
    if tamper:
        body = open(os.path.join(serve_dir, "latest.json"), "rb").read()
        open(os.path.join(serve_dir, "latest.json"), "wb").write(body.replace(b'"schema": 1', b'"schema":  1'))

def reg_value(name):
    try:
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER, REG_KEY) as k:
            return winreg.QueryValueEx(k, name)[0]
    except FileNotFoundError:
        return None

class Quiet(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *a):
        pass

server = http.server.ThreadingHTTPServer(("127.0.0.1", PORT), lambda *a, **k: Quiet(*a, directory=serve_dir, **k))
threading.Thread(target=server.serve_forever, daemon=True).start()
key_file = os.path.join(work, "test-signing-key.pem")
pub_file = os.path.join(work, "test-public-key.txt")
env = dict(os.environ, APPDATA=os.path.join(work, "appdata"), AUDIONET_TEST_PROFILE=PROFILE)
procs = []
try:
    if window_pid():
        raise SystemExit("a previous update test instance is still running")
    try:
        winreg.DeleteKey(winreg.HKEY_CURRENT_USER, REG_KEY)
    except FileNotFoundError:
        pass
    sign_tool("keygen", key_file, pub_file)
    build = subprocess.run(["powershell", "-ExecutionPolicy", "Bypass", "-File", os.path.join(ROOT, "scripts", "package-windows.ps1"),
                            "-UpdateUrl", f"http://127.0.0.1:{PORT}/latest.json", "-PublicKeyFile", pub_file,
                            "-SigningKey", key_file, "-OutDir", os.path.join(work, "build"),
                            "-TargetDir", os.path.join(ROOT, "target", "updatetest")],
                           cwd=ROOT, capture_output=True, text=True)
    if build.returncode != 0:
        raise SystemExit("test build failed:\n" + build.stdout[-2000:] + build.stderr[-2000:])
    built = glob.glob(os.path.join(work, "build", "audionet-windows-x64-*.zip"))[0]
    real_version = os.path.basename(built)[len("audionet-windows-x64-"):-len(".zip")]
    with zipfile.ZipFile(built) as z:
        z.extractall(install_dir)
        package = {n: z.read(n) for n in z.namelist()}
    exe = os.path.join(install_dir, "audionet-desktop.exe")

    # 1-2. A good update.
    publish("audionet-windows-x64-9.9.9.zip", {**package, "UPDATE-MARKER.txt": b"9.9.9"}, "9.9.9")
    old = subprocess.Popen([exe], env=env)
    procs.append(old)
    check("installed test copy starts", wait_for(lambda: window_pid() == old.pid, 15))
    press("Check for updates now")
    check("old copy exits cleanly after installing", wait_for(lambda: old.poll() is not None, 60) and old.returncode == 0,
          f"exit code {old.poll()}")
    check("new copy takes over", wait_for(lambda: window_pid() not in (None, old.pid), 30))
    new_pid = window_pid()
    check("new copy shows its window, as the old one did",
          bool(new_pid) and wait_for(lambda: bool(u.IsWindowVisible(u.FindWindowW(CLASS, None))), 10))
    check("new files installed", os.path.exists(os.path.join(install_dir, "UPDATE-MARKER.txt")))
    check("new copy reports the update", wait_for(lambda: "AudioNet was updated from version" in status_log(), 10))
    check("renamed old files removed", wait_for(lambda: not glob.glob(os.path.join(install_dir, "*.old-update*")), 10),
          str(os.listdir(install_dir)))
    check("staging folder removed", not glob.glob(os.path.join(install_dir, ".audionet-update-*")))
    check("downloaded zip removed", not os.path.exists(os.path.join(tempfile.gettempdir(), "audionet-update-9.9.9.zip")))
    check("installed version recorded", reg_value("UpdatedTo") == "9.9.9", str(reg_value("UpdatedTo")))

    # 3. No loop on a release that still reports the old version.
    press("Check for updates now")
    check("same release is not installed again", wait_for(lambda: "is up to date" in status_log(), 20)
          and window_pid() == new_pid, status_log()[-300:])

    # 4. Tampered manifest.
    publish("audionet-windows-x64-9.9.10.zip", {**package, "UPDATE-MARKER.txt": b"tampered"}, "9.9.10", tamper=True)
    press("Check for updates now")
    check("tampered update rejected", wait_for(lambda: "not signed by the AudioNet release key" in status_log(), 20)
          and open(os.path.join(install_dir, "UPDATE-MARKER.txt")).read() == "9.9.9" and window_pid() == new_pid)

    # 5. An update whose app cannot start is rolled back.
    before = hashlib.sha256(open(exe, "rb").read()).hexdigest()
    publish("audionet-windows-x64-9.9.11.zip", {**package, "audionet-desktop.exe": b"not a program", "UPDATE-MARKER.txt": b"broken"}, "9.9.11")
    press("Check for updates now")
    check("broken update rolled back", wait_for(lambda: "did not start" in status_log(), 60), status_log()[-300:])
    check("old copy keeps running with its files", window_pid() == new_pid
          and hashlib.sha256(open(exe, "rb").read()).hexdigest() == before
          and open(os.path.join(install_dir, "UPDATE-MARKER.txt")).read() == "9.9.9")
finally:
    server.shutdown()
    pid = window_pid()
    if pid:
        subprocess.run(["taskkill", "/PID", str(pid), "/F"], capture_output=True)
    for p in procs:
        if p.poll() is None:
            p.kill()
    try:
        winreg.DeleteKey(winreg.HKEY_CURRENT_USER, REG_KEY)
    except FileNotFoundError:
        pass
    time.sleep(1)
    shutil.rmtree(work, ignore_errors=True)
failed = [n for n, ok in results if not ok]
print(f"{len(results) - len(failed)} of {len(results)} checks passed")
sys.exit(1 if failed or not results else 0)
