# Builds the AudioNet plugin for NVGT on Windows: nvgt\release\audionet.dll
# (the engine linked in; nothing else to ship), and copies it into NVGT's
# lib folder if -Install is given.
#
#   powershell -ExecutionPolicy Bypass -File nvgt\build-plugin.ps1 [-Install] [-NvgtDir C:\nvgt]
#
# Needs Rust, Git and Visual Studio's C++ build tools. NVGT's plugin header
# and AngelScript's header come from the commits NVGT is built from (pinned
# below): a plugin must match NVGT's plugin API (version 5) and AngelScript
# version exactly. NVGT builds with the static C runtime (/MT), so the
# plugin and the engine inside it do too.
param(
    [switch]$Install,
    [string]$NvgtDir = "C:\nvgt"
)
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$deps = Join-Path $root "target\nvgt-deps"
$out = Join-Path $PSScriptRoot "release"

# NVGT at a known commit (plugin API 5), and the AngelScript it uses.
$nvgtCommit = "d7e31aca50fd928bc990b1bf6843203956f2693f"
$angelscriptCommit = "3fc00a6abc07c7ad2dec4ed14ff6018a194a54d6"
$nvgtSrc = Join-Path $deps "nvgt-src"
$asInclude = Join-Path $deps "angelscript"
New-Item -ItemType Directory -Force -Path $deps, $asInclude, $out | Out-Null
if (-not (Test-Path (Join-Path $nvgtSrc ".git"))) {
    git clone --quiet --filter=blob:none https://github.com/samtupy/nvgt $nvgtSrc
}
git -C $nvgtSrc fetch --quiet origin $nvgtCommit 2>$null
git -C $nvgtSrc checkout --quiet $nvgtCommit
$asHeader = Join-Path $asInclude "angelscript.h"
if (-not (Test-Path $asHeader)) {
    Invoke-WebRequest -UseBasicParsing -OutFile $asHeader `
        "https://raw.githubusercontent.com/anjo76/angelscript/$angelscriptCommit/sdk/angelscript/include/angelscript.h"
}

# The engine as a static library with the static C runtime, in a target
# folder of its own (the normal builds stay as they are).
$env:RUSTFLAGS = "-C target-feature=+crt-static"
cargo build --release -p audionet-c --target-dir (Join-Path $root "target\nvgt")
if ($LASTEXITCODE -ne 0) { throw "building the AudioNet engine failed" }
Remove-Item Env:RUSTFLAGS
$engineLib = Join-Path $root "target\nvgt\release\audionet_c.lib"

# The plugin, with Visual Studio's 64-bit compiler.
$vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
$vs = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
if (-not $vs) { throw "Visual Studio's C++ build tools were not found" }
$vcvars = Join-Path $vs "VC\Auxiliary\Build\vcvars64.bat"
$obj = Join-Path $deps "obj"
New-Item -ItemType Directory -Force -Path $obj | Out-Null
$flags = "/nologo /LD /MT /O2 /EHsc /J /utf-8 /std:c++20 /permissive- /Zc:inline /DNDEBUG /DUNICODE"
$includes = "/I`"$asInclude`" /I`"$nvgtSrc\src`" /I`"$nvgtSrc\ASAddon\include`""
$sources = "`"$PSScriptRoot\plugin\audionet_plugin.cpp`" `"$nvgtSrc\ASAddon\plugin\scriptarray.cpp`""
$libs = "`"$engineLib`" bcrypt.lib advapi32.lib kernel32.lib ntdll.lib userenv.lib ws2_32.lib dbghelp.lib ole32.lib oleaut32.lib user32.lib propsys.lib winmm.lib avrt.lib"
$cmd = "`"$vcvars`" >nul && cl $flags $includes $sources /Fo`"$obj\\`" /Fe`"$out\audionet.dll`" /link /NOIMPLIB /NOEXP /NODEFAULTLIB:msvcrt.lib $libs"
cmd /c $cmd
if ($LASTEXITCODE -ne 0) { throw "building the plugin failed" }
Write-Host "Built $out\audionet.dll"

if ($Install) {
    Copy-Item -Force (Join-Path $out "audionet.dll") (Join-Path $NvgtDir "lib\audionet.dll")
    Write-Host "Installed into $NvgtDir\lib"
}
