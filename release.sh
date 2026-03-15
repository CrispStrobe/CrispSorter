#!/usr/bin/env bash
# CrispSorter GitHub Release Script (macOS)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# 1. Locate GitHub CLI
if ! command -v gh &>/dev/null; then
    echo "Error: GitHub CLI (gh) not found. Install it with: brew install gh" >&2
    exit 1
fi
echo "Using GitHub CLI at: $(command -v gh)"

# 2. Get version from package.json
VERSION="v$(python3 -c "import json,sys; print(json.load(open('$SCRIPT_DIR/package.json'))['version'])")"
echo "Releasing version: $VERSION"

# 3. Identify build artifacts
BUNDLE_DIR="$SCRIPT_DIR/src-tauri/target/release/bundle"
ARTIFACTS=()

# DMG
DMG=$(find "$BUNDLE_DIR/dmg" -name "*.dmg" 2>/dev/null | head -1)
if [[ -n "$DMG" ]]; then
    ARTIFACTS+=("$DMG")
fi

# .app.tar.gz (updater artifact)
TARBALL=$(find "$BUNDLE_DIR/macos" -name "*.app.tar.gz" 2>/dev/null | head -1)
if [[ -n "$TARBALL" ]]; then
    ARTIFACTS+=("$TARBALL")
fi

if [[ ${#ARTIFACTS[@]} -eq 0 ]]; then
    echo "Error: No build artifacts found in $BUNDLE_DIR. Did you run 'npm run tauri build'?" >&2
    exit 1
fi

echo "Artifacts found:"
for f in "${ARTIFACTS[@]}"; do echo "  - $f"; done

# 4. Create/update GitHub release and upload
echo "Releasing $VERSION to GitHub..."
gh release create "$VERSION" --title "CrispSorter $VERSION" \
    --notes "Automated release for version $VERSION" 2>/dev/null || true

echo "Uploading artifacts..."
gh release upload "$VERSION" "${ARTIFACTS[@]}" --clobber

echo "Release $VERSION successfully updated with artifacts!"
