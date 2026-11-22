# Builds the Windows release: a portable zip and an Inno Setup installer in dist/.
#
# Usage (from the repo root):
#   powershell -ExecutionPolicy Bypass -File packaging/windows/package.ps1 [-NoInstaller] [-SkipBuild]
#
# Needs: the MSVC Rust toolchain, Inno Setup 7 (ISCC.exe; found on PATH, via $env:ISCC, or in
# the usual install folders) unless -NoInstaller. libmpv and ffmpeg are fetched into
# third_party/ if missing (fetch-mpv.ps1, fetch-ffmpeg.ps1).

param(
    [switch]$NoInstaller,
    [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"
$root = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
Set-Location $root

# Version from [workspace.package] in Cargo.toml
$cargoToml = Get-Content (Join-Path $root "Cargo.toml") -Raw
if ($cargoToml -notmatch '(?ms)^\[workspace\.package\].*?^version\s*=\s*"([^"]+)"') { throw "no workspace version in Cargo.toml" }
$version = $Matches[1]
Write-Host "Keepcut $version"

# Third-party binaries
$mpvDir = Join-Path $root "third_party\mpv"
$ffDir = Join-Path $root "third_party\ffmpeg"
if (-not (Test-Path (Join-Path $mpvDir "mpv.lib"))) { & (Join-Path $PSScriptRoot "fetch-mpv.ps1") }
if (-not (Test-Path (Join-Path $ffDir "ffmpeg.exe"))) { & (Join-Path $PSScriptRoot "fetch-ffmpeg.ps1") }
$mpvZip = Get-ChildItem $mpvDir -Filter "libmpv-v*-x86_64-w64-mingw32-*.zip" | Select-Object -First 1
$mpvVersion = if ($mpvZip -and $mpvZip.Name -match '^libmpv-(v[\d.]+)-') { $Matches[1] } else { "unknown" }
$ffVersion = ((& (Join-Path $ffDir "ffmpeg.exe") -hide_banner -version | Select-Object -First 1) -replace '^ffmpeg version (\S+).*$', '$1')

# Build
if (-not $SkipBuild) {
    cargo build --release --locked -p keepcut -p keepcut-cli
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
}
$target = Join-Path $root "target\release"

# Stage
$dist = Join-Path $root "dist"
$name = "Keepcut-$version-windows-x64"
$stage = Join-Path $dist $name
if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
New-Item -ItemType Directory -Force (Join-Path $stage "licenses") | Out-Null
Copy-Item (Join-Path $target "keepcut.exe"), (Join-Path $target "keepcut-cli.exe"), (Join-Path $target "libmpv-2.dll") $stage
Copy-Item (Join-Path $ffDir "ffmpeg.exe"), (Join-Path $ffDir "ffprobe.exe") $stage
$lic = Join-Path $stage "licenses"
Copy-Item (Join-Path $root "LICENSE") (Join-Path $lic "Keepcut-GPL-3.0.txt")
Copy-Item (Join-Path $mpvDir "unpacked\LICENSE.LGPL") (Join-Path $lic "mpv-LGPL-2.1.txt")
Copy-Item (Join-Path $ffDir "LICENSE") (Join-Path $lic "ffmpeg-GPL-3.0.txt")
Copy-Item (Join-Path $ffDir "README.txt") (Join-Path $lic "ffmpeg-build-README.txt")

@"
Keepcut $version
================

Mark the parts of your clips to keep, then cut and join them losslessly.
https://github.com/borisburger/keepcut

Start keepcut.exe. Nothing needs installing: settings, logs and autosaves are kept in
%APPDATA%\Keepcut and %LOCALAPPDATA%\Keepcut.

Windows may warn that the app is from an unknown publisher (the build is not code-signed):
click "More info", then "Run anyway".

keepcut-cli.exe is the command-line exporter (keepcut-cli --help).

Third-party components (see licenses\):
- mpv $mpvVersion (libmpv-2.dll), LGPL-2.1-or-later. Source: https://github.com/mpv-player/mpv/tree/$mpvVersion
- FFmpeg $ffVersion (ffmpeg.exe, ffprobe.exe), GPL-3.0, built by gyan.dev.
  Source and build configuration: https://www.gyan.dev/ffmpeg/builds/ and licenses\ffmpeg-build-README.txt;
  FFmpeg source: https://ffmpeg.org/download.html

Keepcut is free software under the GNU General Public License v3 or later
(licenses\Keepcut-GPL-3.0.txt). Source: https://github.com/borisburger/keepcut
"@ | Set-Content -Encoding utf8 (Join-Path $stage "README.txt")

# Zip
$zip = Join-Path $dist "$name.zip"
if (Test-Path $zip) { Remove-Item -Force $zip }
Compress-Archive -Path $stage -DestinationPath $zip -CompressionLevel Optimal
Write-Host ("zip:       {0} ({1:N0} MB)" -f $zip, ((Get-Item $zip).Length / 1MB))

# Installer
if (-not $NoInstaller) {
    $iscc = $env:ISCC
    if (-not $iscc) { $iscc = (Get-Command ISCC.exe -ErrorAction SilentlyContinue).Source }
    if (-not $iscc) {
        $iscc = @(
            "$env:LOCALAPPDATA\Programs\Inno Setup 7\ISCC.exe",
            "${env:ProgramFiles}\Inno Setup 7\ISCC.exe",
            "${env:ProgramFiles(x86)}\Inno Setup 7\ISCC.exe"
        ) | Where-Object { Test-Path $_ } | Select-Object -First 1
    }
    if (-not $iscc) { throw "ISCC.exe (Inno Setup 7) not found; install it or pass -NoInstaller" }
    & $iscc /Q "/DAppVersion=$version" "/DSourceDir=$stage" "/DOutputDir=$dist" (Join-Path $PSScriptRoot "keepcut.iss")
    if ($LASTEXITCODE -ne 0) { throw "ISCC failed" }
    $setup = Join-Path $dist "$name-setup.exe"
    Write-Host ("installer: {0} ({1:N0} MB)" -f $setup, ((Get-Item $setup).Length / 1MB))
}
