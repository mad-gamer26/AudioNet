"""Unit tests for the add-on's parts that do not need NVDA: the account
store (tokens encrypted with DPAPI) and the words the add-on says.

    python -m unittest discover nvda-addon/tests
"""
import os
import sys
import tempfile
import unittest

sys.path.insert(
	0,
	os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "addon", "globalPlugins", "audionet"),
)

import accounts  # noqa: E402
import model  # noqa: E402


class AccountStoreTests(unittest.TestCase):
	def setUp(self):
		self.dir = tempfile.mkdtemp()
		self.path = os.path.join(self.dir, "audionet", "accounts.json")

	def test_tokens_are_encrypted_and_come_back(self):
		store = accounts.AccountStore(self.path)
		a = store.add("https://audionet.example.com", "alice", "ans_secret-token")
		with open(self.path, encoding="utf-8") as f:
			text = f.read()
		self.assertNotIn("ans_secret-token", text)
		self.assertIn("alice", text)
		again = accounts.AccountStore(self.path)
		self.assertEqual(again.get(a.id).token, "ans_secret-token")
		self.assertEqual(again.get(a.id).label, "alice on audionet.example.com")

	def test_signing_in_again_renews_rather_than_adding(self):
		store = accounts.AccountStore(self.path)
		a = store.add("https://audionet.example.com", "alice", "ans_1")
		b = store.add("https://audionet.example.com", "ALICE", "ans_2")
		self.assertEqual(a.id, b.id)
		self.assertEqual(len(store.accounts), 1)
		store.add("https://other.example.org", "alice", "ans_3")
		self.assertEqual(len(store.accounts), 2)

	def test_remove_and_clear(self):
		store = accounts.AccountStore(self.path)
		a = store.add("https://audionet.example.com", "alice", "ans_1")
		store.clear_token(a.id)
		self.assertEqual(accounts.AccountStore(self.path).get(a.id).token, "")
		store.remove(a.id)
		self.assertEqual(accounts.AccountStore(self.path).accounts, [])

	def test_a_damaged_token_means_signing_in_again(self):
		store = accounts.AccountStore(self.path)
		store.add("https://audionet.example.com", "alice", "ans_1")
		with open(self.path, encoding="utf-8") as f:
			text = f.read()
		with open(self.path, "w", encoding="utf-8") as f:
			f.write(text.replace(store.accounts[0].id, store.accounts[0].id).replace('"token": "', '"token": "AAAA'))
		self.assertEqual(accounts.AccountStore(self.path).accounts[0].token, "")

	def test_no_file_is_no_accounts(self):
		self.assertEqual(accounts.AccountStore(self.path).accounts, [])


class WordsTests(unittest.TestCase):
	def test_devices(self):
		d = {"name": "Studio PC", "online": True, "sharing": True, "platform": "windows"}
		self.assertEqual(model.device_label(d), "Studio PC: online, sharing, Windows")
		d.update(sharing=False, platform="ios")
		self.assertEqual(model.device_label(d), "Studio PC: online, not sharing, iPhone")
		self.assertEqual(model.device_label({"name": "Mac", "online": False, "platform": None}), "Mac: offline")

	def test_sources(self):
		self.assertEqual(
			model.source_label({"name": "Speakers", "source_type": "loopback"}), "Sound playing on Speakers"
		)
		self.assertEqual(model.source_label({"name": "Yeti", "source_type": "input"}), "Yeti")

	def test_streams(self):
		s = model.Stream("s1", "a", "listen", "n", "Studio PC", "loopback:1", "Sound playing on Speakers", "out", "Headphones")
		self.assertEqual(s.label(), "Listening to Sound playing on Speakers on Studio PC, playing on Headphones: connecting")
		s.state, s.volume, s.muted = "connected", 60, True
		self.assertTrue(s.label().endswith(": connected, volume 60 percent, muted"), s.label())
		t = model.Stream("s2", "a", "send", "n", "iPhone", "output:1", "Speaker", "input:1", "Yeti")
		self.assertEqual(t.short_title, "Sending to iPhone")
		self.assertEqual(t.title, "Sending Yeti to Speaker on iPhone")

	def test_offline_devices_sort_last(self):
		st = model.AccountState(accounts.Account("a", "https://x.example", "u", "t"))
		st.devices = {
			"1": {"node_id": "1", "name": "b", "online": False},
			"2": {"node_id": "2", "name": "Z", "online": True},
			"3": {"node_id": "3", "name": "a", "online": True},
		}
		self.assertEqual([d["name"] for d in st.sorted_devices()], ["a", "Z", "b"])

	def test_log_is_bounded(self):
		m = model.Model()
		for i in range(model.LOG_LINES + 20):
			m.add_log(f"line {i}")
		self.assertEqual(len(m.log), model.LOG_LINES)
		self.assertTrue(m.log[-1].endswith(f"line {model.LOG_LINES + 19}"))


if __name__ == "__main__":
	unittest.main()
