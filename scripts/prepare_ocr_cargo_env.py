#!/usr/bin/env python3
"""Keep an explicitly built OCR runtime visible to nested mobile Cargo calls.

Run in an isolated CI checkout after the runtime has been built and qualified.
Tauri's iOS Xcode invocation replaces its environment, so an exported custom
variable alone does not reach Cargo. Cargo's forced [env] value does. The
optional host protoc path also reaches dependency build scripts that compile
protobuf schemas. This helper only supplies paths; it does not qualify an
archive, select a target, download a runtime, or change cross-compilation checks.

Use the repository workspace root for --project. npm-generated mobile build
scripts run Cargo from that root, while direct CLI calls may run it from
src-tauri. A root .cargo/config.toml is discoverable from either directory.
"""

from __future__ import annotations

import argparse
import copy
import json
import os
from pathlib import Path
import stat
import tempfile
import tomllib


VARIABLE = "CRISPEMBED_SYS_LIB_DIR"
MARKER = "# Paired OCR runtime for this isolated CI checkout."


def prepare(project: Path, runtime_lib_dir: Path, protoc: Path | None = None,
            ort_lib_dir: Path | None = None, ort_xcframework: Path | None = None) -> Path:
    """Append forced Cargo environment values without rewriting other TOML.

    An existing exact forced value is left byte-for-byte intact. Conflicting
    values, ambiguous legacy/current config files, and TOML that cannot be
    extended without rewriting it are rejected before writing anything.
    """
    project = project.resolve(strict=True)
    if not project.is_dir() or not (project / "Cargo.toml").is_file():
        raise ValueError("--project must be the Rust project directory containing Cargo.toml")
    if not runtime_lib_dir.is_absolute():
        raise ValueError("--runtime-lib-dir must be an absolute path")
    runtime_lib_dir = runtime_lib_dir.resolve(strict=True)
    if not runtime_lib_dir.is_dir():
        raise ValueError("--runtime-lib-dir must be a directory")
    if not any((runtime_lib_dir / name).is_file() for name in ("libcrispembed.a", "libcrispembed.so")):
        raise ValueError("runtime directory must contain libcrispembed.a or libcrispembed.so")
    requested_values = {VARIABLE: str(runtime_lib_dir)}
    if protoc is not None:
        if not protoc.is_absolute():
            raise ValueError("--protoc must be an absolute host executable path")
        protoc = protoc.resolve(strict=True)
        if not protoc.is_file() or not os.access(protoc, os.X_OK):
            raise ValueError("--protoc must name an executable file")
        requested_values["PROTOC"] = str(protoc)
    if ort_lib_dir is not None and ort_xcframework is not None:
        raise ValueError("select one Android library directory or iOS XCFramework, not both")
    if ort_lib_dir is not None:
        if not ort_lib_dir.is_absolute():
            raise ValueError("--ort-lib-dir must be an absolute directory")
        ort_lib_dir = ort_lib_dir.resolve(strict=True)
        if not (ort_lib_dir / "libonnxruntime.so").is_file():
            raise ValueError("--ort-lib-dir must contain the qualified libonnxruntime.so")
        requested_values.update({"ORT_LIB_LOCATION": str(ort_lib_dir), "ORT_PREFER_DYNAMIC_LINK": "1"})
    if ort_xcframework is not None:
        if not ort_xcframework.is_absolute():
            raise ValueError("--ort-xcframework must be an absolute directory")
        ort_xcframework = ort_xcframework.resolve(strict=True)
        if not (ort_xcframework / "Info.plist").is_file() or not any(
            (ort_xcframework / target / "onnxruntime.framework/onnxruntime").is_file()
            for target in ("ios-arm64", "ios-arm64_x86_64-simulator", "ios-arm64-simulator")
        ):
            raise ValueError("--ort-xcframework must contain the qualified iOS static framework")
        requested_values["ORT_IOS_XCFWK_LOCATION"] = str(ort_xcframework)

    cargo_dir = project / ".cargo"
    if cargo_dir.is_symlink() or (cargo_dir.exists() and not cargo_dir.is_dir()):
        raise ValueError("refusing to modify a symlink or non-directory .cargo path")
    legacy = cargo_dir / "config"
    current = cargo_dir / "config.toml"
    for candidate in (legacy, current):
        if candidate.is_symlink() or (candidate.exists() and not candidate.is_file()):
            raise ValueError(f"refusing to modify a symlink or non-file Cargo config: {candidate}")
    if legacy.exists() and current.exists():
        raise ValueError("both .cargo/config and .cargo/config.toml exist; resolve the ambiguity first")
    config = legacy if legacy.exists() else current
    original = config.read_bytes() if config.exists() else b""
    original_text = original.decode("utf-8")
    parsed = tomllib.loads(original_text)
    existing_env = parsed.get("env", {})
    if not isinstance(existing_env, dict):
        raise ValueError("existing Cargo env configuration is not a table")
    missing = {}
    for variable, value in requested_values.items():
        requested = {"value": value, "force": True}
        if variable in existing_env:
            existing = existing_env[variable]
            if isinstance(existing, dict) and existing == requested and existing.get("force") is True:
                continue
            raise ValueError(f"existing Cargo {variable} conflicts with the requested forced path")
        missing[variable] = requested
    if not missing:
        return config

    # A separate child table extends ordinary [env] tables without changing
    # comments, target settings, or unrelated variables. JSON string escaping
    # is valid for the path characters accepted by a TOML basic string here.
    addition = f"\n\n{MARKER}\n" + "\n".join(
        f"[env.{variable}]\n"
        f"value = {json.dumps(requested['value'], ensure_ascii=False)}\n"
        "force = true\n"
        for variable, requested in missing.items()
    )
    candidate_text = original_text + addition
    try:
        result = tomllib.loads(candidate_text)
    except tomllib.TOMLDecodeError as error:
        raise ValueError("Cargo config cannot be extended without rewriting existing TOML") from error
    expected = copy.deepcopy(parsed)
    expected.setdefault("env", {}).update(missing)
    if result != expected:
        raise ValueError("refusing a Cargo config edit that changes unrelated settings")

    cargo_dir.mkdir(exist_ok=True)
    mode = stat.S_IMODE(config.stat().st_mode) if config.exists() else 0o644
    temporary: Path | None = None
    try:
        with tempfile.NamedTemporaryFile(prefix=".ocr-config-", dir=cargo_dir, delete=False) as handle:
            temporary = Path(handle.name)
            handle.write(candidate_text.encode("utf-8"))
            handle.flush()
            os.fsync(handle.fileno())
        temporary.chmod(mode)
        os.replace(temporary, config)
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)
    return config


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--project", type=Path, required=True, help="Rust workspace root containing Cargo.toml; use . for the Tauri npm build")
    parser.add_argument("--runtime-lib-dir", type=Path, required=True, help="absolute directory of the qualified runtime")
    parser.add_argument("--protoc", type=Path, help="absolute host protoc executable for dependency build scripts")
    ort = parser.add_mutually_exclusive_group()
    ort.add_argument("--ort-lib-dir", type=Path, help="qualified Android ONNX Runtime library directory")
    ort.add_argument("--ort-xcframework", type=Path, help="qualified iOS ONNX Runtime XCFramework directory")
    args = parser.parse_args()
    try:
        config = prepare(args.project, args.runtime_lib_dir, args.protoc, args.ort_lib_dir, args.ort_xcframework)
    except (OSError, UnicodeError, ValueError) as error:
        parser.error(str(error))
    variables = f"{VARIABLE} and PROTOC" if args.protoc else VARIABLE
    if args.ort_lib_dir or args.ort_xcframework:
        variables += " and the qualified mobile ONNX Runtime"
    print(f"Pinned {variables} in {config}")


if __name__ == "__main__":
    main()
