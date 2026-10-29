#!/usr/bin/env bash
# Sign the finished app, notarize/staple it, then create and notarize its DMG.
# All credentials must already be installed/provided; never creates certificates.
set -euo pipefail
if [[ $# -ne 2 ]]; then
  echo 'usage: sign_macos_release.sh path/to/CrispSorter.app path/to/output.dmg' >&2
  exit 2
fi
APP="$1"
DMG="$2"
: "${DEVELOPER_ID_SHA1:?Set DEVELOPER_ID_SHA1 to the Developer ID Application identity SHA-1}"
: "${APPLE_TEAM_ID:?Set APPLE_TEAM_ID}"
: "${APPLE_NOTARY_KEY_PATH:?Set APPLE_NOTARY_KEY_PATH to the App Store Connect API key file}"
: "${APPLE_NOTARY_KEY_ID:?Set APPLE_NOTARY_KEY_ID}"
: "${APPLE_NOTARY_ISSUER_ID:?Set APPLE_NOTARY_ISSUER_ID}"
[[ -d "$APP" && "$APP" == *.app && -f "$APPLE_NOTARY_KEY_PATH" && "$DMG" == *.dmg ]] || { echo 'Invalid app, DMG, or notary key path' >&2; exit 1; }
[[ "$DEVELOPER_ID_SHA1" =~ ^[0-9A-Fa-f]{40}$ && "$APPLE_TEAM_ID" =~ ^[A-Z0-9]{10}$ ]] || { echo 'Invalid identity fingerprint or team ID' >&2; exit 1; }
identity=$(security find-identity -v -p codesigning | grep -iF "$DEVELOPER_ID_SHA1" || true)
[[ "$identity" == *'Developer ID Application:'* && "$identity" == *"($APPLE_TEAM_ID)"* ]] || { echo 'Developer ID Application identity with private key for this team was not found' >&2; exit 1; }
work=$(mktemp -d "${TMPDIR:-/tmp}/crispsorter-notary.XXXXXXXX")
trap 'rm -rf "$work"' EXIT

# Sign Mach-O code inside-out, followed by nested bundles and the app itself.
# Avoid --deep for signing: it applies one set of options indiscriminately.
while IFS= read -r -d '' item; do
  if file -b "$item" | grep -q 'Mach-O'; then
    codesign --force --timestamp --options runtime --sign "$DEVELOPER_ID_SHA1" "$item"
  fi
done < <(find "$APP/Contents" -type f -print0)
while IFS= read -r -d '' item; do
  codesign --force --timestamp --options runtime --preserve-metadata=entitlements --sign "$DEVELOPER_ID_SHA1" "$item"
done < <(find "$APP/Contents" -depth -type d \( -name '*.framework' -o -name '*.app' -o -name '*.xpc' -o -name '*.appex' \) -print0)
codesign --force --timestamp --options runtime --sign "$DEVELOPER_ID_SHA1" "$APP"
codesign --verify --deep --strict --verbose=2 "$APP"
details=$(codesign -dv --verbose=4 "$APP" 2>&1)
[[ "$details" == *"TeamIdentifier=$APPLE_TEAM_ID"* && "$details" == *'Authority=Developer ID Application:'* && "$details" == *'Timestamp='* && "$details" == *'runtime'* ]] || { echo 'App signature lacks required identity, timestamp, or hardened runtime' >&2; exit 1; }

notarize() {
  local artifact="$1" result="$work/result.json" id status
  xcrun notarytool submit "$artifact" --key "$APPLE_NOTARY_KEY_PATH" --key-id "$APPLE_NOTARY_KEY_ID" --issuer "$APPLE_NOTARY_ISSUER_ID" --wait --output-format json > "$result"
  id=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1])).get("id", ""))' "$result")
  status=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1])).get("status", ""))' "$result")
  if [[ "$status" != Accepted || -z "$id" ]]; then
    echo "Notarization failed for $artifact (status: $status, id: $id)" >&2
    [[ -z "$id" ]] || xcrun notarytool log "$id" --key "$APPLE_NOTARY_KEY_PATH" --key-id "$APPLE_NOTARY_KEY_ID" --issuer "$APPLE_NOTARY_ISSUER_ID" >&2 || true
    exit 1
  fi
  echo "Notarization accepted: $artifact ($id)"
}

ditto -c -k --keepParent "$APP" "$work/app.zip"
notarize "$work/app.zip"
xcrun stapler staple "$APP"
xcrun stapler validate "$APP"
codesign --verify --deep --strict --verbose=2 "$APP"
spctl --assess --type execute --verbose=2 "$APP"
mkdir -p "$(dirname "$DMG")"
# Build in a temporary directory so a failure cannot leave a valid-looking DMG.
hdiutil create -volname "$(basename "$APP" .app)" -srcfolder "$APP" -format UDZO "$work/release.dmg" >/dev/null
codesign --force --timestamp --sign "$DEVELOPER_ID_SHA1" "$work/release.dmg"
codesign --verify --verbose=2 "$work/release.dmg"
notarize "$work/release.dmg"
xcrun stapler staple "$work/release.dmg"
xcrun stapler validate "$work/release.dmg"
cp "$work/release.dmg" "$DMG"
echo "Signed and notarized app and DMG: $APP, $DMG"
