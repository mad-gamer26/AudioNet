# AudioNet for NVDA: the AudioNet window, signing in, the status log and the
# AudioNet category of NVDA's settings.
#
# Standard controls only, each labelled: a tree view of devices (a device,
# then "Sounds to listen to" and "Outputs to send to"), choices, a list of
# streams, a slider and a check box. Updates change items in place, so what
# is expanded, the selection and NVDA's position are kept.
# Copyright (c) 2026 The AudioNet contributors. Licensed under the MIT license.

import addonHandler
import config
import gui
import ui
import wx
from gui import guiHelper
from gui.message import MessageDialog, ReturnCode
from gui.settingsDialogs import NVDASettingsDialog, SettingsPanel

from .model import device_label, source_label

addonHandler.initTranslation()

# The GlobalPlugin, set when it starts.
plugin = None

VISITOR_NOTE = _(
	"NVDA joins your account as a visitor, like the web client: it is not added as a device, "
	"your other devices cannot listen to it, and your password is not kept."
)


def _alert(message, parent):
	MessageDialog.alert(message, _("AudioNet"), parent=parent)


class AddAccountDialog(wx.Dialog):
	"""Signs NVDA in to an AudioNet account, as a visitor."""

	@classmethod
	def run(cls, parent, then_open_window=False):
		dlg = cls(parent)
		gui.mainFrame.prePopup()
		result = dlg.ShowModal()
		gui.mainFrame.postPopup()
		dlg.Destroy()
		if result == wx.ID_OK and then_open_window:
			wx.CallAfter(plugin.open_window)
		return result == wx.ID_OK

	def __init__(self, parent):
		# Translators: title of the dialog that signs in to an account.
		super().__init__(parent, title=_("Add an AudioNet account"))
		main = wx.BoxSizer(wx.VERTICAL)
		sHelper = guiHelper.BoxSizerHelper(self, orientation=wx.VERTICAL)
		known = [a.server for a in plugin.store.accounts]
		from . import default_server

		server = known[-1] if known else default_server()
		self.server = sHelper.addLabeledControl(_("&Server address:"), wx.TextCtrl, value=server)
		self.username = sHelper.addLabeledControl(_("Account &name:"), wx.TextCtrl)
		self.password = sHelper.addLabeledControl(_("&Password:"), wx.TextCtrl, style=wx.TE_PASSWORD)
		sHelper.addItem(wx.StaticText(self, label=VISITOR_NOTE))
		sHelper.addItem(
			wx.StaticText(self, label=_("No account yet? Create one in your server's web client."))
		)
		forgot = sHelper.addItem(wx.Button(self, label=_("&Forgot password?")))
		forgot.Bind(wx.EVT_BUTTON, self.onForgot)
		buttons = self.CreateStdDialogButtonSizer(wx.OK | wx.CANCEL)
		self.ok = self.FindWindow(wx.ID_OK)
		self.ok.SetLabel(_("Sign &in"))
		sHelper.addDialogDismissButtons(buttons, separated=True)
		self.ok.Bind(wx.EVT_BUTTON, self.onSignIn)
		main.Add(sHelper.sizer, border=guiHelper.BORDER_FOR_DIALOGS, flag=wx.ALL)
		self.SetSizer(main)
		main.Fit(self)
		self.CentreOnScreen()
		(self.username if server else self.server).SetFocus()

	def onForgot(self, evt):
		if not plugin.forgot_password(self.server.GetValue()):
			_alert(_("Enter the server address first, starting with https://."), self)
			self.server.SetFocus()

	def _busy(self, busy):
		for c in (self.server, self.username, self.password, self.ok):
			c.Enable(not busy)

	def onSignIn(self, evt):
		fields = [(self.server, _("the server address")), (self.username, _("your account name")), (self.password, _("your password"))]
		for ctrl, what in fields:
			if not ctrl.GetValue().strip():
				_alert(_("Enter {what}.").format(what=what), self)
				ctrl.SetFocus()
				return
		server = self.server.GetValue().strip()
		if not server.startswith(("https://", "http://")):
			server = "https://" + server
		password = self.password.GetValue()
		self.password.SetValue("")
		self._busy(True)
		ui.message(_("Signing in"))

		def done(account, error):
			if not self:
				return
			self._busy(False)
			if error:
				hint = _(" If you forgot the password, choose Forgot password.") if "password is incorrect" in error else ""
				_alert(_("Signing in failed: {error}").format(error=error) + hint, self)
				self.password.SetFocus()
				return
			ui.message(_("Signed in to {account}").format(account=account.label))
			self.EndModal(wx.ID_OK)

		plugin.sign_in(server, self.username.GetValue().strip(), password, done)


class StatusLogDialog(wx.Dialog):
	"""The events of this NVDA session and the streams' measurements, as
	read-only text."""

	def __init__(self, parent):
		super().__init__(parent, title=_("AudioNet status log"))
		main = wx.BoxSizer(wx.VERTICAL)
		sHelper = guiHelper.BoxSizerHelper(self, orientation=wx.VERTICAL)
		events = plugin.model.log or [_("Nothing has happened yet.")]
		diags = [
			f"{s.title}\n{getattr(s, 'diagnostics', _('Not measured yet.'))}"
			for s in plugin.model.streams.values()
		]
		text = (
			"\n".join(events)
			+ "\n\n"
			+ _("Diagnostics")
			+ "\n"
			+ ("\n\n".join(diags) if diags else _("No active streams."))
		)
		self.text = sHelper.addLabeledControl(
			_("&Log:"),
			wx.TextCtrl,
			value=text,
			style=wx.TE_MULTILINE | wx.TE_READONLY | wx.HSCROLL,
			size=self.scaleSize((600, 350)) if hasattr(self, "scaleSize") else (600, 350),
		)
		buttons = guiHelper.ButtonHelper(wx.HORIZONTAL)
		copy = buttons.addButton(self, label=_("&Copy"))
		copy.Bind(wx.EVT_BUTTON, self.onCopy)
		close = buttons.addButton(self, id=wx.ID_CLOSE, label=_("Close"))
		close.Bind(wx.EVT_BUTTON, lambda e: self.EndModal(wx.ID_CLOSE))
		sHelper.addDialogDismissButtons(buttons)
		self.SetEscapeId(wx.ID_CLOSE)
		main.Add(sHelper.sizer, border=guiHelper.BORDER_FOR_DIALOGS, flag=wx.ALL)
		self.SetSizer(main)
		main.Fit(self)
		self.CentreOnScreen()
		# Start reading at the newest event (the diagnostics follow it).
		self.text.SetInsertionPoint(self.text.XYToPosition(0, len(events) - 1))
		self.text.SetFocus()

	def onCopy(self, evt):
		if wx.TheClipboard.Open():
			wx.TheClipboard.SetData(wx.TextDataObject(self.text.GetValue()))
			wx.TheClipboard.Close()
			ui.message(_("Status log copied"))


class MainWindow(wx.Dialog):
	"""Devices of an account, listening and sending, and the streams NVDA
	started. Closing it keeps the streams running."""

	def __init__(self, owner):
		# Translators: title of the AudioNet window.
		super().__init__(gui.mainFrame, title=_("AudioNet"))
		self.owner = owner
		self._accountIds = []
		self._streamIds = []
		self._localKey = None
		main = wx.BoxSizer(wx.VERTICAL)
		sHelper = guiHelper.BoxSizerHelper(self, orientation=wx.VERTICAL)
		self.account = sHelper.addLabeledControl(_("&Account:"), wx.Choice, choices=[])
		self.account.Bind(wx.EVT_CHOICE, lambda e: self.refresh_tree(rebuild=True))
		self.tree = sHelper.addLabeledControl(
			_("&Devices:"),
			wx.TreeCtrl,
			style=wx.TR_HAS_BUTTONS | wx.TR_HIDE_ROOT | wx.TR_LINES_AT_ROOT | wx.TR_SINGLE,
			size=(520, 260),
		)
		self.root = self.tree.AddRoot("devices")
		self.tree.Bind(wx.EVT_TREE_SEL_CHANGED, lambda e: self.update_buttons())
		self.tree.Bind(wx.EVT_TREE_ITEM_ACTIVATED, self.onActivate)
		self.playOn = sHelper.addLabeledControl(_("Play &on this computer:"), wx.Choice, choices=[])
		self.playOn.Bind(wx.EVT_CHOICE, self.onPlayOn)
		self.sendFrom = sHelper.addLabeledControl(_("Send &from this computer:"), wx.Choice, choices=[])
		self.sendFrom.Bind(wx.EVT_CHOICE, self.onSendFrom)
		actions = guiHelper.ButtonHelper(wx.HORIZONTAL)
		self.listenButton = actions.addButton(self, label=_("&Listen"))
		self.listenButton.Bind(wx.EVT_BUTTON, lambda e: self.listen())
		self.sendButton = actions.addButton(self, label=_("S&end"))
		self.sendButton.Bind(wx.EVT_BUTTON, lambda e: self.send())
		sHelper.addItem(actions)
		self.streams = sHelper.addLabeledControl(_("S&treams:"), wx.ListBox, choices=[], size=(520, 110))
		self.streams.Bind(wx.EVT_LISTBOX, lambda e: self.update_stream_controls())
		self.stopButton = sHelper.addItem(wx.Button(self, label=_("&Stop")))
		self.stopButton.Bind(wx.EVT_BUTTON, self.onStop)
		self.volume = sHelper.addLabeledControl(_("&Volume:"), wx.Slider, value=100, minValue=0, maxValue=100)
		self.volume.SetLineSize(5)
		self.volume.SetPageSize(10)
		self.volume.Bind(wx.EVT_SLIDER, self.onVolume)
		self.mute = sHelper.addItem(wx.CheckBox(self, label=_("&Mute")))
		self.mute.Bind(wx.EVT_CHECKBOX, self.onVolume)
		bottom = guiHelper.ButtonHelper(wx.HORIZONTAL)
		log = bottom.addButton(self, label=_("Status lo&g..."))
		log.Bind(wx.EVT_BUTTON, self.onLog)
		accounts = bottom.addButton(self, label=_("A&ccounts..."))
		accounts.Bind(wx.EVT_BUTTON, self.onAccounts)
		close = bottom.addButton(self, id=wx.ID_CLOSE, label=_("Close"))
		close.Bind(wx.EVT_BUTTON, lambda e: self.Close())
		sHelper.addDialogDismissButtons(bottom, separated=True)
		self.SetEscapeId(wx.ID_CLOSE)
		self.Bind(wx.EVT_CLOSE, self.onClose)
		main.Add(sHelper.sizer, border=guiHelper.BORDER_FOR_DIALOGS, flag=wx.ALL)
		self.SetSizer(main)
		main.Fit(self)
		self.CentreOnScreen()
		self.refresh()
		self.owner.refresh_local_audio()
		(self.tree if self.tree.GetCount() else self.account).SetFocus()

	# ─── what is shown ─────────────────────────────────────────────────────

	def current_account(self):
		i = self.account.GetSelection()
		if 0 <= i < len(self._accountIds):
			return self.owner.model.accounts.get(self._accountIds[i])
		return None

	@staticmethod
	def _setChoice(choice, labels, selected_index):
		if [choice.GetString(i) for i in range(choice.GetCount())] != labels:
			choice.Set(labels)
		if labels and choice.GetSelection() != selected_index:
			choice.SetSelection(max(0, selected_index))

	def refresh(self):
		model = self.owner.model
		chosen = self.current_account()
		chosen_id = chosen.account.id if chosen else None
		states = [model.accounts[a.id] for a in self.owner.store.accounts if a.id in model.accounts]
		self._accountIds = [st.account.id for st in states]
		index = self._accountIds.index(chosen_id) if chosen_id in self._accountIds else 0
		self._setChoice(self.account, [st.label() for st in states], index)
		self.refresh_tree()
		self.refresh_local()
		self.refresh_streams()

	def refresh_log(self):
		pass

	def refresh_local(self):
		model = self.owner.model
		key = (tuple(d["id"] for d in model.local_destinations), tuple(s["id"] for s in model.local_sources))
		if key == self._localKey:
			return
		self._localKey = key
		dests = model.local_destinations
		play = self.owner.default_destination()
		self._setChoice(
			self.playOn,
			[d["name"] for d in dests],
			next((i for i, d in enumerate(dests) if d["id"] == play), 0),
		)
		sources = model.local_sources
		send = self.owner.default_source()
		self._setChoice(
			self.sendFrom,
			[source_label(s) for s in sources],
			next((i for i, s in enumerate(sources) if s["id"] == send), 0),
		)
		self.update_buttons()

	def _wanted(self, st):
		"""The tree as it should be: (key, label, children) for each device."""
		out = []
		for d in st.sorted_devices() if st else []:
			node = d["node_id"]
			groups = []
			if d.get("online"):
				if d.get("sharing", True) and d.get("sources"):
					groups.append(
						(
							("group", node, "sources"),
							_("Sounds to listen to"),
							[(("source", node, s["id"]), source_label(s), []) for s in d["sources"]],
						)
					)
				if d.get("destinations"):
					groups.append(
						(
							("group", node, "destinations"),
							_("Outputs to send to"),
							[(("dest", node, x["id"]), x["name"], []) for x in d["destinations"]],
						)
					)
			out.append((("device", node), device_label(d), groups))
		return out

	def _children(self, parent):
		items = []
		item, cookie = self.tree.GetFirstChild(parent)
		while item.IsOk():
			items.append(item)
			item, cookie = self.tree.GetNextChild(parent, cookie)
		return items

	def _sync(self, parent, wanted, rebuild=False):
		existing = self._children(parent)
		if not rebuild and [self.tree.GetItemData(i) for i in existing] == [w[0] for w in wanted]:
			for item, (key, label, children) in zip(existing, wanted):
				if self.tree.GetItemText(item) != label:
					self.tree.SetItemText(item, label)
				self._sync(item, children)
			return
		# Different items: rebuild this level, keeping what was expanded and
		# selected.
		selected = self.tree.GetSelection()
		selected_key = self.tree.GetItemData(selected) if selected.IsOk() else None
		expanded = set()

		def remember(item):
			for c in self._children(item):
				if self.tree.IsExpanded(c):
					expanded.add(self.tree.GetItemData(c))
				remember(c)

		remember(parent)
		self.tree.DeleteChildren(parent)
		restore = []

		def add(under, items):
			for key, label, children in items:
				item = self.tree.AppendItem(under, label)
				self.tree.SetItemData(item, key)
				add(item, children)
				if key in expanded:
					restore.append(item)
				if key == selected_key:
					restore.append(("select", item))

		add(parent, wanted)
		for r in restore:
			if isinstance(r, tuple):
				self.tree.SelectItem(r[1])
			else:
				self.tree.Expand(r)

	def refresh_tree(self, rebuild=False):
		self._sync(self.root, self._wanted(self.current_account()), rebuild=rebuild)
		self.update_buttons()

	def refresh_streams(self):
		streams = list(self.owner.model.streams.values())
		selected = self.streams.GetSelection()
		selected_id = self._streamIds[selected] if 0 <= selected < len(self._streamIds) else None
		labels = [s.label() for s in streams]
		self._streamIds = [s.session_id for s in streams]
		current = [self.streams.GetString(i) for i in range(self.streams.GetCount())]
		if current != labels:
			if len(current) == len(labels):
				for i, label in enumerate(labels):
					if current[i] != label:
						self.streams.SetString(i, label)
			else:
				self.streams.Set(labels)
		if selected_id in self._streamIds:
			self.streams.SetSelection(self._streamIds.index(selected_id))
		elif labels:
			self.streams.SetSelection(0)
		self.update_stream_controls()

	def selected_stream(self):
		i = self.streams.GetSelection()
		if 0 <= i < len(self._streamIds):
			return self.owner.model.streams.get(self._streamIds[i])
		return None

	def update_stream_controls(self):
		s = self.selected_stream()
		for c in (self.stopButton, self.volume, self.mute):
			c.Enable(s is not None)
		if s is not None:
			if self.volume.GetValue() != s.volume:
				self.volume.SetValue(s.volume)
			if self.mute.GetValue() != s.muted:
				self.mute.SetValue(s.muted)

	def selected_key(self):
		item = self.tree.GetSelection()
		return self.tree.GetItemData(item) if item.IsOk() else None

	def update_buttons(self):
		if getattr(self, "_closing", False):
			return
		key = self.selected_key() or ()
		kind = key[0] if key else None
		self.listenButton.Enable(kind == "source" and self.playOn.GetCount() > 0)
		self.sendButton.Enable(kind == "dest" and self.sendFrom.GetCount() > 0)

	# ─── actions ───────────────────────────────────────────────────────────

	def _chosen(self, choice, items):
		i = choice.GetSelection()
		return items[i]["id"] if 0 <= i < len(items) else None

	def listen(self):
		key, st = self.selected_key(), self.current_account()
		dest = self._chosen(self.playOn, self.owner.model.local_destinations)
		if not key or key[0] != "source" or not st or not dest:
			return
		self.owner.listen(st.account.id, key[1], key[2], dest)

	def send(self):
		key, st = self.selected_key(), self.current_account()
		source = self._chosen(self.sendFrom, self.owner.model.local_sources)
		if not key or key[0] != "dest" or not st or not source:
			return
		self.owner.send(st.account.id, key[1], key[2], source)

	def onActivate(self, evt):
		key = self.tree.GetItemData(evt.GetItem()) or ()
		if key and key[0] == "source":
			self.listen()
		elif key and key[0] == "dest":
			self.send()
		else:
			evt.Skip()

	def onPlayOn(self, evt):
		dest = self._chosen(self.playOn, self.owner.model.local_destinations)
		if dest:
			config.conf["audionet"]["playOn"] = dest

	def onSendFrom(self, evt):
		source = self._chosen(self.sendFrom, self.owner.model.local_sources)
		if source:
			config.conf["audionet"]["sendFrom"] = source

	def onStop(self, evt):
		s = self.selected_stream()
		if s:
			self.owner.stop(s.session_id)
			ui.message(_("Stopped {stream}").format(stream=s.short_title))
			(self.streams if self.streams.GetCount() else self.tree).SetFocus()

	def onVolume(self, evt):
		s = self.selected_stream()
		if s:
			self.owner.set_volume(s, self.volume.GetValue(), self.mute.GetValue())

	def onLog(self, evt):
		dlg = StatusLogDialog(self)
		dlg.ShowModal()
		dlg.Destroy()

	def onAccounts(self, evt):
		gui.mainFrame.popupSettingsDialog(NVDASettingsDialog, AudioNetPanel)

	def onClose(self, evt):
		self.owner.window = None
		# While the window is destroyed the tree reports its items going
		# away as selection changes, after the buttons are gone.
		self._closing = True
		self.tree.Unbind(wx.EVT_TREE_SEL_CHANGED)
		self.Destroy()


class AudioNetPanel(SettingsPanel):
	# Translators: the AudioNet category in NVDA's settings.
	title = _("AudioNet")

	def makeSettings(self, settingsSizer):
		sHelper = guiHelper.BoxSizerHelper(self, sizer=settingsSizer)
		self.accounts = sHelper.addLabeledControl(_("&Accounts:"), wx.ListBox, choices=[])
		buttons = guiHelper.ButtonHelper(wx.HORIZONTAL)
		add = buttons.addButton(self, label=_("A&dd account..."))
		add.Bind(wx.EVT_BUTTON, self.onAdd)
		self.remove = buttons.addButton(self, label=_("&Remove account"))
		self.remove.Bind(wx.EVT_BUTTON, self.onRemove)
		sHelper.addItem(buttons)
		sHelper.addItem(wx.StaticText(self, label=VISITOR_NOTE))
		conf = config.conf["audionet"]
		self.announceDevices = sHelper.addItem(
			wx.CheckBox(self, label=_("Announce when devices go &online or offline"))
		)
		self.announceDevices.SetValue(conf["announceDevices"])
		self.announceStreams = sHelper.addItem(
			wx.CheckBox(self, label=_("Announce when &streams connect and end"))
		)
		self.announceStreams.SetValue(conf["announceStreams"])
		self.refreshAccounts()

	def refreshAccounts(self, select=None):
		states = [plugin.model.accounts.get(a.id) for a in plugin.store.accounts]
		labels = [st.label() if st else a.label for st, a in zip(states, plugin.store.accounts)]
		self.accounts.Set(labels)
		if labels:
			self.accounts.SetSelection(select if select is not None and select < len(labels) else 0)
		self.remove.Enable(bool(labels))

	def onAdd(self, evt):
		if AddAccountDialog.run(self):
			self.refreshAccounts(select=len(plugin.store.accounts) - 1)
			self.accounts.SetFocus()

	def onRemove(self, evt):
		i = self.accounts.GetSelection()
		if not 0 <= i < len(plugin.store.accounts):
			return
		account = plugin.store.accounts[i]
		answer = MessageDialog.confirm(
			_(
				"Remove {account}? NVDA signs out of it and stops its streams. Your devices and the account are not affected."
			).format(account=account.label),
			_("Remove account"),
			parent=self,
		)
		if answer != ReturnCode.OK:
			return
		plugin.remove_account(account)
		self.refreshAccounts(select=max(0, i - 1))
		self.accounts.SetFocus()

	def onSave(self):
		conf = config.conf["audionet"]
		conf["announceDevices"] = self.announceDevices.GetValue()
		conf["announceStreams"] = self.announceStreams.GetValue()
