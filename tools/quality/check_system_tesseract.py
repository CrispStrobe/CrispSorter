#!/usr/bin/env python3
"""Compile the production word-evidence module in a small, isolated Rust harness.

This does not qualify the full Tauri application or its other native engines.
It exercises the actual source file, its feature gates and optional live adapter
without loading unrelated model bindings. Full integration has its own workflow.
"""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--fixture", type=Path, help="Run the real adapter against this PNG after parser/gate checks")
    args = parser.parse_args()
    repository = Path(__file__).resolve().parents[2]
    source = repository / "src-tauri/src/ocr_word_evidence.rs"
    if args.fixture is not None and not args.fixture.is_file():
        parser.error("The live fixture must be an existing image.")
    with tempfile.TemporaryDirectory(prefix="crispsorter-word-evidence-") as directory:
        harness = Path(directory)
        (harness / "Cargo.toml").write_text('''[package]
name = "crispsorter-word-evidence-check"
version = "0.0.0"
edition = "2021"

[lib]
path = "lib.rs"

[features]
default = []
sidecars = []
desktop-mas = []

[dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
image = { version = "0.25.10", default-features = false, features = ["png"] }
tempfile = "3"
''', encoding="utf-8")
        (harness / "lib.rs").write_text(
            f'#[path = {json.dumps(source.as_posix(), ensure_ascii=False)}]\npub mod ocr_word_evidence;\n', encoding="utf-8")
        command = ["cargo", "test", "--manifest-path", str(harness / "Cargo.toml"), "--no-default-features"]
        for features in [None, "sidecars", "sidecars,desktop-mas"]:
            feature_args = ["--features", features] if features else []
            subprocess.run(command + feature_args + ["--", "--test-threads=1"], check=True)
        if args.fixture is not None:
            environment = dict(os.environ)
            environment["CRISPSORTER_OCR_FIXTURE"] = str(args.fixture.resolve())
            subprocess.run(command + ["--features", "sidecars", "system_tesseract_live_fixture_has_real_word_evidence",
                                     "--", "--ignored", "--nocapture", "--test-threads=1"],
                           check=True, env=environment)


if __name__ == "__main__":
    main()
