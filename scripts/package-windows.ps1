# Builds the Windows release package: dist/audionet-windows-x64-<version>.zip
#
#   powershell -ExecutionPolicy Bypass -File scripts/package-windows.ps1 `
#       [-DefaultServer https://audionet.example.com] `
#       [-UpdateUrl https://audionet.example.com/downloads/latest.json]
#
# -DefaultServer pre-fills the server address in the desktop app (official
# builds set their hosted instance; leave empty for a neutral build).
#
# -UpdateUrl makes the app update itself from that signed manifest. The
# build embeds the public key from -PublicKeyFile, and the package script
# signs latest.json (written next to the zip) with -SigningKey, after
# checking that the two belong together. Without -UpdateUrl the app never
# updates itself. See docs/releasing.md.
#
# -OutDir and -TargetDir let test builds stay apart from real ones.
param(
    [string]$DefaultServer = "",
    [string]$UpdateUrl = "",
    [string]$PublicKeyFile = "deploy/official/update-public-key.txt",
    [string]$SigningKey = (Join-Path $env:USERPROFILE ".audionet-release\update-signing-key.pem"),
    [string]$OutDir = "dist",
    [string]$TargetDir = "target"
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

if ($DefaultServer -ne "") { $env:AUDIONET_DEFAULT_SERVER = $DefaultServer } else { Remove-Item Env:AUDIONET_DEFAULT_SERVER -ErrorAction SilentlyContinue }
if ($UpdateUrl -ne "") {
    $publicKey = (Get-Content $PublicKeyFile -Raw).Trim()
    $signerKey = (python scripts/release_sign.py public $SigningKey).Trim()
    if ($LASTEXITCODE -ne 0) { throw "could not read the signing key $SigningKey" }
    if ($signerKey -ne $publicKey) { throw "the signing key does not match $PublicKeyFile; refusing to build an update this app would reject" }
    $env:AUDIONET_UPDATE_URL = $UpdateUrl
    $env:AUDIONET_UPDATE_PUBLIC_KEY = $publicKey
} else {
    Remove-Item Env:AUDIONET_UPDATE_URL -ErrorAction SilentlyContinue
    Remove-Item Env:AUDIONET_UPDATE_PUBLIC_KEY -ErrorAction SilentlyContinue
}
$env:CARGO_TARGET_DIR = $TargetDir
cargo build --release -p audionet-cli -p audionet-desktop
if ($LASTEXITCODE -ne 0) { throw "build failed" }

$version = (Select-String -Path Cargo.toml -Pattern '^version = "(.*)"' | Select-Object -First 1).Matches[0].Groups[1].Value
$name = "audionet-windows-x64-$version"
New-Item -ItemType Directory -Force $OutDir | Out-Null
$stage = Join-Path $OutDir $name
Remove-Item -Recurse -Force $stage -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $stage | Out-Null
Copy-Item (Join-Path $TargetDir "release/audionet.exe"), (Join-Path $TargetDir "release/audionet-desktop.exe"), LICENSE $stage
$updates = if ($UpdateUrl -ne "") {
    "AudioNet updates itself: it checks for new versions, installs them when no`r`none is listening through this computer, and restarts. You can turn this`r`noff with ""Keep AudioNet up to date automatically"" in the window."
} else {
    "This build does not update itself; download new versions yourself."
}
@"
AudioNet $version for Windows (64-bit)

audionet-desktop.exe  The AudioNet app. Sign this computer in to your
                      AudioNet server and keep it available to your
                      devices. Built for screen readers with standard
                      Windows controls (checked with automated UI
                      Automation tests; NVDA, JAWS and Narrator sessions
                      are still to be done).
audionet.exe          Command-line tools: list devices, direct LAN
                      streaming, diagnostics. Run "audionet --help".
                      It can also run this computer as a device without
                      the app: "audionet node sign-in", then
                      "audionet node run -b" (in the background, without
                      a window; it tells you how to stop it).

Start: run audionet-desktop.exe, enter your server address, account name
and password, and press "Sign in". The computer is then online in your
account while AudioNet runs; press "Start sharing" to let your other
devices listen to it and to send its audio (each account remembers its
choice). Tick "Start AudioNet automatically" in Settings to start it at
sign-in.

Closing the window keeps AudioNet running in the system tray (Windows+B,
then the arrow keys). Press Enter on its icon to open the window, or the
Applications key for a menu with Exit. To quit, use Exit in the window or
in that menu. Both behaviors are checkboxes in Settings.

$updates

The programs are not yet code-signed, so Windows SmartScreen may warn when
you first run them.

Documentation: docs folder of the AudioNet source, or your server's help.
Licensed under the MIT license (see LICENSE).
"@ | Set-Content -Encoding utf8 (Join-Path $stage "README.txt")

$zip = Join-Path $OutDir "$name.zip"
Remove-Item -Force $zip -ErrorAction SilentlyContinue
Compress-Archive -Path "$stage/*" -DestinationPath $zip
$hash = (Get-FileHash -Algorithm SHA256 $zip).Hash.ToLower()
# One line ending in LF, so `sha256sum -c` / `shasum -c` read it on any system.
[System.IO.File]::WriteAllText("$zip.sha256", "$hash  $name.zip`n", [System.Text.Encoding]::ASCII)
Write-Output "Package: $zip"
Write-Output "SHA-256: $hash"
if ($UpdateUrl -ne "") {
    python scripts/release_sign.py manifest $SigningKey $zip $version $OutDir
    if ($LASTEXITCODE -ne 0) { throw "signing failed" }
    Write-Output "Update manifest: $(Join-Path $OutDir 'latest.json') (+ .sig)"
}
