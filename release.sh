#!/usr/bin/env bash
# CrispSorter GitHub Release Script (macOS)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# target/ moved to the workspace root with the crisp-index-server
# integration (commit 7326771); fall back to the legacy path so this
# script still works on branches that haven't picked it up.
if [[ -d "$SCRIPT_DIR/target/release/bundle" ]]; then
    BUNDLE_DIR="$SCRIPT_DIR/target/release/bundle"
else
    BUNDLE_DIR="$SCRIPT_DIR/src-tauri/target/release/bundle"
fi

# 1. Locate GitHub CLI
if ! command -v gh &>/dev/null; then
    echo "Error: GitHub CLI (gh) not found. Install it with: brew install gh" >&2
    exit 1
fi
echo "Using GitHub CLI at: $(command -v gh)"

# 2. Get version from package.json
RAW_VERSION="$(python3 -c "import json,sys; print(json.load(open('$SCRIPT_DIR/package.json'))['version'])")"
VERSION="v${RAW_VERSION}"
echo "Releasing version: $VERSION"

# 3. Build (tauri build produces .app but its DMG step is broken on newer macOS —
#    we rebuild the DMG ourselves with create-dmg below)
echo "Building..."
cd "$SCRIPT_DIR"
npm run tauri build -- --bundles app 2>&1 || true   # build .app only, skip broken DMG bundler

# 4. Bundle native libs into the .app
APP="$BUNDLE_DIR/macos/CrispSorter.app"
echo "Bundling native libs..."
"$SCRIPT_DIR/scripts/bundle_macos_native_libs.sh" "$APP"

# 5. Build DMG with create-dmg
APP="$BUNDLE_DIR/macos/CrispSorter.app"
DMG_DIR="$BUNDLE_DIR/dmg"
DMG_NAME="CrispSorter_${RAW_VERSION}_aarch64.dmg"
DMG="$DMG_DIR/$DMG_NAME"
ICON="$APP/Contents/Resources/icon.icns"

if [[ ! -d "$APP" ]]; then
    echo "Error: .app not found at $APP" >&2
    exit 1
fi
if ! command -v create-dmg &>/dev/null; then
    echo "Error: create-dmg not found. Install it with: brew install create-dmg" >&2
    exit 1
fi

echo "Building DMG..."
mkdir -p "$DMG_DIR"
rm -f "$DMG"
create-dmg \
    --volname "CrispSorter" \
    --volicon "$ICON" \
    --window-pos 200 120 \
    --window-size 600 400 \
    --icon-size 128 \
    --icon "CrispSorter.app" 150 190 \
    --hide-extension "CrispSorter.app" \
    --app-drop-link 450 190 \
    "$DMG" \
    "$APP"
echo "DMG built: $DMG"

# 6. Collect artifacts
ARTIFACTS=("$DMG")

# .app.tar.gz (updater artifact, if present)
TARBALL=$(find "$BUNDLE_DIR/macos" -name "*.app.tar.gz" 2>/dev/null | head -1)
if [[ -n "$TARBALL" ]]; then
    ARTIFACTS+=("$TARBALL")
fi

echo "Artifacts:"
for f in "${ARTIFACTS[@]}"; do echo "  - $f"; done

# 7. Detect repo from git remote
REPO=$(git -C "$SCRIPT_DIR" remote get-url origin | sed 's|.*github.com[:/]\(.*\)\.git|\1|; s|.*github.com[:/]\(.*\)|\1|')
echo "Target repo: $REPO"

# 8. Create/update GitHub release and upload
echo "Releasing $VERSION to GitHub..."
gh release create "$VERSION" --repo "$REPO" --title "CrispSorter $VERSION" \
    --notes "Automated release for version $VERSION" 2>/dev/null || true

echo "Uploading artifacts..."
gh release upload "$VERSION" "${ARTIFACTS[@]}" --repo "$REPO" --clobber

echo "Release $VERSION successfully updated with artifacts!"
