# AudioNet for NVDA: what the add-on knows, and how it is said in words.
# Plain Python with no NVDA imports.
# Copyright (c) 2026 The AudioNet contributors. Licensed under the MIT license.

import time

PLATFORMS = {
	"windows": "Windows",
	"mac_os": "Mac",
	"linux": "Linux",
	"ios": "iPhone",
	"android": "Android",
	"browser": "web browser",
}

LOG_LINES = 500


def source_label(s):
	"""A sound a device offers, or one of this computer's. Devices already
	name them in words ("Sound playing on Speakers", "Yeti Microphone")."""
	return s["name"]


def device_label(d):
	"""A visible device (only visible devices are listed): its name and
	platform."""
	platform = PLATFORMS.get(d.get("platform") or "", "")
	return d["name"] + (f", {platform}" if platform else "")


class AccountState:
	"""An account's connection, as the engine reports it."""

	def __init__(self, account):
		self.account = account
		self.status = "not connected"
		self.connected = False
		# node_id -> device (as the server describes it).
		self.devices = {}

	def label(self):
		return f"{self.account.label}, {self.status}"

	def sorted_devices(self):
		return sorted(self.devices.values(), key=lambda d: d["name"].lower())


class Stream:
	"""A stream NVDA started: listening to a device, or sending to one."""

	def __init__(self, session_id, account_id, kind, node_id, device, remote_id, remote_name, local_id, local_name):
		self.session_id = session_id
		self.account_id = account_id
		self.kind = kind  # "listen" or "send"
		self.node_id = node_id
		self.device = device
		self.remote_id = remote_id
		self.remote_name = remote_name
		self.local_id = local_id
		self.local_name = local_name
		self.state = "connecting"
		# Whether "connected" was announced: both devices report progress,
		# and it is said once.
		self.announced = False
		self.volume = 100
		self.muted = False

	@property
	def title(self):
		if self.kind == "listen":
			return f"Listening to {self.remote_name} on {self.device}, playing on {self.local_name}"
		return f"Sending {self.local_name} to {self.remote_name} on {self.device}"

	@property
	def short_title(self):
		if self.kind == "listen":
			return f"Listening to {self.device}"
		return f"Sending to {self.device}"

	def label(self):
		text = f"{self.title}: {self.state}"
		if self.volume != 100:
			text += f", volume {self.volume} percent"
		if self.muted:
			text += ", muted"
		return text


class Model:
	def __init__(self):
		# account id -> AccountState
		self.accounts = {}
		# session id -> Stream, in start order.
		self.streams = {}
		# This computer's sounds and outputs, from the engine.
		self.local_sources = []
		self.local_destinations = []
		self.log = []
		# The last listen request, to start it again.
		self.last_listen = None

	def add_log(self, text):
		self.log.append(f"{time.strftime('%H:%M:%S')}  {text}")
		del self.log[:-LOG_LINES]

	def device(self, account_id, node_id):
		a = self.accounts.get(account_id)
		return a.devices.get(node_id) if a else None

	def streams_of(self, account_id):
		return [s for s in self.streams.values() if s.account_id == account_id]


def percent(v):
	return f"{round(v * 100)}%"


def sound_rows(d):
	"""Another device's sound settings as rows for its dialog, or a single
	("note", text) row when there is nothing to change. Each row is a dict
	with "type" (section, heading, volume, mute, button, text, note) and a
	"key" that stays the same while the device keeps that control."""
	if d is None:
		return [{"type": "note", "key": "note", "text": "This device is no longer visible."}]
	if not d.get("allows_control"):
		return [{"type": "note", "key": "note", "text": f"{d['name']} doesn't let other devices change its sound settings."}]
	rows = []

	def endpoint(e):
		c = e["control"]
		roles = [r for r, on in (("default", e.get("is_default")), ("default for communications", c["is_default_communications"])) if on]
		rows.append({"type": "heading", "key": "h:" + e["id"], "text": e["name"] + (f" ({', '.join(roles)})" if roles else "")})
		if c["volume"] is not None and c["can_set_volume"]:
			rows.append({"type": "volume", "key": "v:" + e["id"], "target": e["id"], "name": e["name"], "value": c["volume"]})
		elif c["volume"] is not None:
			rows.append({"type": "text", "key": "t:" + e["id"], "text": f"Volume of {e['name']}: {percent(c['volume'])}"})
		if c["can_mute"] and c["muted"] is not None:
			rows.append({"type": "mute", "key": "m:" + e["id"], "target": e["id"], "name": e["name"], "value": c["muted"]})
		if c["can_set_default"] and not e.get("is_default"):
			rows.append({"type": "button", "key": "d:" + e["id"], "text": f"Make {e['name']} the default",
						 "action": {"kind": "set_default", "target": e["id"], "communications": False}})
		if c["can_set_default_communications"] and not c["is_default_communications"]:
			rows.append({"type": "button", "key": "c:" + e["id"], "text": f"Make {e['name']} the default for communications",
						 "action": {"kind": "set_default", "target": e["id"], "communications": True}})
		if c["can_enable"]:
			rows.append({"type": "button", "key": "x:" + e["id"], "text": f"Disable {e['name']}",
						 "action": {"kind": "set_enabled", "target": e["id"], "enabled": False}})

	outputs = [o for o in d.get("destinations", []) if o.get("control")]
	mics = [s for s in d.get("sources", []) if s.get("source_type") == "input" and s.get("control")]
	if outputs:
		rows.append({"type": "section", "key": "s:out", "text": "Outputs"})
		for o in outputs:
			endpoint(o)
	if mics:
		rows.append({"type": "section", "key": "s:mic", "text": "Microphones"})
		for m in mics:
			endpoint(m)
	volumes = d.get("volumes") or []
	if volumes:
		rows.append({"type": "section", "key": "s:vol", "text": "Volumes"})
	for v in volumes:
		rows.append({"type": "volume", "key": "v:" + v["id"], "target": v["id"], "name": v["name"], "value": v["volume"]})
		if v["can_mute"] and v["muted"] is not None:
			rows.append({"type": "mute", "key": "m:" + v["id"], "target": v["id"], "name": v["name"].lower(), "value": v["muted"]})
	disabled = d.get("disabled") or []
	if disabled:
		rows.append({"type": "section", "key": "s:dis", "text": "Disabled"})
	for x in disabled:
		kind = "microphone" if x["kind"] == "input" else "output"
		rows.append({"type": "button", "key": "e:" + x["id"], "text": f"Enable {x['name']} ({kind})",
					 "action": {"kind": "set_enabled", "target": x["id"], "enabled": True}})
	if not rows:
		text = ("An iPhone's sound settings can't be changed from other devices." if d.get("platform") == "ios"
				else f"{d['name']} doesn't report its sound settings. It may need a newer AudioNet.")
		return [{"type": "note", "key": "note", "text": text}]
	return rows
