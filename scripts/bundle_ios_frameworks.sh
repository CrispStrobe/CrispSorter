#!/bin/bash
# Bundle CrispASR + CrispEmbed xcframeworks into the Tauri iOS project.
#
# Prerequisites:
#   - CrispASR built for iOS: ../CrispASR/build-ios/CrispASR.xcframework
#   - CrispEmbed built for iOS: ../CrispEmbed/build-ios/CrispEmbed.xcframework
#
# Usage:
#   ./scripts/bundle_ios_frameworks.sh
#
# The xcframeworks are copied into src-tauri/gen/apple/Frameworks/ and
# need to be referenced in the Xcode project (either manually or via
# a post-build script that adds them to the "Embed Frameworks" phase).

set -e
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

FRAMEWORKS="$PROJECT_DIR/src-tauri/gen/apple/Frameworks"
mkdir -p "$FRAMEWORKS"

CRISPASR_FW="${CRISPASR_FRAMEWORK:-$PROJECT_DIR/../CrispASR/build-ios/CrispASR.xcframework}"
CRISPEMBED_FW="${CRISPEMBED_FRAMEWORK:-$PROJECT_DIR/../CrispEmbed/build-ios/CrispEmbed.xcframework}"

FOUND=0

# CrispASR
if [ -d "$CRISPASR_FW" ]; then
    rm -rf "$FRAMEWORKS/CrispASR.xcframework"
    cp -R "$CRISPASR_FW" "$FRAMEWORKS/"
    echo "✓ Bundled CrispASR.xcframework"
    FOUND=$((FOUND + 1))
else
    echo "⚠ CrispASR.xcframework not found at $CRISPASR_FW"
    echo "  Run: cd ../CrispASR && ./build-ios.sh"
fi

# CrispEmbed
if [ -d "$CRISPEMBED_FW" ]; then
    rm -rf "$FRAMEWORKS/CrispEmbed.xcframework"
    cp -R "$CRISPEMBED_FW" "$FRAMEWORKS/"
    echo "✓ Bundled CrispEmbed.xcframework"
    FOUND=$((FOUND + 1))
else
    echo "⚠ CrispEmbed.xcframework not found at $CRISPEMBED_FW"
    echo "  Run: cd ../CrispEmbed && ./build-ios.sh"
fi

echo ""
echo "=== Frameworks/ contents ==="
ls -d "$FRAMEWORKS"/*.xcframework 2>/dev/null || echo "(empty)"
echo ""
if [ $FOUND -eq 0 ]; then
    echo "No frameworks bundled. The app will work but without on-device ASR/embedding."
fi

if [ $FOUND -gt 0 ]; then
    echo ""
    echo "Next steps:"
    echo "  1. Open src-tauri/gen/apple/ in Xcode"
    echo "  2. Add the xcframeworks to the target's 'Frameworks, Libraries, and Embedded Content'"
    echo "  3. Set 'Embed & Sign' for each framework"
    echo "  Or use CrisperWeaver's wire_ios_xcframework.rb script to automate this."
fi
