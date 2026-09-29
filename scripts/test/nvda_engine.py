"""Checks the NVDA add-on's engine for real: the add-on's own engine client
(nvda-addon/.../engine.py) drives audionet-visitor.exe against a local
server started for the test (temporary database), with a command-line
AudioNet device on this computer (real WASAPI audio) to listen and send to:

1. The engine answers, and lists this computer's sounds and outputs.
2. Signing in returns a web session (ans_), with no device added.
3. Connected as a visitor, it sees the device online, and not itself.
4. Listening to the device connects, and audio packets arrive.
5. Volume and mute are accepted; stopping ends the stream.
6. Sending this computer's source to the device's output connects.
7. The account still has only the one device, while connected.
8. A wrong password is refused, in words.
9. Signing out ends the web session; the connection stops for good.
10. When the add-on goes away (standard input closes), the engine exits.

Silent virtual devices are used where present (Virtual Audio Cable's
"UniMic Output" and "Line 1"), so nothing is heard.

Environment: AUDIONET_SERVER, AUDIONET_CLI, AUDIONET_VISITOR (default
target/release/*.exe). Requires: pip install requests.
"""
import os, socket, subprocess, sys, tempfile, time

import requests

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
REL = os.path.join(ROOT, "target", "release")
SERVER = os.environ.get("AUDIONET_SERVER", os.path.join(REL, "audionet-server.exe"))
CLI = os.environ.get("AUDIONET_CLI", os.path.join(REL, "audionet.exe"))
VISITOR = os.environ.get("AUDIONET_VISITOR", os.path.join(REL, "audionet-visitor.exe"))
sys.path.insert(0, os.path.join(ROOT, "nvda-addon", "addon", "globalPlugins", "audionet"))
import engine  # noqa: E402

PASSWORD = "correct horse battery"
results = []
events = []


def check(name, ok, detail=""):
	results.append(ok)
	print(f"{name}: {'PASS' if ok else 'FAIL'}{'' if ok or not detail else '  (' + str(detail)[:300] + ')'}")


def wait_for(pred, timeout=30):
	end = time.time() + timeout
	while time.time() < end:
		found = pred()
		if found:
			return found
		time.sleep(0.2)
	return None


def event(kind, **match):
	def find():
		for e in events:
			if e.get("event") == kind and all(e.get(k) == v for k, v in match.items()):
				return e
		return None

	return find


def free_port():
	with socket.socket() as s:
		s.bind(("127.0.0.1", 0))
		return s.getsockname()[1]


def pick(items, *names):
	"""The first item whose name contains one of names, else the first."""
	for n in names:
		for i in items:
			if n in i["name"]:
				return i
	return items[0]


work = tempfile.mkdtemp(prefix="audionet-nvda-engine-")
port = free_port()
base = f"http://127.0.0.1:{port}"
cfg = os.path.join(work, "config.toml")
with open(cfg, "w") as f:
	f.write(f'public_url = "{base}"\nbind = "127.0.0.1:{port}"\ndatabase = "{os.path.join(work, "a.db").replace(chr(92), "/")}"\n')
subprocess.run([SERVER, "-c", cfg, "user", "add", "tester"], input=(PASSWORD + "\n").encode(), check=True, capture_output=True)
procs = [subprocess.Popen([SERVER, "-c", cfg, "serve"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)]
eng = None
try:
	def listening():
		try:
			socket.create_connection(("127.0.0.1", port), timeout=0.2).close()
			return True
		except OSError:
			return False

	wait_for(listening, 10)
	node_cfg = os.path.join(work, "node.toml")
	subprocess.run(
		[CLI, "node", "sign-in", "--server", base, "--user", "tester", "--password-stdin", "--name", "Test PC", "--config", node_cfg],
		input=PASSWORD + "\n", text=True, check=True, capture_output=True,
	)
	procs.append(subprocess.Popen([CLI, "node", "run", "--config", node_cfg], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))

	eng = engine.Engine(VISITOR, events.append)
	eng.start()
	hello = eng.call("hello")
	check("1. the engine answers", hello.get("name") == "audionet-visitor", hello)
	local = eng.call("local_audio")
	check("...and lists this computer's sounds and outputs", local["sources"] and local["destinations"], local)

	signed = eng.call("sign_in", server=base, username="TESTER", password=PASSWORD)
	token = signed["token"]
	check("2. signing in returns a web session", token.startswith("ans_") and signed["username"] == "tester", signed["username"])
	api = requests.Session()
	api.headers["Authorization"] = f"Bearer {token}"
	check("...and adds no device", [n["name"] for n in api.get(f"{base}/api/v1/nodes").json()["nodes"]] == ["Test PC"])

	eng.call("connect", account="acct", server=signed["server"], username=signed["username"], token=token)
	check("3. connects as a visitor", wait_for(event("connected", account="acct")) is not None, events[-3:])

	def device():
		for e in reversed(events):
			if e.get("event") == "devices":
				return next((d for d in e["devices"] if d["name"] == "Test PC" and d["online"] and d["sources"]), None)
			if e.get("event") == "device_update" and e["device"]["name"] == "Test PC" and e["device"]["online"]:
				return e["device"]
		return None

	pc = wait_for(device)
	check("...and sees the device online", pc is not None)
	all_devices = next(e["devices"] for e in events if e.get("event") == "devices")
	check("...and not itself", all(d["name"] == "Test PC" for d in all_devices), [d["name"] for d in all_devices])

	src = pick([s for s in pc["sources"] if s["source_type"] == "loopback"], "UniMic Output")
	out = pick(local["destinations"], "Line 1 (Virtual")
	print(f"   listening to \"{src['name']}\" on \"{out['name']}\"")
	sid = eng.call("listen", account="acct", node_id=pc["node_id"], source_id=src["id"], destination_id=out["id"])["session_id"]
	active = wait_for(event("session", session_id=sid, state="active"))
	check("4. listening connects", active is not None, [e for e in events if e.get("session_id") == sid][-3:])

	def packets():
		for e in reversed(events):
			if e.get("event") == "diagnostics" and e["session_id"] == sid and "Receiving:" in e["text"]:
				n = int(e["text"].split("Receiving:")[1].split("packets")[0].strip().replace(",", ""))
				return n if n > 50 else None
		return None

	got = wait_for(packets, 20)
	check("...and audio packets arrive", got is not None, got)
	eng.call("set_volume", session_id=sid, volume=0.5, muted=True)
	check("5. volume and mute are accepted", True)
	eng.call("stop", session_id=sid)
	ended = wait_for(event("session_ended", session_id=sid))
	check("...stopping ends the stream", ended is not None and ended["reason"] == "You stopped it.", ended)
	try:
		eng.call("stop", session_id=sid)
		check("...and it is gone", False, "a second stop was accepted")
	except engine.EngineError as e:
		check("...and it is gone", "ended" in str(e), e)

	mic = pick([s for s in local["sources"] if s["source_type"] == "input"], "UniMic Input")
	dest = pick(pc["destinations"], "Line 1 (Virtual")
	print(f"   sending \"{mic['name']}\" to \"{dest['name']}\"")
	sid2 = eng.call("send", account="acct", node_id=pc["node_id"], destination_id=dest["id"], source_id=mic["id"])["session_id"]
	check("6. sending connects", wait_for(event("session", session_id=sid2, state="active")) is not None,
		[e for e in events if e.get("session_id") == sid2][-3:])
	nodes = api.get(f"{base}/api/v1/nodes").json()["nodes"]
	check("7. the account still has only the device", [n["name"] for n in nodes] == ["Test PC"], [n["name"] for n in nodes])
	eng.call("stop", session_id=sid2)

	try:
		eng.call("sign_in", server=base, username="tester", password="not the password")
		check("8. a wrong password is refused", False)
	except engine.EngineError as e:
		check("8. a wrong password is refused, in words", "incorrect" in str(e), e)

	eng.call("sign_out", server=base, token=token)
	check("9. signing out ends the web session", api.get(f"{base}/api/v1/me").status_code == 401)
	eng.call("disconnect", account="acct")
	eng.call("connect", account="acct", server=base, username="tester", token=token)
	stopped = wait_for(event("stopped", account="acct"), 20)
	check("...and connecting with it stops for good, in words", stopped is not None and "Sign in again" in stopped["reason"], stopped)

	proc = eng._proc
	proc.stdin.close()
	try:
		proc.wait(timeout=10)
		check("10. the engine exits when the add-on goes away", True)
	except subprocess.TimeoutExpired:
		check("10. the engine exits when the add-on goes away", False)
		proc.kill()
	eng._proc = None
finally:
	if eng:
		eng.stop()
	for p in procs:
		p.terminate()
print(f"{sum(bool(r) for r in results)} of {len(results)} checks passed")
sys.exit(0 if results and all(results) else 1)
