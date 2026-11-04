# enable-crispembed.ps1
#
# Build CrispSorter with the CrispEmbed (GGUF) backend turned on.
#
# What it does:
#   1. Makes sure the CrispEmbed source repo is at `..\CrispEmbed` (clones it
#      via `git clone` if not -- that's where the high-level `crispembed`
#      Rust crate lives; without source we can't compile the high-level API).
#   2. Verifies a pinned release archive and extracts it into an isolated
#      directory under `src-tauri\crispembed-prebuilt-cache\`.
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
#   . .\enable-crispembed.ps1 -SkipDownload     # reuse verified cached archive

param(
    [ValidateSet('dev','build')]
    [string]$Mode = 'dev',
    [ValidateSet('vulkan','cuda','metal','cpu')]
    [string]$Backend = '',
    [switch]$SkipDownload,
    [switch]$Force, # retained for compatibility; verified archives are replaced on download
    # Pass-through to cargo clean before the build (matches recompile.ps1).
    [switch]$Clean,
    # Retained for a clear error: custom binaries cannot be verified here.
    [string]$LibDir = '',
    # Stop after preparing env vars and feature flag (no npm/cargo invocation).
    # Useful for self-test / CI smoke check.
    [switch]$DryRun
)

$ErrorActionPreference = 'Stop'
$ProjectRoot = $PSScriptRoot
if (-not $ProjectRoot) { $ProjectRoot = Get-Location }
$CrispEmbedRef = 'v0.17.12'
$CrispEmbedCommit = '78493a31fc3043f3af4ab6fd2f197a32da6f07f0'

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
        & git clone --branch $CrispEmbedRef --depth 1 https://github.com/CrispStrobe/CrispEmbed.git
        if ($LASTEXITCODE -ne 0) { throw "git clone failed" }
    } finally {
        Pop-Location
    }
    $SiblingRoot = Resolve-Path (Join-Path $ProjectRoot '..\CrispEmbed')
}
$SourceCommit = & git -C $SiblingRoot rev-parse HEAD
if ($LASTEXITCODE -ne 0) { throw 'Cannot determine CrispEmbed source revision' }
$SourceDirty = & git -C $SiblingRoot status --porcelain --untracked-files=no
if ($LASTEXITCODE -ne 0) { throw 'Cannot inspect CrispEmbed source status' }
if ($SourceCommit -ne $CrispEmbedCommit -or $SourceDirty) {
    throw "CrispEmbed source must be clean at $CrispEmbedCommit (found $SourceCommit); no files were changed."
}
Write-Host "CrispEmbed source: $SiblingRoot" -ForegroundColor Green

# 3. Locate or download the prebuilt C++ library.
#
# -SkipDownload requires the cached release archive; its SHA256 is always checked.
if ($LibDir) { throw '-LibDir is unsupported: custom binaries have no verified release provenance.' }
$arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
if ($IsMacOS -and $arch -eq 'Arm64') {
    $AssetName = 'crispembed-macos-arm64.tar.gz'; $Sha256 = '6bdbf65587eac063ec6658d9e03345a63614cee680ea887ef51eaabcc72f7a56'
} elseif ($IsLinux -and $arch -eq 'Arm64') {
    $AssetName = 'crispembed-linux-arm64.tar.gz'; $Sha256 = '38cbaaa020cd17d4cd4f0b78003c79fba7a1051628b06b8e39734c39f15032bb'
} elseif ($IsLinux -and $arch -eq 'X64') {
    $AssetName = 'crispembed-linux-x86_64.tar.gz'; $Sha256 = '1e53efd8d78e6f6abeed252e3f715c5c34c8b97508922c5983285400c96e2b63'
} elseif (-not $IsMacOS -and -not $IsLinux -and $arch -eq 'X64') {
    $AssetName = 'crispembed-windows-x86_64.zip'; $Sha256 = '49a1e4a89bf6f7d2df100794914f15aeb9a2b69513c3dbfb93364208994cc717'
} else { throw "No pinned CrispEmbed asset for architecture $arch" }
$CacheDir = Join-Path $ProjectRoot "src-tauri/crispembed-prebuilt-cache/$CrispEmbedRef"
New-Item -ItemType Directory -Force -Path $CacheDir | Out-Null
if (-not $SkipDownload) {
    Write-Host "Downloading $AssetName from CrispEmbed $CrispEmbedRef ..." -ForegroundColor Yellow

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
        & $GH_EXE release download $CrispEmbedRef --repo CrispStrobe/CrispEmbed --pattern $AssetName --dir $CacheDir --clobber
        if ($LASTEXITCODE -ne 0) {
            throw "Could not download $AssetName from CrispEmbed releases. Make sure 'gh auth login' has been run."
        }
    } finally {
        Pop-Location
    }

}
$ArchivePath = Join-Path $CacheDir $AssetName
if (-not (Test-Path $ArchivePath)) { throw 'Pinned archive missing; run without -SkipDownload.' }
if ((Get-FileHash -Algorithm SHA256 $ArchivePath).Hash -ne $Sha256) { throw 'CrispEmbed archive SHA256 mismatch' }
# Isolate every extraction; stale files from an earlier release cannot leak in.
$PrebuiltDir = Join-Path $CacheDir ([guid]::NewGuid().ToString())
New-Item -ItemType Directory -Path $PrebuiltDir | Out-Null
if ($AssetName.EndsWith('.zip')) {
    Expand-Archive -Path $ArchivePath -DestinationPath $PrebuiltDir
} else {
    & tar -xzf $ArchivePath -C $PrebuiltDir
    if ($LASTEXITCODE -ne 0) { throw 'tar extract failed' }
}

# 4. Set CRISPEMBED_SYS_LIB_DIR and pass it through to the build.
$env:CRISPEMBED_SYS_LIB_DIR = $PrebuiltDir
Write-Host "CRISPEMBED_SYS_LIB_DIR = $env:CRISPEMBED_SYS_LIB_DIR" -ForegroundColor Green

# 3b. Record the staged library, so recompile.ps1 / recompile-exe.ps1 can see
#     that CrispEmbed was enabled without inheriting this process'
#     environment (a normal invocation does not set the variable in the
#     caller's session). Drop the previous extraction, otherwise every run
#     leaves another ~100 MB of GUID directories behind.
$ActivePtr = Join-Path (Split-Path $CacheDir -Parent) 'active.txt'
if ((Test-Path $ActivePtr) -and -not $DryRun) {
    $PreviousDir = (Get-Content $ActivePtr -Raw -ErrorAction SilentlyContinue)
    if ($PreviousDir) { $PreviousDir = $PreviousDir.Trim() }
    if ($PreviousDir -and $PreviousDir -ne $PrebuiltDir -and (Test-Path $PreviousDir)) {
        try { Remove-Item -Recurse -Force $PreviousDir -ErrorAction Stop }
        catch { Write-Host "Could not remove the previous extraction at $PreviousDir (still in use?); leaving it." -ForegroundColor DarkYellow }
    }
}
if (-not $DryRun) {
    Set-Content -Path $ActivePtr -Value $PrebuiltDir -NoNewline -Encoding ascii
}

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
    # Honour CARGO_TARGET_DIR (Cargo's standard knob for redirecting
    # build artefacts to a different drive / partition). Set it before
    # running this script to keep target/ off the boot drive, e.g.:
    #   $env:CARGO_TARGET_DIR = 'D:\cargo-target\crispsorter'
    #   .\enable-crispembed.ps1 -Backend cuda
    # Without it, fall back to the workspace-root target/.
    $CargoTargetRoot = if ($env:CARGO_TARGET_DIR) {
        $env:CARGO_TARGET_DIR
    } else {
        Join-Path $ProjectRoot 'target'
    }
    $TargetDirs = @(
        (Join-Path $CargoTargetRoot 'debug'),
        (Join-Path $CargoTargetRoot 'release'),
        (Join-Path $ProjectRoot   'src-tauri\bin')
    )
    $copiedTotal = 0

    foreach ($TargetDir in $TargetDirs) {
        New-Item -ItemType Directory -Force -Path $TargetDir | Out-Null
        foreach ($Dll in $RuntimeDlls) {
            $dest = Join-Path $TargetDir $Dll.Name
            # Skip when the destination has identical content -- this is
            # a no-op re-stage and trying to re-copy fails with sharing
            # violations if a prior dev server / .exe has the DLL mapped.
            if ((Test-Path $dest) -and ((Get-FileHash -Algorithm SHA256 $dest).Hash -eq (Get-FileHash -Algorithm SHA256 $Dll.FullName).Hash)) {
                continue
            }
            try {
                Copy-Item -Force -Path $Dll.FullName -Destination $dest -ErrorAction Stop
                $copiedTotal++
            } catch {
                throw "Could not stage $dest; stop the running application and retry. $_"
            }
        }
    }
    if ($copiedTotal -gt 0) {
        Write-Host ("Staged " + $copiedTotal + " runtime DLL(s) to '$CargoTargetRoot\{debug,release}\' and 'src-tauri\bin\'") -ForegroundColor Green
    } else {
        Write-Host ("DLLs already up to date in '$CargoTargetRoot\{debug,release}\' and 'src-tauri\bin\' (" + $RuntimeDlls.Count + " files, no copy needed)") -ForegroundColor DarkGreen
    }

}

# 4b. Warn when GPU-backed feature was requested but the staged tarball is
#     CPU-only (no ggml-cuda.dll / ggml-vulkan.dll present).
if ($Backend -in @('cuda', 'vulkan', 'metal')) {
    $hasGpu = @(Get-ChildItem -Path $PrebuiltDir -Filter "*ggml-$Backend*" -ErrorAction SilentlyContinue).Count -gt 0
    if (-not $hasGpu) {
        Write-Host ""
        Write-Host "NOTE: requested -Backend $Backend but the upstream CrispEmbed" -ForegroundColor Yellow
        Write-Host "      prebuilt has no ggml-$Backend library. The" -ForegroundColor Yellow
        Write-Host "      app will run, but inference will fall back to CPU." -ForegroundColor Yellow
        Write-Host "      For real GPU acceleration, build CrispEmbed from source:" -ForegroundColor Yellow
        Write-Host "        cd ..\\CrispEmbed && build-cuda.bat" -ForegroundColor Yellow
        Write-Host "      Custom builds require a separate manual build workflow." -ForegroundColor Yellow
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
