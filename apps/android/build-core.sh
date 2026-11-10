#!/bin/sh
# Builds AudioNet's native engine (crates/audionet-ffi) for Android phones
# (arm64-v8a) and the x86_64 emulator into app/src/main/jniLibs, and
# generates its Kotlin bindings into app/src/main/java/uniffi (both
# generated, not committed). The macOS and Linux counterpart of
# build-core.ps1; release builds are made on a Mac (docs/releasing.md).
#
#   apps/android/build-core.sh [--emulator-only]
#
# Needs Rust with the aarch64-linux-android and x86_64-linux-android
# targets, cargo-ndk (cargo install cargo-ndk), and the Android NDK (r28 or
# later: 16 KB pages). ANDROID_NDK_HOME may name the NDK; otherwise the
# newest one in the Android SDK (ANDROID_HOME, or the usual place) is used.
# Android 8.0 (API 26) and later.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
if [ -z "${ANDROID_NDK_HOME:-}" ]; then
    sdk=${ANDROID_HOME:-}
    if [ -z "$sdk" ]; then
        case "$(uname)" in
            Darwin) sdk="$HOME/Library/Android/sdk" ;;
            *) sdk="$HOME/Android/Sdk" ;;
        esac
    fi
    ndk=$(ls -d "$sdk"/ndk/*/ 2>/dev/null | sort -V | tail -n 1)
    if [ -z "$ndk" ]; then
        echo "No Android NDK found in $sdk: install one with the SDK manager (ndk;28...)" >&2
        exit 1
    fi
    export ANDROID_NDK_HOME="${ndk%/}"
fi
jni="$here/app/src/main/jniLibs"
if [ "${1:-}" = "--emulator-only" ]; then
    set -- -t x86_64
else
    set -- -t arm64-v8a -t x86_64
fi
cd "$root"
cargo ndk "$@" --platform 26 -o "$jni" build --release -p audionet-ffi --lib
# Bindings from one of the libraries just built (identical interface).
cargo run --release -q -p audionet-ffi --features bindgen --bin uniffi-bindgen -- \
    generate --library "$root/target/x86_64-linux-android/release/libaudionet_ffi.so" \
    --language kotlin --no-format --out-dir "$here/app/src/main/java"
echo "Engine in $jni; Kotlin bindings in app/src/main/java/uniffi"
