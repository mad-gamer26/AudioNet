# Builds AudioNet's native engine (crates/audionet-ffi) for Android phones
# (arm64-v8a) and the x86_64 emulator into app/src/main/jniLibs, and
# generates its Kotlin bindings into app/src/main/java/uniffi (both
# generated, not committed).
#
#   powershell -ExecutionPolicy Bypass -File apps\android\build-core.ps1 [-EmulatorOnly]
#
# Needs Rust with the aarch64-linux-android and x86_64-linux-android
# targets, cargo-ndk (cargo install cargo-ndk), and the Android NDK (r28 or
# later: 16 KB pages). ANDROID_NDK_HOME may name the NDK; otherwise the
# newest one in the Android SDK is used. Android 8.0 (API 26) and later.
param([switch]$EmulatorOnly)
$ErrorActionPreference = "Stop"
$here = $PSScriptRoot
$root = Resolve-Path (Join-Path $here "..\..")
if (-not $env:ANDROID_NDK_HOME) {
    $sdk = if ($env:ANDROID_HOME) { $env:ANDROID_HOME } else { Join-Path $env:LOCALAPPDATA "Android\Sdk" }
    $ndk = Get-ChildItem (Join-Path $sdk "ndk") -Directory | Sort-Object Name | Select-Object -Last 1
    if (-not $ndk) { throw "No Android NDK found: install one with the SDK manager (ndk;30...)" }
    $env:ANDROID_NDK_HOME = $ndk.FullName
}
$jniLibs = Join-Path $here "app\src\main\jniLibs"
$targets = if ($EmulatorOnly) { @("-t", "x86_64") } else { @("-t", "arm64-v8a", "-t", "x86_64") }
Push-Location $root
try {
    cargo ndk @targets --platform 26 -o $jniLibs build --release -p audionet-ffi --lib
    if ($LASTEXITCODE -ne 0) { throw "building the engine for Android failed" }
    # Bindings from one of the libraries just built (identical interface).
    $lib = Join-Path $root "target\x86_64-linux-android\release\libaudionet_ffi.so"
    $out = Join-Path $here "app\src\main\java"
    cargo run --release -q -p audionet-ffi --features bindgen --bin uniffi-bindgen -- `
        generate --library $lib --language kotlin --no-format --out-dir $out
    if ($LASTEXITCODE -ne 0) { throw "generating the Kotlin bindings failed" }
} finally {
    Pop-Location
}
Write-Host "Engine in $jniLibs; Kotlin bindings in app\src\main\java\uniffi"
