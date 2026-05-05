#!/usr/bin/env bash
#
# Configure + build CrispASR's libcrispasr.dylib (with Metal acceleration
# on macOS) into a stable per-arch directory under the sibling CrispASR
# checkout. CrispSorter's Tauri bundle script
# (`bundle_macos_native_libs.sh`) reads the resulting libs from the
# same path; crispasr-sys's build.rs auto-discovers the same `build*`
# directories without any env-var plumbing.
#
# Mirrors CrisperWeaver's `scripts/build_macos.sh` but without the
# Flutter-specific bundling — that part lives in the post-build script.
#
# Usage:
#   scripts/build_crispasr_macos.sh
#
# Env (all optional):
#   CRISPASR_DIR          path to sibling CrispASR repo
#                         (default: ../CrispASR resolved from this script)
#   CRISPASR_BUILD_SUBDIR cmake binary dir inside CRISPASR_DIR
#                         (default: build-flutter-bundle — picked to
#                         match CrisperWeaver so an existing build is
#                         reused. Override with build-metal/build-vulkan
#                         when isolating per-platform.)
#   CMAKE_JOBS            parallelism (default: sysctl hw.logicalcpu)

set -euo pipefail

CRISPASR_DIR="${CRISPASR_DIR:-$(cd "$(dirname "$0")/../.." && pwd)/CrispASR}"
CRISPASR_BUILD_SUBDIR="${CRISPASR_BUILD_SUBDIR:-build-flutter-bundle}"
BUILDDIR="$CRISPASR_DIR/$CRISPASR_BUILD_SUBDIR"
JOBS="${CMAKE_JOBS:-$(sysctl -n hw.logicalcpu)}"

if [[ ! -d "$CRISPASR_DIR" ]]; then
  echo "error: sibling CrispASR repo not found at $CRISPASR_DIR" >&2
  echo "       Set CRISPASR_DIR or check out the repo alongside CrispSorter:" >&2
  echo "         git clone https://github.com/CrispStrobe/CrispASR ../CrispASR" >&2
  exit 2
fi

if [[ ! -f "$BUILDDIR/CMakeCache.txt" ]]; then
  echo "==> cmake configure → $BUILDDIR"
  cmake -S "$CRISPASR_DIR" -B "$BUILDDIR" \
    -DCMAKE_BUILD_TYPE=Release \
    -DBUILD_SHARED_LIBS=ON \
    -DGGML_METAL=ON \
    -DGGML_METAL_EMBED_LIBRARY=ON \
    -DCRISPASR_BUILD_TESTS=OFF \
    -DCRISPASR_BUILD_EXAMPLES=OFF \
    -DCRISPASR_BUILD_SERVER=OFF
else
  echo "==> reusing existing cmake config at $BUILDDIR"
fi

echo "==> cmake build (jobs=$JOBS) → libcrispasr.dylib + ggml backends"
# Build the `crispasr` shared lib target. ggml/backend libs (libggml*,
# ggml-metal, ggml-blas, ggml-cpu) get pulled in transitively because
# crispasr depends on them.
cmake --build "$BUILDDIR" --config Release -j "$JOBS" --target crispasr

echo
echo "Built libs under $BUILDDIR/src:"
ls -l "$BUILDDIR/src" | grep -E "lib(crispasr|whisper)\." | awk '{print "  " $NF}'

echo
echo "Built backends under $BUILDDIR/ggml/src:"
find "$BUILDDIR/ggml/src" -name "libggml*.dylib" -type f 2>/dev/null \
  | sed 's|^|  |'
