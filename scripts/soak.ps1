# Long-stream test: runs the release build under heavy simulated chat and records memory each minute.
# Run on Windows: powershell -File scripts/soak.ps1 [-Minutes 480] [-Rate 170] [-Firehose <url>] [-NoBuild]
# With -Firehose it reads real chat from a public firehose instead of simulated chat.
# Writes .dev/bench/soak.csv; memory should level off and stay flat.
param([int]$Minutes = 30, [double]$Rate = 170, [string]$Firehose = "", [switch]$NoBuild)
$ErrorActionPreference = 'Stop'
Set-Location (Split-Path $PSScriptRoot)
if (-not $NoBuild) {
    # Simulated chat and the firehose are only in builds with the sim feature.
    cargo build --release -p rill-app --features sim
    if ($LASTEXITCODE -ne 0) { Write-Host "$([char]0x2717) Build failed. Fix the errors above and run the soak test again." -ForegroundColor Red; exit 1 }
}
$out = Join-Path (Resolve-Path ".").Path ".dev\bench"
New-Item -ItemType Directory -Force $out | Out-Null
# Runs a copy, so rebuilding during a long test doesn't touch the running program.
$exe = Join-Path $out "rill-soak.exe"
Copy-Item (Resolve-Path "target\release\rill.exe").Path $exe -Force
# The release build's settings and logs stay in .dev too.
$env:RILL_DATA_DIR = Join-Path $out "data"
$csv = Join-Path $out "soak.csv"
# CPU from the processor's cycle counter: Windows' CPU time misses work done in short bursts.
Add-Type @'
using System; using System.Runtime.InteropServices;
public static class SoakCycles {
  [DllImport("kernel32.dll")] static extern bool QueryProcessCycleTime(IntPtr process, out ulong cycles);
  [DllImport("kernel32.dll")] static extern bool QueryThreadCycleTime(IntPtr thread, out ulong cycles);
  [DllImport("kernel32.dll")] static extern IntPtr GetCurrentThread();
  public static ulong Of(IntPtr process) { ulong c; QueryProcessCycleTime(process, out c); return c; }
  public static double PerSecond() {
    ulong a, b; var w = System.Diagnostics.Stopwatch.StartNew();
    QueryThreadCycleTime(GetCurrentThread(), out a);
    while (w.ElapsedMilliseconds < 500) { }
    QueryThreadCycleTime(GetCurrentThread(), out b);
    return (b - a) / w.Elapsed.TotalSeconds;
  }
}
'@
$hz = [SoakCycles]::PerSecond()
"minute,working_set_mb,private_mb,cpu_pct" | Set-Content -Encoding utf8 $csv

$appArgs = if ($Firehose) { "--firehose $Firehose" } else { "--rate $Rate --bursts --flaky" }
$p = Start-Process -FilePath $exe -ArgumentList $appArgs -PassThru
Start-Sleep -Seconds 5
foreach ($m in 1..$Minutes) {
    $c0 = [SoakCycles]::Of($p.Handle)
    Start-Sleep -Seconds 60
    if ($p.HasExited) { Write-Host "$([char]0x2717) Rill exited after $m minutes (code $($p.ExitCode)). Check .dev/bench/data/logs." -ForegroundColor Red; exit 1 }
    $p.Refresh()
    $cpu = ([SoakCycles]::Of($p.Handle) - $c0) / $hz / 60 / [Environment]::ProcessorCount * 100
    "{0},{1:N0},{2:N0},{3:N2}" -f $m, ($p.WorkingSet64 / 1MB), ($p.PrivateMemorySize64 / 1MB), $cpu | Add-Content -Encoding utf8 $csv
}
Stop-Process $p -Force
$rows = Import-Csv $csv
$first = $rows | Select-Object -Skip ([math]::Min(5, $rows.Count - 1)) -First 1
$last = $rows[-1]
"Private memory: {0} MB at minute {1}, {2} MB at minute {3}." -f $first.private_mb, $first.minute, $last.private_mb, $last.minute
Write-Host "$([char]0x2713) Soak test finished; see $csv" -ForegroundColor Green
