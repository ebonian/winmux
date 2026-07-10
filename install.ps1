<#
.SYNOPSIS
    Installs (or updates) winmux, a tmux-style terminal multiplexer for Windows.

.DESCRIPTION
    Downloads the latest winmux release from GitHub, verifies its checksum,
    installs it to a per-user directory (no admin rights needed), and adds
    that directory to the user PATH.

    One-liner:
        irm https://raw.githubusercontent.com/ebonian/winmux/main/install.ps1 | iex

    Re-running the installer updates to the latest release; if the installed
    version is already current, it exits without downloading anything.

.PARAMETER Version
    Install a specific release tag (e.g. "v0.1.0") instead of the latest.

.PARAMETER InstallDir
    Override the install directory. Default: %LOCALAPPDATA%\Programs\winmux

.PARAMETER Force
    Reinstall even if the requested version is already installed.
#>
[CmdletBinding()]
param(
    [string]$Version,
    [string]$InstallDir = (Join-Path $env:LOCALAPPDATA 'Programs\winmux'),
    [switch]$Force
)

$ErrorActionPreference = 'Stop'
# Windows PowerShell 5.1's progress bar slows Invoke-WebRequest downloads ~10x.
$ProgressPreference = 'SilentlyContinue'
$repo = 'ebonian/winmux'

# Windows PowerShell 5.1 defaults to TLS 1.0; GitHub requires TLS 1.2+.
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

function Write-Step($msg) { Write-Host "==> $msg" -ForegroundColor Cyan }

# --- Resolve the target release -------------------------------------------

if ($Version) {
    if ($Version -notmatch '^v') { $Version = "v$Version" }
    $apiUrl = "https://api.github.com/repos/$repo/releases/tags/$Version"
} else {
    $apiUrl = "https://api.github.com/repos/$repo/releases/latest"
}

Write-Step "Resolving winmux release..."
try {
    $release = Invoke-RestMethod -Uri $apiUrl
} catch {
    Write-Error "Could not query GitHub for a winmux release ($apiUrl): $_"
    exit 1
}
$tag = $release.tag_name
Write-Host "    Target version: $tag"

# --- Idempotence: skip if already current ---------------------------------

$versionFile = Join-Path $InstallDir 'version.txt'
if (-not $Force -and (Test-Path $versionFile)) {
    $installed = (Get-Content $versionFile -TotalCount 1).Trim()
    if ($installed -eq $tag) {
        Write-Host "winmux $tag is already installed and up to date." -ForegroundColor Green
        return
    }
    Write-Host "    Installed version: $installed -> updating"
}

# --- Refuse to overwrite a running winmux ---------------------------------

$exePath = Join-Path $InstallDir 'winmux.exe'
if (Test-Path $exePath) {
    try {
        $handle = [IO.File]::Open($exePath, 'Open', 'ReadWrite', 'None')
        $handle.Close()
    } catch [System.IO.IOException] {
        Write-Error @"
winmux appears to be running ($exePath is locked).
Detach from any winmux sessions and stop the server first:

    winmux kill-server

then re-run this installer.
"@
        exit 1
    }
}

# --- Download and verify ---------------------------------------------------

$zipAsset = $release.assets | Where-Object { $_.name -like 'winmux-*-x86_64-pc-windows-msvc.zip' } | Select-Object -First 1
$sumsAsset = $release.assets | Where-Object { $_.name -eq 'SHA256SUMS' } | Select-Object -First 1
if (-not $zipAsset) {
    Write-Error "Release $tag has no winmux zip asset for x86_64 Windows."
    exit 1
}

$tmpDir = Join-Path $env:TEMP "winmux-install-$([Guid]::NewGuid().ToString('N').Substring(0, 8))"
New-Item -ItemType Directory $tmpDir | Out-Null
try {
    $zipPath = Join-Path $tmpDir $zipAsset.name
    Write-Step "Downloading $($zipAsset.name)..."
    Invoke-WebRequest -Uri $zipAsset.browser_download_url -OutFile $zipPath

    if ($sumsAsset) {
        Write-Step "Verifying checksum..."
        $sumsPath = Join-Path $tmpDir 'SHA256SUMS'
        Invoke-WebRequest -Uri $sumsAsset.browser_download_url -OutFile $sumsPath
        $expectedLine = Get-Content $sumsPath | Where-Object { $_ -match [regex]::Escape($zipAsset.name) } | Select-Object -First 1
        if (-not $expectedLine) {
            Write-Error "SHA256SUMS has no entry for $($zipAsset.name)."
            exit 1
        }
        $expected = ($expectedLine -split '\s+')[0].ToLower()
        $actual = (Get-FileHash $zipPath -Algorithm SHA256).Hash.ToLower()
        if ($expected -ne $actual) {
            Write-Error "Checksum mismatch for $($zipAsset.name): expected $expected, got $actual. Aborting."
            exit 1
        }
    } else {
        Write-Warning "Release $tag has no SHA256SUMS asset; skipping checksum verification."
    }

    # --- Install -----------------------------------------------------------

    Write-Step "Installing to $InstallDir..."
    New-Item -ItemType Directory -Force $InstallDir | Out-Null
    Expand-Archive -Path $zipPath -DestinationPath $InstallDir -Force
    Set-Content -Path $versionFile -Value $tag
} finally {
    Remove-Item -Recurse -Force $tmpDir -ErrorAction SilentlyContinue
}

# The updater stub: on PATH, so `winmux-update` works from any PowerShell.
# It always fetches the canonical installer, so update logic never goes stale.
$updateStub = @"
# winmux updater -- fetches and runs the latest winmux installer.
irm https://raw.githubusercontent.com/$repo/main/install.ps1 | iex
"@
Set-Content -Path (Join-Path $InstallDir 'winmux-update.ps1') -Value $updateStub

# --- User PATH -------------------------------------------------------------

# Read/write the user PATH via the registry (not [Environment]::SetEnvironmentVariable,
# which expands %VAR% entries and rewrites REG_EXPAND_SZ as REG_SZ).
$envKey = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment', $true)
try {
    $rawPath = $envKey.GetValue('Path', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
    $entries = $rawPath -split ';' | Where-Object { $_ }
    $expanded = $entries | ForEach-Object { [Environment]::ExpandEnvironmentVariables($_).TrimEnd('\') }
    if ($expanded -notcontains $InstallDir.TrimEnd('\')) {
        Write-Step "Adding $InstallDir to your user PATH..."
        $newPath = if ($rawPath) { $rawPath.TrimEnd(';') + ";$InstallDir" } else { $InstallDir }
        $kind = if ($rawPath) { $envKey.GetValueKind('Path') } else { [Microsoft.Win32.RegistryValueKind]::ExpandString }
        $envKey.SetValue('Path', $newPath, $kind)

        # Tell running apps (Explorer, so NEW terminals inherit it) that the
        # environment changed.
        if (-not ('WinmuxInstall.Native' -as [type])) {
            Add-Type -Namespace WinmuxInstall -Name Native -MemberDefinition @'
[DllImport("user32.dll", SetLastError = true, CharSet = CharSet.Auto)]
public static extern IntPtr SendMessageTimeout(IntPtr hWnd, uint Msg, UIntPtr wParam, string lParam, uint fuFlags, uint uTimeout, out UIntPtr lpdwResult);
'@
        }
        [UIntPtr]$result = [UIntPtr]::Zero
        [WinmuxInstall.Native]::SendMessageTimeout([IntPtr]0xffff, 0x1A, [UIntPtr]::Zero, 'Environment', 2, 5000, [ref]$result) | Out-Null
    }
} finally {
    $envKey.Close()
}

# Make `winmux` work in THIS session too.
if (($env:Path -split ';') -notcontains $InstallDir) {
    $env:Path = "$env:Path;$InstallDir"
}

Write-Host ""
Write-Host "winmux $tag installed successfully." -ForegroundColor Green
Write-Host ""
Write-Host "  Run it:      winmux"
Write-Host "  Update it:   winmux-update"
Write-Host "  Uninstall:   winmux kill-server; Remove-Item -Recurse '$InstallDir'"
Write-Host ""
Write-Host "If 'winmux' is not found in other open terminals, restart them to pick up the new PATH."
