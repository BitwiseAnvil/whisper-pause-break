param(
    [string]$Model = (Join-Path (Split-Path -Parent $PSScriptRoot) '.runtime\models\ggml-base.en.bin')
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$root = Split-Path -Parent $PSScriptRoot
$exe = Join-Path $root 'dist\whisper-pause-break.exe'
$data = Join-Path $root '.runtime\smoke'
if (!(Test-Path -LiteralPath $exe)) { throw 'Build the release package first.' }
if (!(Test-Path -LiteralPath $Model)) { throw 'Download a model first, or pass -Model.' }
if (Get-Process -Name 'whisper-pause-break','whisper-pause-break-cuda' -ErrorAction SilentlyContinue) {
    throw 'Stop existing Whisper Pause/Break instances before running this smoke check.'
}
New-Item -ItemType Directory -Force -Path $data | Out-Null
$modelPath = (Resolve-Path -LiteralPath $Model).Path.Replace('\', '/')
Set-Content -LiteralPath (Join-Path $data 'config.toml') -Value "model = '$modelPath'" -Encoding utf8
Set-Content -LiteralPath (Join-Path $data 'app.log') -Value '' -Encoding utf8
$app = Start-Process -FilePath $exe -ArgumentList @('--data-dir', "`"$data`"", 'run') -WindowStyle Hidden -PassThru
try {
    $deadline = [DateTime]::UtcNow.AddSeconds(30)
    do {
        Start-Sleep -Milliseconds 250
        $app.Refresh()
        if ($app.HasExited) { throw "Application exited early: $($app.ExitCode)" }
        $logText = Get-Content -LiteralPath (Join-Path $data 'app.log') -Raw
        if ($logText -match 'Whisper ready: (CUDA|CPU)') { break }
        if ([DateTime]::UtcNow -gt $deadline) { throw "Startup timed out. $logText" }
    } while ($true)
    Start-Sleep -Milliseconds 1000
    $logText = Get-Content -LiteralPath (Join-Path $data 'app.log') -Raw
    if ($logText -match 'Microphone:|Microphone stream failure:|Audio cues unavailable:|startup failed:') { throw "Audio/model startup failed. $logText" }
    if (([regex]::Matches($logText, 'Default microphone ready')).Count -gt 1) { throw "Microphone is repeatedly reconnecting. $logText" }
    Write-Host 'PASS: background startup, Whisper warm-up, microphone and speaker streams.'
} finally {
    $app.Refresh()
    if (!$app.HasExited) {
        & $exe stop | Out-Host
        if (!$app.WaitForExit(5000)) { Stop-Process -Id $app.Id; throw 'Application did not stop cleanly.' }
    }
}
$logText = Get-Content -LiteralPath (Join-Path $data 'app.log') -Raw
if ($logText -notmatch 'Application stopped') { throw 'Graceful shutdown was not logged.' }
# Closing a job schedules child termination; allow the GPU driver time to finish teardown.
$cleanupDeadline = [DateTime]::UtcNow.AddSeconds(5)
while (Get-Process -Name 'whisper-pause-break','whisper-pause-break-cuda' -ErrorAction SilentlyContinue) {
    if ([DateTime]::UtcNow -gt $cleanupDeadline) { throw 'A worker remained after shutdown.' }
    Start-Sleep -Milliseconds 100
}
Write-Host 'PASS: tray/message-loop shutdown and worker cleanup. No audio was recorded or text inserted.'
