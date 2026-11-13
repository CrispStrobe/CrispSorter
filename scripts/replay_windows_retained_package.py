#!/usr/bin/env python3
"""Diagnose one exact original-source unsigned Windows ZIP without rebuilding."""
import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import signal
import stat
import struct
import subprocess
import sys
import tempfile
import time
import zipfile

import replay_macos_package_validation as controls
from check_release_target import validate_release_target

SELECTOR = "windows_validate_b739"
PIN = "scripts/windows-retained-package-lock.json"
PREFLIGHT = "scripts/fixtures/windows-retained-package/original-preflight.json"


def guard(env):
    if (
        env.get("GITHUB_EVENT_NAME") != "workflow_dispatch"
        or env.get("CRISPSORTER_RELEASE_SELECTOR") != SELECTOR
        or env.get("CRISPSORTER_RELEASE_PUBLISH") != "false"
        or env.get("CRISPSORTER_RELEASE_MAC_APPSTORE") != "false"
        or env.get("CRISPSORTER_MOBILE_RELEASE_PROFILE") != "standard"
    ):
        raise ValueError(
            "Windows replay requires its explicit manual selector, standard profile and publish/mac_appstore=false"
        )


def draft(repo, pin):
    validate_release_target(repo, controls.TAG, controls.SOURCE)
    pages = json.loads(
        subprocess.check_output(
            [
                "gh",
                "api",
                "--paginate",
                "--slurp",
                f"repos/{repo}/releases?per_page=100",
            ],
            text=True,
        )
    )
    if not isinstance(pages, list) or not all(isinstance(page, list) for page in pages):
        raise ValueError("Invalid complete draft inventory")
    matches = [
        row for page in pages for row in page if row.get("tag_name") == controls.TAG
    ]
    if (
        len(matches) != 1
        or matches[0].get("draft") is not True
        or matches[0].get("target_commitish") != controls.SOURCE
    ):
        raise ValueError("Expected the unique original-source unpublished draft")
    expected = pin["package"]
    assets = [
        row
        for row in matches[0].get("assets", [])
        if row.get("name") == expected["filename"]
    ]
    if (
        len(assets) != 1
        or assets[0].get("size") != expected["bytes"]
        or assets[0].get("digest") != "sha256:" + expected["sha256"]
    ):
        raise ValueError("Retained Windows ZIP identity changed")
    return {"tag": controls.TAG, "target": controls.SOURCE, "unpublished": True}


def verify_package(path, pin):
    expected = pin["package"]
    if (
        path.name != expected["filename"]
        or path.is_symlink()
        or not path.is_file()
        or path.stat().st_size != expected["bytes"]
        or controls.sha(path) != expected["sha256"]
    ):
        raise ValueError("Downloaded Windows ZIP differs from its exact reviewed pin")


def extract_package(path, destination):
    with zipfile.ZipFile(path) as archive:
        rows = archive.infolist()
        names = [row.filename.casefold() for row in rows]
        if (
            len(rows) > 10000
            or len(set(names)) != len(rows)
            or sum(row.file_size for row in rows) > 2 * 1024**3
        ):
            raise ValueError("Excessive ZIP or duplicate Windows member names")
        reserved = {
            "CON",
            "NUL",
            "AUX",
            "PRN",
            *[f"COM{i}" for i in range(1, 10)],
            *[f"LPT{i}" for i in range(1, 10)],
        }
        for row in rows:
            name = PurePosixPath(row.filename)
            if (
                name.is_absolute()
                or ".." in name.parts
                or "\\" in row.filename
                or row.filename.rstrip("/") != str(name)
                or len(row.filename) > 240
                or any(
                    ":" in part
                    or part.rstrip(". ") != part
                    or part.split(".")[0].upper() in reserved
                    for part in name.parts
                )
                or stat.S_ISLNK(row.external_attr >> 16)
                or row.flag_bits & 1
                or row.file_size > 512 * 1024**2
            ):
                raise ValueError("Unsafe Windows archive member")
        for row in rows:
            target = destination / row.filename
            if row.is_dir():
                target.mkdir(parents=True, exist_ok=True)
            else:
                target.parent.mkdir(parents=True, exist_ok=True)
                with archive.open(row) as source, target.open("xb") as output:
                    import shutil

                    shutil.copyfileobj(source, output, 1024 * 1024)
    executable = destination / "crispsorter.exe"
    if not executable.is_file() or executable.is_symlink():
        raise ValueError("Exact ZIP must contain one root application executable")
    with executable.open("rb") as stream:
        header = stream.read(64)
        if len(header) != 64 or header[:2] != b"MZ":
            raise ValueError("Packaged application lacks a Windows PE header")
        offset = struct.unpack_from("<I", header, 60)[0]
        if offset < 64 or offset > 8 * 1024**2:
            raise ValueError("Packaged PE metadata exceeds its inspection budget")
        stream.seek(offset)
        pe = stream.read(24)
        if (
            len(pe) != 24
            or pe[:4] != b"PE\0\0"
            or struct.unpack_from("<H", pe, 4)[0] != 0x8664
        ):
            raise ValueError("Packaged application must be Windows x86_64 PE")
    return executable


def inventory(root):
    return {
        path.relative_to(root).as_posix(): {
            "bytes": path.stat().st_size,
            "sha256": controls.sha(path),
        }
        for path in root.rglob("*")
        if path.is_file()
    }


def windows_env(home, system_root=None):
    system_root = (
        system_root or os.environ.get("SystemRoot") or os.environ.get("SYSTEMROOT")
    )
    if not system_root:
        raise ValueError("Windows system root is required")
    home.mkdir()
    for child in ["roaming", "local", "temp"]:
        (home / child).mkdir()
    system = Path(system_root)
    return {
        "SystemRoot": str(system),
        "WINDIR": str(system),
        "COMSPEC": str(system / "System32/cmd.exe"),
        "PATH": str(system / "System32") + ";" + str(system),
        "HOME": str(home),
        "USERPROFILE": str(home),
        "APPDATA": str(home / "roaming"),
        "LOCALAPPDATA": str(home / "local"),
        "TMP": str(home / "temp"),
        "TEMP": str(home / "temp"),
        "CRISPEMBED_CACHE_DIR": str(home / "model-cache"),
        "NO_COLOR": "1",
    }


def command(executable, arguments, env, output, label, timeout=120):
    """Windows pipes cannot use POSIX selectors; capture into private temporary files.

    Poll file-size limits while the known pinned CLI runs. Raw capture is never
    copied to artifacts; only bounded sanitized snapshots and statuses survive.
    """
    executable = Path(executable)
    timed_out = oversized = stop_failed = False
    with tempfile.TemporaryFile() as stdout, tempfile.TemporaryFile() as stderr:
        options = (
            {"creationflags": subprocess.CREATE_NEW_PROCESS_GROUP}
            if os.name == "nt"
            else {"start_new_session": True}
        )
        proc = subprocess.Popen(
            [str(executable), *arguments],
            cwd=output,
            env=env,
            stdin=subprocess.DEVNULL,
            stdout=stdout,
            stderr=stderr,
            **options,
        )
        deadline = time.monotonic() + timeout
        while proc.poll() is None:
            oversized = any(
                os.fstat(stream.fileno()).st_size > controls.OUTPUT_LIMIT
                for stream in [stdout, stderr]
            )
            timed_out = time.monotonic() >= deadline
            if oversized or timed_out:
                if os.name == "nt":
                    try:
                        subprocess.run(
                            [
                                str(Path(env["SystemRoot"]) / "System32/taskkill.exe"),
                                "/PID",
                                str(proc.pid),
                                "/T",
                                "/F",
                            ],
                            env=env,
                            stdin=subprocess.DEVNULL,
                            stdout=subprocess.DEVNULL,
                            stderr=subprocess.DEVNULL,
                            timeout=5,
                            check=True,
                        )
                    except (OSError, subprocess.SubprocessError):
                        stop_failed = True
                else:
                    try:
                        os.killpg(proc.pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                try:
                    proc.kill()
                except ProcessLookupError:
                    pass
                break
            time.sleep(0.02)
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            stop_failed = True
        raw = {}
        counts = {}
        for name, stream in [("stdout", stdout), ("stderr", stderr)]:
            counts[name] = os.fstat(stream.fileno()).st_size
            stream.seek(0)
            raw[name] = stream.read(controls.OUTPUT_LIMIT).decode(
                "utf-8", errors="replace"
            )
        oversized |= any(count > controls.OUTPUT_LIMIT for count in counts.values())
    roots = [env["HOME"], str(output), str(executable.parent)]
    record = {
        "label": label,
        "arguments": [controls.sanitize(str(arg), roots) for arg in arguments],
        "exit_code": proc.returncode,
        "timed_out": timed_out,
        "output_truncated": oversized,
        "output_pipe_failure": False,
        "startup_failed": False,
        "process_not_reaped": stop_failed,
        "diagnostics_rejected": False,
        "output_bytes": counts,
        "capture": "private bounded file snapshots; no redirected pipes",
    }
    for name in ["stdout", "stderr"]:
        if counts[name] > controls.OUTPUT_LIMIT:
            text, rejected = "<output omitted: exceeded bounded retention>\n", False
        else:
            text, rejected = controls.sanitize_output(raw[name], roots)
        record["diagnostics_rejected"] |= rejected
        path = output / (label + "." + name + ".txt")
        path.write_text(text, encoding="utf-8")
        record[name + "_sha256"] = controls.sha(path)
    (output / (label + ".json")).write_text(json.dumps(record, indent=2) + "\n")
    return record, raw


def source_identity(source, tooling, pin, original_workflow):
    result = controls.source_identity(source, tooling)
    if os.environ.get("GITHUB_WORKFLOW_SHA") != result["tooling_source"]:
        raise ValueError(
            "Replay tooling differs from the actual workflow-definition commit"
        )
    result["replay_workflow_source"] = result["tooling_source"]
    result["replay_workflow_tree"] = result["tooling_tree"]
    if (
        pin.get("schema") != "crispsorter-windows-retained-package-v1"
        or pin.get("original_app_source") != controls.SOURCE
        or result["original_app_source_association"] != pin["original_app_source"]
    ):
        raise ValueError("Reviewed retained package source association changed")
    preflight_bytes = (tooling / PREFLIGHT).read_bytes()
    if hashlib.sha256(preflight_bytes).hexdigest() != pin["preflight_sha256"]:
        raise ValueError("Reviewed original preflight bytes changed")
    preflight = json.loads(preflight_bytes)
    for key, expected in [
        ("app_source", pin["original_app_source"]),
        ("app_tree", pin["original_app_tree"]),
        ("workflow_source", pin["original_workflow_source"]),
        ("workflow_tree", pin["original_workflow_tree"]),
    ]:
        if preflight.get(key) != expected:
            raise ValueError("Original app/workflow identity changed")

    def git(root, *args):
        return subprocess.check_output(
            ["git", "-C", str(root), *args], text=True
        ).strip()

    if git(source, "rev-parse", "HEAD^{tree}") != pin["original_app_tree"]:
        raise ValueError("Original application tree differs from preflight")
    if (
        git(original_workflow, "rev-parse", "HEAD") != pin["original_workflow_source"]
        or git(original_workflow, "rev-parse", "HEAD^{tree}")
        != pin["original_workflow_tree"]
        or git(original_workflow, "status", "--porcelain", "--untracked-files=no")
    ):
        raise ValueError("Original build workflow source/tree changed")
    for name, expected in preflight["original_inputs"].items():
        if controls.sha(source / name) != expected:
            raise ValueError("Original preflight source input changed")
    for name in [PIN, PREFLIGHT, "scripts/replay_windows_retained_package.py"]:
        if (
            subprocess.check_output(["git", "-C", str(tooling), "show", "HEAD:" + name])
            != (tooling / name).read_bytes()
        ):
            raise ValueError("Replay evidence/tooling differs from committed bytes")
    result.update(
        original_build_workflow_source=pin["original_workflow_source"],
        original_build_workflow_tree=pin["original_workflow_tree"],
        original_preflight_sha256=pin["preflight_sha256"],
        source_binding_limit=pin["scope"],
    )
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", required=True)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--original-workflow", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    guard(os.environ)
    if sys.platform != "win32":
        raise ValueError("Packaged execution requires Windows")
    tooling = Path(__file__).resolve().parents[1]
    source = args.source.resolve()
    output = args.output.resolve()
    output.mkdir()
    result = {
        "schema": "crispsorter-windows-existing-package-observation-v1",
        "status": "failed",
        "signing": "Unsigned-labelled retained ZIP; no Authenticode/signing claim",
        "satisfies_publication_gate": False,
        "license_consent_granted": False,
        "original_verification": "Failed packaged DBNet/TrOCR OCR exit1; captured rejection text was not retained",
        "qualification": "Packaged CLI fixture control only; no GUI/device/corpus or compiled-source attestation",
    }
    try:
        pin = json.loads((tooling / PIN).read_text())
        identity = source_identity(
            source, tooling, pin, args.original_workflow.resolve()
        )
        result.update(identity)
        result["reviewed_package_pin_sha256"] = controls.sha(tooling / PIN)
        result["draft"] = draft(args.repo, pin)
        packages = output / "packages"
        packages.mkdir()
        package = packages / pin["package"]["filename"]
        subprocess.run(
            [
                "gh",
                "release",
                "download",
                controls.TAG,
                "--repo",
                args.repo,
                "--pattern",
                package.name,
                "--dir",
                str(packages),
            ],
            check=True,
            capture_output=True,
        )
        verify_package(package, pin)
        draft(args.repo, pin)
        result["package"] = pin["package"]
        extracted = output / "extracted"
        extracted.mkdir()
        executable = extract_package(package, extracted)
        contents = inventory(extracted)
        result["application_files"] = contents
        result["status"] = "running"
        (output / "observation.json").write_text(json.dumps(result, indent=2) + "\n")
        env = windows_env(output / "isolated-home")
        passed, records, provisioned = controls.probe_executables(
            [executable],
            source / "tests/quality/ocr-task.pdf",
            env,
            output,
            target="windows",
            arch="x86_64",
            runner=command,
        )
        result.update(
            status="passed" if passed else "failed",
            commands=records,
            permissive_models_provisioned=provisioned,
            original_ocr_results={
                r["label"]: r["fixture_evidence_matched"]
                for r in records
                if "original-ocr" in r["label"]
            },
            retry_ocr_results={
                r["label"]: r["fixture_evidence_matched"]
                for r in records
                if "retry-ocr" in r["label"]
            },
        )
        if inventory(extracted) != contents:
            raise ValueError("Extracted application changed during execution")
        if (
            source_identity(source, tooling, pin, args.original_workflow.resolve())
            != identity
        ):
            raise ValueError("Original source or replay tooling changed")
        verify_package(package, pin)
        draft(args.repo, pin)
        if not passed:
            raise ValueError(
                "Packaged CLI control failed; inspect retained original/retry diagnostics"
            )
    except Exception as error:
        result.update(
            status="failed",
            error_type=type(error).__name__,
            error=controls.sanitize(str(error), [str(output), str(source)]),
        )
    finally:
        (output / "observation.json").write_text(json.dumps(result, indent=2) + "\n")
    if result["status"] != "passed":
        raise SystemExit(
            "Windows retained package replay failed; sanitized evidence retained"
        )


if __name__ == "__main__":
    main()
