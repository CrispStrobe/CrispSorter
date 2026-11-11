#!/usr/bin/env python3
"""Measure the production system OCR adapter on all six frozen public pages.

Source images, transcripts, and predictions stay outside the repository. The
summary retains hashes, licenses, all failures, accuracy, latency, and native
word-evidence counts. All assets and language packs are checked before inference.
"""
from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import subprocess
import urllib.request

from real_ocr_panel import child


def digest(path: Path) -> str:
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def download(url: str, path: Path, expected: str, maximum: int) -> Path:
    if path.exists():
        if digest(path) != expected:
            raise ValueError(f"Frozen input changed: {path.name}")
        return path
    path.parent.mkdir(parents=True, exist_ok=True)
    staging = path.with_suffix(path.suffix + ".partial")
    try:
        request = urllib.request.Request(url, headers={"User-Agent": "CrispSorter-frozen-OCR-panel/1"})
        with urllib.request.urlopen(request, timeout=120) as response, staging.open("wb") as stream:
            count = 0
            while block := response.read(1024 * 1024):
                count += len(block)
                if count > maximum:
                    raise ValueError("Frozen input exceeds its byte budget")
                stream.write(block)
        if digest(staging) != expected:
            raise ValueError(f"Downloaded input differs from the frozen receipt: {path.name}")
        staging.replace(path)
        return path
    finally:
        staging.unlink(missing_ok=True)


def save(path: Path, value: dict) -> None:
    stage = path.with_suffix(".json.partial")
    stage.write_text(json.dumps(value, indent=2, ensure_ascii=False, allow_nan=False) + "\n", encoding="utf-8")
    stage.replace(path)


def safe_diagnostics(value: dict) -> dict:
    """Keep reproducible settings/evidence checks without source text or paths."""
    fields = {"alignment_verified", "confidence_calibrated", "confidence_source", "coordinate_space", "elapsed_ms",
              "engine", "engine_version", "internal_cleanup", "language", "page_segmentation_mode", "word_count", "word_offset_unit"}
    return {key: value[key] for key in fields if key in value}


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input-dir", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--executable", type=Path, required=True)
    parser.add_argument("--plan", type=Path)
    parser.add_argument("--prepare-only", action="store_true")
    args = parser.parse_args()
    if os.name != "posix":
        parser.error("This measurement wrapper needs POSIX process-group and resource accounting; the native adapter has its own platform gates")
    repository = Path(__file__).resolve().parents[2]
    plan_path = (args.plan or repository / "tests/quality/system-ocr-panel.json").resolve()
    plan = json.loads(plan_path.read_text())
    if plan.get("schema") != "crispsorter-system-ocr-panel-v1" or plan.get("frozenBeforeInference") is not True:
        raise ValueError("Expected a frozen system OCR plan")
    source_plan_path = repository / plan["sourcePlan"]
    if digest(source_plan_path) != plan["sourcePlanSha256"]:
        raise ValueError("The original public-page selection changed")
    source_plan = json.loads(source_plan_path.read_text())
    selected = {case["id"]: case for case in source_plan["cases"]}
    if {case["id"] for case in plan["cases"]} != set(selected) or len(plan["cases"]) != len(selected):
        raise ValueError("Every frozen page must be attempted exactly once")
    cache, output, executable = args.input_dir.resolve(), args.output.resolve(), args.executable.resolve()
    for directory in [cache, output]:
        if directory == repository or repository in directory.parents:
            parser.error("Keep raw evaluation assets and predictions outside the Git checkout")
    output.mkdir(parents=True, exist_ok=True)
    if (output / "result.json").exists():
        parser.error("Choose a new output directory to preserve previous measurements")
    base = "https://raw.githubusercontent.com/CrispStrobe/CrispEmbed/" + source_plan["upstream_source"] + "/"
    manifest_file = download(base + source_plan["manifest_path"], cache / "manifest.json", source_plan["manifest_sha256"], 4 * 1024 * 1024)
    scorer_file = download(base + source_plan["scorer_path"], cache / "score.py", source_plan["scorer_sha256"], 1024 * 1024)
    pages = {page["id"]: page for page in json.loads(manifest_file.read_text())["pages"]}
    report = {"schema": "crispsorter-system-ocr-result-v1", "completed": False, "all_cases_succeeded": False,
              "scope": plan["scope"], "decision_rule": plan["decisionRule"], "plan_sha256": digest(plan_path),
              "source_plan_sha256": digest(source_plan_path), "manifest_sha256": digest(manifest_file), "scorer_sha256": digest(scorer_file),
              "source_revision": source_plan["upstream_source"], "images": [], "runs": [], "packs": plan["packs"],
              "platform": platform.platform(), "logical_cpus": os.cpu_count(), "python": platform.python_version(),
              "omp_thread_limit": plan["ompThreadLimit"], "case_timeout_seconds": plan["caseTimeoutSeconds"],
              "application_revision": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=repository, text=True).strip(),
              "adapter_sha256": digest(repository / "src-tauri/src/ocr_word_evidence.rs"),
              "wrapper_sha256": digest(repository / "tests/ocr-word-contract/main.rs")}
    save(output / "result.json", report)
    inputs = {}
    for case in plan["cases"]:
        source = selected[case["id"]]
        page = pages[case["id"]]
        if hashlib.sha256(page["ground_truth"].encode()).hexdigest() != source["ground_truth_sha256"]:
            raise ValueError("Frozen reference text changed")
        image = page["image"]
        if image["sha256"] != source["image_sha256"]:
            raise ValueError("Page image receipt differs from the frozen selection")
        if "hf_parquet" in image:
            info = image["hf_parquet"]
            parquet = download(info["url"], cache / "cord.parquet", info["sha256"], 256 * 1024 * 1024)
            import pyarrow.parquet as pq
            data = pq.read_table(parquet, columns=["image"]).slice(info["row"], 1).to_pylist()[0]["image"]["bytes"]
            path = cache / (case["id"] + ".png")
            if len(data) > 32 * 1024 * 1024 or hashlib.sha256(data).hexdigest() != source["image_sha256"]:
                raise ValueError("Frozen receipt image changed")
            if path.exists() and digest(path) != source["image_sha256"]:
                raise ValueError("Existing receipt image changed")
            path.write_bytes(data)
            # Release the full receipt archive before launching native OCR.
            # The selected image is already written and hash-verified.
            del data
            import gc
            import pyarrow
            gc.collect()
            pyarrow.default_memory_pool().release_unused()
        else:
            path = download(image["url"], cache / (case["id"] + ".jpg"), source["image_sha256"], 32 * 1024 * 1024)
        inputs[case["id"]] = path
        report["images"].append({"id": case["id"], "sha256": digest(path), "ground_truth_sha256": source["ground_truth_sha256"],
                                 "image_license": page["image_license"], "ground_truth_license": page["ground_truth_license"],
                                 "ground_truth_provenance": page["ground_truth_provenance"], "primary_metric": source["primary_metric"]})
    for pack in plan["packs"]:
        download(pack["url"], cache / "tessdata" / pack["name"], pack["sha256"], pack["bytes"])
    report["inputs_verified"] = True
    save(output / "result.json", report)
    if args.prepare_only:
        print(json.dumps({"inputsVerified": True, "pages": len(inputs), "inferenceRun": False}))
        return
    if not executable.is_file():
        raise ValueError("Build the production OCR harness executable before running inference")
    temporary = output / "temporary"
    temporary.mkdir(exist_ok=True)
    environment = {**os.environ, "TESSDATA_PREFIX": str(cache / "tessdata"), "OMP_THREAD_LIMIT": str(plan["ompThreadLimit"]), "TMPDIR": str(temporary)}
    capabilities = json.loads(subprocess.check_output([str(executable), "capabilities"], env=environment, text=True, timeout=15))
    report["capabilities"] = capabilities
    save(output / "result.json", report)
    required = {language for case in plan["cases"] for language in case["language"].split("+")}
    if not capabilities.get("available") or not required.issubset(set(capabilities.get("languages", []))):
        raise ValueError("The native adapter cannot load every frozen language pack")
    report["executable_sha256"] = digest(executable)
    spec = importlib.util.spec_from_file_location("frozen_system_ocr_scorer", scorer_file)
    scoring = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(scoring)
    save(output / "result.json", report)
    for case in plan["cases"]:
        identity = case["id"]
        payload_path, log = output / (identity + ".json"), output / (identity + ".log")
        measured = child([str(executable), str(inputs[identity]), case["language"], str(payload_path)], log,
                         plan["caseTimeoutSeconds"], env=environment)
        # wait4 observes the launch process and inherited fork memory. Do not
        # present this as the OCR engine's isolated peak allocation.
        measured["launcher_peak_rss_mb"] = measured.pop("peak_rss_mb")
        row = {"id": identity, "language": case["language"], "primary_metric": selected[identity]["primary_metric"], **measured}
        if measured["exit_code"] == 0:
            payload = json.loads(payload_path.read_text())
            words = [word for region in payload["regions"] for word in region["word_evidence"]["words"]]
            row.update(scoring.score(payload["text"], pages[identity]["ground_truth"]))
            row.update(prediction_sha256=hashlib.sha256(payload["text"].encode()).hexdigest(), native_wall_seconds=payload["native_wall_seconds"],
                       diagnostics=safe_diagnostics(payload["diagnostics"]), warnings=payload["warnings"], regions=len(payload["regions"]),
                       evidenced_words=len(words), words_with_confidence=sum(word.get("confidence") is not None for word in words),
                       confidence_calibrated=False)
        else:
            row["error"] = log.read_text(errors="replace")[-2000:]
        report["runs"].append(row)
        save(output / "result.json", report)
        print(json.dumps(row, ensure_ascii=False, allow_nan=False), flush=True)
    report["completed"] = True
    report["all_cases_succeeded"] = all(row["exit_code"] == 0 for row in report["runs"])
    report["successful_cases"] = sum(row["exit_code"] == 0 for row in report["runs"])
    save(output / "result.json", report)


if __name__ == "__main__":
    main()
