#!/bin/sh
# Builds AudioNet's native engine (crates/audionet-ffi) as a static library
# for this Mac and generates its Swift bindings into Generated/.
#   apps/macos/build-core.sh [--universal]
# --universal builds arm64 and x86_64 and joins them (for distribution).
set -eu
# The C parts (libopus, crypto) must target the same oldest macOS as the
# app (project.yml), not the macOS doing the build.
export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-14.0}"
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
out="$here/Generated"
mkdir -p "$out"
cd "$root"
if [ "${1:-}" = "--universal" ]; then
    for t in aarch64-apple-darwin x86_64-apple-darwin; do
        cargo build --release -p audionet-ffi --target "$t"
    done
    lipo -create \
        target/aarch64-apple-darwin/release/libaudionet_ffi.a \
        target/x86_64-apple-darwin/release/libaudionet_ffi.a \
        -output "$out/libaudionet_ffi.a"
    lib=target/aarch64-apple-darwin/release/libaudionet_ffi.dylib
else
    cargo build --release -p audionet-ffi
    cp target/release/libaudionet_ffi.a "$out/"
    lib=target/release/libaudionet_ffi.dylib
fi
cargo run --release -q -p audionet-ffi --features bindgen --bin uniffi-bindgen -- \
    generate --library "$lib" --language swift --out-dir "$out"
# Xcode imports the C part as a module named after the modulemap.
mv -f "$out/audionet_ffiFFI.modulemap" "$out/module.modulemap"
echo "Engine and Swift bindings in $out"
