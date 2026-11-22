# Fetches the ffmpeg build that the Windows package bundles: BtbN's FFmpeg-Builds
# "win64-gpl-shared" from the 9.0 release branch (GPLv3; includes libx264/libx265,
# libsvtav1/libaom, dav1d, NVENC, QSV, AMF), from github.com/FFmpeg-Builds/FFmpeg-Builds,
# into third_party/ffmpeg/ (ffmpeg.exe, ffprobe.exe, the FFmpeg and library DLLs they share,
# LICENSE, README.txt with the build's origin).
#
# "Shared" keeps the libraries in DLLs that both exes load, instead of a copy in each exe.
# Compared with gyan.dev's builds: "essentials" has no SVT-AV1, and "full, shared" adds many
# filter libraries Keepcut doesn't use (installer 86 MB instead of 65 MB).
#
# Usage (from the repo root):
#   powershell -ExecutionPolicy Bypass -File packaging/windows/fetch-ffmpeg.ps1
#
# BtbN keeps daily builds for a few weeks but month-end builds long-term, so a month-end
# release is pinned below with the SHA-256 from its checksums.sha256. To update: pick a newer
# month-end "autobuild-YYYY-MM-..." release, take the win64-gpl-shared-9.x asset name and
# its line in checksums.sha256.

param(
    [string]$Release = "autobuild-2026-10-31-12-59",
    [string]$Asset = "ffmpeg-n9.0.2-39-g7adfbe40dc-win64-gpl-shared-9.0.zip",
    [string]$Sha256 = "1d9823c35c7a7e2f54d1438cf61030d6efabe3278de01c191309d1e828bb74df"
)

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"   # Invoke-WebRequest is very slow with the progress bar
$root = Resolve-Path (Join-Path $PSScriptRoot "..\..")
$dest = Join-Path $root "third_party\ffmpeg"
New-Item -ItemType Directory -Force $dest | Out-Null
$headers = @{ "User-Agent" = "keepcut-fetch-ffmpeg" }

$url = "https://github.com/FFmpeg-Builds/FFmpeg-Builds/releases/download/$Release/$Asset"
$archive = Join-Path $dest $Asset
if (-not (Test-Path $archive)) {
    Write-Host "downloading $url"
    Invoke-WebRequest -Uri $url -OutFile "$archive.part" -Headers $headers
    Move-Item -Force "$archive.part" $archive
}

$hash = (Get-FileHash -Algorithm SHA256 $archive).Hash.ToLowerInvariant()
if ($hash -ne $Sha256.ToLowerInvariant()) {
    Remove-Item $archive
    throw "SHA-256 mismatch for ${Asset}: got $hash, expected $Sha256"
}

# Replace what an earlier fetch left (another build's exes and DLLs), keep the archives.
Get-ChildItem $dest -File | Where-Object { $_.Extension -in ".exe", ".dll" -or $_.Name -in "LICENSE", "README.txt" } |
    Remove-Item -Force

$unpacked = Join-Path $dest "unpacked"
if (Test-Path $unpacked) { Remove-Item -Recurse -Force $unpacked }
Expand-Archive -Path $archive -DestinationPath $unpacked
$top = (Get-ChildItem -Directory $unpacked | Select-Object -First 1).FullName
foreach ($f in @("bin\ffmpeg.exe", "bin\ffprobe.exe", "LICENSE.txt")) {
    if (-not (Test-Path (Join-Path $top $f))) { throw "$f not found in $Asset" }
}
Copy-Item (Join-Path $top "bin\ffmpeg.exe"), (Join-Path $top "bin\ffprobe.exe") $dest
Copy-Item (Join-Path $top "LICENSE.txt") (Join-Path $dest "LICENSE")
$dlls = Get-ChildItem (Join-Path $top "bin") -Filter "*.dll"
if (-not $dlls) { throw "no DLLs in $Asset (not a shared build?)" }
$dlls | Copy-Item -Destination $dest
Remove-Item -Recurse -Force $unpacked

$ver = (& (Join-Path $dest "ffmpeg.exe") -hide_banner -version | Select-Object -First 1)
@"
FFmpeg for Windows, built by BtbN's FFmpeg-Builds (https://github.com/FFmpeg-Builds/FFmpeg-Builds).

Build:    $Asset
Release:  https://github.com/FFmpeg-Builds/FFmpeg-Builds/releases/tag/$Release
Version:  $ver
SHA-256:  $Sha256

The build scripts (which list every library and its source) are in the FFmpeg-Builds
repository; the FFmpeg source is the commit named in the version above, from
https://git.ffmpeg.org/ffmpeg.git (release/9.0 branch).
"@ | Set-Content -Encoding utf8 (Join-Path $dest "README.txt")

Write-Host "ready in ${dest}: $ver ($($dlls.Count) DLLs)"
