#!/usr/bin/env python3
"""Seed an isolated lock from the application, then reject any version drift."""
import hashlib
import json
import pathlib
import re
import sys
import tomllib

repo = pathlib.Path(__file__).resolve().parents[2]
here = pathlib.Path(__file__).resolve().parent
root_text = (repo / "Cargo.lock").read_text()
root = tomllib.loads(root_text)["package"]
contract_lock = here / "Cargo.lock"
if sys.argv[1] == "seed":
    if not contract_lock.exists():
        replacement = '''[[package]]
name = "ocr-text-export-contract"
version = "0.1.0"
dependencies = [
 "docx-rs",
 "serde_json",
 "unicode-bidi",
]
'''
        pattern = r'\[\[package\]\]\nname = "crispsorter"\n.*?(?=\n\[\[package\]\]|\Z)'
        seeded, count = re.subn(pattern, replacement, root_text, count=1, flags=re.S)
        assert count == 1
        contract_lock.write_text(seeded)
    print("Seeded from the app's existing lock; Cargo may prune unused packages, but no version drift is accepted.")
elif sys.argv[1] == "check":
    contract = tomllib.loads(contract_lock.read_text())["package"]
    pins = {(item["name"], item["version"]): item for item in root if "source" in item}
    checked = []
    for package in contract:
        if "source" not in package:
            assert package["name"] == "ocr-text-export-contract"
            continue
        expected = pins[(package["name"], package["version"])]
        assert (package["source"], package.get("checksum")) == (expected["source"], expected.get("checksum"))
        checked.append({key: package[key] for key in ["name", "version", "source", "checksum"]})
    assert not {"image", "libc", "cc", "openssl-sys", "libz-sys"}.intersection(item["name"] for item in contract), "Text exporter unexpectedly needs image/native libraries"
    app = tomllib.loads((repo / "src-tauri/Cargo.toml").read_text())
    assert app["dependencies"]["docx-rs"]["default-features"] is False
    assert app["features"]["ocr-text-export"] == ["dep:docx-rs", "dep:unicode-bidi"]
    for feature in ["desktop", "ocr-workspace", "ocr-mobile"]:
        assert "ocr-text-export" in app["features"][feature]
    assert "desktop" not in app["features"]["ocr-mobile"]
    output = pathlib.Path(sys.argv[2])
    output.mkdir(parents=True, exist_ok=True)
    report = {"packages": checked, "image_decoder_enabled": False, "native_library_dependencies": [],
              "source_sha256": hashlib.sha256((repo / "src-tauri/src/ocr_text_export.rs").read_bytes()).hexdigest(),
              "application_lock_sha256": hashlib.sha256((repo / "Cargo.lock").read_bytes()).hexdigest(),
              "contract_lock_sha256": hashlib.sha256(contract_lock.read_bytes()).hexdigest()}
    (output / "dependency-pins.json").write_text(json.dumps(report, indent=2) + "\n")
    print(f"All {len(checked)} exact dependency versions/checksums match the application lock.")
else:
    raise SystemExit("Expected seed or check")
