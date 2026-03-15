# CrispSorter Environment Setup
# This script ensures project-specific and Rust binaries are at the FRONT of the PATH.

$ProjectRoot = $PSScriptRoot
if (-not $ProjectRoot) { $ProjectRoot = Get-Location }

# Paths we want to prioritize
$PriorityPaths = @(
    "$env:USERPROFILE\.cargo\bin",
    (Join-Path $ProjectRoot "gh_temp\bin"),
    (Join-Path $ProjectRoot "src-tauri\target\release")
)

foreach ($Path in $PriorityPaths) {
    if (Test-Path $Path) {
        # Force to the front, even if it exists elsewhere in the string
        $env:PATH = "$Path;" + ($env:PATH -replace [regex]::Escape("$Path;"), "" -replace [regex]::Escape(";$Path"), "")
        Write-Host "Prioritized in PATH: $Path" -ForegroundColor Green
    } else {
        if ($Path -like "*cargo*") {
            Write-Warning "Rust/Cargo not found at $Path."
        }
    }
}

# Verify which cargo we are actually using now
$CargoPath = (Get-Command cargo -ErrorAction SilentlyContinue).Source
Write-Host "`nUsing Cargo at: $CargoPath" -ForegroundColor Cyan

if ($CargoPath -like "*chocolatey*") {
    Write-Error "CRITICAL: Still using the broken Chocolatey version of Cargo. Please manually remove C:\ProgramData\chocolatey\bin\cargo.exe or fix your system PATH."
} else {
    Write-Host "Environment ready. Run: npm run tauri dev" -ForegroundColor Green
}
