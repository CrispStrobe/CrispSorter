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

    print("CLI operation contract passed: pure JSON, stderr receipts, verbosity, failure and redaction")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cli", type=Path, default=Path(os.environ.get("CRISP_CLI", "target/debug/crispsorter")))
    verify(parser.parse_args().cli.resolve())
