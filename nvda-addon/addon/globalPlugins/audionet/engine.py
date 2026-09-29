# AudioNet for NVDA: the engine client.
# Starts audionet-visitor.exe (the AudioNet engine, outside NVDA's process)
# and talks to it in JSON lines. Plain Python with no NVDA imports, so it
# can be tested on its own.
# Copyright (c) 2026 The AudioNet contributors. Licensed under the MIT license.

import itertools
import json
import os
import subprocess
import threading


class EngineError(Exception):
	"""A request the engine could not carry out; the message is in words."""


class Engine:
	"""The running engine program.

	on_event(dict) receives every event, and on_exit(str) is called once if
	the program stops unexpectedly. Both are called through call_after, which
	in NVDA is wx.CallAfter so they run on the main thread.
	"""

	def __init__(self, exe_path, on_event, on_exit=None, call_after=None):
		self._exe = exe_path
		self._on_event = on_event
		self._on_exit = on_exit
		self._call_after = call_after or (lambda f, *a: f(*a))
		self._proc = None
		self._ids = itertools.count(1)
		# Request id -> (callback, answer on the reader thread).
		self._pending = {}
		self._lock = threading.Lock()
		self._stopping = False

	@property
	def running(self):
		return self._proc is not None and self._proc.poll() is None

	def start(self):
		if self.running:
			return
		self._stopping = False
		flags = getattr(subprocess, "CREATE_NO_WINDOW", 0)
		self._proc = subprocess.Popen(
			[self._exe],
			stdin=subprocess.PIPE,
			stdout=subprocess.PIPE,
			stderr=subprocess.DEVNULL,
			cwd=os.path.dirname(self._exe),
			creationflags=flags,
		)
		threading.Thread(
			target=self._read,
			args=(self._proc,),
			name="AudioNet engine reader",
			daemon=True,
		).start()

	def _answer(self, waiter, result, error):
		callback, direct = waiter
		if direct:
			callback(result, error)
		else:
			self._call_after(callback, result, error)

	def _read(self, proc):
		for raw in proc.stdout:
			try:
				msg = json.loads(raw.decode("utf-8"))
			except ValueError:
				continue
			if "event" in msg:
				self._call_after(self._on_event, msg)
				continue
			with self._lock:
				waiter = self._pending.pop(msg.get("id"), None)
			if waiter is not None:
				if msg.get("ok"):
					self._answer(waiter, msg.get("result"), None)
				else:
					self._answer(waiter, None, msg.get("error") or "unknown problem")
		# The program ended: whatever was waiting will not be answered.
		with self._lock:
			waiting = list(self._pending.values())
			self._pending.clear()
		for waiter in waiting:
			self._answer(waiter, None, "The AudioNet engine stopped.")
		if not self._stopping and self._on_exit is not None:
			self._call_after(self._on_exit, "The AudioNet engine stopped unexpectedly.")

	def request(self, cmd, callback=None, _direct=False, **params):
		"""Sends a request. callback(result, error) is called with the answer:
		result a dict, or error a message in words."""
		waiter = (callback, _direct) if callback is not None else None
		if not self.running:
			if waiter:
				self._answer(waiter, None, "The AudioNet engine is not running.")
			return
		rid = next(self._ids)
		line = json.dumps(dict(params, id=rid, cmd=cmd)) + "\n"
		with self._lock:
			if waiter:
				self._pending[rid] = waiter
			try:
				self._proc.stdin.write(line.encode("utf-8"))
				self._proc.stdin.flush()
			except OSError:
				self._pending.pop(rid, None)
				if waiter:
					self._answer(waiter, None, "The AudioNet engine stopped.")

	def call(self, cmd, timeout=30, **params):
		"""Sends a request and waits for its answer. Not for NVDA's main
		thread (that would block NVDA). Returns the result or raises
		EngineError."""
		done = threading.Event()
		box = {}

		def got(result, error):
			box["result"], box["error"] = result, error
			done.set()

		self.request(cmd, got, _direct=True, **params)
		if not done.wait(timeout):
			raise EngineError(f"The AudioNet engine did not answer {cmd}.")
		if box["error"]:
			raise EngineError(box["error"])
		return box["result"]

	def stop(self):
		"""Ends every stream and the program."""
		proc = self._proc
		if proc is None:
			return
		self._stopping = True
		try:
			proc.stdin.close()
		except OSError:
			pass
		try:
			proc.wait(timeout=3)
		except subprocess.TimeoutExpired:
			proc.kill()
		self._proc = None
