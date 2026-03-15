# CrispSorter Environment Setup
# Dynamically configures MSVC, CUDA, and Rust paths without hardcoded usernames.

$ProjectRoot = $PSScriptRoot
if (-not $ProjectRoot) { $ProjectRoot = Get-Location }

Write-Host "--- Configuring Environment ---" -ForegroundColor Cyan

# 1. Detect Visual Studio / MSVC (Required for CUDA kernels)
$VSPath = "C:\Program Files\Microsoft Visual Studio"
if (Test-Path $VSPath) {
    $cl = Get-ChildItem -Path $VSPath -Filter cl.exe -Recurse -ErrorAction SilentlyContinue | 
          Where-Object { $_.FullName -like "*Hostx64\x64*" } | 
          Select-Object -First 1
    
    if ($cl) {
        $MSVCBin = $cl.DirectoryName
        Write-Host "Detected MSVC: $MSVCBin" -ForegroundColor Green
        $env:PATH = "$MSVCBin;" + $env:PATH
    }
}

# 2. Locate the REAL Rust Toolchain (Bypassing broken Chocolatey shims)
$RustupBin = "$env:USERPROFILE\.rustup\toolchains"
if (Test-Path $RustupBin) {
    $StablePath = Get-ChildItem -Path $RustupBin -Filter "stable-x86_64-pc-windows-msvc" -Directory | Select-Object -First 1
    if ($StablePath) {
        $RealRustBin = Join-Path $StablePath.FullName "bin"
        Write-Host "Detected Rust: $RealRustBin" -ForegroundColor Green
        $env:PATH = "$RealRustBin;" + $env:PATH
        $env:CARGO = Join-Path $RealRustBin "cargo.exe"
        $env:RUSTC = Join-Path $RealRustBin "rustc.exe"
    }
}

# 3. Project specific priorities
$PriorityPaths = @(
    "$env:USERPROFILE\.cargo\bin",
    (Join-Path $ProjectRoot "gh_temp\bin"),
    (Join-Path $ProjectRoot "src-tauri\target\release")
)

foreach ($Path in $PriorityPaths) {
    if (Test-Path $Path) {
        $env:PATH = "$Path;" + ($env:PATH -replace [regex]::Escape("$Path;"), "" -replace [regex]::Escape(";$Path"), "")
    }
}

# 4. Cleanup broken paths (Chocolatey shims often cause the 'cargo metadata' error)
$PathArray = $env:PATH -split ';'
$CleanedPath = ($PathArray | Where-Object { $_ -notlike "*chocolatey*rust*" -and $_ -notlike "*ProgramData\chocolatey\bin" }) -join ';'
$env:PATH = $CleanedPath

# Final Verification
$CargoPath = (Get-Command cargo -ErrorAction SilentlyContinue).Source
Write-Host "Active Cargo: $CargoPath" -ForegroundColor Yellow
Write-Host "Environment configured successfully.`n" -ForegroundColor Green
