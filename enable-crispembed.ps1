# enable-crispembed.ps1
#
# Build CrispSorter with the CrispEmbed (GGUF) backend turned on.
#
# What it does:
#   1. Makes sure the CrispEmbed source repo is at `..\CrispEmbed` (clones it
#      via `gh repo clone` if not -- that's where the high-level `crispembed`
#      Rust crate lives; without source we can't compile the high-level API).
#   2. Downloads the latest prebuilt **C++ library** tarball for your OS from
#      the CrispEmbed GitHub release and unpacks it into
#      `src-tauri\crispembed-prebuilt\`.
#   3. Points `CRISPEMBED_SYS_LIB_DIR` at that directory so `crispembed-sys`
#      links against the prebuilt `crispembed.lib` instead of running a
#      ~15-minute CMake build.
#   4. Hands off to `recompile.ps1` (dev) or `recompile-exe.ps1` (release)
#      with the right Cargo feature flag for your platform.
#
# Usage:
#   . .\enable-crispembed.ps1                   # dev mode (npm run tauri dev)
#   . .\enable-crispembed.ps1 -Mode build       # production .exe (npm run tauri build)
#   . .\enable-crispembed.ps1 -Backend cuda     # force CUDA backend
#   . .\enable-crispembed.ps1 -Backend vulkan   # force Vulkan backend (default on Win/Linux)
#   . .\enable-crispembed.ps1 -Backend cpu      # CPU-only, no GPU
#   . .\enable-crispembed.ps1 -SkipDownload     # reuse already-extracted libs

param(
    [ValidateSet('dev','build')]
    [string]$Mode = 'dev',
    [ValidateSet('vulkan','cuda','metal','cpu')]
    [string]$Backend = '',
    [switch]$SkipDownload,
    [switch]$Force,
    # Pass-through to cargo clean before the build (matches recompile.ps1).
    [switch]$Clean,
    # Use a custom prebuilt directory instead of the GitHub release tarball.
    # Useful when you've built CrispEmbed yourself with GPU support, e.g.:
    #   cd ..\CrispEmbed && .\build-cuda.bat
    #   .\enable-crispembed.ps1 -Backend cuda -LibDir ..\CrispEmbed\build-cuda\src\Release
    [string]$LibDir = '',
    # Stop after preparing env vars and feature flag (no npm/cargo invocation).
    # Useful for self-test / CI smoke check.
    [switch]$DryRun
)

$ErrorActionPreference = 'Stop'
$ProjectRoot = $PSScriptRoot
if (-not $ProjectRoot) { $ProjectRoot = Get-Location }

# 1. Pick a default backend per OS if not specified.
if (-not $Backend) {
    if ($IsMacOS) { $Backend = 'metal' }
    elseif ($IsLinux) { $Backend = 'vulkan' }
    else { $Backend = 'vulkan' }   # Windows
}
$CargoFeature = "crispembed-$Backend"
if ($Backend -eq 'cpu') { $CargoFeature = 'crispembed' }

Write-Host "=== Enable CrispEmbed (GGUF) -- backend: $Backend ===" -ForegroundColor Cyan

# 2. Ensure the CrispEmbed source repo is checked out next to this one.
#    The path dep in `src-tauri\Cargo.toml` points at `..\..\CrispEmbed\crispembed`.
$SiblingRoot = Resolve-Path (Join-Path $ProjectRoot '..\CrispEmbed') -ErrorAction SilentlyContinue
if (-not $SiblingRoot -or -not (Test-Path (Join-Path $SiblingRoot 'crispembed\Cargo.toml'))) {
    Write-Host "CrispEmbed sibling repo not found at ..\CrispEmbed" -ForegroundColor Yellow
    $clonePath = (Resolve-Path (Join-Path $ProjectRoot '..')).Path
    Write-Host "Cloning into $clonePath\CrispEmbed ..." -ForegroundColor Yellow
    Push-Location $clonePath
    try {
        & git clone https://github.com/CrispStrobe/CrispEmbed.git
        if ($LASTEXITCODE -ne 0) { throw "git clone failed" }
    } finally {
        Pop-Location
    }
    $SiblingRoot = Resolve-Path (Join-Path $ProjectRoot '..\CrispEmbed')
}
Write-Host "CrispEmbed source: $SiblingRoot" -ForegroundColor Green

# 3. Locate or download the prebuilt C++ library.
#
# Order of precedence:
#   1. Explicit -LibDir argument (user-built, e.g. with GPU support)
#   2. CRISPEMBED_SYS_LIB_DIR already set in env
#   3. -SkipDownload + existing src-tauri\crispembed-prebuilt\crispembed.lib
#   4. Fresh download from CrispEmbed GitHub release
$PrebuiltDir = Join-Path $ProjectRoot 'src-tauri\crispembed-prebuilt'

if ($LibDir) {
    $resolved = Resolve-Path $LibDir -ErrorAction SilentlyContinue
    if (-not $resolved -or -not (Test-Path (Join-Path $resolved 'crispembed.lib'))) {
        Write-Error "-LibDir '$LibDir' does not contain crispembed.lib"
        exit 1
    }
    $PrebuiltDir = $resolved.Path
    Write-Host "Using user-supplied -LibDir: $PrebuiltDir" -ForegroundColor Green
} elseif ($env:CRISPEMBED_SYS_LIB_DIR -and (Test-Path (Join-Path $env:CRISPEMBED_SYS_LIB_DIR 'crispembed.lib'))) {
    $PrebuiltDir = $env:CRISPEMBED_SYS_LIB_DIR
    Write-Host "Honouring pre-set CRISPEMBED_SYS_LIB_DIR: $PrebuiltDir" -ForegroundColor Green
} elseif ($SkipDownload -and (Test-Path (Join-Path $PrebuiltDir 'crispembed.lib'))) {
    Write-Host "Reusing existing prebuilt at $PrebuiltDir" -ForegroundColor Green
} else {
    if (Test-Path $PrebuiltDir) {
        if ($Force) { Remove-Item -Recurse -Force $PrebuiltDir }
    }
    New-Item -ItemType Directory -Force -Path $PrebuiltDir | Out-Null

    # Pick the right asset for this platform.
    $AssetName = if ($IsMacOS) { 'crispembed-macos-arm64.tar.gz' }
                 elseif ($IsLinux) { 'crispembed-linux-x86_64.tar.gz' }
                 else { 'crispembed-windows-x86_64.zip' }

    Write-Host "Downloading $AssetName from CrispEmbed latest release ..." -ForegroundColor Yellow

    # Find gh.exe (release.ps1 has the same fallback list -- keep these in sync).
    $GH_EXE = 'gh'
    if (-not (Get-Command $GH_EXE -ErrorAction SilentlyContinue)) {
        $cands = @(
            "$ProjectRoot\gh_temp\bin\gh.exe",
            "$env:ProgramFiles\GitHub CLI\gh.exe",
            "${env:ProgramFiles(x86)}\GitHub CLI\gh.exe",
            "$env:USERPROFILE\AppData\Local\Programs\GitHub CLI\gh.exe"
        )
        foreach ($c in $cands) { if (Test-Path $c) { $GH_EXE = $c; break } }
    }

    Push-Location $SiblingRoot
    try {
        & $GH_EXE release download --pattern $AssetName --dir $PrebuiltDir --clobber
        if ($LASTEXITCODE -ne 0) {
            throw "Could not download $AssetName from CrispEmbed releases. Make sure 'gh auth login' has been run."
        }
    } finally {
        Pop-Location
    }

    $ArchivePath = Join-Path $PrebuiltDir $AssetName
    Write-Host "Extracting $AssetName ..." -ForegroundColor Yellow
    if ($AssetName.EndsWith('.zip')) {
        Expand-Archive -Path $ArchivePath -DestinationPath $PrebuiltDir -Force
    } else {
        & tar -xzf $ArchivePath -C $PrebuiltDir
        if ($LASTEXITCODE -ne 0) { throw "tar extract failed" }
    }
    Remove-Item -Force $ArchivePath
}

# 4. Set CRISPEMBED_SYS_LIB_DIR and pass it through to the build.
$env:CRISPEMBED_SYS_LIB_DIR = $PrebuiltDir
Write-Host "CRISPEMBED_SYS_LIB_DIR = $env:CRISPEMBED_SYS_LIB_DIR" -ForegroundColor Green

# 4a. Stage runtime DLLs so the built .exe can load them.
#
# Windows resolves dependent DLLs by looking next to the .exe, in CWD, and
# along PATH. The crispembed.lib import library tells cargo *what* to link,
# but the matching crispembed.dll / ggml*.dll have to be findable at runtime
# or the launcher fails with STATUS_DLL_NOT_FOUND (0xc0000135).
#
# We copy them into:
#   - target\debug\          (dev mode, run via cargo run / tauri dev)
#   - target\release\        (production .exe in target/release)
#   - src-tauri\bin\         (Tauri bundles `bin/*.dll` into the
#                             installer per tauri.conf.json)
#
# NOTE: target/ moved from src-tauri/target/ to the workspace root after
# crisp-index-server / crisp-index-protocol joined the Cargo workspace
# (commit 7326771). Anything still writing to src-tauri/target is stale.
$RuntimeDlls = Get-ChildItem -Path $PrebuiltDir -Filter '*.dll' -ErrorAction SilentlyContinue
if ($RuntimeDlls) {
    $TargetDirs = @(
        (Join-Path $ProjectRoot 'target\debug'),
        (Join-Path $ProjectRoot 'target\release'),
        (Join-Path $ProjectRoot 'src-tauri\bin')
    )
    $copiedTotal = 0
    $skippedLocked = 0
    foreach ($TargetDir in $TargetDirs) {
        New-Item -ItemType Directory -Force -Path $TargetDir | Out-Null
        foreach ($Dll in $RuntimeDlls) {
            $dest = Join-Path $TargetDir $Dll.Name
            # Skip when the destination already has the right size -- this is
            # a no-op re-stage and trying to re-copy fails with sharing
            # violations if a prior dev server / .exe has the DLL mapped.
            if ((Test-Path $dest) -and ((Get-Item $dest).Length -eq $Dll.Length)) {
                continue
            }
            try {
                Copy-Item -Force -Path $Dll.FullName -Destination $dest -ErrorAction Stop
                $copiedTotal++
            } catch {
                $skippedLocked++
            }
        }
    }
    if ($copiedTotal -gt 0) {
        Write-Host ("Staged " + $copiedTotal + " runtime DLL(s) to target\debug, target\release, and bin\") -ForegroundColor Green
    } else {
        Write-Host ("DLLs already up to date in target\debug, target\release, and bin\ (" + $RuntimeDlls.Count + " files, no copy needed)") -ForegroundColor DarkGreen
    }
    if ($skippedLocked -gt 0) {
        Write-Host ("(" + $skippedLocked + " copy(ies) skipped: file in use by a running CrispSorter -- restart it to pick up new DLLs.)") -ForegroundColor DarkYellow
    }
}

# 4b. Warn when GPU-backed feature was requested but the staged tarball is
#     CPU-only (no ggml-cuda.dll / ggml-vulkan.dll present).
if ($Backend -in @('cuda', 'vulkan', 'metal')) {
    $hasGpu = Test-Path (Join-Path $PrebuiltDir ("ggml-$Backend.dll"))
    if (-not $hasGpu) {
        Write-Host ""
        Write-Host "NOTE: requested -Backend $Backend but the upstream CrispEmbed" -ForegroundColor Yellow
        Write-Host "      prebuilt tarball is CPU-only (no ggml-$Backend.dll). The" -ForegroundColor Yellow
        Write-Host "      app will run, but inference will fall back to CPU." -ForegroundColor Yellow
        Write-Host "      For real GPU acceleration, build CrispEmbed from source:" -ForegroundColor Yellow
        Write-Host "        cd ..\\CrispEmbed && build-cuda.bat" -ForegroundColor Yellow
        Write-Host "      then re-run this script with -SkipDownload." -ForegroundColor Yellow
        Write-Host ""
    }
}

# 5. Make Tauri pass `--features <CargoFeature>` to cargo. The CLI accepts
#    feature flags via the TAURI__BUILD__FEATURES env var or the `--features`
#    arg after `--`; we use the env-var form so it works for both `dev` and
#    `build` without surface changes in package.json.
$env:TAURI_FEATURES = $CargoFeature  # Tauri 2 reads this for cargo feature passthrough

if ($DryRun) {
    Write-Host "DryRun: stopping before paths.ps1 / npm. Resolved feature: $CargoFeature" -ForegroundColor Magenta
    return
}

# 6. Configure Rust/MSVC env (paths.ps1) and run the requested mode.
. (Join-Path $ProjectRoot 'paths.ps1')

if ($Clean) {
    Write-Host "Cleaning Rust artifacts..." -ForegroundColor Yellow
    Push-Location (Join-Path $ProjectRoot 'src-tauri')
    try { & $env:CARGO clean } finally { Pop-Location }
}

if ($Mode -eq 'dev') {
    Write-Host "Starting CrispSorter in dev mode with --features $CargoFeature ..." -ForegroundColor Cyan
    & npm run tauri -- dev --features $CargoFeature
} else {
    Write-Host "Building CrispSorter (production) with --features $CargoFeature ..." -ForegroundColor Cyan
    & npm run tauri -- build --features $CargoFeature
}
