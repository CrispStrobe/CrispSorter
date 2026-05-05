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

# 5. Ensure `protoc` is available before cargo runs.
#
# `lance-encoding` (transitive via lancedb) calls `prost-build`, which
# spawns `protoc` AND requires the well-known `.proto` files
# (`google/protobuf/empty.proto` etc.) that ship in protoc's `include/`
# directory. Each build script runs in its own process, so a `PROTOC`
# env var set inside our own `build.rs` doesn't propagate — the only
# reliable fix is to have `protoc.exe` on PATH BEFORE cargo starts.
#
# Install layout matches the upstream protoc release:
#     gh_temp\protoc\bin\protoc.exe
#     gh_temp\protoc\include\google\protobuf\*.proto
#
# protoc finds its bundled `include/` automatically by looking at
# `<exe-dir>\..\include\` — that's why we keep them next to each other.
$ProtocRoot      = Join-Path $ProjectRoot "gh_temp\protoc"
$ProtocBinDir    = Join-Path $ProtocRoot  "bin"
$ProtocCachedExe = Join-Path $ProtocBinDir "protoc.exe"
$ProtocIncludeOk = Test-Path (Join-Path $ProtocRoot "include\google\protobuf\empty.proto")
$ProtocOnPath    = Get-Command protoc -ErrorAction SilentlyContinue
if (-not $ProtocOnPath -and (-not (Test-Path $ProtocCachedExe) -or -not $ProtocIncludeOk)) {
    Write-Host "Bootstrapping protoc into gh_temp\protoc (one-time download) ..." -ForegroundColor Yellow
    try {
        $ProtocVersion = "29.0"
        $ProtocAsset   = "protoc-$ProtocVersion-win64.zip"
        $ProtocUrl     = "https://github.com/protocolbuffers/protobuf/releases/download/v$ProtocVersion/$ProtocAsset"
        if (Test-Path $ProtocRoot) { Remove-Item -Recurse -Force $ProtocRoot }
        New-Item -ItemType Directory -Force -Path $ProtocRoot | Out-Null
        $ZipPath = Join-Path $ProtocRoot $ProtocAsset
        Invoke-WebRequest -Uri $ProtocUrl -OutFile $ZipPath -UseBasicParsing
        # Extract directly so bin/ and include/ end up at the right
        # protoc-relative-paths layout.
        Expand-Archive -Path $ZipPath -DestinationPath $ProtocRoot -Force
        Remove-Item -Force $ZipPath
        Write-Host "Installed protoc $ProtocVersion at $ProtocBinDir" -ForegroundColor Green
    } catch {
        Write-Host "WARNING: failed to download protoc — lance-encoding compile will fail. Install protoc manually and re-run." -ForegroundColor Red
        Write-Host "Error: $_" -ForegroundColor Red
    }
}
if (Test-Path $ProtocCachedExe) {
    $env:PATH = "$ProtocBinDir;" + $env:PATH
    $env:PROTOC = $ProtocCachedExe
}

# Final Verification
$CargoPath = (Get-Command cargo -ErrorAction SilentlyContinue).Source
Write-Host "Active Cargo: $CargoPath" -ForegroundColor Yellow
Write-Host "Environment configured successfully.`n" -ForegroundColor Green
