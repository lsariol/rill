# Rill side by side with the platforms' own pop-out chats in Chrome, on the same live channels.
# Run on Windows from anywhere:
#   powershell -File scripts/compare/compare.ps1 -Twitch <login> -YouTubeVideo <id> -Kick <channel>
#       [-Seconds 600] [-Warmup 60] [-Left 0] [-Top 0] [-Width 480] [-Height 1040] [-NoBuild]
# Needs Chrome, Node.js 22 or later, Python 3, and Twitch, YouTube and Kick signed in to a debug
# run of Rill (it uses the development sign-ins and the app IDs in .dev\settings.json).
# Rill and the three pop-outs run at the same time, in four windows side by side starting at
# Left/Top, so both see the same messages. Keep the windows uncovered and the pointer off them:
# browsers stop drawing covered windows, and Twitch pauses its chat under the pointer.
# Writes everything it records to .dev\compare\<date-time>; scripts/compare/summarize.py turns it
# into tables.
param(
    [Parameter(Mandatory)][string]$Twitch,
    [Parameter(Mandatory)][string]$YouTubeVideo,
    [Parameter(Mandatory)][string]$Kick,
    [int]$Seconds = 600,
    [int]$Warmup = 60,
    [int]$Every = 5,
    [int]$Left = 0,
    [int]$Top = 0,
    [int]$Width = 480,
    [int]$Height = 1040,
    [string]$Chrome = "$env:ProgramFiles\Google\Chrome\Application\chrome.exe",
    [switch]$NoBuild
)
$ErrorActionPreference = 'Stop'
$repo = Split-Path (Split-Path $PSScriptRoot)
$dir = $PSScriptRoot
$fail = [char]0x2717; $ok = [char]0x2713
foreach ($tool in 'node', 'python') {
    if (-not (Get-Command $tool -ErrorAction SilentlyContinue)) { Write-Host "$fail $tool not found. Install it and run the comparison again." -ForegroundColor Red; exit 1 }
}
if (-not (Test-Path $Chrome)) { Write-Host "$fail Chrome not found at $Chrome. Pass its path with -Chrome." -ForegroundColor Red; exit 1 }
$devSettings = Join-Path $repo '.dev\settings.json'
if (-not (Test-Path $devSettings)) { Write-Host "$fail $devSettings not found. Set up a debug run of Rill first (README: Platform setup)." -ForegroundColor Red; exit 1 }

# Built like the installer's copy, with debug assertions on so it signs in with the development
# sign-ins instead of an installed Rill's, and with symbols so threads.ps1 can name functions.
$target = Join-Path $repo 'target\compare'
if (-not $NoBuild) {
    Write-Host "Building Rill..."
    $env:CARGO_TARGET_DIR = $target
    $env:CARGO_PROFILE_RELEASE_DEBUG_ASSERTIONS = 'true'
    $env:CARGO_PROFILE_RELEASE_STRIP = 'false'
    $env:CARGO_PROFILE_RELEASE_DEBUG = 'line-tables-only'
    Push-Location $repo
    try { cargo build --release -p rill-app } finally { Pop-Location }
    if ($LASTEXITCODE -ne 0) { Write-Host "$fail Build failed. Fix the errors above and run the comparison again." -ForegroundColor Red; exit 1 }
}
$exe = Join-Path $target 'release\rill.exe'

$out = Join-Path $repo (".dev\compare\{0:yyyyMMdd-HHmm}" -f (Get-Date))
New-Item -ItemType Directory -Force $out | Out-Null
# Rill's settings for the run: the development app IDs, pointed at the three channels. They hold
# secrets, so they're kept outside the repository and deleted at the end.
$data = Join-Path ([IO.Path]::GetTempPath()) ("rill-compare-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force $data | Out-Null
$settings = Get-Content -Raw -Encoding utf8 $devSettings | ConvertFrom-Json
function Set-Field($obj, $name, $value) { $obj | Add-Member -NotePropertyName $name -NotePropertyValue $value -Force }
foreach ($p in 'twitch', 'youtube', 'kick', 'overlay') { if (-not $settings.$p) { Set-Field $settings $p ([pscustomobject]@{}) } }
Set-Field $settings.twitch 'channel' $Twitch
Set-Field $settings.youtube 'video' $YouTubeVideo
Set-Field $settings.kick 'channel' $Kick
Set-Field $settings.overlay 'enabled' $false
Set-Field $settings 'auto_update' $false
Set-Field $settings 'disabled_platforms' @('rumble')
$settings | ConvertTo-Json -Depth 10 | Set-Content -Encoding utf8 (Join-Path $data 'settings.json')

$profileDir = Join-Path $data 'chrome-profile'
$port = 9333
$urls = @("https://www.twitch.tv/popout/$Twitch/chat", "https://www.youtube.com/live_chat?is_popout=1&v=$YouTubeVideo", "https://kick.com/popout/$Kick/chat")
@{ twitch = $Twitch; youtube_video = $YouTubeVideo; kick = $Kick; seconds = $Seconds; warmup = $Warmup; every = $Every
   window = "${Width}x${Height}"; chrome = (Get-Item $Chrome).VersionInfo.ProductVersion; started = (Get-Date).ToString('o') } |
    ConvertTo-Json | Set-Content -Encoding utf8 (Join-Path $out 'run.json')

Add-Type @'
using System; using System.Runtime.InteropServices;
public static class CompareWindows {
  [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int cx, int cy, uint flags);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
}
'@
Add-Type -AssemblyName System.Drawing
. (Join-Path $dir 'procinfo.ps1')

function Log($m) { "{0:HH:mm:ss} {1}" -f (Get-Date), $m | Tee-Object -FilePath (Join-Path $out 'log.txt') -Append }
function Shot($name) {
    $bmp = New-Object Drawing.Bitmap ($Width * 4), $Height
    $g = [Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($Left, $Top, 0, 0, $bmp.Size)
    $bmp.Save((Join-Path $out $name), [Drawing.Imaging.ImageFormat]::Png)
    $g.Dispose(); $bmp.Dispose()
}
function Chrome-Processes { Get-CimInstance Win32_Process -Filter "Name='chrome.exe'" | Where-Object { $_.CommandLine -like "*$profileDir*" } }

$rill = $null
try {
    # Chrome: a fresh profile, one app window per chat, as a pop-out opens.
    $common = "--user-data-dir=`"$profileDir`" --no-first-run --no-default-browser-check --remote-debugging-port=$port"
    foreach ($i in 0..2) {
        Start-Process $Chrome "$common --app=$($urls[$i]) --window-position=$($Left + ($i + 1) * $Width),$Top --window-size=$Width,$Height" | Out-Null
        Start-Sleep -Seconds 3
    }
    Log 'Chrome started'

    # Rill reads the same three channels, with no console window.
    $psi = New-Object Diagnostics.ProcessStartInfo $exe
    $psi.Arguments = "--exit-after $($Warmup + $Seconds + 120) --stats `"$out\rill-stats.txt`""
    $psi.UseShellExecute = $false
    $psi.CreateNoWindow = $true
    $psi.EnvironmentVariables['RILL_DATA_DIR'] = $data
    $psi.EnvironmentVariables['RILL_LOG'] = 'info,rill::image_cache=debug'
    $rill = [Diagnostics.Process]::Start($psi)
    $sw = [Diagnostics.Stopwatch]::StartNew()
    while ($rill.MainWindowHandle -eq 0 -and $sw.Elapsed.TotalSeconds -lt 30) { Start-Sleep -Milliseconds 100; $rill.Refresh() }
    [CompareWindows]::ShowWindow($rill.MainWindowHandle, 9) | Out-Null
    [CompareWindows]::SetWindowPos($rill.MainWindowHandle, [IntPtr]::Zero, $Left, $Top, $Width, $Height, 0x0014) | Out-Null
    Log "Rill started, pid $($rill.Id)"

    # Once the pages have loaded: YouTube to Live chat, message counters, window places.
    Start-Sleep -Seconds 20
    $setup = node (Join-Path $dir 'chrome.mjs') $port setup $Left $Top $Width $Height
    $setup | Set-Content -Encoding utf8 (Join-Path $out 'setup.json')
    Log "Setup: $(($setup | ConvertFrom-Json | Select-Object windows, counters, renderer, youtubeMode | ConvertTo-Json -Compress -Depth 4))"
    [CompareWindows]::SetWindowPos($rill.MainWindowHandle, [IntPtr]::Zero, $Left, $Top, $Width, $Height, 0x0014) | Out-Null
    $browserPid = (Chrome-Processes | Where-Object { $_.CommandLine -notmatch '--type=' } | Select-Object -First 1).ProcessId
    Log "Chrome browser pid $browserPid"
    Start-Sleep -Seconds ([math]::Max(0, $Warmup - 30))

    # Measurement.
    node (Join-Path $dir 'chrome.mjs') $port reset | Out-Null
    node (Join-Path $dir 'chrome.mjs') $port procs | Set-Content -Encoding utf8 (Join-Path $out 'chrome-procs-start.json')
    $irc = Start-Process python -ArgumentList "-I `"$dir\twitch-rate.py`" $Seconds $Twitch" -RedirectStandardOutput (Join-Path $out 'twitch-irc.txt') -WindowStyle Hidden -PassThru
    $n = [int]($Seconds / $Every)
    $counters = Start-Job -ArgumentList $n, $Every, (Join-Path $out 'counters.csv') -ScriptBlock {
        param($n, $every, $file)
        Get-Counter -Counter '\GPU Engine(*)\Utilization Percentage', '\GPU Process Memory(*)\Dedicated Usage', '\GPU Process Memory(*)\Shared Usage', '\Processor(_Total)\% Processor Time', '\Memory\Available MBytes' -SampleInterval $every -MaxSamples $n |
            ForEach-Object { $t = $_.Timestamp.ToString('o'); $_.CounterSamples | Where-Object { $_.CookedValue -gt 0 } | ForEach-Object { "$t|$($_.Path)|$($_.CookedValue)" } } |
            Set-Content -Encoding utf8 $file
    }
    Start-Process powershell -ArgumentList "-NoProfile -ExecutionPolicy Bypass -File `"$dir\threads.ps1`" -ProcessId $($rill.Id) -Out `"$out\rill-threads`" -Seconds $($Seconds + 5)" -WindowStyle Hidden | Out-Null
    [ProcInfo]::CyclesPerSecond() | Set-Content -Encoding utf8 (Join-Path $out 'cycles-per-second.txt')
    Log 'Measuring'
    $rows = New-Object System.Collections.Generic.List[string]
    $rows.Add('t_ms,pid,who,cpu_ms,cycles,private_ws_mb,private_mb,ws_mb,threads,handles,peak_ws_mb,page_faults,read_ops,write_ops,other_ops,read_bytes,write_bytes,other_bytes,context_switches')
    $clock = [Diagnostics.Stopwatch]::StartNew()
    for ($i = 0; $i -le $n; $i++) {
        while ($clock.Elapsed.TotalSeconds -lt $i * $Every) { Start-Sleep -Milliseconds 20 }
        $t = $clock.Elapsed.TotalMilliseconds
        foreach ($r in [ProcInfo]::All()) {
            $who = if ($r.Pid -eq $rill.Id) { 'rill' } elseif ($r.Pid -eq $browserPid -or $r.Parent -eq $browserPid) { 'chrome' } else { $null }
            if (-not $who) { continue }
            $rows.Add(('{0:F0},{1},{2},{3:F1},{4},{5:F2},{6:F2},{7:F2},{8},{9},{10:F2},{11},{12},{13},{14},{15},{16},{17},{18}' -f $t, $r.Pid, $who, ($r.CpuTicks / 10000), $r.Cycles, ($r.PrivateWs / 1MB), ($r.Private / 1MB), ($r.Ws / 1MB), $r.Threads, $r.Handles, ($r.PeakWs / 1MB), $r.PageFaults, $r.ReadOps, $r.WriteOps, $r.OtherOps, $r.ReadBytes, $r.WriteBytes, $r.OtherBytes, $r.ContextSwitches))
        }
        if ($i -eq [int]($n / 2)) { Shot 'midway.png'; Log 'Screenshot taken' }
        if ($i % 12 -eq 0) {
            $pages = node (Join-Path $dir 'chrome.mjs') $port read
            ('{{"t_ms":{0:F0},"pages":{1}}}' -f $t, $pages) | Add-Content -Encoding utf8 (Join-Path $out 'pages.jsonl')
        }
    }
    $rows | Set-Content -Encoding utf8 (Join-Path $out 'procs.csv')
    node (Join-Path $dir 'chrome.mjs') $port procs | Set-Content -Encoding utf8 (Join-Path $out 'chrome-procs-end.json')
    node (Join-Path $dir 'chrome.mjs') $port read | Set-Content -Encoding utf8 (Join-Path $out 'read.json')
    Log 'Measured; waiting for Rill to write its counters'
    Wait-Job $counters -Timeout 60 | Out-Null; Remove-Job $counters -Force
    $irc.WaitForExit(60000) | Out-Null
    $rill.WaitForExit(240000) | Out-Null
    Copy-Item (Join-Path $data 'logs\*') $out -ErrorAction SilentlyContinue
} finally {
    if ($rill -and -not $rill.HasExited) { Stop-Process -Id $rill.Id -Force -ErrorAction SilentlyContinue }
    Chrome-Processes | ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
    Start-Sleep -Seconds 2
    Remove-Item -Recurse -Force $data -ErrorAction SilentlyContinue
}
Log 'Done'
Write-Host "$ok Comparison finished. Summarize it with: python scripts/compare/summarize.py `"$out`"" -ForegroundColor Green
