# Fetches the ffmpeg build that the Windows package bundles: gyan.dev "essentials"
# (GPLv3; includes libx264/libx265, NVENC, QSV, AMF), from its GitHub mirror
# github.com/GyanD/codexffmpeg (asset "ffmpeg-<ver>-essentials_build.zip"),
# into third_party/ffmpeg/ (ffmpeg.exe, ffprobe.exe, LICENSE, README.txt).
#
# Usage (from the repo root):
#   powershell -ExecutionPolicy Bypass -File packaging/windows/fetch-ffmpeg.ps1 [-Version 9.1]
#
# The SHA-256 of each known version is pinned below; an unknown version prints its hash
# so that it can be added after checking it against https://www.gyan.dev/ffmpeg/builds/.

param(
    [string]$Version = "9.1"
)

$known = @{
    "9.1" = "b9bbbb3acece000cbeec994f856649354e52c773807841e78908374b24d658fd"
}

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"   # Invoke-WebRequest is very slow with the progress bar
$root = Resolve-Path (Join-Path $PSScriptRoot "..\..")
$dest = Join-Path $root "third_party\ffmpeg"
New-Item -ItemType Directory -Force $dest | Out-Null
$headers = @{ "User-Agent" = "keepcut-fetch-ffmpeg" }

$name = "ffmpeg-$Version-essentials_build.zip"
$url = "https://github.com/GyanD/codexffmpeg/releases/download/$Version/$name"
$archive = Join-Path $dest $name
if (-not (Test-Path $archive)) {
    Write-Host "downloading $url"
    Invoke-WebRequest -Uri $url -OutFile "$archive.part" -Headers $headers
    Move-Item -Force "$archive.part" $archive
}

$hash = (Get-FileHash -Algorithm SHA256 $archive).Hash.ToLowerInvariant()
$expected = $known[$Version]
if ($expected -and $expected -ne "PIN_ME") {
    if ($hash -ne $expected) {
        Remove-Item $archive
        throw "SHA-256 mismatch for ${name}: got $hash, expected $expected"
    }
} else {
    Write-Warning "no pinned SHA-256 for $Version; got $hash (pin it in fetch-ffmpeg.ps1)"
}

$unpacked = Join-Path $dest "unpacked"
if (Test-Path $unpacked) { Remove-Item -Recurse -Force $unpacked }
Expand-Archive -Path $archive -DestinationPath $unpacked
$top = Get-ChildItem -Directory $unpacked | Select-Object -First 1
foreach ($f in @("bin\ffmpeg.exe", "bin\ffprobe.exe", "LICENSE", "README.txt")) {
    $src = Join-Path $top.FullName $f
    if (-not (Test-Path $src)) { throw "$f not found in $name" }
    Copy-Item -Force $src $dest
}
Remove-Item -Recurse -Force $unpacked

$ver = (& (Join-Path $dest "ffmpeg.exe") -hide_banner -version | Select-Object -First 1)
Write-Host "ready in ${dest}: $ver"
