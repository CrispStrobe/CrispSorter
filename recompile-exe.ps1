# CrispSorter Production Build (EXE)
# Configures the environment and builds a production-ready optimized executable.

$ProjectRoot = $PSScriptRoot
if (-not $ProjectRoot) { $ProjectRoot = Get-Location }

Write-Host "--- Starting Production Build Process ---" -ForegroundColor Cyan

# 1. Setup Environment (MSVC, CUDA, Rust)
. (Join-Path $ProjectRoot "paths.ps1")

# 2. Clean rust artifacts if requested
if ($args -contains "--clean") {
    Write-Host "Cleaning build cache for fresh compile..." -ForegroundColor Yellow
    Set-Location (Join-Path $ProjectRoot "src-tauri")
    & $env:CARGO clean
    Set-Location $ProjectRoot
}

# 3. Production Build
Write-Host "Building optimized executable with Tauri..." -ForegroundColor Cyan
& npm run tauri build

# 4. Success Reporting
$ExePath = Join-Path $ProjectRoot "src-tauri\target\release\CrispSorter.exe"
if (-not (Test-Path $ExePath)) {
    # Fallback check for default tauri-app name
    $ExePath = Join-Path $ProjectRoot "src-tauri\target\release\tauri-app.exe"
}

if (Test-Path $ExePath) {
    Write-Host "`nBuild Successful!" -ForegroundColor Green
    Write-Host "Executable located at: $ExePath" -ForegroundColor Yellow
    
    # Optional: Open the folder
    $ExplorerPath = Split-Path $ExePath
    explorer.exe $ExplorerPath
} else {
    Write-Error "Build appeared to finish, but executable was not found in expected location."
}
