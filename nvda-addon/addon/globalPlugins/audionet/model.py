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
	if not d.get("online"):
		state = "offline"
	elif d.get("sharing", True):
		state = "online, sharing"
	else:
		state = "online, not sharing"
	platform = PLATFORMS.get(d.get("platform") or "", "")
	return f"{d['name']}: {state}" + (f", {platform}" if platform else "")


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
		return sorted(self.devices.values(), key=lambda d: (not d.get("online"), d["name"].lower()))


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
