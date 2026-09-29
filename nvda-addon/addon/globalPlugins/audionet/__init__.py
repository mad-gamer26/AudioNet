# AudioNet for NVDA: listen to your AudioNet devices and send audio to them
# from NVDA, as a visitor of your accounts (no device is added to them).
#
# The AudioNet engine runs as a separate program (bin/audionet-visitor.exe),
# so audio and networking never slow NVDA down or stop it.
# Copyright (c) 2026 The AudioNet contributors. Licensed under the MIT license.

import functools
import json
import os
import webbrowser

import addonHandler
import config
import globalPluginHandler
import globalVars
import gui
import ui
import wx
from gui.settingsDialogs import NVDASettingsDialog
from logHandler import log
from scriptHandler import script

from .accounts import AccountStore
from .engine import Engine
from .model import AccountState, Model, Stream, device_label, source_label

addonHandler.initTranslation()

HERE = os.path.dirname(os.path.abspath(__file__))
ENGINE = os.path.join(HERE, "bin", "audionet-visitor.exe")
VOLUME_STEP = 10

config.conf.spec["audionet"] = {
	"announceDevices": "boolean(default=false)",
	"announceStreams": "boolean(default=true)",
	# This computer's output to play on and source to send (engine ids;
	# empty: the system default).
	"playOn": "string(default='')",
	"sendFrom": "string(default='')",
}


def default_server():
	"""The server address an official build suggests (defaults.json, written
	at packaging), or none."""
	try:
		with open(os.path.join(HERE, "defaults.json"), encoding="utf-8") as f:
			return json.load(f).get("server") or ""
	except (OSError, ValueError):
		return ""


# The command layer (after NVDA+Alt+A): key -> script.
LAYER = {
	"kb:w": "openWindow",
	"kb:o": "openWindow",
	"kb:s": "stopAll",
	"kb:r": "reportStreams",
	"kb:m": "toggleMute",
	"kb:upArrow": "volumeUp",
	"kb:downArrow": "volumeDown",
	"kb:d": "reportDevices",
	"kb:l": "listenAgain",
	"kb:h": "layerHelp",
	"kb:escape": "layerCancel",
}
# The layer's commands that end it (the others keep it on).
LAYER_ENDERS = {"script_openWindow", "script_layerCancel"}


class GlobalPlugin(globalPluginHandler.GlobalPlugin):
	scriptCategory = _("AudioNet")

	def __init__(self):
		super().__init__()
		self._inLayer = False
		self.window = None
		self.disabled = bool(globalVars.appArgs.secure)
		if self.disabled:
			# Never on secure screens (sign-in, UAC).
			return
		self.store = AccountStore(os.path.join(globalVars.appArgs.configPath, "audionet", "accounts.json"))
		self.model = Model()
		for a in self.store.accounts:
			self.model.accounts[a.id] = AccountState(a)
		self.engine = Engine(ENGINE, self._on_event, self._on_engine_exit, wx.CallAfter)
		from . import dialogs

		self.dialogs = dialogs
		dialogs.plugin = self
		NVDASettingsDialog.categoryClasses.append(dialogs.AudioNetPanel)
		tools = gui.mainFrame.sysTrayIcon.toolsMenu
		# Translators: the AudioNet item in NVDA's Tools menu.
		self._menuItem = tools.Append(wx.ID_ANY, _("&AudioNet..."), _("Listen to your AudioNet devices and send audio to them"))
		gui.mainFrame.sysTrayIcon.Bind(wx.EVT_MENU, lambda e: self.open_window(), self._menuItem)

	def terminate(self):
		if self.disabled:
			return
		try:
			NVDASettingsDialog.categoryClasses.remove(self.dialogs.AudioNetPanel)
		except ValueError:
			pass
		try:
			gui.mainFrame.sysTrayIcon.toolsMenu.Remove(self._menuItem)
		except Exception:
			pass
		if self.window:
			self.window.Destroy()
		self.engine.stop()
		super().terminate()

	# ─── the engine ────────────────────────────────────────────────────────

	def ensure_engine(self):
		"""Starts the engine and connects the accounts, if not running."""
		if self.engine.running:
			return True
		if not os.path.isfile(ENGINE):
			self.say(_("The AudioNet engine is missing from the add-on. Reinstall AudioNet."))
			return False
		try:
			self.engine.start()
		except OSError as e:
			log.error(f"AudioNet: could not start the engine: {e}")
			self.say(_("The AudioNet engine could not start: {error}").format(error=e))
			return False
		self.model.add_log("AudioNet engine started.")
		self.engine.request("local_audio", self._got_local_audio)
		for a in self.store.accounts:
			self.connect(a)
		return True

	def _got_local_audio(self, result, error):
		if error:
			self.model.add_log(f"Could not list this computer's sounds and outputs: {error}")
			return
		self.model.local_sources = result["sources"]
		self.model.local_destinations = result["destinations"]
		self._refresh()

	def refresh_local_audio(self):
		if self.ensure_engine():
			self.engine.request("local_audio", self._got_local_audio)

	def connect(self, account):
		state = self.model.accounts.setdefault(account.id, AccountState(account))
		if not account.token:
			state.status = "signed out: sign in again in AudioNet settings"
			return
		state.status = "connecting"
		self.engine.request(
			"connect",
			account=account.id,
			server=account.server,
			username=account.username,
			token=account.token,
		)

	def _on_engine_exit(self, message):
		self.model.add_log(message)
		had_streams = bool(self.model.streams)
		self.model.streams.clear()
		for st in self.model.accounts.values():
			st.connected = False
			st.status = "not connected"
		if had_streams:
			self.say(_("{message} Its streams ended; use AudioNet again to restart it.").format(message=message))
		self._refresh()

	def _on_event(self, msg):
		kind = msg.get("event")
		st = self.model.accounts.get(msg.get("account"))
		conf = config.conf["audionet"]
		if kind == "status":
			self.model.add_log(msg["text"])
			self._refresh_log()
			return
		if kind == "fatal":
			self.model.add_log(msg.get("message", ""))
			return
		if st is None:
			return
		name = st.account.label
		if kind == "connected":
			st.connected = True
			st.status = "connected"
			self.model.add_log(f"{name}: connected.")
		elif kind == "disconnected":
			st.connected = False
			st.status = f"reconnecting ({msg['reason']})"
		elif kind == "stopped":
			st.connected = False
			st.status = "signed out: sign in again in AudioNet settings"
			self.store.clear_token(st.account.id)
			self.model.add_log(f"{name}: {msg['reason']}")
			self.say(_("AudioNet: {account} was signed out: {reason}").format(account=name, reason=msg["reason"]))
		elif kind == "devices":
			st.devices = {d["node_id"]: d for d in msg["devices"]}
		elif kind == "device_update":
			d = msg["device"]
			old = st.devices.get(d["node_id"])
			st.devices[d["node_id"]] = d
			if old is not None and bool(old.get("online")) != bool(d.get("online")):
				text = _("{device} is online").format(device=d["name"]) if d.get("online") else _("{device} is offline").format(device=d["name"])
				self.model.add_log(text)
				if conf["announceDevices"]:
					self.say(text)
		elif kind == "session":
			s = self.model.streams.get(msg["session_id"])
			if s is not None:
				self.model.add_log(f"{s.title}: {msg['detail']}")
				state = msg["state"]
				if state == "active":
					s.state = "connected"
					if not s.announced:
						s.announced = True
						if conf["announceStreams"]:
							self.say(_("{stream}: connected").format(stream=s.short_title))
				elif state == "failed":
					s.state = msg["detail"]
				elif state == "starting" and not s.announced:
					# Progress from either device; once connected, the other
					# device's "starting" is old news.
					s.state = "connecting"
		elif kind == "session_ended":
			s = self.model.streams.pop(msg["session_id"], None)
			if s is not None:
				reason = msg["reason"]
				self.model.add_log(f"{s.title} ended: {reason}")
				if reason != "You stopped it." and conf["announceStreams"]:
					self.say(_("{stream} ended: {reason}").format(stream=s.short_title, reason=reason))
		elif kind == "diagnostics":
			s = self.model.streams.get(msg["session_id"])
			if s is not None:
				s.diagnostics = msg["text"]
			self._refresh_log()
			return
		elif kind == "server_error":
			self.model.add_log(f"{name}: {msg['message']}")
			self.say(_("AudioNet: {message}").format(message=msg["message"]))
		self._refresh()

	def _refresh(self):
		if self.window:
			self.window.refresh()

	def _refresh_log(self):
		if self.window:
			self.window.refresh_log()

	def say(self, text):
		ui.message(text)

	# ─── accounts ──────────────────────────────────────────────────────────

	def sign_in(self, server, username, password, done):
		"""Signs in (done(account, error) on the main thread)."""
		if not self.ensure_engine():
			done(None, _("The AudioNet engine could not start."))
			return

		def got(result, error):
			if error:
				done(None, error)
				return
			account = self.store.add(result["server"], result["username"], result["token"])
			self.model.accounts[account.id] = st = self.model.accounts.get(account.id) or AccountState(account)
			st.account = account
			self.model.add_log(f"Signed in to {account.label} as a visitor.")
			self.connect(account)
			self._refresh()
			done(account, None)

		self.engine.request("sign_in", got, server=server, username=username, password=password)

	def remove_account(self, account):
		for s in self.model.streams_of(account.id):
			self.model.streams.pop(s.session_id, None)
		if self.engine.running:
			self.engine.request("disconnect", account=account.id)
			if account.token:
				self.engine.request("sign_out", server=account.server, token=account.token)
		self.store.remove(account.id)
		self.model.accounts.pop(account.id, None)
		self.model.add_log(f"Signed out of {account.label}.")
		self._refresh()

	def forgot_password(self, server):
		server = server.strip().rstrip("/")
		if server.startswith(("https://", "http://")):
			webbrowser.open(f"{server}/?forgot")
			return True
		return False

	# ─── streams ───────────────────────────────────────────────────────────

	def _local_name(self, items, item_id, fallback):
		for i in items:
			if i["id"] == item_id:
				return source_label(i) if "source_type" in i else i["name"]
		return fallback

	def default_destination(self):
		wanted = config.conf["audionet"]["playOn"]
		ids = [d["id"] for d in self.model.local_destinations]
		if wanted in ids:
			return wanted
		return next((d["id"] for d in self.model.local_destinations if d.get("is_default")), ids[0] if ids else "")

	def default_source(self):
		wanted = config.conf["audionet"]["sendFrom"]
		ids = [s["id"] for s in self.model.local_sources]
		if wanted in ids:
			return wanted
		inputs = [s for s in self.model.local_sources if s.get("source_type") == "input"]
		return next((s["id"] for s in inputs if s.get("is_default")), inputs[0]["id"] if inputs else "")

	def listen(self, account_id, node_id, source_id, destination_id):
		if not self.ensure_engine():
			return
		d = self.model.device(account_id, node_id) or {"name": node_id, "sources": []}
		source = next((s for s in d.get("sources", []) if s["id"] == source_id), None)
		remote_name = source_label(source) if source else source_id
		local_name = self._local_name(self.model.local_destinations, destination_id, "this computer")
		self.model.last_listen = (account_id, node_id, source_id, destination_id)

		def got(result, error):
			if error:
				self.say(_("Could not listen to {device}: {error}").format(device=d["name"], error=error))
				return
			s = Stream(result["session_id"], account_id, "listen", node_id, d["name"], source_id, remote_name, destination_id, local_name)
			self.model.streams[s.session_id] = s
			self.model.add_log(f"{s.title}: starting.")
			self._refresh()

		self.engine.request(
			"listen", got, account=account_id, node_id=node_id, source_id=source_id, destination_id=destination_id
		)

	def send(self, account_id, node_id, destination_id, source_id):
		if not self.ensure_engine():
			return
		d = self.model.device(account_id, node_id) or {"name": node_id, "destinations": []}
		dest = next((x for x in d.get("destinations", []) if x["id"] == destination_id), None)
		remote_name = dest["name"] if dest else destination_id
		local_name = self._local_name(self.model.local_sources, source_id, "this computer's microphone")

		def got(result, error):
			if error:
				self.say(_("Could not send to {device}: {error}").format(device=d["name"], error=error))
				return
			s = Stream(result["session_id"], account_id, "send", node_id, d["name"], destination_id, remote_name, source_id, local_name)
			self.model.streams[s.session_id] = s
			self.model.add_log(f"{s.title}: starting.")
			self._refresh()

		self.engine.request(
			"send", got, account=account_id, node_id=node_id, destination_id=destination_id, source_id=source_id
		)

	def stop(self, session_id):
		s = self.model.streams.pop(session_id, None)
		if s is not None:
			self.model.add_log(f"{s.title}: stopped.")
		self.engine.request("stop", session_id=session_id)
		self._refresh()

	def set_volume(self, stream, volume, muted):
		stream.volume = max(0, min(100, volume))
		stream.muted = muted
		self.engine.request("set_volume", session_id=stream.session_id, volume=stream.volume / 100, muted=muted)
		self._refresh()

	# ─── scripts ───────────────────────────────────────────────────────────

	def open_window(self):
		if self.disabled:
			return
		self.ensure_engine()
		if not self.store.accounts:
			self.dialogs.AddAccountDialog.run(gui.mainFrame, then_open_window=True)
			return
		if self.window is None:
			self.window = self.dialogs.MainWindow(self)
		gui.mainFrame.prePopup()
		self.window.Show()
		self.window.Raise()
		gui.mainFrame.postPopup()

	@script(
		# Translators: describes a command in Input Gestures.
		description=_("Opens the AudioNet window: your devices, listening, sending and your streams"),
	)
	def script_openWindow(self, gesture):
		wx.CallAfter(self.open_window)

	@script(description=_("Stops every AudioNet stream NVDA started"))
	def script_stopAll(self, gesture):
		if self.disabled:
			return
		streams = list(self.model.streams.values())
		if not streams:
			self.say(_("No AudioNet streams are running."))
			return
		for s in streams:
			self.stop(s.session_id)
		self.say(_("Stopped {count} AudioNet streams.").format(count=len(streams)) if len(streams) > 1 else _("Stopped {stream}.").format(stream=streams[0].short_title))

	@script(description=_("Reports the AudioNet streams NVDA started and their state"))
	def script_reportStreams(self, gesture):
		if self.disabled:
			return
		streams = list(self.model.streams.values())
		if not streams:
			self.say(_("No AudioNet streams are running."))
			return
		self.say(". ".join(s.label() for s in streams))

	@script(description=_("Mutes or unmutes every AudioNet stream NVDA started"))
	def script_toggleMute(self, gesture):
		if self.disabled:
			return
		streams = list(self.model.streams.values())
		if not streams:
			self.say(_("No AudioNet streams are running."))
			return
		mute = not all(s.muted for s in streams)
		for s in streams:
			self.set_volume(s, s.volume, mute)
		self.say(_("AudioNet muted") if mute else _("AudioNet unmuted"))

	def _change_volume(self, step):
		streams = list(self.model.streams.values())
		if not streams:
			self.say(_("No AudioNet streams are running."))
			return
		for s in streams:
			self.set_volume(s, s.volume + step, s.muted)
		levels = sorted({s.volume for s in streams})
		self.say(_("AudioNet volume {level} percent").format(level=", ".join(str(v) for v in levels)))

	@script(description=_("Turns every AudioNet stream NVDA started up by 10 percent"))
	def script_volumeUp(self, gesture):
		if not self.disabled:
			self._change_volume(VOLUME_STEP)

	@script(description=_("Turns every AudioNet stream NVDA started down by 10 percent"))
	def script_volumeDown(self, gesture):
		if not self.disabled:
			self._change_volume(-VOLUME_STEP)

	@script(description=_("Reports which of your AudioNet devices are online"))
	def script_reportDevices(self, gesture):
		if self.disabled:
			return
		if not self.store.accounts:
			self.say(_("No AudioNet accounts. Add one in NVDA settings, AudioNet category."))
			return
		self.ensure_engine()
		parts = []
		for st in self.model.accounts.values():
			online = [d for d in st.sorted_devices() if d.get("online")]
			if not st.connected:
				parts.append(f"{st.account.label}: {st.status}")
			elif not online:
				parts.append(_("{account}: no devices online").format(account=st.account.label))
			else:
				names = ", ".join(device_label(d) for d in online)
				parts.append(f"{st.account.label}: {names}")
		self.say(". ".join(parts))

	@script(description=_("Listens again to the sound you last listened to with AudioNet"))
	def script_listenAgain(self, gesture):
		if self.disabled:
			return
		if not self.model.last_listen:
			self.say(_("Nothing to listen to again yet."))
			return
		self.listen(*self.model.last_listen)
		self.say(_("Listening again"))

	# The command layer.

	@script(
		description=_("AudioNet commands: keys after this one run AudioNet commands until Escape (H lists them)"),
		gesture="kb:NVDA+alt+a",
	)
	def script_layer(self, gesture):
		if self.disabled:
			return
		if self._inLayer:
			self._exitLayer()
			self.say(_("AudioNet commands off"))
			return
		self.bindGestures(LAYER)
		self._inLayer = True
		self.say(_("AudioNet"))

	def _exitLayer(self):
		for g in LAYER:
			try:
				self.removeGestureBinding(g)
			except LookupError:
				pass
		self._inLayer = False

	def getScript(self, gesture):
		if not self._inLayer:
			return super().getScript(gesture)
		found = super().getScript(gesture)
		if found is None:
			return self.script_layerUnknown
		if found.__name__ not in LAYER_ENDERS:
			# Everything else keeps the layer on, so several commands (volume
			# steps, reports) follow one another.
			return found

		# Opening the window or Escape ends the layer. The wrapper keeps the
		# script's name and description, which NVDA reads.
		@functools.wraps(found)
		def last(g):
			self._exitLayer()
			found(g)

		return last

	# Layer-only scripts: no description, so Input Gestures does not list them.

	def script_layerUnknown(self, gesture):
		self.say(_("Not an AudioNet command. Escape leaves AudioNet commands."))

	def script_layerCancel(self, gesture):
		self.say(_("AudioNet commands off"))

	def script_layerHelp(self, gesture):
		self.say(
			_(
				"AudioNet commands: W or O, open the AudioNet window. L, listen again. S, stop all streams. "
				"R, report streams. M, mute or unmute. Up and down arrows, volume. D, report devices. "
				"The commands stay on until you press Escape or open the window."
			)
		)
