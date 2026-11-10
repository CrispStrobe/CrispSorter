#!/usr/bin/env python3
"""Test the production logical-document validator without unrelated model bindings.

This validates the Rust module and its range/link contracts. The paired native
workflow separately compiles its Tauri export integration; neither is a rendered
DOCX/PDF qualification receipt.
"""
from __future__ import annotations

import json
from pathlib import Path
import subprocess
import tempfile


def main() -> None:
    repository = Path(__file__).resolve().parents[2]
    source = repository / "src-tauri/src/ocr_document.rs"
    with tempfile.TemporaryDirectory(prefix="crispsorter-document-structure-") as directory:
        harness = Path(directory)
        (harness / "Cargo.toml").write_text('''[package]
name = "crispsorter-document-structure-check"
version = "0.0.0"
edition = "2021"

[lib]
path = "lib.rs"

[dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
unicode-segmentation = "=1.13.3"
''', encoding="utf-8")
        (harness / "lib.rs").write_text(
            f'#[path = {json.dumps(source.as_posix(), ensure_ascii=False)}]\npub mod ocr_document;\n',
            encoding="utf-8")
        subprocess.run(["cargo", "test", "--manifest-path", str(harness / "Cargo.toml"),
                        "--", "--test-threads=1"], check=True)


if __name__ == "__main__":
    main()
