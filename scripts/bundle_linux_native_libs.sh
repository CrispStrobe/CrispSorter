#!/usr/bin/env bash
#
# Patch a Tauri-bundled .deb to include the CrispASR + CrispEmbed
# native shared libs and their ggml backends.
#
# Sibling script to scripts/bundle_macos_native_libs.sh — same job,
# different OS conventions.
#
# Why post-bundle and not via tauri.conf.json bundle.linux.deb.files:
#   * Tauri's `bundle.linux.deb.files` requires the lib paths to be
#     known at config-eval time. Our libs come from upstream releases
#     downloaded in a CI step, not from a stable repo path.
#   * patchelf-ing the binary's RUNPATH to land the libs in a
#     CrispSorter-private dir (instead of system /usr/lib) is cleanest
#     done after the .deb already exists — same shape as the macOS
#     install_name_tool dance.
#
# Layout produced:
#   usr/bin/tauri-app                         ← binary (RUNPATH patched)
#   usr/lib/CrispSorter/native/lib*.so        ← our libs (NEW dir)
#   usr/lib/CrispSorter/bin/lib*.so           ← llama's libs (already there)
#
# The new `native/` dir is searched BEFORE `bin/` in the binary's
# RUNPATH, so when crispembed wants `libggml-base.so` it finds 0.10.0
# (its build) not 0.9.7 (llama's). A symlink-shuffle approach would
# work too but two parallel dirs are easier to debug.
#
# Usage:
#   scripts/bundle_linux_native_libs.sh path/to/CrispSorter_*.deb
#
# Env (matches macOS script's defaults):
#   CRISPASR_BUILD_DIR    where libcrispasr.so + ggml libs got extracted
#                         (default: ../CrispASR/build-flutter-bundle)
#   CRISPEMBED_BUILD_DIR  same for libcrispembed.so + ggml libs
#                         (default: ../CrispEmbed/build)
#
# Requires: patchelf (apt: patchelf), ar, tar, gzip/xz.

set -euo pipefail

DEB="${1:-}"
if [[ -z "$DEB" || ! -f "$DEB" ]]; then
  echo "usage: $0 path/to/foo.deb" >&2
  exit 2
fi
DEB_ABS="$(cd "$(dirname "$DEB")" && pwd)/$(basename "$DEB")"

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CRISPASR_BUILD_DIR="${CRISPASR_BUILD_DIR:-$REPO_ROOT/../CrispASR/build-flutter-bundle}"
CRISPEMBED_BUILD_DIR="${CRISPEMBED_BUILD_DIR:-$REPO_ROOT/../CrispEmbed/build}"

# Bail early if neither lib bundle is present — no work to do.
if [[ ! -d "$CRISPASR_BUILD_DIR" && ! -d "$CRISPEMBED_BUILD_DIR" ]]; then
  echo "[bundle-linux] neither $CRISPASR_BUILD_DIR nor $CRISPEMBED_BUILD_DIR exist — nothing to bundle" >&2
  exit 0
fi

# ── Workspace ───────────────────────────────────────────────────────────
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
echo "[bundle-linux] work dir: $WORK"

cd "$WORK"
ar x "$DEB_ABS"

# data.tar.* may be .gz or .xz depending on dpkg/tauri-bundler version;
# probe for both. control + debian-binary stay verbatim.
if [[ -f data.tar.xz ]]; then
  DATA_EXT=xz
  tar xJf data.tar.xz
elif [[ -f data.tar.gz ]]; then
  DATA_EXT=gz
  tar xzf data.tar.gz
elif [[ -f data.tar.zst ]]; then
  DATA_EXT=zst
  zstd -d -c data.tar.zst | tar xf -
else
  echo "[bundle-linux] no data.tar.{xz,gz,zst} in $DEB" >&2
  ls -la
  exit 3
fi

# ── Find the binary ─────────────────────────────────────────────────────
BIN="$(find usr/bin -maxdepth 1 -type f | head -1 || true)"
if [[ -z "$BIN" ]]; then
  echo "[bundle-linux] no binary under usr/bin/ in the .deb" >&2
  exit 3
fi
echo "[bundle-linux] binary: $BIN"

# ── Stage CrispASR + CrispEmbed libs into usr/lib/CrispSorter/native/ ──
NATIVE="usr/lib/CrispSorter/native"
mkdir -p "$NATIVE"

stage_libs() {
  local label="$1" dir="$2"
  if [[ ! -d "$dir" ]]; then
    echo "  [$label] $dir not present — skipping"
    return
  fi
  local count=0
  while IFS= read -r f; do
    cp -L "$f" "$NATIVE/"
    chmod u+w "$NATIVE/$(basename "$f")"
    count=$((count + 1))
  done < <(find "$dir" -maxdepth 5 \( -name "lib*.so" -o -name "lib*.so.*" \) -type f 2>/dev/null)
  echo "  [$label] staged $count libs from $dir"
}
stage_libs "crispasr"   "$CRISPASR_BUILD_DIR"
stage_libs "crispembed" "$CRISPEMBED_BUILD_DIR"

if [[ -z "$(ls -A "$NATIVE" 2>/dev/null)" ]]; then
  echo "[bundle-linux] no libs staged — nothing to do, leaving .deb unchanged"
  exit 0
fi

echo "[bundle-linux] staged into $NATIVE:"
ls -la "$NATIVE" | sed 's|^|  |'

# ── Patch the binary's RUNPATH ──────────────────────────────────────────
# Existing RUNPATH (from build.rs) already includes `$ORIGIN/../lib` and
# `$ORIGIN`. Prepend our private dir so it's searched first, beating
# any name-collision with llama's older ggml under /usr/lib/CrispSorter/bin/.
EXISTING_RPATH="$(patchelf --print-rpath "$BIN" 2>/dev/null || true)"
NEW_RPATH='$ORIGIN/../lib/CrispSorter/native'
if [[ -n "$EXISTING_RPATH" ]]; then
  NEW_RPATH="${NEW_RPATH}:${EXISTING_RPATH}"
fi
patchelf --force-rpath --set-rpath "$NEW_RPATH" "$BIN"
echo "[bundle-linux] new RUNPATH on $BIN: $(patchelf --print-rpath "$BIN")"

# ── Repack data.tar.<ext> ───────────────────────────────────────────────
case "$DATA_EXT" in
  xz)  tar cJf data.tar.xz  usr ;;
  gz)  tar czf data.tar.gz  usr ;;
  zst) tar cf - usr | zstd -o data.tar.zst ;;
esac

# ── Re-create the .deb ──────────────────────────────────────────────────
# Order matters: debian-binary, then control.tar.gz, then data.tar.*
ar rcs "${DEB_ABS}.tmp" debian-binary control.tar.gz "data.tar.${DATA_EXT}"
mv "${DEB_ABS}.tmp" "$DEB_ABS"

echo "[bundle-linux] repacked: $DEB_ABS"
ls -lh "$DEB_ABS"
