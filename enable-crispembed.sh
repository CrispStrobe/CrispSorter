#!/usr/bin/env bash
# enable-crispembed.sh -- macOS / Linux equivalent of enable-crispembed.ps1.
#
# Usage:
#   ./enable-crispembed.sh                   # dev mode
#   ./enable-crispembed.sh build             # production build
#   ./enable-crispembed.sh dev --backend cuda
#   ./enable-crispembed.sh dev --skip-download

set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MODE="dev"
BACKEND=""
SKIP_DOWNLOAD=0
DRY_RUN=0

# Parse args
if [[ "${1:-}" == "build" ]]; then MODE="build"; shift
elif [[ "${1:-}" == "dev" ]]; then shift; fi
while [[ $# -gt 0 ]]; do
    case "$1" in
        --backend) BACKEND="$2"; shift 2;;
        --skip-download) SKIP_DOWNLOAD=1; shift;;
        --dry-run) DRY_RUN=1; shift;;
        *) echo "Unknown arg: $1" >&2; exit 1;;
    esac
done

# Default backend per platform
OS="$(uname -s)"
if [[ -z "$BACKEND" ]]; then
    if [[ "$OS" == "Darwin" ]]; then BACKEND="metal"
    else BACKEND="vulkan"; fi
fi
CARGO_FEATURE="crispembed-$BACKEND"
[[ "$BACKEND" == "cpu" ]] && CARGO_FEATURE="crispembed"

echo "=== Enable CrispEmbed (GGUF) -- backend: $BACKEND ==="

# 1. Ensure CrispEmbed source repo exists at ../CrispEmbed
SIBLING="$PROJECT_ROOT/../CrispEmbed"
if [[ ! -f "$SIBLING/crispembed/Cargo.toml" ]]; then
    echo "CrispEmbed sibling repo not found at $SIBLING -- cloning..."
    (cd "$PROJECT_ROOT/.." && git clone https://github.com/CrispStrobe/CrispEmbed.git)
fi
echo "CrispEmbed source: $SIBLING"

# 2. Download prebuilt lib if needed
PREBUILT_DIR="$PROJECT_ROOT/src-tauri/crispembed-prebuilt"
if [[ "$SKIP_DOWNLOAD" -eq 1 && (-f "$PREBUILT_DIR/libcrispembed.dylib" || -f "$PREBUILT_DIR/libcrispembed.so") ]]; then
    echo "Reusing existing prebuilt at $PREBUILT_DIR"
else
    mkdir -p "$PREBUILT_DIR"
    if [[ "$OS" == "Darwin" ]]; then
        ASSET="crispembed-macos-arm64.tar.gz"
    elif [[ "$(uname -m)" == "aarch64" || "$(uname -m)" == "arm64" ]]; then
        ASSET="crispembed-linux-arm64.tar.gz"
    else
        ASSET="crispembed-linux-x86_64.tar.gz"
    fi
    echo "Downloading $ASSET from CrispEmbed latest release..."
    (cd "$SIBLING" && gh release download --pattern "$ASSET" --dir "$PREBUILT_DIR" --clobber)
    tar -xzf "$PREBUILT_DIR/$ASSET" -C "$PREBUILT_DIR"
    rm -f "$PREBUILT_DIR/$ASSET"
fi

# 3. Wire env var
export CRISPEMBED_SYS_LIB_DIR="$PREBUILT_DIR"
echo "CRISPEMBED_SYS_LIB_DIR = $CRISPEMBED_SYS_LIB_DIR"

# 3a. Stage runtime libs next to the .exe and into src-tauri/bin/ (which
#     Tauri bundles into the installer per tauri.conf.json).
SHARED_LIBS=()
shopt -s nullglob
SHARED_LIBS+=("$PREBUILT_DIR"/*.dylib "$PREBUILT_DIR"/*.so "$PREBUILT_DIR"/*.so.* "$PREBUILT_DIR"/*.dll)
shopt -u nullglob
if [[ ${#SHARED_LIBS[@]} -gt 0 ]]; then
    for d in "$PROJECT_ROOT/src-tauri/target/debug" \
             "$PROJECT_ROOT/src-tauri/target/release" \
             "$PROJECT_ROOT/src-tauri/bin"; do
        mkdir -p "$d"
        for lib in "${SHARED_LIBS[@]}"; do
            cp -f "$lib" "$d/"
        done
    done
    echo "Copied ${#SHARED_LIBS[@]} runtime lib(s) to target/{debug,release}/ and src-tauri/bin/"
fi

# 3b. Warn when GPU backend was requested but the staged tarball is CPU-only.
if [[ "$BACKEND" == "cuda" || "$BACKEND" == "vulkan" || "$BACKEND" == "metal" ]]; then
    if ! ls "$PREBUILT_DIR"/*ggml-${BACKEND}* >/dev/null 2>&1; then
        echo ""
        echo "NOTE: requested --backend $BACKEND but the upstream CrispEmbed"
        echo "      prebuilt is CPU-only (no ggml-${BACKEND} library). The app"
        echo "      will run, but inference will fall back to CPU."
        echo "      For real GPU acceleration, build CrispEmbed from source."
        echo ""
    fi
fi

if [[ "$DRY_RUN" -eq 1 ]]; then
    echo "DryRun: stopping before npm. Resolved feature: $CARGO_FEATURE"
    exit 0
fi

# 4. Run dev or build
if [[ "$MODE" == "dev" ]]; then
    echo "Starting CrispSorter in dev mode with --features $CARGO_FEATURE..."
    npm run tauri -- dev --features "$CARGO_FEATURE"
else
    echo "Building CrispSorter (production) with --features $CARGO_FEATURE..."
    npm run tauri -- build --features "$CARGO_FEATURE"
fi
