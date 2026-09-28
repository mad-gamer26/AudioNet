"""Settings for the test scripts that name machines: the Mac they reach
over SSH, the iOS simulator, the server's SSH address. They come from the
environment, or from scripts/test/local.env (KEY=value lines, not committed:
it names your own machines). Nothing here has a built-in default."""
import os, sys

_HERE = os.path.dirname(os.path.abspath(__file__))
_LOCAL = os.path.join(_HERE, "local.env")
if os.path.exists(_LOCAL):
    for line in open(_LOCAL, encoding="utf-8"):
        line = line.strip()
        if line and not line.startswith("#") and "=" in line:
            key, value = line.split("=", 1)
            os.environ.setdefault(key.strip(), value.strip())


def need(key, example):
    """The setting `key`, or a clear message saying how to give it."""
    value = os.environ.get(key, "").strip()
    if not value:
        sys.exit(f"Set {key} (for example {example}) in the environment or in scripts/test/local.env.")
    return value
