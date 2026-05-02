#!/usr/bin/env bash
#
# Copy CrispASR's libcrispasr.dylib (+ ggml backend libs) into a built
# crispsorter.app bundle so dyld can resolve `@rpath/libcrispasr.1.dylib`
# at launch. Adapted from CrisperWeaver's `scripts/bundle_macos_dylibs.sh`
# for Tauri's `.app/Contents/Frameworks/` layout.
#
# Adds (mirroring CrisperWeaver):
#   * libcrispasr.dylib + the SOVERSION-1 alias the binary actually links
#     against (`libcrispasr.1.dylib`)
#   * `libwhisper.dylib` symlink — kept because libcrispasr's own LC_ID
#     historically used the whisper alias
#   * libggml*.dylib (recursive find under <build>/ggml/src/) — backend
#     subdirs (ggml-metal, ggml-blas, ggml-cpu, …) all get flattened
#     into Frameworks/
#   * Homebrew transitives that libcrispasr happens to link absolute
#     (kokoro pulls espeak-ng on macOS) — install_name in libcrispasr
#     is rewritten to @rpath/<basename> after copying so the bundled
#     copy wins over the missing absolute path on the user's machine
#
# Re-codesigns the .app ad-hoc at the end so Gatekeeper accepts the
# modified bundle locally. CI release builds get a real Developer ID
# signature later in the workflow if signing creds are configured.
#
# Usage:
#   scripts/bundle_macos_native_libs.sh [path/to/.app]
#
# Env:
#   CRISPASR_BUILD_DIR   path to the cmake build dir produced by
#                        `scripts/build_crispasr_macos.sh` or by hand
#                        (default: ../CrispASR/build-flutter-bundle —
#                        matches CrisperWeaver's convention so a dev who
#                        already built libs for that project gets a
#                        free reuse).
#
# Default app path is autodiscovered:
#   src-tauri/target/(aarch64|x86_64)-apple-darwin/release/bundle/macos/*.app
#   src-tauri/target/release/bundle/macos/*.app
#   src-tauri/target/debug/bundle/macos/*.app

set -euo pipefail

APP="${1:-}"
if [[ -z "$APP" ]]; then
  for cand in \
    src-tauri/target/aarch64-apple-darwin/release/bundle/macos/*.app \
    src-tauri/target/x86_64-apple-darwin/release/bundle/macos/*.app \
    src-tauri/target/release/bundle/macos/*.app \
    src-tauri/target/debug/bundle/macos/*.app
  do
    if [[ -d "$cand" ]]; then APP="$cand"; break; fi
  done
fi
if [[ -z "$APP" || ! -d "$APP" ]]; then
  echo "error: .app bundle not found. Pass it explicitly:" >&2
  echo "  $0 path/to/MyApp.app" >&2
  exit 2
fi

CRISPASR_BUILD_DIR="${CRISPASR_BUILD_DIR:-$(cd "$(dirname "$0")/../.." && pwd)/CrispASR/build-flutter-bundle}"
SRCDIR="$CRISPASR_BUILD_DIR/src"
GGMLDIR="$CRISPASR_BUILD_DIR/ggml/src"

if [[ ! -d "$SRCDIR" ]]; then
  echo "error: CrispASR build tree not found at $SRCDIR" >&2
  echo "       Set CRISPASR_BUILD_DIR or run scripts/build_crispasr_macos.sh first." >&2
  exit 3
fi

FRAMEWORKS="$APP/Contents/Frameworks"
mkdir -p "$FRAMEWORKS"

# Wipe any previous bundle so stale per-backend dylibs from earlier runs
# don't linger across rebuilds. Be careful to only remove things we
# actually added (lib*.dylib).
rm -f "$FRAMEWORKS"/lib*.dylib

# ── Core: libcrispasr ────────────────────────────────────────────────────
#
# CMake produces libcrispasr.{SOVERSION_FULL}.dylib plus two symlinks
# (libcrispasr.dylib, libcrispasr.{SOVERSION_MAJOR}.dylib). We copy the
# concrete versioned file and recreate both symlinks inside Frameworks/
# so:
#   * `@rpath/libcrispasr.1.dylib` (the SONAME the binary records via
#     LC_LOAD_DYLIB) resolves
#   * any consumer reaching for the unversioned name also finds it
VERSIONED=""
for pattern in 'libcrispasr.[0-9]*.dylib' 'libwhisper.[0-9]*.dylib'; do
  found="$(find "$SRCDIR" -maxdepth 1 -type f -name "$pattern" 2>/dev/null | sort | head -1)"
  if [[ -n "$found" ]]; then VERSIONED="$found"; break; fi
done
if [[ -z "$VERSIONED" ]]; then
  for cand in "$SRCDIR/libcrispasr.dylib" "$SRCDIR/libwhisper.dylib"; do
    if [[ -f "$cand" || -L "$cand" ]]; then VERSIONED="$cand"; break; fi
  done
fi
if [[ -z "$VERSIONED" ]]; then
  echo "error: libcrispasr / libwhisper dylib not found under $SRCDIR" >&2
  exit 4
fi
cp -L "$VERSIONED" "$FRAMEWORKS/libcrispasr.dylib"
ln -sf libcrispasr.dylib "$FRAMEWORKS/libwhisper.dylib"
ln -sf libcrispasr.dylib "$FRAMEWORKS/libcrispasr.1.dylib"

# ── ggml: every shared lib under <build>/ggml/src/, recursive ───────────
#
# Backend libs (ggml-metal, ggml-blas, ggml-cpu, …) live in their own
# subdirs (e.g. `ggml/src/ggml-metal/libggml-metal.dylib`). Tauri's
# Frameworks/ is flat, so we glob recursively and copy.
if [[ -d "$GGMLDIR" ]]; then
  while IFS= read -r f; do
    cp -L "$f" "$FRAMEWORKS/$(basename "$f")"
  done < <(find "$GGMLDIR" -name "libggml*.dylib" -type f 2>/dev/null)
  # Also pull in symlinks (libggml.0.dylib → libggml.0.10.0.dylib etc.)
  # so SONAME-based lookups resolve.
  while IFS= read -r f; do
    base="$(basename "$f")"
    [[ -e "$FRAMEWORKS/$base" ]] && continue
    target="$(basename "$(readlink "$f" 2>/dev/null || echo "$f")")"
    if [[ -f "$FRAMEWORKS/$target" ]]; then
      ln -sf "$target" "$FRAMEWORKS/$base"
    fi
  done < <(find "$GGMLDIR" -name "libggml*.dylib" -type l 2>/dev/null)
fi

# ── Homebrew transitives that libcrispasr links absolute ─────────────────
#
# Backends that pull in system libs via /opt/homebrew/... (kokoro →
# espeak-ng is the canonical one) need those copied next to libcrispasr
# AND the install_name in libcrispasr rewritten to @rpath/<basename> so
# the bundled copy wins over a missing /opt/homebrew/... on the user's
# machine.
external_deps_of() {
  otool -L "$1" 2>/dev/null \
    | awk 'NR>1 {print $1}' \
    | grep -E '^/(opt/homebrew|usr/local)/' || true
}

# Recursively process a lib: copy it next to libcrispasr, rewrite its
# own LC_ID_DYLIB to @rpath/<basename>, rewrite the loader's reference
# to the bundled name, then recurse into ITS transitive deps. Hash-set
# of already-processed basenames prevents loops + duplicate work.
declare -a processed=()
already_processed() {
  local needle="$1"
  # `${processed[@]+…}` expands to the array contents only when set;
  # otherwise to nothing — keeps `set -u` happy on the first call when
  # the array is empty.
  for p in ${processed[@]+"${processed[@]}"}; do
    [[ "$p" == "$needle" ]] && return 0
  done
  return 1
}
bundle_external_recursive() {
  local loader="$1"
  for dep in $(external_deps_of "$loader"); do
    local base
    base="$(basename "$dep")"
    if [[ -f "$dep" && ! -f "$FRAMEWORKS/$base" ]]; then
      cp -L "$dep" "$FRAMEWORKS/$base"
      chmod u+w "$FRAMEWORKS/$base"
      install_name_tool -id "@rpath/$base" "$FRAMEWORKS/$base" 2>/dev/null || true
    fi
    # Rewrite loader's LC_LOAD_DYLIB to point at the bundled copy.
    install_name_tool -change "$dep" "@rpath/$base" "$loader" 2>/dev/null || true
    # Recurse — but only if we haven't seen this lib yet.
    if ! already_processed "$base"; then
      processed+=("$base")
      [[ -f "$FRAMEWORKS/$base" ]] && bundle_external_recursive "$FRAMEWORKS/$base"
    fi
  done
}
bundle_external_recursive "$FRAMEWORKS/libcrispasr.dylib"

# ── libcrispasr's own LC_RPATH — make Frameworks/ the only search path ─
#
# When libcrispasr.dylib loads its OWN transitive deps via @rpath/...,
# dyld searches *libcrispasr's* LC_RPATH, not the binary's. CMake bakes
# in two classes of paths that have to go:
#
#   * absolute build-tree paths (`…/build-flutter-bundle/…`) — leak the
#     dev machine's filesystem into the shipped binary.
#   * absolute homebrew paths (`/opt/homebrew/…`, `/usr/local/…`) —
#     also dev-machine-specific; if dyld finds them first it loads the
#     unbundled copy (which then transitively pulls in *its own*
#     unbundled deps, and the user's machine without homebrew gets a
#     load failure).
#
# Strategy: delete every absolute rpath entry first, then add
# `@loader_path/.` (= libcrispasr's own directory = Contents/Frameworks)
# as the single, deterministic search location.
while IFS= read -r p; do
  install_name_tool -delete_rpath "$p" "$FRAMEWORKS/libcrispasr.dylib" 2>/dev/null || true
done < <(otool -l "$FRAMEWORKS/libcrispasr.dylib" \
  | awk '/cmd LC_RPATH/{getline;getline; print $2}' \
  | grep -E '^/' || true)
# `-add_rpath` on an existing path warns on stderr; idempotent enough.
install_name_tool -add_rpath "@loader_path/." \
  "$FRAMEWORKS/libcrispasr.dylib" 2>/dev/null || true

# ── Re-codesign ───────────────────────────────────────────────────────────
#
# Ad-hoc signing is enough for local testing — the binary launches and
# Gatekeeper grumbles only on first open (right-click → Open to bypass).
# CI release jobs that have a Developer ID configured should re-sign
# with that identity instead by passing CODESIGN_IDENTITY.
CODESIGN_IDENTITY="${CODESIGN_IDENTITY:-}"
if [[ -n "$CODESIGN_IDENTITY" ]]; then
  codesign --force --deep --options runtime --sign "$CODESIGN_IDENTITY" "$APP"
else
  codesign --force --deep --sign - "$APP" >/dev/null
fi

echo "Bundled into $APP/Contents/Frameworks:"
( cd "$FRAMEWORKS" && ls -l ./*.dylib 2>/dev/null ) | sed 's|^|  |'

# ── Optional: regenerate the .dmg next to the .app ──────────────────────
#
# Tauri's bundler runs `tauri build` → produces .app → packs .dmg in one
# pass, with no hook between the two. So if we want the published .dmg
# to actually contain the patched .app, we have to repack.
#
# Skip with REPACK_DMG=0; defaults to repacking only if the original
# .dmg is sitting alongside the .app (i.e. tauri produced both).
REPACK_DMG="${REPACK_DMG:-1}"
if [[ "$REPACK_DMG" != "0" ]]; then
  APP_DIR="$(dirname "$APP")"
  # Tauri puts .dmg under …/bundle/dmg/ (sibling of bundle/macos/).
  DMG_DIR="$(cd "$APP_DIR/.." && pwd)/dmg"
  if [[ -d "$DMG_DIR" ]]; then
    OLD_DMG="$(find "$DMG_DIR" -maxdepth 1 -name '*.dmg' -type f | head -1)"
    if [[ -n "$OLD_DMG" ]]; then
      DMG_BASENAME="$(basename "$OLD_DMG")"
      VOLNAME="$(basename "$APP" .app)"
      echo
      echo "Repacking dmg: $DMG_BASENAME (volname=$VOLNAME)"
      rm -f "$OLD_DMG"
      hdiutil create \
        -volname "$VOLNAME" \
        -srcfolder "$APP" \
        -ov \
        -format UDZO \
        "$DMG_DIR/$DMG_BASENAME" \
        >/dev/null
      echo "  → $DMG_DIR/$DMG_BASENAME"
    else
      echo
      echo "(no existing .dmg in $DMG_DIR — skipping repack)"
    fi
  fi
fi
