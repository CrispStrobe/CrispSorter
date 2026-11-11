#!/usr/bin/env python3
"""Exercise the built CLI's output/diagnostic contract without model downloads."""

import argparse
import base64
import json
import os
from pathlib import Path
import subprocess
import tempfile


def operations(stderr: str) -> list[dict]:
    events = []
    for line in stderr.splitlines():
        _, marker, encoded = line.partition("] operation ")
        if marker:
            event = json.loads(encoded)
            assert isinstance(event, dict), "operation receipt is not an object"
            assert event.get("operation_id"), "operation receipt has no correlation ID"
            assert isinstance(event.get("elapsed_ms"), int), "operation receipt has no duration"
            events.append(event)
    return events


def text_pdf(path: Path) -> None:
    """A valid one-page fixture; no external PDF generator or model needed."""
    content = b"BT /F1 12 Tf 72 720 Td (private-text-sentinel) Tj ET\n"
    objects = [
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Count 1 /Kids [3 0 R] >>",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] "
        b"/Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>",
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
        b"<< /Length " + str(len(content)).encode() + b" >>\nstream\n" + content + b"endstream",
    ]
    body = bytearray(b"%PDF-1.4\n")
    offsets = []
    for number, obj in enumerate(objects, 1):
        offsets.append(len(body))
        body.extend(f"{number} 0 obj\n".encode() + obj + b"\nendobj\n")
    xref = len(body)
    body.extend(f"xref\n0 {len(objects) + 1}\n0000000000 65535 f \n".encode())
    for offset in offsets:
        body.extend(f"{offset:010} 00000 n \n".encode())
    body.extend(f"trailer\n<< /Size {len(objects) + 1} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n".encode())
    path.write_bytes(body)


def verify(executable: Path) -> None:
    def run(*arguments: str) -> subprocess.CompletedProcess:
        return subprocess.run([str(executable), *arguments], capture_output=True,
                              text=True, encoding="utf-8", timeout=45, check=False)

    success = run("--format", "json", "--log-verbosity", "debug", "version")
    assert success.returncode == 0, success.stderr
    assert isinstance(json.loads(success.stdout), dict), "stdout must be exactly one JSON result"
    events = [event for event in operations(success.stderr) if event["operation"] == "cli.command"]
    assert [event["phase"] for event in events] == ["started", "completed"], events
    assert events[0]["operation_id"] == events[1]["operation_id"], events
    assert events[0]["context"]["mode"] == "version", events

    quiet = run("--format", "json", "version", "--log-verbosity", "silent")
    assert quiet.returncode == 0, quiet.stderr
    assert json.loads(quiet.stdout) == json.loads(success.stdout)
    assert not operations(quiet.stderr), "silent successful command emitted operation diagnostics"

    with tempfile.TemporaryDirectory(prefix="crisp-cli-contract-") as directory:
        root = Path(directory)
        private_name = "private-document-sentinel.png"
        failure = run("--log-verbosity", "silent", "ocr", str(root / private_name))
        assert failure.returncode != 0, "missing OCR input reported success"
        assert not failure.stdout.strip(), "failed command leaked a partial stdout result"
        events = operations(failure.stderr)
        assert len(events) == 1 and events[0]["phase"] == "failed", events
        assert events[0]["code"] == "command_failed", events
        assert private_name not in json.dumps(events), "receipt disclosed a document path"

        # An existing image reaches configuration loading. Invalid shared policy
        # must fail before recognition/model acquisition and leave no output.
        image = root / private_name
        image.write_bytes(base64.b64decode(
            "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+j4QAAAABJRU5ErkJggg=="))
        config = root / "private-policy-sentinel.json"
        config.write_text('{"engine":"missing-engine"}', encoding="utf-8")
        invalid = run("ocr", str(image), "--ocr-config", str(config),
                      "--data-dir", str(root / "unused-profile"), "--log-verbosity", "debug")
        assert invalid.returncode != 0 and not invalid.stdout.strip(), invalid.stdout
        events = operations(invalid.stderr)
        settings = [event for event in events if event["operation"] == "ocr.settings"]
        assert [event["phase"] for event in settings] == ["started", "failed"], events
        assert settings[-1]["code"] == "settings_load_failed", events
        assert not any(event["phase"] == "completed" for event in events), events
        assert private_name not in json.dumps(events) and config.name not in json.dumps(events)
        assert not (root / "unused-profile").exists(), "explicit snapshot mutated app settings"

        source = root / "private-source-sentinel.pdf"
        text_pdf(source)
        original = source.read_bytes()
        for kind in ("text", "markdown"):
            destination = root / f"private-export-sentinel.{kind}"
            exported = run("--format", "json", "--log-verbosity", "debug", "pdf", "export",
                           str(source), "--to", kind, "--out", str(destination))
            assert exported.returncode == 0, exported.stderr
            result = json.loads(exported.stdout)
            assert result["pages"] == 1 and result["format"] == kind and result["source"] == "text_layer", result
            assert "private-text-sentinel" in destination.read_text(encoding="utf-8")
            events = operations(exported.stderr)
            receipts = [e for e in events if e["operation"] == "pdf.export_text"]
            assert receipts[0]["phase"] == "started" and receipts[-1]["phase"] == "completed", receipts
            assert any(e["operation"] == "output.publish" and e["phase"] == "completed" for e in events), events
            assert "private-" not in json.dumps(events), "PDF export receipts disclosed input/output content"

        recipe = root / "private-edits-sentinel.json"
        destination = root / "private-edited-sentinel.pdf"
        recipe.write_text(json.dumps([{"kind": "rotate", "pages": [0], "degrees": 90}]), encoding="utf-8")
        edited = run("pdf", "apply", str(source), "--operations", str(recipe), "--out", str(destination))
        assert edited.returncode == 0, edited.stderr
        assert json.loads(edited.stdout)["operations"] == 1
        info = run("pdf", "info", str(destination))
        assert info.returncode == 0, info.stderr
        assert json.loads(info.stdout)["pages"][0]["rotation"] == 90, info.stdout
        completed = destination.read_bytes()
        recipe.write_text(json.dumps([
            {"kind": "rotate", "pages": [0], "degrees": 180},
            {"kind": "remove_pages", "pages": [0]},
        ]), encoding="utf-8")
        rejected = run("pdf", "apply", str(source), "--operations", str(recipe), "--out", str(destination))
        assert rejected.returncode != 0 and not rejected.stdout.strip(), rejected.stdout
        assert destination.read_bytes() == completed, "failed recipe replaced a complete output"
        assert source.read_bytes() == original, "PDF operations mutated the input unexpectedly"
        events = operations(rejected.stderr)
        receipts = [e for e in events if e["operation"] == "pdf.recipe"]
        assert [e["phase"] for e in receipts] == ["started", "failed"], receipts
        assert "private-" not in json.dumps(events), "PDF recipe receipts disclosed input/output content"

    print("CLI operation contract passed: JSON, stderr receipts, verbosity, OCR policy, PDF export/edit, atomic failure and redaction")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cli", type=Path, default=Path(os.environ.get("CRISP_CLI", "target/debug/crispsorter")))
    verify(parser.parse_args().cli.resolve())
