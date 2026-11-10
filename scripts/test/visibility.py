"""What an account's other devices see: the devices the server lists as
visible, read the way the web client reads them (a browser connection
with the account's session cookie). GET /api/v1/nodes lists every device
in the account, visible or not, so it cannot tell.

Requires: pip install requests websocket-client
"""
import json

import websocket


def visible_devices(base, session):
    """{name: node summary} of the account's visible devices."""
    base = base.rstrip("/")
    url = base.replace("https://", "wss://").replace("http://", "ws://") + "/api/v1/ws"
    cookie = "; ".join(f"{c.name}={c.value}" for c in session.cookies)
    ws = websocket.create_connection(url, header=[f"Cookie: {cookie}", f"Origin: {base}"], timeout=10)
    try:
        ws.send(json.dumps({"type": "hello", "protocol_version": {"major": 0, "minor": 1},
                            "client": {"kind": "browser", "software": "visibility check", "platform": None}}))
        ws.send(json.dumps({"type": "list_nodes"}))
        while True:
            m = json.loads(ws.recv())
            if m["type"] == "nodes":
                return {n["name"]: n for n in m["nodes"]}
            if m["type"] == "error":
                raise RuntimeError(m["message"])
    finally:
        ws.close()


def registered_devices(base, session):
    """{name: device} of every device in the account (visible or not)."""
    base = base.rstrip("/")
    nodes = session.get(f"{base}/api/v1/nodes", headers={"Origin": base}).json()["nodes"]
    return {n["name"]: n for n in nodes}
