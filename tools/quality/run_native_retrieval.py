#!/usr/bin/env python3
"""Compile and measure the production native index on prepared judged inputs.

Builds a focused adapter harness, freezes source hashes around compilation,
then executes the resulting binary. It does not use the harness model stub:
the benchmark supplies actual precomputed model vectors directly to the index.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import subprocess
import time


def digest(path: Path) -> str:
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def source_hashes(repository: Path) -> dict[str, str]:
    roots = ["crisp-index-server/src", "crisp-index-protocol/src", "crates/crisp-search-query/src",
             "tests/server-search-contract", "shared"]
    files = [path for root in roots for path in (repository / root).rglob("*.rs")]
    files += [repository / name for name in ["tests/server-search-contract/Cargo.toml", "tests/server-search-contract/Cargo.lock",
                                            "crisp-index-protocol/Cargo.toml", "crates/crisp-search-query/Cargo.toml"]]
    return {str(path.relative_to(repository)): digest(path) for path in sorted(files)}


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--profile", choices=["dev", "release"], default="dev",
                        help="Record the build profile; debug measurements are diagnostic, not release performance")
    args = parser.parse_args()
    repository = Path(__file__).resolve().parents[2]
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    if any((output / name).exists() for name in ["result.json", "execution.json", "queries.jsonl"]):
        parser.error("Choose a new output directory to preserve previous measurements")
    revision = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=repository, text=True).strip()
    status = subprocess.check_output(["git", "status", "--porcelain"], cwd=repository, text=True)
    before = source_hashes(repository)
    command = ["cargo", "test", "--locked", "--manifest-path", str(repository / "tests/server-search-contract/Cargo.toml"),
               "--no-run", "--message-format=json"]
    if args.profile == "release":
        command.append("--release")
    started = time.monotonic()
    with (output / "build.log").open("w", encoding="utf-8") as build_log:
        built = subprocess.run(command, cwd=repository, text=True, stdout=subprocess.PIPE, stderr=build_log)
    if built.returncode:
        raise RuntimeError(f"Native benchmark compilation failed; inspect {output / 'build.log'}")
    after = source_hashes(repository)
    if before != after:
        raise RuntimeError("A benchmark source changed during compilation; rerun once edits have settled")
    executables = []
    for line in built.stdout.splitlines():
        item = json.loads(line)
        if item.get("reason") == "compiler-artifact" and item.get("profile", {}).get("test") and item.get("executable"):
            executables.append(Path(item["executable"]))
    if len(executables) != 1:
        raise ValueError("Expected exactly one compiled native contract binary")
    executable = executables[0]
    execution = {"revision": revision, "dirtyWorkingTree": status.splitlines(), "sourceSha256": after,
                 "executableSha256": digest(executable), "buildProfile": args.profile, "buildSeconds": time.monotonic() - started,
                 "rustc": subprocess.check_output(["rustc", "-Vv"], text=True).strip(),
                 "platform": platform.platform(), "machine": platform.machine(), "logicalCpus": os.cpu_count(),
                 "nativeThreadEnvironment": {name: os.environ.get(name) for name in ["RAYON_NUM_THREADS", "OMP_NUM_THREADS", "OMP_THREAD_LIMIT", "TOKIO_WORKER_THREADS"]},
                 "scope": "Focused production native search/index harness; full desktop, network, and model-adapter integration are separate gates."}
    (output / "execution.json").write_text(json.dumps(execution, indent=2) + "\n", encoding="utf-8")
    environment = {**os.environ, "CRISP_QUALITY_INPUT": str(args.input.resolve()), "CRISP_QUALITY_OUTPUT": str(output),
                   "CRISP_QUALITY_REVISION": revision}
    environment.pop("RUST_MIN_STACK", None)
    with (output / "native.log").open("w", encoding="utf-8") as stream:
        completed = subprocess.run([str(executable), "native_judged_retrieval_benchmark", "--ignored", "--nocapture", "--test-threads=1"],
                                   cwd=repository, env=environment, stdout=stream, stderr=subprocess.STDOUT)
    if completed.returncode:
        raise RuntimeError(f"Native measurement failed; its partial results and log remain in {output}")
    result = json.loads((output / "result.json").read_text())
    if not result.get("completed") or len(result.get("modes", [])) != 4:
        raise ValueError("Native runner did not complete every planned comparison")
    result["execution"] = execution
    (output / "result.json").write_text(json.dumps(result, indent=2, allow_nan=False) + "\n", encoding="utf-8")
    print(json.dumps({"completed": True, "allQueriesSucceeded": result["all_queries_succeeded"],
                      "result": str(output / "result.json"), "metrics": result["modes"]}, allow_nan=False))


if __name__ == "__main__":
    main()
