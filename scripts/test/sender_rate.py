"""Checks that a sending device delivers a full real-time stream even when
its short waits are slow, as on the many Windows PCs where no program has
raised the timer resolution (a "1 ms" wait then lasts a 15.6 ms tick).

A command-line AudioNet device on this computer (real WASAPI audio, the
silent Virtual Audio Cable "UniMic Output" loopback where present) sends to
a visitor on this computer through a local server started for the test.
The device runs with AUDIONET_TEST_MIN_WAIT_MS=15 (test only: its media
loop never waits less than 15 ms for network input). Checked:

1. The device sends 100 packets a second and WebRTC refuses none.
2. About 100 packets a second arrive, with no underruns.

Before the fix (str0m's due deadlines were only handled after a socket
wait) this measured 79 packets a second, frames refused, and an underrun
every few hundred milliseconds; from a laptop across the internet, about 92.

Environment: AUDIONET_SERVER, AUDIONET_CLI, AUDIONET_VISITOR (default
target/release/*.exe), SENDER_MIN_WAIT_MS (default 15), SECONDS (default 30).
"""
import os, re, socket, subprocess, sys, tempfile, time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
REL = os.path.join(ROOT, "target", "release")
SERVER = os.environ.get("AUDIONET_SERVER", os.path.join(REL, "audionet-server.exe"))
CLI = os.environ.get("AUDIONET_CLI", os.path.join(REL, "audionet.exe"))
VISITOR = os.environ.get("AUDIONET_VISITOR", os.path.join(REL, "audionet-visitor.exe"))
MIN_WAIT = os.environ.get("SENDER_MIN_WAIT_MS", "15")
SECONDS = int(os.environ.get("SECONDS", "30"))
sys.path.insert(0, os.path.join(ROOT, "nvda-addon", "addon", "globalPlugins", "audionet"))
import engine  # noqa: E402

results = []


def check(name, ok, detail=""):
	results.append(ok)
	print(f"{name}: {'PASS' if ok else 'FAIL'}{'' if ok or not detail else '  (' + str(detail)[:300] + ')'}")


work = tempfile.mkdtemp(prefix="audionet-sender-rate-")
with socket.socket() as s:
	s.bind(("127.0.0.1", 0))
	port = s.getsockname()[1]
base = f"http://127.0.0.1:{port}"
cfg = os.path.join(work, "config.toml")
with open(cfg, "w") as f:
	f.write(f'public_url = "{base}"\nbind = "127.0.0.1:{port}"\ndatabase = "{os.path.join(work, "a.db").replace(os.sep, "/")}"\n')
password = "correct horse battery"
subprocess.run([SERVER, "-c", cfg, "user", "add", "tester"], input=(password + "\n").encode(), check=True, capture_output=True)
procs = [subprocess.Popen([SERVER, "-c", cfg, "serve"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)]
eng = None
try:
	time.sleep(1.5)
	node_cfg = os.path.join(work, "node.toml")
	subprocess.run(
		[CLI, "node", "sign-in", "--server", base, "--user", "tester", "--password-stdin", "--name", "Sender", "--config", node_cfg],
		input=password + "\n", text=True, check=True, capture_output=True,
	)
	log_path = os.path.join(work, "node.log")
	env = dict(os.environ, AUDIONET_TEST_MIN_WAIT_MS=MIN_WAIT)
	procs.append(subprocess.Popen([CLI, "node", "run", "--config", node_cfg], stdout=open(log_path, "w"), stderr=subprocess.STDOUT, env=env))
	events = []
	eng = engine.Engine(VISITOR, events.append)
	eng.start()
	signed = eng.call("sign_in", server=base, username="tester", password=password)
	eng.call("connect", account="a", server=base, username="tester", token=signed["token"])
	device = None
	for _ in range(150):
		device = next((d for e in events if e.get("event") == "devices" for d in e["devices"] if d["online"] and d["sources"]), None)
		if device:
			break
		time.sleep(0.2)
	loopbacks = [s for s in device["sources"] if s["source_type"] == "loopback"]
	src = next((s for s in loopbacks if "UniMic Output" in s["name"]), loopbacks[0])
	outs = eng.call("local_audio")["destinations"]
	out = next((d for d in outs if d["name"].startswith("Line 1 (Virtual")), outs[0])
	print(f"   sender waits at least {MIN_WAIT} ms; listening to \"{src['name']}\" on \"{out['name']}\" for {SECONDS} s")
	sid = eng.call("listen", account="a", node_id=device["node_id"], source_id=src["id"], destination_id=out["id"])["session_id"]
	time.sleep(SECONDS)

	with open(log_path, encoding="utf-8", errors="replace") as f:
		sends = [l for l in f if "Sending:" in l]
	last_send = sends[-1] if sends else ""
	print(f"   device: {last_send[last_send.find('Sending'):].strip()[:130]}")
	m = re.search(r"Sending: ([0-9.]+) packets a second \((\d+) encoded frames refused", last_send)
	check("1. the device sends 100 packets a second", m and float(m.group(1)) >= 98.0, last_send)
	check("...and WebRTC refuses no frames", m and all(
		re.search(r"\((\d+) encoded", l).group(1) == "0" for l in sends if re.search(r"\((\d+) encoded", l)), sends)
	diags = [e["text"] for e in events if e.get("event") == "diagnostics" and e["session_id"] == sid]
	rates = [float(x) for d in diags[-5:] for x in re.findall(r"Arriving: ([0-9.]+) packets", d)]
	underruns = [int(x) for x in re.findall(r"underruns (\d+)", diags[-1])] if diags else []
	print(f"   arriving: {rates}; underruns {underruns}")
	check("2. about 100 packets a second arrive", rates and sum(rates) / len(rates) >= 97.0, rates)
	check("...with no underruns", underruns == [0], underruns)
	eng.call("stop", session_id=sid)
finally:
	if eng:
		eng.stop()
	for p in procs:
		p.terminate()
print(f"{sum(bool(r) for r in results)} of {len(results)} checks passed")
sys.exit(0 if results and all(results) else 1)
