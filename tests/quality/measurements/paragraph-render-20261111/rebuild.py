#!/usr/bin/env python3
"""Rebuild the pinned OCR text renderer and run probe.py without Rust or models.

Prepare the exact source commit once (no full checkout is needed):
  git init --bare renderer-source.git
  git -C renderer-source.git fetch --depth 1 https://github.com/CrispStrobe/CrispEmbed.git e14136d3a99572367e68619045943fe77a299caa
Then, with cc, c++, pdftotext and Python pypdf installed:
  python3 rebuild.py --source renderer-source.git --output renderer-check

This qualification excludes raster decoding and the application's Rust binding.
Only selected renderer/header and vendored Unicode source files are materialized.
"""
import argparse
import hashlib
import io
import json
from pathlib import Path
import subprocess
import sys
import tarfile

PIN = "e14136d3a99572367e68619045943fe77a299caa"
PATHS = ["src/ocr_render.cpp", "src/ocr_render.h", "src/ocr_pdf_font.h",
         "src/ocr_pdf_metrics.h", "src/ocr_pdf_srgb.h", "vendor/sheenbidi"]

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    source, output = args.source.resolve(), args.output.resolve()
    resolved = subprocess.check_output(["git", "-C", str(source), "rev-parse", PIN + "^{commit}"], text=True).strip()
    if resolved != PIN:
        raise SystemExit("Source commit does not match the recorded renderer pin")
    output.mkdir(parents=True, exist_ok=True)
    data = subprocess.check_output(["git", "-C", str(source), "archive", PIN, *PATHS])
    with tarfile.open(fileobj=io.BytesIO(data)) as archive:
        archive.extractall(output, filter="data")
    (output / "no_image.cpp").write_text('''#include <cstdlib>
extern "C" {
int stbi_info(const char *, int *, int *, int *) { return 0; }
unsigned char *stbi_load(const char *, int *, int *, int *, int) { return nullptr; }
void stbi_image_free(void *p) { std::free(p); }
unsigned char *stbi_zlib_compress(unsigned char *, int, int *, int) { return nullptr; }
}
''')
    subprocess.run(["cc", "-std=c11", "-O1", "-fPIC", "-DSB_CONFIG_UNITY",
        "-Ivendor/sheenbidi/Headers", "-Ivendor/sheenbidi/Source", "-c",
        "vendor/sheenbidi/Source/SheenBidi.c", "-o", "sheenbidi.o"], cwd=output, check=True)
    subprocess.run(["c++", "-std=c++17", "-O1", "-fPIC", "-shared", "-DCRISPEMBED_BUILD",
        "-Isrc", "-Ivendor/sheenbidi/Headers", "src/ocr_render.cpp", "no_image.cpp",
        "sheenbidi.o", "-o", "libocr_render_test.so"], cwd=output, check=True)
    manifest = {name: hashlib.sha256((output / name).read_bytes()).hexdigest()
        for name in ["src/ocr_render.cpp", "src/ocr_render.h", "no_image.cpp", "libocr_render_test.so"]}
    (output / "source-pins.json").write_text(json.dumps(manifest, indent=2) + "\n")
    subprocess.run([sys.executable, str(Path(__file__).with_name("probe.py"))], cwd=output, check=True)

if __name__ == "__main__":
    main()
