#!/usr/bin/env bash
# Run the production mounted-file reader tests without Tauri/model dependencies.
set -euo pipefail
repo=$(cd "$(dirname "$0")/.." && pwd)
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
cp "$repo/src-tauri/src/drives/mounted_read.rs" "$scratch/mounted_read.rs"
printf 'mod mounted_read;\n' > "$scratch/test.rs"
rustc --edition=2021 --test "$scratch/test.rs" -o "$scratch/mounted-read-tests"
"$scratch/mounted-read-tests"
