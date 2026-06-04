#!/bin/bash
# Bundle CrispASR + CrispEmbed native .so files into the Tauri Android project.
#
# Prerequisites:
#   - CrispASR built for Android: ../CrispASR/build-android/arm64-v8a/libcrispasr.so
#   - CrispEmbed built for Android: ../CrispEmbed/build-android/arm64-v8a/libcrispembed.so
#
# Usage:
#   ./scripts/bundle_android_native_libs.sh
#   ./scripts/bundle_android_native_libs.sh --abi arm64-v8a    (default)
#   ./scripts/bundle_android_native_libs.sh --abi armeabi-v7a  (32-bit ARM)
#
# The .so files are copied into src-tauri/gen/android/app/src/main/jniLibs/<abi>/
# where Gradle automatically bundles them into the APK.

set -e
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
ABI="arm64-v8a"

while [[ "$#" -gt 0 ]]; do
    case $1 in
        --abi) ABI="$2"; shift ;;
        --help) echo "Usage: $0 [--abi arm64-v8a|armeabi-v7a|x86_64]"; exit 0 ;;
    esac
    shift
done

JNILIBS="$PROJECT_DIR/src-tauri/gen/android/app/src/main/jniLibs/$ABI"
mkdir -p "$JNILIBS"

CRISPASR_DIR="${CRISPASR_BUILD_DIR:-$PROJECT_DIR/../CrispASR/build-android/$ABI}"
CRISPEMBED_DIR="${CRISPEMBED_BUILD_DIR:-$PROJECT_DIR/../CrispEmbed/build-android/$ABI}"

FOUND=0

# CrispASR
if [ -f "$CRISPASR_DIR/libcrispasr.so" ]; then
    cp "$CRISPASR_DIR/libcrispasr.so" "$JNILIBS/"
    echo "✓ Bundled libcrispasr.so ($(du -h "$JNILIBS/libcrispasr.so" | cut -f1))"
    FOUND=$((FOUND + 1))
    # Also copy GGML backend libs if present
    for lib in "$CRISPASR_DIR"/libggml*.so; do
        [ -f "$lib" ] && cp "$lib" "$JNILIBS/" && echo "  + $(basename "$lib")"
    done
else
    echo "⚠ libcrispasr.so not found at $CRISPASR_DIR"
    echo "  Run: cd ../CrispASR && ./build-android.sh --abi $ABI"
fi

# CrispEmbed
if [ -f "$CRISPEMBED_DIR/libcrispembed.so" ]; then
    cp "$CRISPEMBED_DIR/libcrispembed.so" "$JNILIBS/"
    echo "✓ Bundled libcrispembed.so ($(du -h "$JNILIBS/libcrispembed.so" | cut -f1))"
    FOUND=$((FOUND + 1))
    # Also copy GGML backend libs if present (avoid duplicates from CrispASR)
    for lib in "$CRISPEMBED_DIR"/libggml*.so; do
        [ -f "$lib" ] && [ ! -f "$JNILIBS/$(basename "$lib")" ] && cp "$lib" "$JNILIBS/" && echo "  + $(basename "$lib")"
    done
else
    echo "⚠ libcrispembed.so not found at $CRISPEMBED_DIR"
    echo "  Run: cd ../CrispEmbed && ./build-android.sh --abi $ABI"
fi

echo ""
echo "=== jniLibs/$ABI contents ==="
ls -lh "$JNILIBS/"*.so 2>/dev/null || echo "(empty)"
echo ""
if [ $FOUND -eq 0 ]; then
    echo "No native libs bundled. The app will work but without on-device ASR/embedding."
    echo "Cloud LLM providers and cb-api server fallback still function."
fi
