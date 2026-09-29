# AudioNet for NVDA: the accounts NVDA is signed in to.
# Each account keeps its server, name and web sign-in (a session token, not
# the password). Tokens are encrypted with Windows DPAPI for the current
# Windows user, so the file is useless to anyone else. Plain Python with no
# NVDA imports.
# Copyright (c) 2026 The AudioNet contributors. Licensed under the MIT license.

import base64
import ctypes
import json
import os
import uuid
from ctypes import wintypes


class _Blob(ctypes.Structure):
	_fields_ = [("cbData", wintypes.DWORD), ("pbData", ctypes.POINTER(ctypes.c_char))]


def _blob(data):
	buf = ctypes.create_string_buffer(data, len(data))
	return _Blob(len(data), ctypes.cast(buf, ctypes.POINTER(ctypes.c_char))), buf


def _crypt(fn, data):
	crypt32 = ctypes.windll.crypt32
	kernel32 = ctypes.windll.kernel32
	src, _keep = _blob(data)
	out = _Blob()
	# CRYPTPROTECT_UI_FORBIDDEN: never show a prompt.
	ok = getattr(crypt32, fn)(ctypes.byref(src), None, None, None, None, 0x1, ctypes.byref(out))
	if not ok:
		raise OSError(f"Windows could not {'protect' if fn == 'CryptProtectData' else 'read'} the sign-in.")
	try:
		return ctypes.string_at(out.pbData, out.cbData)
	finally:
		kernel32.LocalFree(out.pbData)


def protect(text):
	return base64.b64encode(_crypt("CryptProtectData", text.encode("utf-8"))).decode("ascii")


def unprotect(stored):
	return _crypt("CryptUnprotectData", base64.b64decode(stored)).decode("utf-8")


class Account:
	def __init__(self, id, server, username, token):
		self.id = id
		self.server = server
		self.username = username
		self.token = token

	@property
	def host(self):
		return self.server.split("://", 1)[-1].rstrip("/")

	@property
	def label(self):
		return f"{self.username} on {self.host}"


class AccountStore:
	"""The accounts, kept in a JSON file (tokens encrypted)."""

	def __init__(self, path):
		self.path = path
		self.accounts = []
		self.load()

	def load(self):
		self.accounts = []
		try:
			with open(self.path, encoding="utf-8") as f:
				data = json.load(f)
		except (OSError, ValueError):
			return
		for a in data.get("accounts", []):
			try:
				token = unprotect(a["token"])
			except (OSError, KeyError, ValueError):
				# Another Windows user's file, or damaged: sign in again.
				token = ""
			self.accounts.append(Account(a["id"], a["server"], a["username"], token))

	def save(self):
		os.makedirs(os.path.dirname(self.path), exist_ok=True)
		data = {
			"version": 1,
			"accounts": [
				{"id": a.id, "server": a.server, "username": a.username, "token": protect(a.token) if a.token else ""}
				for a in self.accounts
			],
		}
		tmp = self.path + ".tmp"
		with open(tmp, "w", encoding="utf-8") as f:
			json.dump(data, f, indent=1)
		os.replace(tmp, self.path)

	def get(self, account_id):
		return next((a for a in self.accounts if a.id == account_id), None)

	def find(self, server, username):
		return next(
			(a for a in self.accounts if a.server == server and a.username.lower() == username.lower()),
			None,
		)

	def add(self, server, username, token):
		"""Adds an account, or renews the sign-in of the same one."""
		existing = self.find(server, username)
		if existing:
			existing.token = token
			existing.username = username
			self.save()
			return existing
		account = Account(uuid.uuid4().hex, server, username, token)
		self.accounts.append(account)
		self.save()
		return account

	def remove(self, account_id):
		self.accounts = [a for a in self.accounts if a.id != account_id]
		self.save()

	def clear_token(self, account_id):
		a = self.get(account_id)
		if a:
			a.token = ""
			self.save()
