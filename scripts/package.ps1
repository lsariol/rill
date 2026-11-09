# Builds Rill's Windows installer and update packages with Velopack.
# Run on Windows: powershell -File scripts/package.ps1 [-Version 0.1.0] [-Out <dir>] [-NoBuild]
#   [-AzureSignFile <metadata.json>]
# Needs vpk (dotnet tool install -g vpk). Writes Setup.exe, a portable zip and the update feed
# (releases.win.json and the .nupkg files) to -Out, .dev/releases by default; packaging several
# versions into the same folder gives a feed that updates from one to the next.
# Release builds carry the app IDs from the environment at build time (see README).
param([string]$Version = "", [string]$Out = "", [switch]$NoBuild, [string]$AzureSignFile = "")
$ErrorActionPreference = 'Stop'
Set-Location (Split-Path $PSScriptRoot)
$fail = [char]0x2717; $ok = [char]0x2713

if (-not $Version) {
    $line = Select-String -Path Cargo.toml -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1
    $Version = $line.Matches[0].Groups[1].Value
}
if (-not $Out) { $Out = Join-Path (Resolve-Path ".").Path ".dev\releases" }

# vpk from PATH, else where `dotnet tool install` puts it for this user.
$vpk = (Get-Command vpk -ErrorAction SilentlyContinue).Source
if (-not $vpk) {
    foreach ($p in @("$env:USERPROFILE\.dotnet\tools\vpk.exe")) { if (Test-Path $p) { $vpk = $p } }
}
if (-not $vpk) {
    Write-Host "$fail vpk not found. Install the .NET 8 SDK, then: dotnet tool install -g vpk" -ForegroundColor Red
    exit 1
}
if (-not $env:DOTNET_ROOT -and (Test-Path "$env:USERPROFILE\.dotnet\dotnet.exe")) {
    $env:DOTNET_ROOT = "$env:USERPROFILE\.dotnet"
}

if (-not $NoBuild) {
    Write-Host "Building the release binary..."
    cargo build --release -p rill-app
    if ($LASTEXITCODE -ne 0) { Write-Host "$fail Build failed. Fix the errors above and package again." -ForegroundColor Red; exit 1 }
}

# Only the program goes in the package: its assets and icon are built into rill.exe.
$stage = Join-Path (Resolve-Path ".").Path ".dev\package\stage"
Remove-Item -Recurse -Force $stage -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $stage | Out-Null
Copy-Item "target\release\rill.exe" $stage

$args = @(
    "pack",
    "--packId", "Rill",
    "--packVersion", $Version,
    "--packDir", $stage,
    "--mainExe", "rill.exe",
    "--packTitle", "Rill",
    "--packAuthors", "Mobasity",
    "--icon", "crates\rill-app\assets\app\rill.ico",
    "--shortcuts", "StartMenuRoot,Desktop",
    "--outputDir", $Out,
    "--yes"
)
if ($AzureSignFile) { $args += @("--azureTrustedSignFile", $AzureSignFile) }
& $vpk @args
if ($LASTEXITCODE -ne 0) { Write-Host "$fail vpk pack failed. See its messages above." -ForegroundColor Red; exit 1 }
Write-Host "$ok Rill $Version packaged in $Out" -ForegroundColor Green
Get-ChildItem $Out | Select-Object Name, @{n = 'MB'; e = { [math]::Round($_.Length / 1MB, 1) } } | Format-Table -AutoSize
