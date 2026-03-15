# CrispSorter GitHub Release Script
# This script automates the creation of a GitHub release using the 'gh' CLI.

# 1. Locate GitHub CLI (gh.exe)
$GH_EXE = "gh"
if (-not (Get-Command $GH_EXE -ErrorAction SilentlyContinue)) {
    $CommonPaths = @(
        "$PSScriptRoot\gh_temp\bin\gh.exe",
        "$env:ProgramFiles\GitHub CLI\gh.exe",
        "${env:ProgramFiles(x86)}\GitHub CLI\gh.exe",
        "$env:USERPROFILE\AppData\Local\Programs\GitHub CLI\gh.exe"
    )
    
    foreach ($Path in $CommonPaths) {
        if (Test-Path $Path) {
            $GH_EXE = $Path
            break
        }
    }
}

if (-not (Get-Command $GH_EXE -ErrorAction SilentlyContinue) -and -not (Test-Path $GH_EXE)) {
    Write-Error "GitHub CLI (gh) not found. Please install it or ensure it is in a common location."
    exit 1
}

$GH_Source = (Get-Command $GH_EXE -ErrorAction SilentlyContinue).Source
if (-not $GH_Source) { $GH_Source = $GH_EXE }
Write-Host "Using GitHub CLI at: $GH_Source" -ForegroundColor Cyan

# 2. Get version from package.json
$PackageJson = Get-Content -Raw -Path "$PSScriptRoot\package.json" | ConvertFrom-Json
$Version = "v$($PackageJson.version)"
Write-Host "Releasing version: $Version" -ForegroundColor Cyan

# 3. Identify Build Artifacts
$ReleaseDir = Join-Path $PSScriptRoot "src-tauri\target\release"
$BundleDir = Join-Path $ReleaseDir "bundle"

$Artifacts = @()

# Main EXE (Portable)
$MainExe = Join-Path $ReleaseDir "tauri-app.exe"
if (Test-Path $MainExe) { 
    # Rename to a more descriptive name for the release
    $FinalExe = Join-Path $ReleaseDir "CrispSorter.exe"
    Copy-Item $MainExe $FinalExe -Force
    $Artifacts += $FinalExe 
}

# NSIS Installer
$NsisPath = Join-Path $BundleDir "nsis\CrispSorter_$($PackageJson.version)_x64-setup.exe"
if (Test-Path $NsisPath) { $Artifacts += $NsisPath }

# MSI Installer
$MsiPath = Join-Path $BundleDir "msi\CrispSorter_$($PackageJson.version)_x64_en-US.msi"
if (Test-Path $MsiPath) { $Artifacts += $MsiPath }

if ($Artifacts.Count -eq 0) {
    Write-Error "No build artifacts found in $ReleaseDir. Did you run 'npm run tauri build'?"
    exit 1
}

Write-Host "Artifacts found:" -ForegroundColor Green
$Artifacts | ForEach-Object { Write-Host " - $_" }

# 4. Create/Update GitHub Release
Write-Host "Releasing $Version to GitHub..." -ForegroundColor Yellow

# Try to create the release first (ignores error if it exists)
& $GH_EXE release create $Version --title "CrispSorter $Version" --notes "Automated release for version $Version" 2>$null

# Upload/Update artifacts
Write-Host "Uploading artifacts..." -ForegroundColor Yellow
& $GH_EXE release upload $Version $Artifacts --clobber

if ($LASTEXITCODE -eq 0) {
    Write-Host "Release $Version successfully updated with artifacts!" -ForegroundColor Green
} else {
    Write-Error "Failed to upload artifacts to GitHub release."
}
