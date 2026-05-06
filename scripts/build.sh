#!/usr/bin/env bash
#
# CrispSorter build helper — frontend + backend in one shot, with the
# checks the manual workflow keeps tripping over (target/ symlink to
# the external drive, stale ../build/ for non-dev launches, …).
#
# Usage:
#   scripts/build.sh                       # debug frontend + debug binary
#   scripts/build.sh --release             # release frontend + release binary
#   scripts/build.sh --check               # extra `npm run check` + cargo test
#   scripts/build.sh --no-frontend         # skip the npm step (Rust-only)
#   scripts/build.sh --no-backend          # skip cargo (frontend-only)
#   scripts/build.sh --clean               # rm -rf target/{debug,release} first
#   scripts/build.sh --bundle              # release + `cargo tauri build` (.app/.dmg)
#
# Env:
#   CRISPSORTER_TARGET_VOLUME — where target/ should symlink to.
#                               Default: <external-volume>/code/crispsorter-target
#                               Set to "" to skip the symlink check entirely
#                               (cargo writes to a real local target/ instead).
#
# What this script does NOT do:
#   * `npm run tauri dev` — that's its own world (vite + cargo together,
#     hot reload). For a live dev session use that command directly.
#   * Linting — `npm run check` + `cargo clippy` happen via the --check
#     flag; running them on every build slows the inner loop too much.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SRC_TAURI="$REPO_ROOT/src-tauri"
BUILD_DIR="$REPO_ROOT/build"
TARGET_VOLUME="${CRISPSORTER_TARGET_VOLUME:-<external-volume>/code/crispsorter-target}"

# ── Flag parsing ────────────────────────────────────────────────────────
RELEASE=0
DO_CHECK=0
DO_FRONTEND=1
DO_BACKEND=1
DO_CLEAN=0
DO_BUNDLE=0

for arg in "$@"; do
  case "$arg" in
    --release)     RELEASE=1 ;;
    --check)       DO_CHECK=1 ;;
    --no-frontend) DO_FRONTEND=0 ;;
    --no-backend)  DO_BACKEND=0 ;;
    --clean)       DO_CLEAN=1 ;;
    --bundle)      RELEASE=1; DO_BUNDLE=1 ;;
    -h|--help)
      sed -n '2,30p' "$0" | sed 's/^# \{0,1\}//'
      exit 0 ;;
    *)
      echo "unknown flag: $arg (--help for usage)" >&2
      exit 2 ;;
  esac
done

profile="debug"
[ "$RELEASE" = "1" ] && profile="release"

# ── target/ symlink dance ───────────────────────────────────────────────
# LEARNINGS.md says src-tauri/target should be a symlink to the external
# volume so 12+ GB of build artifacts don't sit on the boot drive. The
# symlink occasionally goes missing (manual `rm -rf target` to recover
# disk, restore from backup not bringing the symlink with it, …). This
# block reasserts it when (a) the volume is mounted and (b) the user
# hasn't disabled the dance via CRISPSORTER_TARGET_VOLUME="".
ensure_target_symlink() {
  if [ -z "$TARGET_VOLUME" ]; then
    echo "[build] CRISPSORTER_TARGET_VOLUME unset — using local target/"
    return
  fi
  # Volume parent must be reachable; if not, fall through to local
  # target/ rather than failing (e.g. external drive unplugged).
  local volume_parent
  volume_parent="$(dirname "$TARGET_VOLUME")"
  if [ ! -d "$volume_parent" ]; then
    echo "[build] target volume parent $volume_parent not mounted — using local target/"
    return
  fi
  mkdir -p "$TARGET_VOLUME"
  # target/ moved to the workspace root with the crisp-index-server
  # integration (commit 7326771). The symlink-to-external-volume trick
  # follows -- now $REPO_ROOT/target instead of $REPO_ROOT/target.
  local current="$REPO_ROOT/target"
  if [ -L "$current" ]; then
    local cur_dest
    cur_dest="$(readlink "$current")"
    if [ "$cur_dest" = "$TARGET_VOLUME" ]; then
      echo "[build] target/ → $TARGET_VOLUME (already linked)"
      return
    fi
    echo "[build] target/ symlink points at $cur_dest, replacing with $TARGET_VOLUME"
    rm "$current"
  elif [ -d "$current" ]; then
    # Real directory at target/. If empty, just remove. Otherwise
    # bail loudly — moving 12 GB without confirmation is rude.
    if [ -z "$(ls -A "$current" 2>/dev/null)" ]; then
      rmdir "$current"
    else
      echo "[build] WARNING: $current is a real directory with contents." >&2
      echo "        Move it manually:" >&2
      echo "          mv $current $TARGET_VOLUME" >&2
      echo "          ln -s $TARGET_VOLUME $current" >&2
      echo "        Or set CRISPSORTER_TARGET_VOLUME='' to keep target/ local." >&2
      exit 3
    fi
  fi
  ln -s "$TARGET_VOLUME" "$current"
  echo "[build] target/ → $TARGET_VOLUME (newly linked)"
}

# ── Steps ───────────────────────────────────────────────────────────────
ensure_target_symlink

if [ "$DO_CLEAN" = "1" ]; then
  echo "[build] cleaning $profile intermediates"
  rm -rf "$REPO_ROOT/target/$profile" "$BUILD_DIR"
fi

if [ "$DO_FRONTEND" = "1" ]; then
  echo "[build] npm run build → static frontend in $BUILD_DIR/"
  ( cd "$REPO_ROOT" && npm run build )
fi

if [ "$DO_CHECK" = "1" ]; then
  echo "[build] npm run check (svelte-check)"
  ( cd "$REPO_ROOT" && npm run check )
  echo "[build] cargo test --lib"
  ( cd "$SRC_TAURI" && cargo test --lib )
fi

if [ "$DO_BACKEND" = "1" ]; then
  if [ "$DO_BUNDLE" = "1" ]; then
    echo "[build] cargo tauri build (.app + .dmg + …)"
    ( cd "$REPO_ROOT" && npm run tauri -- build )
  elif [ "$RELEASE" = "1" ]; then
    echo "[build] cargo build --release --bin tauri-app"
    ( cd "$SRC_TAURI" && cargo build --release --bin tauri-app )
  else
    echo "[build] cargo build --bin tauri-app"
    ( cd "$SRC_TAURI" && cargo build --bin tauri-app )
  fi
fi

# ── Summary ─────────────────────────────────────────────────────────────
echo
echo "[build] done."
binary_path=""
if [ "$DO_BACKEND" = "1" ]; then
  if [ "$RELEASE" = "1" ]; then
    binary_path="$REPO_ROOT/target/release/tauri-app"
  else
    binary_path="$REPO_ROOT/target/debug/tauri-app"
  fi
  if [ -e "$binary_path" ]; then
    size_mb=$(du -m "$binary_path" 2>/dev/null | cut -f1)
    echo "  binary:   $binary_path  (${size_mb} MB)"
  fi
fi
if [ "$DO_FRONTEND" = "1" ] && [ -e "$BUILD_DIR/index.html" ]; then
  echo "  frontend: $BUILD_DIR/index.html"
fi

# How the user actually launches
echo
echo "Launch:"
echo "  npm run tauri dev                      # live dev (vite + cargo, hot reload)"
if [ -n "$binary_path" ] && [ "$RELEASE" = "1" ]; then
  echo "  $binary_path                           # release binary, uses static build/"
fi
echo "  $REPO_ROOT/target/debug/tauri-app version    # CLI mode (no GUI)"
