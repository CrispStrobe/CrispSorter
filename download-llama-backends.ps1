# Downloads CUDA and Vulkan backend DLLs for the bundled llama-server (build b8340)
# into src-tauri/bin/ so Tauri bundles them with the release installer.
#
# Run once from the project root:
#   .\download-llama-backends.ps1
#
# For CUDA 12.4 (older cards):
#   .\download-llama-backends.ps1 -CudaVariant cuda-12.4

param(
    [string]$BuildTag    = "b8340",
    [string]$CudaVariant = "cuda-13.1"
)

$BinDir  = Join-Path $PSScriptRoot "src-tauri\bin"
$TempDir = Join-Path $env:TEMP "llama-backends-$BuildTag"
$BaseUrl = "https://github.com/ggml-org/llama.cpp/releases/download/$BuildTag"

New-Item -ItemType Directory -Force -Path $TempDir | Out-Null
New-Item -ItemType Directory -Force -Path $BinDir  | Out-Null

function Get-And-Extract {
    param([string]$FileName, [string]$ExtractTo)
    $ZipPath = Join-Path $TempDir $FileName
    $Url     = "$BaseUrl/$FileName"
    if (-not (Test-Path $ZipPath)) {
        Write-Host "  Downloading $FileName ..." -ForegroundColor Cyan
        Invoke-WebRequest -Uri $Url -OutFile $ZipPath -UseBasicParsing
    } else {
        Write-Host "  Using cached $FileName" -ForegroundColor DarkGray
    }
    Write-Host "  Extracting ..." -ForegroundColor Cyan
    Expand-Archive -Path $ZipPath -DestinationPath $ExtractTo -Force
}

function Copy-Dlls {
    param([string]$FromDir, [string]$Filter)
    $found = Get-ChildItem -Path $FromDir -Filter $Filter -Recurse -ErrorAction SilentlyContinue
    foreach ($f in $found) {
        Copy-Item $f.FullName $BinDir -Force
        Write-Host "  [+] $($f.Name)" -ForegroundColor Green
    }
    return $found.Count
}

# 1. Vulkan backend
Write-Host "`n[1/3] Vulkan backend (ggml-vulkan.dll)" -ForegroundColor Yellow
$VulkanDir = Join-Path $TempDir "vulkan"
Get-And-Extract "llama-$BuildTag-bin-win-vulkan-x64.zip" $VulkanDir
$n = Copy-Dlls $VulkanDir "ggml-vulkan*.dll"
if ($n -eq 0) { Write-Warning "No ggml-vulkan*.dll found - check archive contents." }

# 2. CUDA backend
Write-Host "`n[2/3] CUDA backend ($CudaVariant)" -ForegroundColor Yellow
$CudaDir = Join-Path $TempDir "cuda"
Get-And-Extract "llama-$BuildTag-bin-win-$CudaVariant-x64.zip" $CudaDir
$n = Copy-Dlls $CudaDir "ggml-cuda*.dll"
if ($n -eq 0) { Write-Warning "No ggml-cuda*.dll found - check archive contents." }

# 3. CUDA runtime DLLs (cublas, cudart, etc.)
Write-Host "`n[3/3] CUDA runtime DLLs" -ForegroundColor Yellow
$CudaRtDir = Join-Path $TempDir "cudart"
Get-And-Extract "cudart-llama-bin-win-$CudaVariant-x64.zip" $CudaRtDir
Copy-Dlls $CudaRtDir "*.dll" | Out-Null

Write-Host "`nDone! DLLs are in: $BinDir" -ForegroundColor Green
Write-Host 'They will be bundled via tauri.conf.json resources: ["bin/*.dll"]' -ForegroundColor DarkGray

Remove-Item $TempDir -Recurse -Force -ErrorAction SilentlyContinue
