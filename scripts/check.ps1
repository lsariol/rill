# Runs every check before a commit: formatting, lints, tests, then licenses and advisories.
# Run from anywhere: powershell -File scripts/check.ps1
$ErrorActionPreference = 'Stop'
Set-Location (Split-Path $PSScriptRoot)
$ok = [char]0x2713; $warn = '!'; $fail = [char]0x2717

function Step([string]$Name, [scriptblock]$Command) {
    Write-Host "Running $Name..."
    & $Command
    if ($LASTEXITCODE -ne 0) {
        Write-Host "$fail $Name failed. Fix the errors above, then run scripts/check.ps1 again." -ForegroundColor Red
        exit 1
    }
}

Step 'rustfmt' { cargo fmt --all --check }
Step 'clippy' { cargo clippy --workspace --all-targets -- -D warnings }
Step 'tests' { cargo test --workspace }
if (Get-Command cargo-deny -ErrorAction SilentlyContinue) {
    Step 'cargo-deny' { cargo deny check }
} else {
    Write-Host "$warn cargo-deny is not installed, so licenses and advisories were not checked. Install it with: cargo install --locked cargo-deny" -ForegroundColor Yellow
}
Write-Host "$ok All checks passed." -ForegroundColor Green
