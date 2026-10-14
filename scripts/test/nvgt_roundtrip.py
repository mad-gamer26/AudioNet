"""Round trip through AudioNet inside NVGT (Windows): a throwaway server
on localhost with a throwaway account, the audionet command-line agent as a
device, and nvgt/test/roundtrip_test.nvgt, which sends a 440 Hz tone from
NVGT to the device's virtual cable output and listens to that output's
loopback back into NVGT through the plugin. Nothing is heard: the cable is
not a speaker, and the NVGT sound is muted.

Needs: the plugin installed (nvgt/build-plugin.ps1 -Install), NVGT, a
"Line 1 (Virtual Audio Cable)" output, and debug builds of the server and
the command-line agent (cargo build -p audionet-server -p audionet-cli).

Passes when the stream connects, the sound's buffer stays near its target
depth, and the tone comes back at its level.

Environment: NVGT (default C:\\nvgt\\nvgt.exe).
"""
import os, re, subprocess, sys, tempfile, time, urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
NVGT = os.environ.get("NVGT", r"C:\nvgt\nvgt.exe")
SERVER = os.path.join(ROOT, "target", "debug", "audionet-server.exe")
CLI = os.path.join(ROOT, "target", "debug", "audionet.exe")
PORT = 18741
PASSWORD = "roundtrip-test-pass"  # a throwaway account on a throwaway server

work = tempfile.mkdtemp(prefix="audionet-nvgt-")
config = os.path.join(work, "config.toml")
with open(config, "w", encoding="utf-8") as f:
    f.write(f'public_url = "http://127.0.0.1:{PORT}"\nbind = "127.0.0.1:{PORT}"\n'
            f'database = "{os.path.join(work, "test.db").replace(os.sep, "/")}"\n'
            f'web_root = "{os.path.join(ROOT, "web").replace(os.sep, "/")}"\n')
subprocess.run([SERVER, "-c", config, "user", "add", "tester"], input=PASSWORD + "\n", text=True, check=True,
               capture_output=True)
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
    subprocess.run([CLI, "node", "sign-in", "--server", f"http://127.0.0.1:{PORT}", "--user", "tester",
                    "--password-stdin", "--name", "Round-trip test device", "--config", node_config],
                   input=PASSWORD + "\n", text=True, check=True, capture_output=True)
    node = subprocess.Popen([CLI, "node", "run", "--config", node_config],
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    test = subprocess.run([NVGT, "roundtrip_test.nvgt"], cwd=os.path.join(ROOT, "nvgt", "test"),
                          capture_output=True, text=True, timeout=120)
    out = test.stdout + test.stderr
    print(out)
    m = re.search(r"buffered (\d+) to (\d+) ms \(target (\d+)\); level mean ([\d.]+)", out)
    playing = "sender sending: true; listener playing: true" in out
    if not (m and playing):
        sys.exit("FAIL: the round trip did not complete")
    low, high, target, level = int(m[1]), int(m[2]), int(m[3]), float(m[4])
    # A 0.3 sine has an RMS of 0.212.
    ok = low >= target / 2 and high <= target and abs(level - 0.212) < 0.03
    print(f"buffer {low}-{high} ms of {target}, level {level:.3f} (expected 0.212)")
    sys.exit(0 if ok else "FAIL: buffer depth or level out of range")
finally:
    if node:
        node.terminate()
    server.terminate()
