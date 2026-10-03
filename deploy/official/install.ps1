# AudioNet installer for Windows (official instance).
#
#   irm https://audionet.mad-gamer.com/install.ps1 | iex
#
# Installs the latest AudioNet for Windows from the project's GitHub
# releases, or updates or uninstalls an existing copy, then starts it.
# Asks a few questions; pressing Enter takes the answer in brackets. No
# administrator rights: everything goes in the current user's folders and
# registry. Served by the official server's nginx from
# /var/www/audionet/install.ps1 (see audionet.mad-gamer.com.conf).
#
# The download is checked against the size and SHA-256 named in the
# release's manifest (latest.json), fetched over HTTPS from GitHub. The
# manifest's Ed25519 signature is not checked here (Windows PowerShell has
# no Ed25519); the installed app checks it for every later update.
#
# Output is plain lines for screen readers: no colours, progress bars or
# cursor movement. Plain ASCII, for Windows PowerShell 5.1.

& {
Set-StrictMode -Version 2
$ErrorActionPreference = 'Stop'
# Windows PowerShell's download progress bar is slow and visual only.
$ProgressPreference = 'SilentlyContinue'

$Release = 'https://github.com/mad-gamer26/AudioNet/releases/latest/download'
$Product = 'audionet-windows-x64'
$AppExe = 'audionet-desktop.exe'
$CliExe = 'audionet.exe'
$PackageFiles = @($AppExe, $CliExe, 'LICENSE', 'README.txt')
$RunKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
$StartMenu = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\AudioNet.lnk'
$Desktop = Join-Path ([Environment]::GetFolderPath('Desktop')) 'AudioNet.lnk'
$DefaultFolder = Join-Path $env:LOCALAPPDATA 'Programs\AudioNet'

function Say([string]$Text = '') { Write-Host $Text }

# A yes or no question; Enter takes the default.
function Ask-YesNo([string]$Question, [bool]$Default) {
    $hint = if ($Default) { 'Y/n' } else { 'y/N' }
    while ($true) {
        $answer = (Read-Host "$Question ($hint)").Trim().ToLowerInvariant()
        if ($answer -eq '') { return $Default }
        if ($answer -in @('y', 'yes')) { return $true }
        if ($answer -in @('n', 'no')) { return $false }
        Say 'Please answer y for yes or n for no.'
    }
}

# A numbered choice; Enter takes choice 1. Returns the number.
function Ask-Choice([string]$Question, [string[]]$Choices) {
    Say $Question
    for ($i = 0; $i -lt $Choices.Count; $i++) {
        Say "$($i + 1). $($Choices[$i])"
    }
    while ($true) {
        $answer = (Read-Host "Choice, 1 to $($Choices.Count) (1)").Trim()
        if ($answer -eq '') { return 1 }
        $n = 0
        if ([int]::TryParse($answer, [ref]$n) -and $n -ge 1 -and $n -le $Choices.Count) { return $n }
        Say "Please type a number from 1 to $($Choices.Count)."
    }
}

function Ask-Folder([string]$Default) {
    $answer = (Read-Host "Install folder ($Default)").Trim().Trim('"')
    if ($answer -eq '') { return $Default }
    return [Environment]::ExpandEnvironmentVariables($answer)
}

# The installed version, from the command-line tool ("audionet 1.2.0"), or
# $null when it cannot be read.
function Installed-Version([string]$Folder) {
    $cli = Join-Path $Folder $CliExe
    if (-not (Test-Path -LiteralPath $cli)) { return $null }
    try {
        $out = & $cli --version 2>$null | Select-Object -First 1
        if ($out -match '(\d+\.\d+\.\d+)') { return $Matches[1] }
    } catch { }
    return $null
}

# The folder of the program the Run value starts, or $null.
function Run-Folder {
    $value = (Get-ItemProperty -LiteralPath $RunKey -Name 'AudioNet' -ErrorAction SilentlyContinue)
    if ($null -eq $value) { return $null }
    if ($value.AudioNet -match '^"([^"]+)"') { return Split-Path -Parent $Matches[1] }
    return $null
}

function Shortcut-Folder([string]$Link) {
    if (-not (Test-Path -LiteralPath $Link)) { return $null }
    try {
        $target = (New-Object -ComObject WScript.Shell).CreateShortcut($Link).TargetPath
        if ($target) { return Split-Path -Parent $target }
    } catch { }
    return $null
}

# Where AudioNet is installed already: the running copy, the Start menu
# shortcut, the start-at-sign-in entry, or the default folder.
function Find-Installed {
    $candidates = @()
    foreach ($p in @(Get-Process -Name 'audionet-desktop' -ErrorAction SilentlyContinue)) {
        try { if ($p.Path) { $candidates += Split-Path -Parent $p.Path } } catch { }
    }
    $candidates += @((Shortcut-Folder $StartMenu), (Run-Folder), $DefaultFolder)
    foreach ($c in $candidates) {
        if ($c -and (Test-Path -LiteralPath (Join-Path $c $AppExe))) {
            return (Resolve-Path -LiteralPath $c).Path
        }
    }
    return $null
}

function Running-In([string]$Folder) {
    $exe = Join-Path $Folder $AppExe
    return @(Get-Process -Name 'audionet-desktop' -ErrorAction SilentlyContinue | Where-Object {
        try { $_.Path -and ($_.Path -ieq $exe) } catch { $false }
    })
}

# Closes the copy running from $Folder, after asking. False: it keeps
# running (the user said no).
function Close-Running([string]$Folder, [string]$Why) {
    $running = @(Running-In $Folder)
    if ($running.Count -eq 0) { return $true }
    Say "AudioNet is running. It must close $Why; any streams stop."
    if (-not (Ask-YesNo 'Close AudioNet now?' $true)) { return $false }
    $running | Stop-Process -Force
    for ($i = 0; $i -lt 20 -and @(Running-In $Folder).Count -gt 0; $i++) { Start-Sleep -Milliseconds 250 }
    Say 'AudioNet closed.'
    return $true
}

function Make-Shortcut([string]$Link, [string]$Target) {
    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $Link) | Out-Null
    $shell = New-Object -ComObject WScript.Shell
    $s = $shell.CreateShortcut($Link)
    $s.TargetPath = $Target
    $s.WorkingDirectory = Split-Path -Parent $Target
    $s.Description = 'AudioNet: stream audio between your devices'
    $s.Save()
}

function User-Path { [Environment]::GetEnvironmentVariable('Path', 'User') }

function Path-Has([string]$Folder) {
    $p = User-Path
    if (-not $p) { return $false }
    return @($p.Split(';') | Where-Object { $_.TrimEnd('\') -ieq $Folder.TrimEnd('\') }).Count -gt 0
}

function Add-ToPath([string]$Folder) {
    if (Path-Has $Folder) { return }
    $p = User-Path
    $new = if ($p) { $p.TrimEnd(';') + ';' + $Folder } else { $Folder }
    [Environment]::SetEnvironmentVariable('Path', $new, 'User')
}

function Remove-FromPath([string]$Folder) {
    if (-not (Path-Has $Folder)) { return }
    $kept = @((User-Path).Split(';') | Where-Object { $_ -and ($_.TrimEnd('\') -ine $Folder.TrimEnd('\')) })
    [Environment]::SetEnvironmentVariable('Path', ($kept -join ';'), 'User')
}

function Get-Manifest {
    $r = Invoke-WebRequest -UseBasicParsing -Uri "$Release/latest.json"
    $text = if ($r.Content -is [byte[]]) { [Text.Encoding]::UTF8.GetString($r.Content) } else { [string]$r.Content }
    $m = $text | ConvertFrom-Json
    if ($m.product -ne $Product -or -not ($m.version -match '^\d+\.\d+\.\d+$') -or -not ($m.file -match '^[A-Za-z0-9._-]+\.zip$')) {
        throw 'the release manifest on GitHub is not one for AudioNet for Windows'
    }
    return $m
}

function Install-Package($Manifest, [string]$Folder) {
    $sizeMb = [math]::Round($Manifest.size / 1MB, 1)
    $work = Join-Path ([IO.Path]::GetTempPath()) ('audionet-install-' + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $work | Out-Null
    try {
        $zip = Join-Path $work $Manifest.file
        Say "Downloading AudioNet $($Manifest.version) ($sizeMb MB) from GitHub."
        Invoke-WebRequest -UseBasicParsing -Uri "$Release/$($Manifest.file)" -OutFile $zip
        $size = (Get-Item -LiteralPath $zip).Length
        $hash = (Get-FileHash -LiteralPath $zip -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($size -ne [int64]$Manifest.size -or $hash -ne $Manifest.sha256.ToLowerInvariant()) {
            throw 'the download does not match the release manifest (size or SHA-256); nothing was installed'
        }
        Say 'Download checked: size and SHA-256 match the release.'
        $unpacked = Join-Path $work 'files'
        Expand-Archive -LiteralPath $zip -DestinationPath $unpacked
        if (-not (Test-Path -LiteralPath (Join-Path $unpacked $AppExe))) {
            throw "the download has no $AppExe; nothing was installed"
        }
        New-Item -ItemType Directory -Force -Path $Folder | Out-Null
        foreach ($f in Get-ChildItem -LiteralPath $unpacked -File) {
            Copy-Item -LiteralPath $f.FullName -Destination (Join-Path $Folder $f.Name) -Force
            Unblock-File -LiteralPath (Join-Path $Folder $f.Name)
        }
    } finally {
        Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
    }
}

function Uninstall([string]$Folder) {
    Say "This removes AudioNet from $Folder, with its shortcuts, start at sign-in and command-line PATH entry."
    if (-not (Ask-YesNo 'Uninstall AudioNet?' $false)) { Say 'Nothing was changed.'; return }
    if (-not (Close-Running $Folder 'to be uninstalled')) { Say 'Nothing was changed.'; return }
    $removeData = Ask-YesNo 'Also remove your AudioNet sign-in and settings from this computer?' $false
    foreach ($name in $PackageFiles) {
        Remove-Item -LiteralPath (Join-Path $Folder $name) -Force -ErrorAction SilentlyContinue
    }
    # What updates leave behind: renamed old programs and staging folders.
    Get-ChildItem -LiteralPath $Folder -Force -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -like '*.old-update' -or $_.Name -like '.audionet-update-*' } |
        Remove-Item -Recurse -Force -ErrorAction SilentlyContinue
    if (@(Get-ChildItem -LiteralPath $Folder -Force -ErrorAction SilentlyContinue).Count -eq 0) {
        Remove-Item -LiteralPath $Folder -Force -ErrorAction SilentlyContinue
    } else {
        Say "Other files in $Folder were left in place."
    }
    foreach ($link in @($StartMenu, $Desktop)) {
        $target = Shortcut-Folder $link
        if ($target -and ($target.TrimEnd('\') -ieq $Folder.TrimEnd('\'))) { Remove-Item -LiteralPath $link -Force }
    }
    $run = Run-Folder
    if ($run -and ($run.TrimEnd('\') -ieq $Folder.TrimEnd('\'))) {
        Remove-ItemProperty -LiteralPath $RunKey -Name 'AudioNet' -ErrorAction SilentlyContinue
    }
    Remove-FromPath $Folder
    if ($removeData) {
        Remove-Item -LiteralPath (Join-Path $env:APPDATA 'AudioNet') -Recurse -Force -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath 'HKCU:\Software\AudioNet' -Recurse -Force -ErrorAction SilentlyContinue
        Say 'Your AudioNet sign-in and settings were removed.'
    }
    Say 'AudioNet was uninstalled. To install it again, run the same command.'
}

function Start-AudioNet([string]$Folder) {
    Start-Process -FilePath (Join-Path $Folder $AppExe) -WorkingDirectory $Folder
}

try {
    Say 'AudioNet installer for Windows.'
    Say 'Press Enter to accept the answer in brackets.'
    Say ''
    if (-not [Environment]::Is64BitOperatingSystem -or [Environment]::OSVersion.Version.Major -lt 10) {
        Say 'AudioNet needs 64-bit Windows 10 or 11. Nothing was installed.'
        return
    }
    # Windows PowerShell 5.1 may not offer TLS 1.2 by itself; GitHub needs it.
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

    Say 'Checking the latest release on GitHub.'
    $manifest = Get-Manifest
    $latest = $manifest.version
    Say "Latest version: $latest"

    $installed = Find-Installed
    $fresh = $true
    if ($installed) {
        $version = Installed-Version $installed
        $versionText = if ($version) { "AudioNet $version" } else { 'AudioNet' }
        Say "Installed: $versionText in $installed"
        Say ''
        if ($version -and ([version]$version -ge [version]$latest)) {
            $choice = Ask-Choice 'It is up to date. What would you like to do?' @('Open AudioNet', "Reinstall AudioNet $latest", 'Uninstall AudioNet', 'Quit')
        } else {
            $choice = Ask-Choice 'What would you like to do?' @("Update to AudioNet $latest", 'Uninstall AudioNet', 'Quit')
            # Same numbering as above: 2 reinstall/update, 3 uninstall, 4 quit.
            $choice = @(0, 2, 3, 4)[$choice]
        }
        switch ($choice) {
            1 {
                if (@(Running-In $installed).Count -gt 0) { Say 'AudioNet is already running: find it in the system tray (Windows+B).' }
                else { Start-AudioNet $installed; Say 'AudioNet is starting.' }
                return
            }
            3 { Uninstall $installed; return }
            4 { Say 'Nothing was changed.'; return }
        }
        $folder = $installed
        $fresh = $false
    } else {
        Say 'AudioNet is not installed on this computer yet.'
        $folder = Ask-Folder $DefaultFolder
    }

    $wantStartMenu = $true
    $wantDesktop = $false
    $atSignIn = $false
    $addPath = $false
    if ($fresh) {
        $wantStartMenu = Ask-YesNo 'Add AudioNet to the Start menu?' $true
        $wantDesktop = Ask-YesNo 'Add an AudioNet shortcut to the desktop?' $false
        $atSignIn = Ask-YesNo 'Start AudioNet (in the system tray) when you sign in to Windows? You can change this later in its Settings.' $false
        $addPath = Ask-YesNo 'Add the audionet command-line tool to your PATH (for use in a terminal)?' $false
    }

    if (-not (Close-Running $folder 'to be updated')) { Say 'Nothing was changed.'; return }
    Install-Package $manifest $folder
    $exe = Join-Path $folder $AppExe

    if ($wantStartMenu) { Make-Shortcut $StartMenu $exe; if ($fresh) { Say 'Added AudioNet to the Start menu.' } }
    if ($wantDesktop) { Make-Shortcut $Desktop $exe; Say 'Added an AudioNet shortcut to the desktop.' }
    $runValue = "`"$exe`" --background"
    if ($atSignIn) {
        New-ItemProperty -LiteralPath $RunKey -Name 'AudioNet' -Value $runValue -PropertyType String -Force | Out-Null
        Say 'AudioNet will start when you sign in to Windows.'
    } elseif (-not $fresh) {
        # An existing start-at-sign-in entry that points at a copy that is
        # gone now points at this one.
        $run = Run-Folder
        if ($run -and -not (Test-Path -LiteralPath (Join-Path $run $AppExe))) {
            New-ItemProperty -LiteralPath $RunKey -Name 'AudioNet' -Value $runValue -PropertyType String -Force | Out-Null
            Say 'Start at sign-in now starts this copy (it pointed at a copy that no longer exists).'
        }
    }
    if ($addPath) {
        Add-ToPath $folder
        Say 'The audionet command works in new terminal windows.'
    }

    Say ''
    Say "AudioNet $latest is installed in $folder"
    Start-AudioNet $folder
    Say 'AudioNet is starting.'
    if ($fresh) {
        Say 'Sign in with your server address, account name and password. No account yet? Create one in your AudioNet server''s web client.'
    }
    Say 'AudioNet keeps itself up to date. To update, reinstall or uninstall it later, run this command again.'
} catch {
    Say ''
    Say "The AudioNet installer stopped: $($_.Exception.Message)"
    if ($_.Exception -is [Net.WebException]) { Say 'Check your internet connection and try again.' }
}
}
