"""Builds the AudioNet NVDA add-on: dist/audionet-<version>.nvda-addon.

    python nvda-addon/build.py [--default-server https://audionet.example.com] [--no-cargo]

Builds the engine (audionet-visitor.exe, release), puts it in the add-on
with the Python code, writes manifest.ini with the workspace version (every
AudioNet part has the same version), and zips it. --default-server fills in
the server address offered when adding an account (official builds);
without it the field starts empty.
"""
import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import zipfile

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)

MANIFEST = """name = audionet
summary = "AudioNet"
description = \"\"\"Listen to your AudioNet devices and send audio to them from NVDA: another computer's or phone's system sound or microphone on this computer, or this computer's microphone or sound on another device, with low delay and encryption. NVDA joins your accounts as a visitor, like the AudioNet web client: it is not added as a device. NVDA+Alt+A, then H, lists the commands.\"\"\"
author = "The AudioNet contributors"
url = https://github.com/mad-gamer26/AudioNet
version = {version}
docFileName = readme.html
minimumNVDAVersion = 2026.1.0
lastTestedNVDAVersion = 2026.2.0
"""


def workspace_version():
	with open(os.path.join(ROOT, "Cargo.toml"), encoding="utf-8") as f:
		m = re.search(r'^version = "([^"]+)"', f.read(), re.M)
	return m.group(1)


def main():
	ap = argparse.ArgumentParser()
	ap.add_argument("--default-server", default="")
	ap.add_argument("--no-cargo", action="store_true", help="use the engine already built")
	args = ap.parse_args()
	version = workspace_version()
	if not args.no_cargo:
		subprocess.run(["cargo", "build", "--release", "-p", "audionet-visitor"], cwd=ROOT, check=True)
	exe = os.path.join(ROOT, "target", "release", "audionet-visitor.exe")
	if not os.path.isfile(exe):
		sys.exit(f"{exe} is missing: build it first")
	stage = tempfile.mkdtemp(prefix="audionet-nvda-")
	try:
		shutil.copytree(
			os.path.join(HERE, "addon"),
			os.path.join(stage, "addon"),
			ignore=shutil.ignore_patterns("__pycache__", "*.pyc", "bin", "defaults.json"),
		)
		addon = os.path.join(stage, "addon")
		plugin = os.path.join(addon, "globalPlugins", "audionet")
		os.makedirs(os.path.join(plugin, "bin"))
		shutil.copy2(exe, os.path.join(plugin, "bin", "audionet-visitor.exe"))
		shutil.copy2(os.path.join(ROOT, "LICENSE"), os.path.join(addon, "LICENSE"))
		if args.default_server:
			with open(os.path.join(plugin, "defaults.json"), "w", encoding="utf-8") as f:
				json.dump({"server": args.default_server.rstrip("/")}, f)
		with open(os.path.join(addon, "manifest.ini"), "w", encoding="utf-8", newline="\n") as f:
			f.write(MANIFEST.format(version=version))
		out_dir = os.path.join(ROOT, "dist")
		os.makedirs(out_dir, exist_ok=True)
		out = os.path.join(out_dir, f"audionet-{version}.nvda-addon")
		with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED) as z:
			for folder, _dirs, files in os.walk(addon):
				for name in sorted(files):
					path = os.path.join(folder, name)
					z.write(path, os.path.relpath(path, addon))
		print(f"Add-on: {out}")
		print(f"Version: {version}")
		print(f"Default server: {args.default_server or 'none'}")
	finally:
		shutil.rmtree(stage, ignore_errors=True)


if __name__ == "__main__":
	main()
