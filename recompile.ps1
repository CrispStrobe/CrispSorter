# CrispSorter Recompile and Run
# Ensures environment is set up, cleans old artifacts, and starts development mode.

$ProjectRoot = $PSScriptRoot
if (-not $ProjectRoot) { $ProjectRoot = Get-Location }

# 1. Setup Environment
. (Join-Path $ProjectRoot "paths.ps1")

# 2. Clean rust artifacts if requested or if switching to CUDA
if ($args -contains "--clean") {
    Write-Host "Cleaning Rust artifacts..." -ForegroundColor Yellow
    Set-Location (Join-Path $ProjectRoot "src-tauri")
    & $env:CARGO clean
    Set-Location $ProjectRoot
}

# 3. Start Tauri Dev
Write-Host "Starting CrispSorter in Dev Mode..." -ForegroundColor Cyan
& npm run tauri dev
