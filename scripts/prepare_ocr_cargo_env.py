#!/usr/bin/env python3
"""Keep an explicitly built OCR runtime visible to nested mobile Cargo calls.

Run in an isolated CI checkout after the runtime has been built and qualified.
Tauri's iOS Xcode invocation replaces its environment, so an exported custom
variable alone does not reach Cargo. Cargo's forced [env] value does. This
helper only supplies the path; it does not qualify an archive, select a target,
download a runtime, or change CrispEmbed's cross-compilation checks.

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


def prepare(project: Path, runtime_lib_dir: Path) -> Path:
    """Append one forced Cargo environment value without rewriting other TOML.

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
    requested = {"value": str(runtime_lib_dir), "force": True}
    if VARIABLE in existing_env:
        existing = existing_env[VARIABLE]
        if isinstance(existing, dict) and existing == requested and existing.get("force") is True:
            return config
        raise ValueError(f"existing Cargo {VARIABLE} conflicts with the requested forced path")

    # A separate child table extends ordinary [env] tables without changing
    # comments, target settings, or unrelated variables. JSON string escaping
    # is valid for the path characters accepted by a TOML basic string here.
    addition = (
        f"\n\n{MARKER}\n"
        f"[env.{VARIABLE}]\n"
        f"value = {json.dumps(str(runtime_lib_dir), ensure_ascii=False)}\n"
        "force = true\n"
    )
    candidate_text = original_text + addition
    try:
        result = tomllib.loads(candidate_text)
    except tomllib.TOMLDecodeError as error:
        raise ValueError("Cargo config cannot be extended without rewriting existing TOML") from error
    expected = copy.deepcopy(parsed)
    expected.setdefault("env", {})[VARIABLE] = requested
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
    args = parser.parse_args()
    try:
        config = prepare(args.project, args.runtime_lib_dir)
    except (OSError, UnicodeError, ValueError) as error:
        parser.error(str(error))
    print(f"Pinned {VARIABLE} in {config}")


if __name__ == "__main__":
    main()
