# Runs the player spike's scripted performance gate (§7) on a set of files and
# prints one JSON line per run. Usage (repo root, after `cargo build --release -p keepcut-player`):
#   powershell -ExecutionPolicy Bypass -File crates/keepcut-player/bench.ps1 [-Mode gl|sw] [-Hwdec auto] [-StepMode auto|mpv|seek|fast] [-Files a.mp4,b.mp4]
param(
    [ValidateSet("gl", "sw")] [string]$Mode = "gl",
    [string]$Hwdec = "",
    [ValidateSet("auto", "mpv", "seek", "fast")] [string]$StepMode = "auto",
    [string[]]$Files = @(
        "samples\Katka50-ToeToe-Najprv2-Hudba.mp4",
        "samples\bandicam 2025-05-19 16-58-11-314.mp4",
        "samples\bandicam 2025-05-10 13-08-45-313.mp4",
        "samples\synth\4k-h264-30.mp4",
        "samples\synth\4k-hevc10-60.mp4",
        "samples\bandicam 2026-11-17 22-37-27-327-nvenc-av1-4k.mp4",
        "samples\bandicam 2026-11-17 22-38-44-032-intel-hevc-4k.mp4"
    )
)
$exe = Join-Path $PSScriptRoot "..\..\target\release\keepcut-player.exe"
foreach ($f in $Files) {
    $argList = @("--bench", "`"$f`"")
    if ($Mode -eq "sw") { $argList += "--sw" }
    if ($Hwdec) { $argList += @("--hwdec", $Hwdec) }
    $argList += @("--step-mode", $StepMode)
    $out = Join-Path $env:TEMP "kp-bench.out"
    $p = Start-Process -FilePath $exe -ArgumentList $argList -RedirectStandardOutput $out -RedirectStandardError (Join-Path $env:TEMP "kp-bench.err") -PassThru -Wait
    $line = Get-Content $out -Tail 1
    if (-not $line) { $line = "{`"file`":`"$f`",`"error`":`"exit $($p.ExitCode)`"}" }
    Write-Output $line
}
