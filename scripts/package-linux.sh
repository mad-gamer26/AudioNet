#!/bin/sh
# Builds the Linux release package: dist/audionet-linux-x64-<version>.tar.gz
# with the `audionet` program (the device agent and command line), the
# Linux guide and the license, and its .sha256.
#
#   scripts/package-linux.sh
#
# Run on Linux (x86_64) with Rust, gcc, pkg-config and the ALSA development
# files (libasound2-dev). Build on the oldest Linux it should run on: the
# program needs that glibc or later (built on Ubuntu 22.04, it needs 2.34).
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
version=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)
target=${CARGO_TARGET_DIR:-target}
cargo build --release -p audionet-cli
name="audionet-linux-x64-$version"
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
mkdir "$stage/$name"
cp "$target/release/audionet" "$stage/$name/audionet"
strip "$stage/$name/audionet"
cp docs/linux.md "$stage/$name/README.md"
cp LICENSE "$stage/$name/LICENSE"
mkdir -p dist
tar -C "$stage" -czf "dist/$name.tar.gz" "$name"
(cd dist && sha256sum "$name.tar.gz" > "$name.tar.gz.sha256")
echo "Package: dist/$name.tar.gz"
echo "SHA-256: $(cut -d' ' -f1 "dist/$name.tar.gz.sha256")"
