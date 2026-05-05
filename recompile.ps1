# CrispSorter Recompile and Run
#
# Runs the dev server. If the CrispEmbed sibling repo and a staged prebuilt
# C++ library are both present, this script automatically hands off to
# `enable-crispembed.ps1` so the GGUF backend gets linked in. Otherwise it
# falls back to the plain `npm run tauri dev` flow.
#
# Flags:
#   --clean          cargo clean before building
#   --no-crispembed  force plain build even if the sibling/prebuilt are ready

param(
    [switch]$Clean,
    [Alias('NoGguf')]
    [switch]$NoCrispEmbed,
    # Internal: stop before invoking npm/cargo. Used by self-test only.
    [switch]$DryRun,
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$Rest
)

# Accept the legacy `--clean` / `--no-crispembed` literal-arg form too.
if ($Rest -contains '--clean')         { $Clean = $true }
if ($Rest -contains '--no-crispembed') { $NoCrispEmbed = $true }
if ($Rest -contains '--dry-run')       { $DryRun = $true }

$ProjectRoot = $PSScriptRoot
if (-not $ProjectRoot) { $ProjectRoot = Get-Location }

# 1. Auto-detect CrispEmbed: both the sibling repo source AND a staged
#    prebuilt C++ library (or a CRISPEMBED_SYS_LIB_DIR) must be present.
$SiblingCargo = Join-Path $ProjectRoot '..\CrispEmbed\crispembed\Cargo.toml'
$DefaultPrebuilt = Join-Path $ProjectRoot 'src-tauri\crispembed-prebuilt\crispembed.lib'
$EnvLibDir = $env:CRISPEMBED_SYS_LIB_DIR
$EnvLibValid = $EnvLibDir -and (Test-Path (Join-Path $EnvLibDir 'crispembed.lib'))

$CrispEmbedReady = (Test-Path $SiblingCargo) -and ((Test-Path $DefaultPrebuilt) -or $EnvLibValid)

if ($CrispEmbedReady -and -not $NoCrispEmbed) {
    Write-Host "CrispEmbed source + prebuilt detected -- delegating to enable-crispembed.ps1." -ForegroundColor Cyan
    Write-Host "(Pass --no-crispembed to skip and use the plain build.)" -ForegroundColor DarkGray
    # Hashtable splat (NOT array splat — array splat is positional and
    # collides with the `[ValidateSet]`-attributed `$Mode` parameter).
    $delegate = @{
        Mode         = 'dev'
        SkipDownload = $true
    }
    if ($Clean)  { $delegate['Clean']  = $true }
    if ($DryRun) { $delegate['DryRun'] = $true }
    & (Join-Path $ProjectRoot 'enable-crispembed.ps1') @delegate
    return
}

# Helpful nudge when the sibling exists but the prebuilt isn't staged yet.
if ((Test-Path $SiblingCargo) -and -not $CrispEmbedReady -and -not $NoCrispEmbed) {
    Write-Host "CrispEmbed source detected at ..\CrispEmbed but no prebuilt staged." -ForegroundColor Yellow
    Write-Host "Run .\enable-crispembed.ps1 once to fetch + stage it; then this script picks it up." -ForegroundColor Yellow
}

# 2. Plain build path (no CrispEmbed feature).
. (Join-Path $ProjectRoot "paths.ps1")

if ($Clean) {
    Write-Host "Cleaning Rust artifacts..." -ForegroundColor Yellow
    Push-Location (Join-Path $ProjectRoot "src-tauri")
    try { & $env:CARGO clean } finally { Pop-Location }
}

if ($DryRun) {
    Write-Host "DryRun: stopping before npm. (plain build path, no CrispEmbed)" -ForegroundColor Magenta
    return
}
Write-Host "Starting CrispSorter in Dev Mode..." -ForegroundColor Cyan
& npm run tauri dev
