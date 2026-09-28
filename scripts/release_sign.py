"""Release signing for AudioNet's automatic updates (Ed25519).

  python scripts/release_sign.py keygen KEY_FILE PUBLIC_KEY_FILE
      Creates a signing key (private: keep it off the repository and back it
      up; losing it means installed copies cannot update past it).
  python scripts/release_sign.py public KEY_FILE
      Prints the public key in hex (what builds embed).
  python scripts/release_sign.py manifest KEY_FILE ZIP VERSION OUT_DIR [--product P --name N]
      Writes OUT_DIR/latest.json for ZIP and its signature latest.json.sig.
      The Mac app: --product audionet-macos-universal --name latest-macos.json

Requires: pip install cryptography
"""
import hashlib, json, os, sys
from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

PRODUCT = "audionet-windows-x64"

def load(path):
    with open(path, "rb") as f:
        return serialization.load_pem_private_key(f.read(), password=None)

def public_hex(key):
    return key.public_key().public_bytes(serialization.Encoding.Raw, serialization.PublicFormat.Raw).hex()

cmd = sys.argv[1] if len(sys.argv) > 1 else ""
if cmd == "keygen":
    key_file, pub_file = sys.argv[2], sys.argv[3]
    if os.path.exists(key_file):
        raise SystemExit(f"{key_file} already exists; not overwriting a release key")
    key = Ed25519PrivateKey.generate()
    os.makedirs(os.path.dirname(os.path.abspath(key_file)), exist_ok=True)
    with open(key_file, "wb") as f:
        f.write(key.private_bytes(serialization.Encoding.PEM, serialization.PrivateFormat.PKCS8,
                                  serialization.NoEncryption()))
    with open(pub_file, "w", newline="\n") as f:
        f.write(public_hex(key) + "\n")
    print("public key:", public_hex(key))
elif cmd == "public":
    print(public_hex(load(sys.argv[2])))
elif cmd == "manifest":
    key_file, zip_path, version, out_dir = sys.argv[2:6]
    opts = dict(zip(sys.argv[6::2], sys.argv[7::2]))
    product = opts.get("--product", PRODUCT)
    name = opts.get("--name", "latest.json")
    data = open(zip_path, "rb").read()
    manifest = {
        "schema": 1,
        "product": product,
        "version": version,
        "file": os.path.basename(zip_path),
        "sha256": hashlib.sha256(data).hexdigest(),
        "size": len(data),
    }
    body = (json.dumps(manifest, indent=2) + "\n").encode()
    with open(os.path.join(out_dir, name), "wb") as f:
        f.write(body)
    with open(os.path.join(out_dir, name + ".sig"), "w", newline="\n") as f:
        f.write(load(key_file).sign(body).hex() + "\n")
    print(f"signed {name} for {manifest['file']} ({version}, {product})")
else:
    raise SystemExit(__doc__)
