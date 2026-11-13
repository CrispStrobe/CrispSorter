#!/usr/bin/env python3
"""Re-evaluate only reviewed retained macOS CLI evidence; never execute packages."""
import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
import stat
import subprocess
import zipfile

import replay_macos_package_validation as replay

PIN = "scripts/macos-retained-validation-lock.json"
FLAGS = {
    "timed_out",
    "output_truncated",
    "output_pipe_failure",
    "startup_failed",
    "process_not_reaped",
    "diagnostics_rejected",
}


def digest(data):
    return hashlib.sha256(data).hexdigest()


def archive_files(path, pin):
    if (
        path.is_symlink()
        or not path.is_file()
        or path.stat().st_size != pin["archive"]["bytes"]
        or replay.sha(path) != pin["archive"]["sha256"]
    ):
        raise ValueError(
            "Archive differs from the independently reviewed artifact digest"
        )
    with zipfile.ZipFile(path) as archive:
        rows = archive.infolist()
        if (
            len(rows) > 100
            or len({row.filename for row in rows}) != len(rows)
            or sum(row.file_size for row in rows) > 2 * 1024**2
            or any(
                len(PurePosixPath(row.filename).parts) != 1
                or ".." in row.filename
                or "\\" in row.filename
                or stat.S_ISLNK(row.external_attr >> 16)
                or row.file_size > 512 * 1024
                for row in rows
            )
        ):
            raise ValueError("Unsafe or excessive retained archive")
        files = {row.filename: archive.read(row) for row in rows}
    if {name: digest(data) for name, data in files.items()} != pin["evidence_sha256"]:
        raise ValueError(
            "Retained files differ from committed independent evidence pins"
        )
    return files


def evaluate(files, pin):
    if {name: digest(data) for name, data in files.items()} != pin["evidence_sha256"]:
        raise ValueError("Retained evidence changed before evaluation")

    def read(name):
        return json.loads(files[name])

    obs = read("observation.json")
    packages = {
        name: {"bytes": size, "sha256": sha}
        for name, (size, sha) in replay.ASSETS.items()
    }
    if (
        pin.get("schema") != "crispsorter-reviewed-macos-replay-evidence-v1"
        or pin["original_app_source"] != replay.SOURCE
        or obs.get("schema") != "crispsorter-macos-existing-package-observation-v1"
        or obs.get("original_app_source_association") != replay.SOURCE
        or obs.get("tooling_source") != pin["original_tooling_source"]
        or obs.get("tooling_tree") != pin["original_tooling_tree"]
        or obs.get("status") != pin["original_status"]
        or pin["original_status"] != "failed"
        or obs.get("detach_failed") is not True
        or pin["original_detach_failed"] is not True
        or obs.get("packages") != packages
        or obs.get("fixture_sha256") != replay.FIXTURE
        or obs.get("cargo_lock_sha256")
        != "cc0a1af878bec8ee7cbe6ea29a0a2981cf878fa1dc89a330cbe98092eb9c00b4"
        or obs.get("runtime_lock_sha256")
        != "b3715018bba76b5c7223736e555be78b6d4df5021cd0ff10a1f5208cc1b397e3"
        or obs.get("draft")
        != {"tag": replay.TAG, "target": replay.SOURCE, "unpublished": True}
        or obs.get("satisfies_publication_gate") is not False
        or obs.get("license_consent_granted") is not False
    ):
        raise ValueError(
            "Original failed observation does not match reviewed source/package/context"
        )
    identity = obs["application_identity"]
    if (
        identity.get("bundle_id") != "com.crispstrobe.crispsorter"
        or identity.get("version") != "0.15.1"
        or identity.get("content_sha256") != pin["application_content_sha256"]
        or digest(json.dumps(identity["files"], sort_keys=True).encode())
        != identity["content_sha256"]
    ):
        raise ValueError("Observed application content inventory changed")
    expected = [
        f"package-{index}-{phase}"
        for index in range(2)
        for phase in ["version", "doctor", "worker", "original-ocr"]
    ]
    expected += [
        "model-plan",
        "permissive-model-pull",
        "package-0-retry-ocr",
        "package-1-retry-ocr",
    ]
    records = obs["commands"]
    if [row["label"] for row in records] != expected:
        raise ValueError(
            "Incomplete, duplicate or reordered retained command inventory"
        )
    by_label = {}
    outputs = {}
    for observed in records:
        label = observed["label"]
        saved = read(label + ".json")
        comparable = {
            key: value
            for key, value in observed.items()
            if key != "fixture_evidence_matched"
        }
        if (
            saved != comparable
            or saved.get("label") != label
            or any(saved.get(flag) is not False for flag in FLAGS)
        ):
            raise ValueError(
                "Command metadata changed or evidence is incomplete: " + label
            )
        streams = {}
        for stream in ["stdout", "stderr"]:
            raw = files[label + "." + stream + ".txt"]
            if (
                len(raw) > replay.OUTPUT_LIMIT
                or digest(raw) != saved[stream + "_sha256"]
            ):
                raise ValueError(
                    "Command output differs from its retained hash: " + label
                )
            streams[stream] = raw.decode("utf-8")
        by_label[label] = saved
        outputs[label] = streams
    for index in range(2):
        prefix = f"package-{index}-"
        version = by_label[prefix + "version"]
        if (
            not replay.completed(version)
            or version["arguments"] != ["--format", "json", "version"]
            or json.loads(outputs[prefix + "version"]["stdout"])
            != {
                "name": "crispsorter",
                "version": "0.15.1",
                "target": "macos",
                "arch": "aarch64",
            }
        ):
            raise ValueError("Version control failed")
        doctor = by_label[prefix + "doctor"]
        if (
            not replay.completed(doctor)
            or doctor["arguments"] != ["--format", "json", "doctor"]
            or not replay.doctor_pass(json.loads(outputs[prefix + "doctor"]["stdout"]))
        ):
            raise ValueError("Corrected source-bound doctor schema still fails")
        worker = by_label[prefix + "worker"]
        if (
            worker["exit_code"] != 1
            or worker["arguments"] != ["--crispsorter-ocr-worker"]
            or "OCR worker requires request and response paths"
            not in outputs[prefix + "worker"]["stderr"]
        ):
            raise ValueError("Worker negative control failed")
        original = by_label[prefix + "original-ocr"]
        retry = by_label[prefix + "retry-ocr"]
        for row in [original, retry]:
            args = row["arguments"]
            if (
                len(args) != 12
                or args[0] != "ocr"
                or not args[1].endswith("/original-source/tests/quality/ocr-task.pdf")
                or args[2:]
                != [
                    "--engine",
                    "dbnet_trocr",
                    "--cleanup",
                    "false",
                    "--render",
                    "hocr",
                    "--min-confidence",
                    "0",
                    "--min-chars",
                    "0",
                ]
            ):
                raise ValueError("Retained OCR invocation changed")
        if (
            original["exit_code"] != 1
            or "missing OCR models:" not in outputs[prefix + "original-ocr"]["stderr"]
        ):
            raise ValueError("Original missing-model rejection was not preserved")
        if not replay.ocr_pass(retry, outputs[prefix + "retry-ocr"]):
            raise ValueError("Retained retry lacks successful actual fixture evidence")
        declared = {
            row["label"]: row.get("fixture_evidence_matched") for row in records
        }
        if (
            declared[prefix + "original-ocr"] is not False
            or declared[prefix + "retry-ocr"] is not True
        ):
            raise ValueError("Per-command original/retry fixture declarations changed")
    for label, phase in [("model-plan", "plan"), ("permissive-model-pull", "pull")]:
        row = by_label[label]
        if not replay.completed(row) or row["arguments"] != [
            "--format",
            "json",
            "models",
            phase,
            "dbnet_trocr",
        ]:
            raise ValueError("Permissive model command failed")
        missing = replay.model_plan(
            json.loads(outputs[label]["stdout"]), installed=phase == "pull"
        )
        if phase == "plan" and not missing:
            raise ValueError(
                "Original plan did not establish missing permissive models"
            )
    model_pins = {
        name: {"bytes": size, "sha256": sha}
        for name, (size, sha) in replay.MODEL_PINS.items()
    }
    if (
        read("verified-permissive-models.json") != model_pins
        or obs.get("permissive_models_provisioned") is not True
    ):
        raise ValueError(
            "Retained model-byte verification differs from independent pins"
        )
    original_results = {f"package-{index}-original-ocr": False for index in range(2)}
    retry_results = {f"package-{index}-retry-ocr": True for index in range(2)}
    if (
        obs.get("original_ocr_results") != original_results
        or obs.get("retry_ocr_results") != retry_results
    ):
        raise ValueError("Original/retry status declarations changed")
    return {
        "schema": "crispsorter-macos-retained-cli-reevaluation-v1",
        "status": "retained CLI controls verified under corrected doctor schema",
        "original_app_source_association": replay.SOURCE,
        "original_tooling_source": pin["original_tooling_source"],
        "original_tooling_tree": pin["original_tooling_tree"],
        "original_observation_sha256": digest(files["observation.json"]),
        "original_observation_status": "failed",
        "original_detach_failed": True,
        "cleanup_status": "Not established; original read-only DMG detach failed",
        "original_error": {"type": obs.get("error_type"), "message": obs.get("error")},
        "packages": packages,
        "application_content_sha256": identity["content_sha256"],
        "original_ocr_results": original_results,
        "retry_ocr_results": retry_results,
        "fixture_sha256": replay.FIXTURE,
        "verified_model_pins": model_pins,
        "original_cargo_lock_sha256": obs["cargo_lock_sha256"],
        "original_runtime_lock_sha256": obs["runtime_lock_sha256"],
        "satisfies_publication_gate": False,
        "signing": "Not established; original packages retain unsigned labels",
        "license_consent_granted": False,
        "execution": "No package/model command, download, GitHub write or re-execution performed",
        "qualification": "Reviewed retained CLI fixture evidence only; no overall replay success, signing, GUI/device/corpus or compiled-source attestation",
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--archive", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    if not args.output.name.endswith(".reevaluation.observation.json"):
        parser.error("Use a distinct .reevaluation.observation.json output")
    root = Path(__file__).resolve().parents[1]

    def git(*values):
        return subprocess.check_output(["git", "-C", str(root), *values])

    if git("status", "--porcelain", "--untracked-files=no").strip():
        raise ValueError("Corrected tooling must be clean and committed")
    for name in [
        PIN,
        "scripts/reevaluate_macos_retained_validation.py",
        "scripts/replay_macos_package_validation.py",
    ]:
        if git("show", "HEAD:" + name) != (root / name).read_bytes():
            raise ValueError("Corrected tooling differs from committed bytes")
    pin_bytes = (root / PIN).read_bytes()
    pin = json.loads(pin_bytes)
    for name, expected in [
        ("tests/quality/ocr-task.pdf", replay.FIXTURE),
        (
            "Cargo.lock",
            "cc0a1af878bec8ee7cbe6ea29a0a2981cf878fa1dc89a330cbe98092eb9c00b4",
        ),
        (
            "scripts/crispembed-runtime-lock.json",
            "b3715018bba76b5c7223736e555be78b6d4df5021cd0ff10a1f5208cc1b397e3",
        ),
    ]:
        if digest(git("show", replay.SOURCE + ":" + name)) != expected:
            raise ValueError("Original committed source identity changed")
    if (
        git("rev-parse", pin["original_tooling_source"] + "^{tree}").decode().strip()
        != pin["original_tooling_tree"]
    ):
        raise ValueError("Retained original tooling source/tree association changed")
    result = evaluate(archive_files(args.archive, pin), pin)
    result.update(
        corrected_tooling_source=git("rev-parse", "HEAD").decode().strip(),
        corrected_tooling_tree=git("rev-parse", "HEAD^{tree}").decode().strip(),
        reviewed_evidence_pin_sha256=digest(pin_bytes),
        archive=pin["archive"],
    )
    with args.output.open("x") as output:
        output.write(json.dumps(result, indent=2) + "\n")
    print(
        "Retained CLI controls verified; original overall failure and unresolved cleanup preserved"
    )


if __name__ == "__main__":
    main()
