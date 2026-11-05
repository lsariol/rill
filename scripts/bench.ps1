# Measures Rill's release build: warm startup, then CPU, memory and GPU under simulated chat.
# Run from anywhere on Windows: powershell -File scripts/bench.ps1 [-Quick] [-NoBuild]
# Keep the Rill window visible while it runs; minimized windows don't render.
param([switch]$Quick, [switch]$NoBuild)
$ErrorActionPreference = 'Stop'
Set-Location (Split-Path $PSScriptRoot)
$fail = [char]0x2717; $ok = [char]0x2713

if (-not $NoBuild) {
    Write-Host "Building the release binary..."
    cargo build --release -p rill-app
    if ($LASTEXITCODE -ne 0) { Write-Host "$fail Build failed. Fix the errors above and run the benchmark again." -ForegroundColor Red; exit 1 }
}
$exe = (Resolve-Path "target\release\rill.exe").Path
$out = Join-Path (Resolve-Path ".").Path ".dev\bench"
New-Item -ItemType Directory -Force $out | Out-Null

function Cpu-Percent($proc, $seconds) {
    $proc.Refresh(); $t0 = $proc.TotalProcessorTime; $w = [Diagnostics.Stopwatch]::StartNew()
    Start-Sleep -Seconds $seconds; $proc.Refresh()
    ($proc.TotalProcessorTime - $t0).TotalMilliseconds / $w.Elapsed.TotalMilliseconds / [Environment]::ProcessorCount * 100
}

function Gpu-Percent($procId) {
    try {
        $samples = (Get-Counter "\GPU Engine(pid_$($procId)_*)\Utilization Percentage" -ErrorAction Stop).CounterSamples
        ($samples | Measure-Object -Property CookedValue -Sum).Sum
    } catch { [double]::NaN }
}

# Warm startup: time until the window exists, best of five after one warm-up run.
$times = @()
foreach ($i in 0..5) {
    $sw = [Diagnostics.Stopwatch]::StartNew()
    $p = Start-Process -FilePath $exe -ArgumentList "--rate 0" -PassThru
    while ($p.MainWindowHandle -eq 0 -and $sw.Elapsed.TotalSeconds -lt 20) { Start-Sleep -Milliseconds 10; $p.Refresh() }
    if ($i -gt 0) { $times += $sw.Elapsed.TotalMilliseconds }
    Start-Sleep -Milliseconds 500; Stop-Process $p -Force; Start-Sleep -Milliseconds 300
}
$startup = ($times | Measure-Object -Minimum -Average)

$secs = if ($Quick) { 20 } else { 60 }
$profiles = [ordered]@{
    'idle (connected, no chat)' = '--rate 0'
    'steady ~50 msg/s'          = '--rate 17'
    'bursts ~500 msg/s peaks'   = '--rate 17 --bursts'
    'heavy ~500 msg/s'          = '--rate 170'
    'emotes ~50 msg/s'          = '--rate 17 --emotes'
}
$rows = @()
foreach ($name in $profiles.Keys) {
    $stats = Join-Path $out "stats.txt"
    if (Test-Path $stats) { Remove-Item $stats }
    $appArgs = "$($profiles[$name]) --exit-after $($secs + 45) --stats `"$stats`""
    $p = Start-Process -FilePath $exe -ArgumentList $appArgs -PassThru
    Start-Sleep -Seconds 5
    $cpu = @(); $gpu = @(); $ws = @(); $priv = @()
    $slices = [math]::Floor($secs / 5)
    foreach ($s in 1..$slices) {
        $cpu += Cpu-Percent $p 5
        $gpu += Gpu-Percent $p.Id
        $p.Refresh(); $ws += $p.WorkingSet64 / 1MB; $priv += $p.PrivateMemorySize64 / 1MB
    }
    $p.WaitForExit(90000) | Out-Null
    $kv = @{}
    if (Test-Path $stats) { Get-Content $stats | ForEach-Object { $k, $v = $_ -split '=', 2; $kv[$k] = $v } }
    $rows += [pscustomobject]@{
        Profile       = $name
        'CPU avg %'   = '{0:N2}' -f ($cpu | Measure-Object -Average).Average
        'CPU max %'   = '{0:N2}' -f ($cpu | Measure-Object -Maximum).Maximum
        'GPU avg %'   = '{0:N1}' -f ($gpu | Measure-Object -Average).Average
        'WS MB'       = '{0:N0}->{1:N0}' -f $ws[0], $ws[-1]
        'Private MB'  = '{0:N0}->{1:N0}' -f $priv[0], $priv[-1]
        'Events/s'    = if ($kv['events']) { '{0:N0}' -f ([double]$kv['events'] / [double]$kv['seconds']) } else { '?' }
        'Renders/s'   = if ($kv['renders']) { '{0:N1}' -f ([double]$kv['renders'] / [double]$kv['seconds']) } else { '?' }
        'Images'      = $kv['images']
    }
}

"Warm startup (window shown): best {0:N0} ms, average {1:N0} ms over {2} runs" -f $startup.Minimum, $startup.Average, $startup.Count
"CPU % is of all {0} logical cores. {1} s per profile." -f [Environment]::ProcessorCount, $secs
$rows | Format-Table -AutoSize | Out-String -Width 200
Write-Host "$ok Benchmark finished." -ForegroundColor Green
