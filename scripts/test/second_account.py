"""A temporary second AudioNet account, for testing apps signed in to more
than one account. It is created with the server's own administration
command (`audionet-server user add`) over SSH, with a random password that
is never printed, and deleted afterwards with its devices.

Environment: AUDIONET_SERVER_SSH (required: the server's SSH address, with
passwordless sudo), AUDIONET_SERVER_USER (the service user, default audionet),
AUDIONET_SERVER_BIN (default /opt/audionet/bin/audionet-server) and
AUDIONET_SERVER_CONFIG (default /etc/audionet/config.toml).
"""
import os, secrets, string, subprocess
import testenv

SSH = testenv.need("AUDIONET_SERVER_SSH", "admin@audionet.example.com")
SERVICE_USER = os.environ.get("AUDIONET_SERVER_USER", "audionet")
BIN = os.environ.get("AUDIONET_SERVER_BIN", "/opt/audionet/bin/audionet-server")
CONFIG = os.environ.get("AUDIONET_SERVER_CONFIG", "/etc/audionet/config.toml")


def _admin(*args, stdin=""):
    cmd = f"sudo -u {SERVICE_USER} {BIN} -c {CONFIG} " + " ".join(args)
    return subprocess.run(["ssh", SSH, cmd], input=stdin, text=True, capture_output=True)


def create():
    """Creates a temporary account; returns (name, password)."""
    name = "uitest-" + secrets.token_hex(4)
    alphabet = string.ascii_letters + string.digits
    password = "".join(secrets.choice(alphabet) for _ in range(24))
    r = _admin("user", "add", name, stdin=password + "\n")
    if r.returncode != 0:
        raise RuntimeError(f"could not create the temporary account: {r.stderr.strip() or r.stdout.strip()}")
    return name, password


def delete(name):
    """Deletes the temporary account and its devices."""
    r = _admin("user", "delete", name, stdin="y\n")
    if r.returncode != 0:
        print(f"clean-up: could not delete the temporary account {name}: {r.stderr.strip() or r.stdout.strip()}")
