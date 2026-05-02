#!/usr/bin/env bash
#
# Copy CrispASR's libcrispasr.dylib and/or CrispEmbed's
# libcrispembed.dylib (+ ggml backend libs + homebrew transitives) into
# a built crispsorter.app bundle so dyld can resolve their @rpath/...
# references at launch.
#
# Adapted from CrisperWeaver's `scripts/bundle_macos_dylibs.sh` for
# Tauri's `.app/Contents/Frameworks/` layout, then generalised to
# handle the two sibling libs the project depends on optionally.
#
# Each wrapper lib is processed independently — if its build dir is
# absent, that wrapper is skipped silently. So a build with only
# `--features crispasr-metal` Just Works without touching CrispEmbed,
# and vice versa.
#
# Per wrapper, the script:
#   * Copies the main lib + recreates SOVERSION-1 / unversioned
#     symlinks (needed because the binary records @rpath/libfoo.1.dylib
#     via LC_LOAD_DYLIB).
#   * Recursively copies every libggml*.{dylib} found under either the
#     CrispASR-style `<build>/ggml/src/` or the CrispEmbed-style flat
#     `<build>/`.
#   * Recursively walks transitive homebrew/usr-local deps (kokoro pulls
#     espeak-ng → pcaudiolib on macOS); copies each, rewrites its
#     LC_ID_DYLIB to @rpath/<basename>, and rewrites the loader's
#     LC_LOAD_DYLIB references.
#   * Deletes every absolute LC_RPATH entry from the wrapper lib (cmake
#     bakes in /opt/homebrew/Cellar/... and the build-tree path, both
#     leak the dev machine and crowd out the bundled @loader_path
#     lookup) and adds @loader_path/. as the only rpath.
#
# Re-codesigns ad-hoc at the end (CI release jobs with a Developer ID
# can override via CODESIGN_IDENTITY). Optionally repacks the .dmg from
# the patched .app so the published artifact picks up the changes —
# Tauri 2 has no hook between "create .app" and "create .dmg".
#
# Usage:
#   scripts/bundle_macos_native_libs.sh [path/to/.app]
#
# Env:
#   CRISPASR_BUILD_DIR    cmake build dir under which `<dir>/src/lib*` +
#                         `<dir>/ggml/src/libggml*` exist
#                         (default: ../CrispASR/build-flutter-bundle)
#   CRISPEMBED_BUILD_DIR  cmake build dir under which libcrispembed.dylib
#                         + libggml*.dylib exist (CrispEmbed uses flat
#                         layout by default, but the script also accepts
#                         the CrispASR-style nested layout)
#                         (default: ../CrispEmbed/build)
#   REPACK_DMG=0          skip the .dmg repack step
#   CODESIGN_IDENTITY     a real Developer ID identity instead of ad-hoc

set -euo pipefail

# ── Locate the .app ─────────────────────────────────────────────────────
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

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
CRISPASR_BUILD_DIR="${CRISPASR_BUILD_DIR:-$REPO_ROOT/CrispASR/build-flutter-bundle}"
CRISPEMBED_BUILD_DIR="${CRISPEMBED_BUILD_DIR:-$REPO_ROOT/CrispEmbed/build}"

FRAMEWORKS="$APP/Contents/Frameworks"
mkdir -p "$FRAMEWORKS"

# Wipe any previous bundle so stale dylibs from earlier runs don't
# linger across rebuilds.
rm -f "$FRAMEWORKS"/lib*.dylib

# ── Helpers ──────────────────────────────────────────────────────────────

# Resolve to a concrete versioned dylib for $name under $dir, falling
# back through alternate names (e.g. libwhisper for libcrispasr) and
# unversioned symlinks. Echoes the chosen path; empty string if none.
locate_versioned_lib() {
  local dir="$1"; shift
  for name in "$@"; do
    local found
    found="$(find "$dir" -maxdepth 1 -type f \
              -name "lib$name.[0-9]*.dylib" 2>/dev/null | sort | head -1)"
    if [[ -n "$found" ]]; then echo "$found"; return; fi
  done
  for name in "$@"; do
    for cand in "$dir/lib$name.dylib"; do
      if [[ -f "$cand" || -L "$cand" ]]; then echo "$cand"; return; fi
    done
  done
  echo ""
}

# Copy every libggml*.dylib under $1 (recursive, type=f) into
# Frameworks/ flat. Then re-create SONAME-versioned symlinks
# (libggml.0.dylib → libggml.0.10.0.dylib …) so dlopen-by-SONAME works.
copy_ggml_libs_from() {
  local src="$1"
  [[ -d "$src" ]] || return 0
  while IFS= read -r f; do
    cp -L "$f" "$FRAMEWORKS/$(basename "$f")"
  done < <(find "$src" -name "libggml*.dylib" -type f 2>/dev/null)
  while IFS= read -r f; do
    base="$(basename "$f")"
    [[ -e "$FRAMEWORKS/$base" ]] && continue
    target="$(basename "$(readlink "$f" 2>/dev/null || echo "$f")")"
    if [[ -f "$FRAMEWORKS/$target" ]]; then
      ln -sf "$target" "$FRAMEWORKS/$base"
    fi
  done < <(find "$src" -name "libggml*.dylib" -type l 2>/dev/null)
}

# List absolute homebrew/usr-local LC_LOAD_DYLIB entries of $1.
external_deps_of() {
  otool -L "$1" 2>/dev/null \
    | awk 'NR>1 {print $1}' \
    | grep -E '^/(opt/homebrew|usr/local)/' || true
}

# Recursive transitive walk: for each `/opt/homebrew/*` dep of
# $1 (the loader), copy it next to the loader, rewrite its
# LC_ID_DYLIB + the loader's LC_LOAD_DYLIB to @rpath/<basename>, then
# recurse into the bundled copy's own deps. Hash-set keyed on
# basename prevents loops and duplicate work.
declare -a processed=()
already_processed() {
  local needle="$1"
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
    install_name_tool -change "$dep" "@rpath/$base" "$loader" 2>/dev/null || true
    if ! already_processed "$base"; then
      processed+=("$base")
      [[ -f "$FRAMEWORKS/$base" ]] && bundle_external_recursive "$FRAMEWORKS/$base"
    fi
  done
}

# Strip every absolute LC_RPATH entry from $1 (cmake bakes in absolute
# build-tree + homebrew paths that crowd out @loader_path on the user's
# machine), then add `@loader_path/.` as the single deterministic
# search path so transitive @rpath/... lookups land in
# Contents/Frameworks/.
canonicalise_loader_rpath() {
  local lib="$1"
  while IFS= read -r p; do
    install_name_tool -delete_rpath "$p" "$lib" 2>/dev/null || true
  done < <(otool -l "$lib" \
    | awk '/cmd LC_RPATH/{getline;getline; print $2}' \
    | grep -E '^/' || true)
  install_name_tool -add_rpath "@loader_path/." "$lib" 2>/dev/null || true
}

# Process one wrapper library: copy + alias + ggml + transitives + rpath.
# Args:
#   $1 — friendly name for the log line
#   $2 — build dir (skip silently if missing)
#   $3 — Frameworks/-side basename to settle on (e.g., libcrispasr.dylib)
#   $4 — comma-separated list of base names to look for in build dir
#         (e.g., "crispasr,whisper" — first match wins)
#   $5 — extra symlink names to create after copying (space-separated;
#         optional; empty for libs without SOVERSION like libcrispembed)
process_wrapper() {
  local label="$1" build_dir="$2" frameworks_name="$3" search_names_csv="$4" extra_aliases="${5:-}"

  if [[ ! -d "$build_dir" ]]; then
    echo "==> [$label] build dir not present at $build_dir — skipping"
    return 0
  fi

  # Lib could live in the CrispASR-style `src/` subdir or flat in
  # the build-dir root (CrispEmbed convention). Probe both.
  local lib_search
  IFS=',' read -ra lib_search <<<"$search_names_csv"
  local versioned=""
  for src_subdir in "$build_dir/src" "$build_dir"; do
    versioned="$(locate_versioned_lib "$src_subdir" "${lib_search[@]}")"
    [[ -n "$versioned" ]] && break
  done
  if [[ -z "$versioned" ]]; then
    echo "==> [$label] no $frameworks_name source dylib found under $build_dir — skipping"
    return 0
  fi

  echo "==> [$label] bundling from $versioned"
  cp -L "$versioned" "$FRAMEWORKS/$frameworks_name"
  chmod u+w "$FRAMEWORKS/$frameworks_name"
  for alias in $extra_aliases; do
    ln -sf "$frameworks_name" "$FRAMEWORKS/$alias"
  done

  # ggml libs — probe both layouts.
  copy_ggml_libs_from "$build_dir/ggml/src"
  copy_ggml_libs_from "$build_dir"

  # Recursive transitive bundle (espeak-ng → pcaudiolib etc.).
  bundle_external_recursive "$FRAMEWORKS/$frameworks_name"

  # Clean LC_RPATH + add @loader_path/. so future transitive @rpath
  # lookups land in Frameworks/.
  canonicalise_loader_rpath "$FRAMEWORKS/$frameworks_name"
}

# ── Process each wrapper ────────────────────────────────────────────────

# CrispASR — SOVERSION 1 alias (libcrispasr.1.dylib) is the SONAME the
# binary records via LC_LOAD_DYLIB. libwhisper.dylib is a legacy alias
# kept for compatibility.
process_wrapper "crispasr" "$CRISPASR_BUILD_DIR" \
  "libcrispasr.dylib" "crispasr,whisper" \
  "libcrispasr.1.dylib libwhisper.dylib"

# CrispEmbed — no SOVERSION, no aliases needed.
process_wrapper "crispembed" "$CRISPEMBED_BUILD_DIR" \
  "libcrispembed.dylib" "crispembed" \
  ""

# ── Re-codesign ──────────────────────────────────────────────────────────
CODESIGN_IDENTITY="${CODESIGN_IDENTITY:-}"
if [[ -n "$CODESIGN_IDENTITY" ]]; then
  codesign --force --deep --options runtime --sign "$CODESIGN_IDENTITY" "$APP"
else
  codesign --force --deep --sign - "$APP" >/dev/null
fi

echo
echo "Bundled into $APP/Contents/Frameworks:"
( cd "$FRAMEWORKS" && ls -l ./*.dylib 2>/dev/null ) | sed 's|^|  |'

# ── Optional: repack .dmg from the patched .app ──────────────────────────
REPACK_DMG="${REPACK_DMG:-1}"
if [[ "$REPACK_DMG" != "0" ]]; then
  APP_DIR="$(dirname "$APP")"
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
