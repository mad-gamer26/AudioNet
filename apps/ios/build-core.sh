#!/bin/sh
# Builds AudioNet's native engine (crates/audionet-ffi) for iPhone and iPad
# (device and Apple silicon simulator) as Generated/AudioNetCore.xcframework
# and generates its Swift bindings into Generated/.
#   apps/ios/build-core.sh
set -eu
# The C parts (libopus, crypto) target the app's oldest iOS (project.yml).
export IPHONEOS_DEPLOYMENT_TARGET="${IPHONEOS_DEPLOYMENT_TARGET:-17.0}"
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
out="$here/Generated"
rm -rf "$out"
mkdir -p "$out/include"
cd "$root"
for t in aarch64-apple-ios aarch64-apple-ios-sim; do
    cargo build --release -p audionet-ffi --target "$t"
done
# Bindings from the host build of the same crate (identical interface).
cargo build --release -p audionet-ffi
cargo run --release -q -p audionet-ffi --features bindgen --bin uniffi-bindgen -- \
    generate --library target/release/libaudionet_ffi.dylib --language swift --out-dir "$out"
mv "$out/audionet_ffiFFI.h" "$out/include/"
mv "$out/audionet_ffiFFI.modulemap" "$out/include/module.modulemap"
xcodebuild -create-xcframework \
    -library target/aarch64-apple-ios/release/libaudionet_ffi.a -headers "$out/include" \
    -library target/aarch64-apple-ios-sim/release/libaudionet_ffi.a -headers "$out/include" \
    -output "$out/AudioNetCore.xcframework" >/dev/null
rm -rf "$out/include"
echo "Engine and Swift bindings in $out"
