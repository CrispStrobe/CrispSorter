#!/usr/bin/env python3
"""Fetch frozen public evaluation inputs; never put weights or corpus text in Git."""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import urllib.request
import zipfile

from prepare_native_retrieval import MODEL_ID, MODEL_REVISION, MODEL_SHA256, TOKENIZER_SHA256

DATA_URL = "https://public.ukp.informatik.tu-darmstadt.de/thakur/BEIR/datasets/scifact.zip"
DATA_SHA256 = "536e14446a0ba56ed1398ab1055f39fe852686ecad24a6306c80c490fa8e0165"


def digest(path: Path) -> str:
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def download(url: str, destination: Path, expected: str, maximum: int) -> dict:
    destination.parent.mkdir(parents=True, exist_ok=True)
    if destination.exists():
        if digest(destination) != expected:
            raise ValueError(f"Existing {destination.name} has the wrong hash; no replacement was attempted")
    else:
        staging = destination.with_suffix(destination.suffix + ".partial")
        try:
            request = urllib.request.Request(url, headers={"User-Agent": "CrispSorter-judged-evaluation/1"})
            with urllib.request.urlopen(request, timeout=120) as response, staging.open("wb") as stream:
                size = 0
                while block := response.read(1024 * 1024):
                    size += len(block)
                    if size > maximum:
                        raise ValueError("Download exceeded its frozen input size budget")
                    stream.write(block)
            if digest(staging) != expected:
                raise ValueError(f"Downloaded {destination.name} differs from the frozen input")
            staging.replace(destination)
        finally:
            staging.unlink(missing_ok=True)
    return {"url": url, "sha256": expected, "bytes": destination.stat().st_size}


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True, help="Directory outside the Git checkout")
    args = parser.parse_args()
    output = args.output.resolve()
    repository = Path(__file__).resolve().parents[2]
    if output == repository or repository in output.parents:
        parser.error("Keep public corpus text and model weights outside the Git checkout")
    output.mkdir(parents=True, exist_ok=True)
    archive = output / "scifact.zip"
    receipt = {"dataset": download(DATA_URL, archive, DATA_SHA256, 4 * 1024 * 1024), "model": {}}
    with zipfile.ZipFile(archive) as container:
        for name in ["scifact/corpus.jsonl", "scifact/queries.jsonl", "scifact/qrels/test.tsv"]:
            info = container.getinfo(name)
            if not 0 < info.file_size <= 32 * 1024 * 1024:
                raise ValueError("Unexpected dataset member size")
            destination = output / name
            destination.parent.mkdir(parents=True, exist_ok=True)
            data = container.read(info)
            if destination.exists() and destination.read_bytes() != data:
                raise ValueError("Existing dataset member differs from the frozen archive")
            destination.write_bytes(data)
    base = f"https://huggingface.co/{MODEL_ID}/resolve/{MODEL_REVISION}/"
    for name, expected in [("onnx/model.onnx", MODEL_SHA256), ("tokenizer.json", TOKENIZER_SHA256)]:
        receipt["model"][name] = download(base + name, output / "minilm" / name, expected, 96 * 1024 * 1024)
    (output / "download-receipt.json").write_text(json.dumps(receipt, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({"completed": True, "dataDir": str(output / "scifact"), "modelDir": str(output / "minilm")}))


if __name__ == "__main__":
    main()
