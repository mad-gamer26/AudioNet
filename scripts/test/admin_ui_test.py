"""Clicks through the admin page in headless Chromium (Playwright), against a
throwaway audionet-server (never a live one):

    audionet-server --config T/config.toml serve      # public :18740, admin :18741
    python scripts/test/admin_ui_test.py http://127.0.0.1:18741 http://127.0.0.1:18740

A fake device (a minimal WebSocket client, standard library only) signs in
like the apps and offers a source, so the Live view and the account page
have a real device to show, disconnect and remove.
"""

import base64
import json
import os
import re
import socket
import sys
import threading
import time
import urllib.request

from playwright.sync_api import expect, sync_playwright

ADMIN, PUBLIC = sys.argv[1].rstrip("/"), sys.argv[2].rstrip("/")
results = []


def ok(what, cond, detail=""):
    results.append(bool(cond))
    print(("PASS " if cond else "FAIL ") + what + ("" if cond else f" ({detail})"), flush=True)


def post(url, body, headers=None):
    req = urllib.request.Request(url, data=json.dumps(body).encode(), method="POST",
                                 headers={"Content-Type": "application/json", **(headers or {})})
    with urllib.request.urlopen(req, timeout=10) as r:
        return json.loads(r.read())


class FakeDevice:
    """A device on the WebSocket: hello, endpoints, then keeps reading."""

    def __init__(self, token):
        host, port = PUBLIC.split("//")[1].split(":")
        self.sock = socket.create_connection((host, int(port)), timeout=10)
        key = base64.b64encode(os.urandom(16)).decode()
        self.sock.sendall((f"GET /api/v1/ws HTTP/1.1\r\nHost: {host}:{port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
                           f"Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\nAuthorization: Bearer {token}\r\n\r\n").encode())
        head = b""
        while b"\r\n\r\n" not in head:
            chunk = self.sock.recv(4096)
            if not chunk:
                raise ConnectionError("closed during the handshake")
            head += chunk
        if b" 101 " not in head.split(b"\r\n")[0]:
            raise ConnectionError(head.split(b"\r\n")[0].decode())
        self.closed = threading.Event()
        self.send({"type": "hello", "protocol_version": {"major": 0, "minor": 1},
                   "client": {"kind": "node", "software": "audionet-desktop 9.9.9", "platform": "windows", "visible": True}})
        self.send({"type": "endpoints", "sources": [{"id": "src1", "name": "Speakers", "source_type": "loopback", "is_default": True}],
                   "destinations": []})
        threading.Thread(target=self._read, daemon=True).start()

    def send(self, obj):
        data = json.dumps(obj).encode()
        mask = os.urandom(4)
        n = len(data)
        head = bytes([0x81]) + (bytes([0x80 | n]) if n < 126 else bytes([0x80 | 126]) + n.to_bytes(2, "big"))
        self.sock.sendall(head + mask + bytes(b ^ mask[i % 4] for i, b in enumerate(data)))

    def _read(self):
        self.sock.settimeout(None)
        try:
            while True:
                b = self.sock.recv(65536)
                if not b:
                    break
                # A ping (0x89) gets a pong so the server keeps us.
                if b[0] == 0x89:
                    mask = os.urandom(4)
                    self.sock.sendall(bytes([0x8A, 0x80]) + mask)
                if b[0] == 0x88:  # close
                    break
        except OSError:
            pass
        self.closed.set()


def unlabeled(page):
    return page.evaluate("""() => [...document.querySelectorAll('input, select, button')]
      .filter(el => !el.closest('[hidden]') && !el.classList.contains('offscreen'))
      .filter(el => el.tagName === 'BUTTON' ? !(el.getAttribute('aria-label') || el.textContent.trim())
        : !((el.labels && el.labels.length) || el.getAttribute('aria-label')))
      .map(el => el.outerHTML.slice(0, 80))""")


def wait_announce(page, pattern, timeout=10):
    end = time.time() + timeout
    t = ""
    while time.time() < end:
        t = page.locator("#announce").text_content() or ""
        if re.search(pattern, t):
            return t
        time.sleep(0.2)
    return t


with sync_playwright() as p:
    browser = p.chromium.launch()
    page = browser.new_page()
    answers = []  # answers for prompt() dialogs, in order
    seen = []
    page.on("dialog", lambda d: (seen.append(d.message), d.accept(answers.pop(0)) if d.type == "prompt" and answers else d.accept()))

    page.goto(ADMIN + "/")
    page.wait_for_selector("text=AudioNet server")
    ok("the overview shows the version and counts", "running for" in page.locator("#view").text_content())
    ok("the settings table shows the admin listener", page.locator("td", has_text="127.0.0.1:18741").count() == 1)
    ok("focus starts on the heading", page.evaluate("document.activeElement.id") == "page-heading")

    # Create an account through the form.
    page.goto(ADMIN + "/#/accounts")
    page.get_by_label("Username").fill("kim")
    page.get_by_label("Password", exact=True).fill("correct horse battery")
    page.get_by_role("button", name="Show password").click()
    ok("Show reveals the password", page.get_by_label("Password", exact=True).get_attribute("type") == "text")
    page.get_by_label("Email address (optional)").fill("kim@example.com")
    ok("every control on the accounts view has a name", not unlabeled(page), unlabeled(page))
    page.get_by_role("button", name="Create the account").click()
    msg = wait_announce(page, "Created the account kim")
    ok("creating is announced", "Created the account kim" in msg, msg)
    expect(page.get_by_role("link", name="kim")).to_be_focused(timeout=5000)
    ok("focus lands on the new account", True)

    # A device and a browser, connected for real.
    token = post(PUBLIC + "/api/v1/nodes/sign-in", {"username": "kim", "password": "correct horse battery",
                                                    "name": "Studio PC", "platform": "windows"})["token"]
    dev = FakeDevice(token)
    tab = browser.new_page()
    tab.goto(PUBLIC + "/")
    tab.locator("#username").fill("kim")
    tab.locator("#password").fill("correct horse battery")
    tab.locator("#sign-in-form button[type=submit]").click()
    tab.wait_for_timeout(2500)

    page.goto(ADMIN + "/#/live")
    page.wait_for_selector("text=Connections (2)", timeout=10000)
    row = page.locator("tr", has=page.locator("td", has_text="Studio PC"))
    txt = row.text_content()
    ok("Live shows the device with its address, app and source", all(x in txt for x in ("127.0.0.1", "audionet-desktop 9.9.9", "Speakers")), txt)
    ok("and the web browser", page.locator("td", has_text="Web browser").count() == 1)
    ok("every control on the Live view has a name", not unlabeled(page), unlabeled(page))

    # The account page: device, browser, sign-ins.
    page.goto(ADMIN + "/#/account/kim")
    page.wait_for_selector("text=Devices (1)")
    ok("the account shows the device as connected", "Connected for" in page.locator("tr", has_text="Studio PC").text_content())
    ok("and one open browser", page.locator("h2", has_text="Browsers connected now (1)").count() == 1)
    ok("every control on the account view has a name", not unlabeled(page), unlabeled(page))

    # Sign-in link.
    page.get_by_role("button", name="Make a sign-in link and copy it").click()
    link = page.get_by_label("Sign-in link", exact=True).input_value()
    ok("a sign-in link is made", link.startswith(PUBLIC + "/?signin=anl_"), link)

    # Rename (a prompt), disconnect, remove.
    answers.append("Mixing desk")
    page.get_by_role("button", name="Rename Studio PC").click()
    msg = wait_announce(page, "Renamed")
    ok("renaming is announced", "Renamed kim's device Studio PC to Mixing desk" in msg, msg)
    page.wait_for_selector("text=Mixing desk")
    page.get_by_role("button", name="Disconnect Mixing desk").click()
    ok("Disconnect drops the device", dev.closed.wait(5))
    dev = FakeDevice(token)
    ok("and it can reconnect (still signed in)", not dev.closed.wait(1))
    page.reload()
    page.wait_for_selector("text=Devices (1)")
    page.get_by_role("button", name="Remove Mixing desk").click()
    msg = wait_announce(page, "Removed")
    ok("removing is announced", "signed out and disconnected" in msg, msg)
    ok("Remove drops the device at once", dev.closed.wait(5))
    try:
        FakeDevice(token)
        ok("and it can't come back", False)
    except ConnectionError:
        ok("and it can't come back", True)

    # Browsers.
    page.get_by_role("button", name="Sign out every browser").click()
    msg = wait_announce(page, "Signed out every browser")
    ok("signing out every browser is announced", "1 open browser closed" in msg, msg)
    live = json.loads(urllib.request.urlopen(ADMIN + "/api/live").read())
    ok("the open browser is gone", not [c for c in live["connections"] if c["kind"] == "browser"], live)

    # Log.
    page.goto(ADMIN + "/#/log")
    page.get_by_label("Only admin actions").check()
    text = page.locator("pre").text_content()
    ok("the log shows the admin actions", all(x in text for x in ("created the account kim", "removed kim's device Mixing desk")), text[-500:])

    # Delete.
    page.goto(ADMIN + "/#/account/kim")
    page.get_by_role("button", name="Delete kim").click()
    page.wait_for_url(re.compile("#/accounts$"))
    msg = wait_announce(page, "Deleted the account kim")
    ok("deleting asks twice and is announced", "Deleted the account kim" in msg and sum("delete" in m.lower() for m in seen) >= 2, msg)
    ok("the account is gone", page.get_by_role("link", name="kim").count() == 0)
    browser.close()

print(f"\n{sum(results)}/{len(results)} checks passed")
sys.exit(0 if all(results) else 1)
