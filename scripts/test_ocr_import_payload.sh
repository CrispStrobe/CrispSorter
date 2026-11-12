#!/usr/bin/env bash
# Exercise the production IPC decoder without Tauri or model dependencies.
set -euo pipefail
repo=$(cd "$(dirname "$0")/.." && pwd)
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
mkdir "$scratch/src"
cp "$repo/src-tauri/src/ocr_import_payload.rs" "$scratch/src/ocr_import_payload.rs"
printf 'mod ocr_import_payload;\n' > "$scratch/src/lib.rs"
cat > "$scratch/Cargo.toml" <<'TOML'
[package]
name = "ocr-import-payload-tests"
version = "0.0.0"
edition = "2021"

[dependencies]
base64 = "0.22"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
TOML
cargo test --offline --manifest-path "$scratch/Cargo.toml"
