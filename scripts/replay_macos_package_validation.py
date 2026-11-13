#!/usr/bin/env python3
"""Diagnose only the pinned, unsigned original-source macOS draft packages.

Never builds, signs, uploads release assets, grants consent or creates a release
receipt. A successful replay is an observation, not publication qualification.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import plistlib
import re
import shutil
import signal
import subprocess
import sys
import tarfile
import selectors
import time

from check_release_target import validate_release_target

SOURCE = "b739a3c0b70f39d3ae19e1bfcb412f3b44f77794"
TAG = "v0.15.1"
SELECTOR = "macos_validate_b739"
ASSETS = {
    "CrispSorter_0.15.1_aarch64-unsigned.dmg": (
        165752528,
        "c5957d581e077848ff3011dd881a068e4d423bd38c783d51de016238170248e9",
    ),
    "CrispSorter_aarch64-unsigned.app.tar.gz": (
        150087263,
        "4b3e0a850666643636caf95829ab6c0f35d4ce1594a13f5aa211cccc1fe33e91",
    ),
}
FIXTURE = "538f504756f35ace8af3ff2b970b9f35794aba5a186036596580a4676de922bb"
MODELS = {"dbnet-det": "apache-2.0", "trocr-printed": "mit"}
MODEL_PINS = {
    "dbnet-ic15-q8_0.gguf": (
        12960448,
        "163c3b3d1211fd47b4ea323e2e684287bdc75ae68f0ffea397ccfcdc29a74de2",
    ),
    "trocr-small-printed-q8_0.gguf": (
        67508320,
        "1aa4cdf3413539d7dbd53a565149abe4ca878f84a7ae2129e9a297f3a26b50d4",
    ),
}
OUTPUT_LIMIT = 256 * 1024


def sha(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def guard(env):
    if (
        env.get("GITHUB_EVENT_NAME") != "workflow_dispatch"
        or env.get("CRISPSORTER_RELEASE_SELECTOR") != SELECTOR
        or env.get("CRISPSORTER_RELEASE_PUBLISH") != "false"
        or env.get("CRISPSORTER_RELEASE_MAC_APPSTORE") != "false"
        or env.get("CRISPSORTER_MOBILE_RELEASE_PROFILE") != "standard"
    ):
        raise ValueError(
            "Replay requires its explicit manual selector, standard profile and publish/mac_appstore=false"
        )


def draft(repo):
    validate_release_target(repo, TAG, SOURCE)
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
        raise ValueError("Invalid release inventory")
    rows = [row for page in pages for row in page if row.get("tag_name") == TAG]
    if (
        len(rows) != 1
        or rows[0].get("draft") is not True
        or rows[0].get("target_commitish") != SOURCE
    ):
        raise ValueError(
            "Replay requires the unique exact original-source unpublished draft"
        )
    for name, (size, digest) in ASSETS.items():
        matches = [a for a in rows[0].get("assets", []) if a.get("name") == name]
        if (
            len(matches) != 1
            or matches[0].get("size") != size
            or matches[0].get("digest") != "sha256:" + digest
        ):
            raise ValueError("Draft package identity changed: " + name)
    return {"tag": TAG, "target": SOURCE, "unpublished": True}


def verify_asset(path):
    size, digest = ASSETS[path.name]
    if (
        path.is_symlink()
        or not path.is_file()
        or path.stat().st_size != size
        or sha(path) != digest
    ):
        raise ValueError("Downloaded package differs from its exact reviewed pin")


def source_identity(source, tooling):
    def git(root, *args):
        return subprocess.check_output(
            ["git", "-C", str(root), *args], text=True
        ).strip()

    if git(source, "rev-parse", "HEAD") != SOURCE or git(
        source, "status", "--porcelain", "--untracked-files=no"
    ):
        raise ValueError(
            "Application fixture checkout must be clean exact original source"
        )
    if git(tooling, "status", "--porcelain", "--untracked-files=no"):
        raise ValueError("Replay tooling must be committed and clean")
    helper = "scripts/replay_macos_package_validation.py"
    if (
        subprocess.check_output(["git", "-C", str(tooling), "show", "HEAD:" + helper])
        != (tooling / helper).read_bytes()
    ):
        raise ValueError("Replay helper differs from committed tooling")
    fixture = source / "tests/quality/ocr-task.pdf"
    if fixture.is_symlink() or sha(fixture) != FIXTURE:
        raise ValueError("Original scanned PDF fixture changed")
    return {
        "original_app_source_association": SOURCE,
        "tooling_source": git(tooling, "rev-parse", "HEAD"),
        "tooling_tree": git(tooling, "rev-parse", "HEAD^{tree}"),
        "fixture_sha256": FIXTURE,
        "cargo_lock_sha256": sha(source / "Cargo.lock"),
        "runtime_lock_sha256": sha(source / "scripts/crispembed-runtime-lock.json"),
        "source_binding_limit": "Original build/draft source association; no original completed package receipt or independent compiled-source attestation",
    }


def extract_tar(artifact, destination):
    """Validate everything before writing; allow only contained app symlinks."""
    with tarfile.open(artifact) as archive:
        rows = archive.getmembers()
        names = {row.name for row in rows}
        if (
            len(rows) > 10000
            or len(names) != len(rows)
            or sum(r.size for r in rows) > 2 * 1024**3
        ):
            raise ValueError("Archive exceeds bounds or contains duplicate members")
        links = {r.name for r in rows if r.issym()}
        for row in rows:
            p = PurePosixPath(row.name)
            if (
                p.is_absolute()
                or ".." in p.parts
                or "\\" in row.name
                or not p.parts
                or row.name != str(p)
                or p.parts[0] != "CrispSorter.app"
                or row.size > 1024**3
                or not (row.isdir() or row.isfile() or row.issym())
                or any(str(parent) in links for parent in p.parents)
            ):
                raise ValueError("Unsafe archive member")
            if row.issym():
                target = PurePosixPath(row.linkname)
                if (
                    target.is_absolute()
                    or ".." in target.parts
                    or "\\" in row.linkname
                    or str(p.parent / target) not in names
                ):
                    raise ValueError("Unsafe archive symlink")
        for row in rows:
            target = destination / row.name
            if row.isdir():
                target.mkdir(parents=True, exist_ok=True)
            elif row.isfile():
                target.parent.mkdir(parents=True, exist_ok=True)
                with archive.extractfile(row) as incoming, target.open("xb") as output:
                    shutil.copyfileobj(incoming, output, 1024 * 1024)
                target.chmod(row.mode & 0o777)
        for row in rows:
            if row.issym():
                (destination / row.name).symlink_to(row.linkname)
    return destination / "CrispSorter.app"


def app_identity(app):
    info = plistlib.loads((app / "Contents/Info.plist").read_bytes())
    if (
        info.get("CFBundleIdentifier") != "com.crispstrobe.crispsorter"
        or info.get("CFBundleShortVersionString") != "0.15.1"
        or info.get("CFBundleExecutable") != "crispsorter"
    ):
        raise ValueError("Package application metadata differs from original release")
    files = {}
    total = 0
    for path in app.rglob("*"):
        if len(files) > 10000:
            raise ValueError("Application file count exceeded")
        key = path.relative_to(app).as_posix()
        if path.is_symlink():
            if not path.resolve(strict=True).is_relative_to(app.resolve()):
                raise ValueError("Application link leaves the bundle")
            files[key] = {"link": os.readlink(path)}
        elif path.is_file():
            total += path.stat().st_size
            if total > 2 * 1024**3:
                raise ValueError("Application byte budget exceeded")
            files[key] = {"bytes": path.stat().st_size, "sha256": sha(path)}
        elif not path.is_dir():
            raise ValueError("Application contains a special file")
    executable = app / "Contents/MacOS/crispsorter"
    if executable.is_symlink() or not os.access(executable, os.X_OK):
        raise ValueError("Missing regular executable")
    return {
        "bundle_id": info["CFBundleIdentifier"],
        "version": info["CFBundleShortVersionString"],
        "files": files,
        "content_sha256": hashlib.sha256(
            json.dumps(files, sort_keys=True).encode()
        ).hexdigest(),
    }


def sanitize_output(text, roots=()):
    """Decode JSON before redaction; reject malformed structured diagnostics."""
    from urllib.parse import urlsplit, urlunsplit

    sensitive = re.compile(
        r"password|passwd|token|secret|credential|authorization|apikey|privatekey|cookie|username",
        re.I,
    )

    def decoded_key(key):
        # Some diagnostics serialize a JSON document more than once. Decode
        # escaped key characters before deciding whether a value is sensitive.
        for _ in range(4):
            decoded = re.sub(
                r"\\u([0-9a-fA-F]{4})", lambda m: chr(int(m.group(1), 16)), key
            )
            if decoded == key:
                break
            key = decoded
        return re.sub(r"[^a-zA-Z0-9]", "", key)

    def sensitive_key(key):
        return bool(sensitive.search(decoded_key(key)))

    def plain(value):
        for key in re.findall(r'"([^"\n]+)"\s*:', value):
            if sensitive_key(key):
                raise ValueError("Unframed sensitive JSON property")

        def url(match):
            try:
                parts = urlsplit(match.group())
                if not parts.hostname:
                    return "<redacted-url>"
                return urlunsplit((parts.scheme, parts.hostname, parts.path, "", ""))
            except ValueError:
                return "<redacted-url>"

        value = re.sub(r'(?i)\b(?:https?|socks5?|ftp)://[^\s<>"\']+', url, value)
        # Decode escaped key characters for detection. Drop the entire remainder
        # of a credential-bearing line rather than guessing escaped value bounds.
        value = re.sub(
            r"\\u([0-9a-fA-F]{4})", lambda m: chr(int(m.group(1), 16)), value
        )
        if any(
            sensitive_key(value[max(0, match.start() - 128) : match.start()])
            for match in re.finditer(r"[:=]", value)
        ):
            # A plaintext assignment may continue over multiple lines. Reject
            # the full diagnostic rather than retaining an ambiguous remainder.
            raise ValueError("Unstructured credential assignment")
        value = re.sub(r"(?i)\bbearer\s+\S[^\r\n]*", "<redacted-bearer>", value)
        value = re.sub(
            r"\b(?:gh[pousr]_[A-Za-z0-9_]+|github_pat_[A-Za-z0-9_]+)\b",
            "<redacted-token>",
            value,
        )
        for root in sorted(map(str, roots), key=len, reverse=True):
            if root:
                value = value.replace(root, "<local-path>")
        return value

    def tree(value, depth=0):
        if depth > 16:
            raise ValueError("Diagnostic nesting exceeds retention limit")
        if isinstance(value, dict):
            # HTTP serializers also represent headers as name/value objects.
            labels = {
                key
                for key, item in value.items()
                if decoded_key(key).lower() in {"name", "key", "header", "headername"}
                and isinstance(item, str)
                and sensitive_key(item)
            }
            if labels:
                return {
                    plain(key): (
                        plain(item) if key in labels else "<redacted-credential>"
                    )
                    for key, item in value.items()
                }
            return {
                plain(key): (
                    "<redacted-credential>"
                    if sensitive_key(key)
                    else tree(item, depth + 1)
                )
                for key, item in value.items()
            }
        if isinstance(value, list):
            # Headers/pairs may be nested arrays instead of object properties.
            if (
                len(value) >= 2
                and isinstance(value[0], str)
                and sensitive_key(value[0])
            ):
                return [plain(value[0]), *["<redacted-credential>" for _ in value[1:]]]
            return [tree(item, depth + 1) for item in value]
        if isinstance(value, str):
            # A JSON string may itself carry a serialized credential object.
            return structured(value, depth + 1)
        return value

    def structured(value, depth=0):
        value = re.sub(r"\x1b\[[0-9;]*[A-Za-z]", "", value)
        if value.lstrip().startswith('"'):
            return json.dumps(
                tree(json.loads(value.strip()), depth + 1), ensure_ascii=True
            )
        parts = []
        position = 0
        decoder = json.JSONDecoder()
        while match := re.search(r"[\{\[]", value[position:]):
            start = position + match.start()
            parts.append(plain(value[position:start]))
            try:
                item, end = decoder.raw_decode(value, start)
            except json.JSONDecodeError:
                # Native runtime diagnostics use [graph]/[info], tensor slices
                # whitespace-separated numeric vectors and model-pull [1/2]
                # counters. These bounded
                # bracket forms contain no JSON keys or credential values.
                marker = re.match(
                    r"\[(?:[1-9][0-9]{0,3}/[1-9][0-9]{0,3}|[A-Za-z_][A-Za-z0-9_-]*|[0-9eE+.,:\s-]+)\]",
                    value[start:],
                )
                if marker is None or sensitive_key(marker.group()):
                    raise
                parts.append(plain(marker.group()))
                position = start + marker.end()
            else:
                parts.append(json.dumps(tree(item, depth + 1), ensure_ascii=True))
                position = end
        parts.append(plain(value[position:]))
        return "".join(parts)

    try:
        return structured(text), False
    except (ValueError, RecursionError, TypeError):
        # Never retain ambiguous fragments that may include escaped secrets.
        return "<diagnostic omitted: malformed or excessive structured output>\n", True


def sanitize(text, roots=()):
    return sanitize_output(text, roots)[0]


def app_env(home):
    home.mkdir()
    return {
        "PATH": "/usr/bin:/bin:/usr/sbin:/sbin:/opt/homebrew/bin",
        "HOME": str(home),
        "TMPDIR": str(home),
        "XDG_CACHE_HOME": str(home / "cache"),
        "XDG_CONFIG_HOME": str(home / "config"),
        "XDG_DATA_HOME": str(home / "data"),
        "NO_COLOR": "1",
    }


def command(executable, arguments, env, output, label, timeout=120):
    """Bound both streams and persist partial evidence even when pipes stay open."""
    executable = Path(executable)
    buffers = {name: bytearray() for name in ["stdout", "stderr"]}
    counts = {name: 0 for name in buffers}
    timed_out = pipe_failure = startup_failed = not_reaped = False
    proc = None
    selector = selectors.DefaultSelector()

    def kill_group():
        if proc is not None:
            try:
                os.killpg(proc.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass

    try:
        proc = subprocess.Popen(
            [str(executable), *arguments],
            cwd=output,
            env=env,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            start_new_session=True,
        )
        for name, pipe in [("stdout", proc.stdout), ("stderr", proc.stderr)]:
            os.set_blocking(pipe.fileno(), False)
            selector.register(pipe, selectors.EVENT_READ, name)
        deadline = time.monotonic() + timeout
        exit_deadline = force_close = None
        while selector.get_map():
            now = time.monotonic()
            if proc.poll() is None and now >= deadline:
                timed_out = True
                kill_group()
            if proc.poll() is not None and exit_deadline is None:
                exit_deadline = now + 1
            if exit_deadline is not None and now >= exit_deadline:
                pipe_failure = True
                kill_group()
                if force_close is None:
                    force_close = now + 0.2
                elif now >= force_close:
                    break
            for key, _ in selector.select(timeout=0.05):
                try:
                    block = os.read(key.fileobj.fileno(), 8192)
                except BlockingIOError:
                    continue
                if not block:
                    selector.unregister(key.fileobj)
                    continue
                name = key.data
                counts[name] += len(block)
                buffers[name].extend(block)
                if len(buffers[name]) > OUTPUT_LIMIT:
                    del buffers[name][:-OUTPUT_LIMIT]
        try:
            proc.wait(timeout=max(0, deadline - time.monotonic()))
        except subprocess.TimeoutExpired:
            timed_out = True
            kill_group()
            try:
                proc.wait(timeout=1)
            except subprocess.TimeoutExpired:
                not_reaped = True
    except OSError as error:
        startup_failed = True
        message = str(error).encode()
        buffers["stderr"].extend(message[:OUTPUT_LIMIT])
        counts["stderr"] += len(message)
        kill_group()
    finally:
        selector.close()
        if proc is not None:
            for pipe in [proc.stdout, proc.stderr]:
                if pipe is not None:
                    pipe.close()
    collected = {
        name: bytes(data).decode("utf-8", errors="replace")
        for name, data in buffers.items()
    }
    roots = [env["HOME"], str(output), str(executable.parent.parent.parent)]
    record = {
        "label": label,
        "arguments": [sanitize(str(a), roots) for a in arguments],
        "exit_code": proc.returncode if proc is not None else None,
        "timed_out": timed_out,
        "output_pipe_failure": pipe_failure,
        "startup_failed": startup_failed,
        "process_not_reaped": not_reaped,
        "diagnostics_rejected": False,
        "output_truncated": any(n > OUTPUT_LIMIT for n in counts.values()),
        "output_bytes": counts,
    }
    for name in ["stdout", "stderr"]:
        path = output / (label + "." + name + ".txt")
        if counts[name] > OUTPUT_LIMIT:
            retained, rejected = "<output omitted: exceeded bounded retention>\n", False
        else:
            retained, rejected = sanitize_output(collected[name], roots)
        record["diagnostics_rejected"] |= rejected
        path.write_text(retained)
        record[name + "_sha256"] = sha(path)
    (output / (label + ".json")).write_text(json.dumps(record, indent=2) + "\n")
    return record, collected


def diagnostics_complete(record):
    return not any(
        record.get(flag, False)
        for flag in [
            "timed_out",
            "output_truncated",
            "output_pipe_failure",
            "startup_failed",
            "process_not_reaped",
            "diagnostics_rejected",
        ]
    )


def completed(record):
    return record["exit_code"] == 0 and diagnostics_complete(record)


def model_plan(payload, installed=False):
    rows = payload.get("models", [])
    if (
        payload.get("engine") != "dbnet_trocr"
        or len(rows) != 2
        or {r.get("name") for r in rows} != set(MODELS)
        or any(
            r.get("requires_consent") is not False
            or str(r.get("license", "")).lower() != MODELS[r["name"]]
            for r in rows
        )
        or not isinstance(payload.get("missing"), list)
        or not set(payload["missing"]) <= set(MODELS)
        or (
            installed
            and (
                payload["missing"] or any(r.get("installed") is not True for r in rows)
            )
        )
    ):
        raise ValueError("Only the exact permissive DBNet/TrOCR model plan is allowed")
    return bool(payload["missing"])


def verify_models(home, output):
    verified = {}
    for filename, (size, digest) in MODEL_PINS.items():
        matches = list(home.rglob(filename))
        if len(matches) != 1:
            raise ValueError(
                "Expected one app-provisioned model in isolated cache: " + filename
            )
        model = matches[0]
        if (
            model.is_symlink()
            or not model.is_file()
            or not model.resolve().is_relative_to(home.resolve())
            or model.stat().st_size != size
            or sha(model) != digest
        ):
            raise ValueError(
                "App-provisioned model differs from independently pinned bytes: "
                + filename
            )
        verified[filename] = {"bytes": size, "sha256": digest}
    (output / "verified-permissive-models.json").write_text(
        json.dumps(verified, indent=2) + "\n"
    )
    return verified


def ocr_pass(record, raw):
    return completed(record) and all(
        word in raw["stdout"].lower()
        for word in ["ocr_page", "cobalt", "warranty", "2031"]
    )


def doctor_pass(payload):
    fields = {
        "tesseract_installed",
        "ocrs_models_available",
        "paddle_ocr_available",
        "pdf_extract_compiled_in",
        "fuse_compiled",
        "fuse_runtime_available",
        "embedder_model_cached",
        "lance_dir_exists",
    }
    return (
        isinstance(payload, dict)
        and set(payload) == fields
        and payload["pdf_extract_compiled_in"] is True
        and all(type(value) is bool for value in payload.values())
    )


def probe(apps, fixture, env, output):
    records = []
    originals = []
    missing_failures = []
    good = True
    args = [
        "ocr",
        str(fixture),
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
    for index, app in enumerate(apps):
        exe = app / "Contents/MacOS/crispsorter"
        for name, arguments in [
            ("version", ["--format", "json", "version"]),
            ("doctor", ["--format", "json", "doctor"]),
        ]:
            rec, raw = command(exe, arguments, env, output, f"package-{index}-{name}")
            records.append(rec)
            payload = json.loads(raw["stdout"]) if completed(rec) else None
            good &= completed(rec) and isinstance(payload, dict)
            if name == "doctor":
                good &= doctor_pass(payload)
            if name == "version":
                good &= payload == {
                    "name": "crispsorter",
                    "version": "0.15.1",
                    "target": "macos",
                    "arch": "aarch64",
                }
        rec, raw = command(
            exe, ["--crispsorter-ocr-worker"], env, output, f"package-{index}-worker"
        )
        records.append(rec)
        good &= (
            rec["exit_code"] == 1
            and diagnostics_complete(rec)
            and "OCR worker requires request and response paths" in raw["stderr"]
        )
        rec, raw = command(exe, args, env, output, f"package-{index}-original-ocr", 600)
        records.append(rec)
        matched = ocr_pass(rec, raw)
        rec["fixture_evidence_matched"] = matched
        originals.append(matched)
        missing_failures.append(
            rec["exit_code"] == 1
            and diagnostics_complete(rec)
            and "missing OCR models:" in raw["stderr"]
        )
    provisioned = False
    if not all(originals):
        exe = apps[0] / "Contents/MacOS/crispsorter"
        rec, raw = command(
            exe,
            ["--format", "json", "models", "plan", "dbnet_trocr"],
            env,
            output,
            "model-plan",
        )
        records.append(rec)
        if not completed(rec):
            return False, records, provisioned
        missing = model_plan(json.loads(raw["stdout"]))
        if not missing or not all(
            ok or missing for ok, missing in zip(originals, missing_failures)
        ):
            return False, records, provisioned
        rec, raw = command(
            exe,
            ["--format", "json", "models", "pull", "dbnet_trocr"],
            env,
            output,
            "permissive-model-pull",
            600,
        )
        records.append(rec)
        if not completed(rec):
            return False, records, provisioned
        model_plan(json.loads(raw["stdout"]), installed=True)
        verify_models(Path(env["HOME"]), output)
        provisioned = True
        for index, app in enumerate(apps):
            rec, raw = command(
                app / "Contents/MacOS/crispsorter",
                args,
                env,
                output,
                f"package-{index}-retry-ocr",
                600,
            )
            rec["fixture_evidence_matched"] = ocr_pass(rec, raw)
            records.append(rec)
            good &= rec["fixture_evidence_matched"]
    return bool(good), records, provisioned


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", required=True)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    guard(os.environ)
    if sys.platform != "darwin":
        raise ValueError("Replay requires macOS")
    tooling = Path(__file__).resolve().parents[1]
    output = args.output.resolve()
    output.mkdir()
    result = {
        "schema": "crispsorter-macos-existing-package-observation-v1",
        "status": "failed",
        "signing": "Unsigned-labelled original assets; no signing/notarization assessment or claim",
        "satisfies_publication_gate": False,
        "license_consent_granted": False,
        "qualification": "Bounded packaged CLI fixture control only; no GUI/device/corpus qualification",
    }
    mount = None
    try:
        result.update(source_identity(args.source.resolve(), tooling))
        result["draft"] = draft(args.repo)
        assets = output / "packages"
        assets.mkdir()
        for name in ASSETS:
            subprocess.run(
                [
                    "gh",
                    "release",
                    "download",
                    TAG,
                    "--repo",
                    args.repo,
                    "--pattern",
                    name,
                    "--dir",
                    str(assets),
                ],
                check=True,
                capture_output=True,
            )
            verify_asset(assets / name)
        draft(args.repo)
        result["packages"] = {
            name: {"bytes": size, "sha256": digest}
            for name, (size, digest) in ASSETS.items()
        }
        extracted = output / "extracted"
        extracted.mkdir()
        app = extract_tar(assets / "CrispSorter_aarch64-unsigned.app.tar.gz", extracted)
        mount = output / "mount"
        mount.mkdir()
        subprocess.run(
            [
                "hdiutil",
                "attach",
                "-readonly",
                "-nobrowse",
                "-mountpoint",
                str(mount),
                str(assets / "CrispSorter_0.15.1_aarch64-unsigned.dmg"),
            ],
            check=True,
            capture_output=True,
        )
        apps = list(mount.glob("*.app"))
        if len(apps) != 1:
            raise ValueError("DMG must contain exactly one app")
        identity = app_identity(app)
        if app_identity(apps[0]) != identity:
            raise ValueError("DMG and archive application contents differ")
        result["application_identity"] = identity
        result["status"] = "running"
        (output / "observation.json").write_text(json.dumps(result, indent=2) + "\n")
        passed, records, provisioned = probe(
            [app, apps[0]],
            args.source.resolve() / "tests/quality/ocr-task.pdf",
            app_env(output / "isolated-home"),
            output,
        )
        result.update(
            status="passed" if passed else "failed",
            commands=records,
            permissive_models_provisioned=provisioned,
            original_ocr_results={
                row["label"]: row["fixture_evidence_matched"]
                for row in records
                if "original-ocr" in row["label"]
            },
            retry_ocr_results={
                row["label"]: row["fixture_evidence_matched"]
                for row in records
                if "retry-ocr" in row["label"]
            },
        )
        if app_identity(app) != identity or app_identity(apps[0]) != identity:
            raise ValueError("Application contents changed during packaged execution")
        draft(args.repo)
        final_identity = source_identity(args.source.resolve(), tooling)
        if final_identity != {key: result[key] for key in final_identity}:
            raise ValueError("Source identity changed during packaged execution")
        for name in ASSETS:
            verify_asset(assets / name)
        if not passed:
            raise ValueError(
                "Packaged control failed; inspect retained original and retry diagnostics"
            )
    except Exception as error:
        result["status"] = "failed"
        result["error_type"] = type(error).__name__
        result["error"] = sanitize(str(error), [str(output), str(args.source)])
        raise SystemExit(
            "macOS package replay failed; inspect retained sanitized observation and command outputs"
        ) from None
    finally:
        if mount is not None:
            detached = subprocess.run(
                ["hdiutil", "detach", str(mount)], capture_output=True
            )
            if detached.returncode:
                result["status"] = "failed"
                result["detach_failed"] = True
        (output / "observation.json").write_text(json.dumps(result, indent=2) + "\n")
    if result["status"] != "passed":
        raise SystemExit(1)


if __name__ == "__main__":
    main()
