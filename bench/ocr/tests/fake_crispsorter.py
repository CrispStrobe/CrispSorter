#!/usr/bin/env python3
"""Stand-in for the `crispsorter` CLI in runner tests (no native build needed).

Behaviour per engine name (the value after --engine):
  fake_ok        prints the JSON payload of `crispsorter -f json ocr` with the text in $FAKE_TEXT_DIR/<image stem>.txt
  fake_raw       prints plain text instead of JSON
  fake_sleep     sleeps 60 s (to be timed out), via a child process (process-group kill check)
  fake_child     spawns a child that holds ~40 MB for a moment, then prints like fake_ok
  fake_crash     dies from a signal (SIGSEGV; SIGTERM on macOS, where a real
                 SIGSEGV writes a crash report and may show a crash dialog)
  fake_unsup     exits 1 with an "Unsupported OCR engine" error
  fake_cuda      exits 1 with the CUDA "no kernel image" message
Every OCR call is appended to $FAKE_CALL_LOG (one JSON argv per line) when set.
"""
import json
import os
import signal
import subprocess
import sys
import time
from pathlib import Path

argv = sys.argv[1:]
if os.environ.get("FAKE_CALL_LOG"):
    with open(os.environ["FAKE_CALL_LOG"], "a") as f:
        f.write(json.dumps(argv) + "\n")
if argv == ["--help"]:
    print("CrispSorter CLI\n\nCommands:\n  version\n  doctor\n  ocr\n"); sys.exit(0)
if argv[:2] == ["ocr", "--help"]:
    print("Usage: crispsorter ocr [OPTIONS] <FILE>\n  --engine <ENGINE>\n"); sys.exit(0)
if "version" in argv:
    print(json.dumps({"name": "crispsorter", "version": "0.0.0-fake", "target": "linux", "arch": "x86_64"})); sys.exit(0)
if "doctor" in argv:
    print(json.dumps({"tesseract": False})); sys.exit(0)
if "ocr" not in argv:
    sys.exit(2)
i = argv.index("ocr")
image = Path(argv[i + 1])
engine = argv[argv.index("--engine") + 1]
text_dir = Path(os.environ.get("FAKE_TEXT_DIR", "."))
text = (text_dir / (image.stem + ".txt")).read_text() if (text_dir / (image.stem + ".txt")).is_file() else "hello world"


def emit():
    print(json.dumps({"file": str(image), "engine": engine, "render": "text", "chars": len(text), "text": text}, indent=2))


if engine == "fake_ok":
    print("[info] diagnostics go to stderr", file=sys.stderr); emit()
elif engine == "fake_raw":
    print(text)
elif engine == "fake_sleep":
    subprocess.run([sys.executable, "-c", "import time; time.sleep(60)"])
elif engine == "fake_child":
    subprocess.run([sys.executable, "-c", "b = bytearray(40 * 1024 * 1024); import time; time.sleep(1.0)"])
    emit()
elif engine == "fake_crash":
    # Any non-SIGKILL signal is classified as "crash" by the runner.
    os.kill(os.getpid(), signal.SIGTERM if sys.platform == "darwin" else signal.SIGSEGV)
elif engine == "fake_unsup":
    print("error: OCR failed: Unsupported OCR engine: fake_unsup", file=sys.stderr); sys.exit(1)
elif engine == "fake_cuda":
    print("CUDA error: no kernel image is available for execution on the device", file=sys.stderr); sys.exit(1)
else:
    print(f"error: unknown engine {engine}", file=sys.stderr); sys.exit(1)
