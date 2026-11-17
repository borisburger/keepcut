# Fetches libmpv for Windows development from the official mpv releases
# (github.com/mpv-player/mpv, asset "libmpv-v<ver>-x86_64-w64-mingw32-<variant>.zip")
# into third_party/mpv/ and creates an MSVC import library (mpv.lib) from libmpv-2.dll.
#
# Usage (from the repo root):
#   powershell -ExecutionPolicy Bypass -File packaging/windows/fetch-mpv.ps1 [-Tag v0.42.0] [-Variant lgpl|gpl]
#
# Variant: "lgpl" (default) is enough for playback: decoders, hwdec, render API and EDL are
# LGPL. "gpl" adds GPL-only FFmpeg parts (x264/x265 encoders, some filters) that the player
# does not use; exports run through the separate ffmpeg.exe.
#
# Needs the MSVC build tools (dumpbin, lib); found via vswhere.

param(
    [string]$Tag = "v0.42.0",
    [ValidateSet("lgpl", "gpl")] [string]$Variant = "lgpl"
)

$ErrorActionPreference = "Stop"
$root = Resolve-Path (Join-Path $PSScriptRoot "..\..")
$dest = Join-Path $root "third_party\mpv"
New-Item -ItemType Directory -Force $dest | Out-Null
$headers = @{ "User-Agent" = "keepcut-fetch-mpv" }

# 1. Find the release asset
$api = if ($Tag -eq "latest") {
    "https://api.github.com/repos/mpv-player/mpv/releases/latest"
} else {
    "https://api.github.com/repos/mpv-player/mpv/releases/tags/$Tag"
}
$release = Invoke-RestMethod -Uri $api -Headers $headers
$asset = $release.assets | Where-Object { $_.name -match "^libmpv-v[\d.]+-x86_64-w64-mingw32-$Variant\.zip$" } | Select-Object -First 1
if (-not $asset) { throw "no libmpv x86_64 $Variant asset in release $($release.tag_name)" }
Write-Host "release $($release.tag_name): $($asset.name) ($([math]::Round($asset.size / 1MB, 1)) MB)"

# 2. Download (skipped if already present with the right size)
$archive = Join-Path $dest $asset.name
if (-not (Test-Path $archive) -or (Get-Item $archive).Length -ne $asset.size) {
    Invoke-WebRequest -Uri $asset.browser_download_url -OutFile $archive -Headers $headers
}
# The .asc signature is downloaded next to it for manual verification with the mpv release key.
$sig = $release.assets | Where-Object { $_.name -eq "$($asset.name).asc" } | Select-Object -First 1
if ($sig) { Invoke-WebRequest -Uri $sig.browser_download_url -OutFile "$archive.asc" -Headers $headers }

# 3. Extract, then flatten: libmpv-2.dll and include/ directly in third_party/mpv
$unpacked = Join-Path $dest "unpacked"
if (Test-Path $unpacked) { Remove-Item -Recurse -Force $unpacked }
Expand-Archive -Path $archive -DestinationPath $unpacked
$dllItem = Get-ChildItem -Recurse $unpacked -Filter "libmpv-2.dll" | Select-Object -First 1
if (-not $dllItem) { throw "libmpv-2.dll not found in the archive" }
Copy-Item $dllItem.FullName $dest -Force
$inc = Get-ChildItem -Recurse -Directory $unpacked -Filter "include" | Select-Object -First 1
if ($inc) { Copy-Item -Recurse -Force $inc.FullName $dest }
$dll = Join-Path $dest "libmpv-2.dll"

# 4. MSVC import library: dumpbin /exports -> mpv.def -> lib /def
$vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
$vs = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
$msvc = Get-ChildItem (Join-Path $vs "VC\Tools\MSVC") | Sort-Object Name -Descending | Select-Object -First 1
$bin = Join-Path $msvc.FullName "bin\Hostx64\x64"
$exports = & (Join-Path $bin "dumpbin.exe") /exports $dll
$names = $exports | ForEach-Object {
    if ($_ -match '^\s+\d+\s+[0-9A-F]+\s+[0-9A-F]+\s+(mpv_\w+)') { $Matches[1] }
}
if ($names.Count -lt 20) { throw "unexpected dumpbin output ($($names.Count) mpv_ exports)" }
$def = Join-Path $dest "mpv.def"
@("LIBRARY libmpv-2.dll", "EXPORTS") + $names | Set-Content -Encoding ascii $def
$implib = Join-Path $dest "mpv.lib"
& (Join-Path $bin "lib.exe") /nologo "/def:$def" /name:libmpv-2.dll "/out:$implib" /machine:x64
if ($LASTEXITCODE -ne 0) { throw "lib.exe failed" }

Write-Host "libmpv $($release.tag_name) ready in $dest ($($names.Count) exports); mpv.lib created"
