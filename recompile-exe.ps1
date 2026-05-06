# CrispSorter Production Build (EXE)
#
# Builds the production-ready optimized executable. If the CrispEmbed
# sibling repo and a staged prebuilt C++ library are both present, this
# script automatically hands off to `enable-crispembed.ps1 -Mode build` so
# the GGUF backend ships in the bundle. Otherwise it falls back to a plain
# `npm run tauri build`.
#
# Flags:
#   --clean          cargo clean before building
#   --no-crispembed  force plain build even if the sibling/prebuilt are ready

param(
    [switch]$Clean,
    [Alias('NoGguf')]
    [switch]$NoCrispEmbed,
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$Rest
)

if ($Rest -contains '--clean')         { $Clean = $true }
if ($Rest -contains '--no-crispembed') { $NoCrispEmbed = $true }

$ProjectRoot = $PSScriptRoot
if (-not $ProjectRoot) { $ProjectRoot = Get-Location }

Write-Host "--- Starting Production Build Process ---" -ForegroundColor Cyan

# 1. Auto-detect CrispEmbed.
$SiblingCargo = Join-Path $ProjectRoot '..\CrispEmbed\crispembed\Cargo.toml'
$DefaultPrebuilt = Join-Path $ProjectRoot 'src-tauri\crispembed-prebuilt\crispembed.lib'
$EnvLibDir = $env:CRISPEMBED_SYS_LIB_DIR
$EnvLibValid = $EnvLibDir -and (Test-Path (Join-Path $EnvLibDir 'crispembed.lib'))

$CrispEmbedReady = (Test-Path $SiblingCargo) -and ((Test-Path $DefaultPrebuilt) -or $EnvLibValid)

if ($CrispEmbedReady -and -not $NoCrispEmbed) {
    Write-Host "CrispEmbed source + prebuilt detected -- delegating to enable-crispembed.ps1 -Mode build." -ForegroundColor Cyan
    Write-Host "(Pass --no-crispembed to skip and use the plain build.)" -ForegroundColor DarkGray
    # Hashtable splat (array splat is positional and collides with
    # `enable-crispembed.ps1`'s `[ValidateSet]`-attributed `$Mode`).
    $delegate = @{
        Mode         = 'build'
        SkipDownload = $true
    }
    if ($Clean) { $delegate['Clean'] = $true }
    & (Join-Path $ProjectRoot 'enable-crispembed.ps1') @delegate

    # Skip the success-reporting branch below -- enable-crispembed has
    # already handed off to `npm run tauri build`.
    return
}

if ((Test-Path $SiblingCargo) -and -not $CrispEmbedReady -and -not $NoCrispEmbed) {
    Write-Host "CrispEmbed source detected at ..\CrispEmbed but no prebuilt staged." -ForegroundColor Yellow
    Write-Host "Run .\enable-crispembed.ps1 -Mode build once to fetch + stage it." -ForegroundColor Yellow
}

# 2. Plain production build path.
. (Join-Path $ProjectRoot "paths.ps1")

if ($Clean) {
    Write-Host "Cleaning build cache for fresh compile..." -ForegroundColor Yellow
    Push-Location (Join-Path $ProjectRoot "src-tauri")
    try { & $env:CARGO clean } finally { Pop-Location }
}

Write-Host "Building optimized executable with Tauri..." -ForegroundColor Cyan
& npm run tauri build

# 3. Success Reporting
# Honour CARGO_TARGET_DIR (e.g. set to D:\cargo-target\crispsorter to
# keep build artefacts off the boot drive). Falls back to the
# workspace-root target/, then the legacy src-tauri\target\ path for
# old branches that haven't picked up the workspace move.
$CargoTargetRoot = if ($env:CARGO_TARGET_DIR) {
    $env:CARGO_TARGET_DIR
} else {
    Join-Path $ProjectRoot "target"
}
$ExeCandidates = @(
    (Join-Path $CargoTargetRoot   "release\CrispSorter.exe"),
    (Join-Path $CargoTargetRoot   "release\tauri-app.exe"),
    (Join-Path $ProjectRoot       "src-tauri\target\release\CrispSorter.exe"),
    (Join-Path $ProjectRoot       "src-tauri\target\release\tauri-app.exe")
)
$ExePath = $ExeCandidates | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $ExePath) { $ExePath = $ExeCandidates[0] }

if (Test-Path $ExePath) {
    Write-Host "`nBuild Successful!" -ForegroundColor Green
    Write-Host "Executable located at: $ExePath" -ForegroundColor Yellow

    $ExplorerPath = Split-Path $ExePath
    explorer.exe $ExplorerPath
} else {
    Write-Error "Build appeared to finish, but executable was not found in expected location."
}
