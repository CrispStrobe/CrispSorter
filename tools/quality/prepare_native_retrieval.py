#!/usr/bin/env python3
"""Prepare hash-checked public retrieval inputs for the real native index.

The model is an existing application option, run through ONNX Runtime on CPU.
This script performs inference; it never substitutes random or fixed vectors.
Raw model/data downloads stay outside the repository. Native retrieval and
metric calculation run separately, so encoding and search latency stay distinct.
"""
from __future__ import annotations

import argparse
import csv
import hashlib
import json
import os
from pathlib import Path
import platform
import time


MODEL_ID = "sentence-transformers/all-MiniLM-L6-v2"
MODEL_REVISION = "1110a243fdf4706b3f48f1d95db1a4f5529b4d41"
MODEL_SHA256 = "6fd5d72fe4589f189f8ebc006442dbb529bb7ce38f8082112682524616046452"
TOKENIZER_SHA256 = "be50c3628f2bf5bb5e3a7f17b1f74611b2561a3a27eeab05e5aa30f411572037"


def digest(path: Path) -> str:
    result = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            result.update(block)
    return result.hexdigest()


def read_rows(path: Path) -> list[dict]:
    rows, seen = [], set()
    with path.open(encoding="utf-8") as stream:
        for line in stream:
            if not line.strip():
                continue
            row = json.loads(line)
            identity = row.get("_id")
            if not isinstance(identity, str) or not identity or identity in seen:
                raise ValueError(f"Missing or duplicate identity in {path.name}")
            if not isinstance(row.get("text"), str) or not isinstance(row.get("title", ""), str):
                raise ValueError(f"Invalid text fields in {path.name}")
            seen.add(identity)
            rows.append(row)
    if not rows:
        raise ValueError(f"No rows in {path.name}")
    return rows


def judgments(path: Path, documents: set[str], queries: set[str]) -> dict[str, dict[str, int]]:
    result: dict[str, dict[str, int]] = {}
    with path.open(encoding="utf-8", newline="") as stream:
        for row in csv.DictReader(stream, delimiter="\t"):
            query, document, gain = row["query-id"], row["corpus-id"], int(row["score"])
            if query not in queries or document not in documents or gain < 0:
                raise ValueError("A judgment names missing data or an invalid gain")
            if document in result.setdefault(query, {}):
                raise ValueError("Duplicate relevance judgment")
            result[query][document] = gain
    if not result or any(not any(gain > 0 for gain in values.values()) for values in result.values()):
        raise ValueError("Every evaluated query must have at least one positive judgment")
    return result


def write_json(path: Path, value: object) -> None:
    staging = path.with_suffix(path.suffix + ".partial")
    staging.write_text(json.dumps(value, ensure_ascii=False, indent=2, allow_nan=False) + "\n", encoding="utf-8")
    staging.replace(path)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--data-dir", type=Path, required=True, help="BEIR directory containing corpus.jsonl, queries.jsonl and qrels/test.tsv")
    parser.add_argument("--model-dir", type=Path, required=True, help="Pinned model directory containing tokenizer.json and onnx/model.onnx")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--corpus-name", default="BEIR SciFact test")
    parser.add_argument("--batch-size", type=int, default=16)
    parser.add_argument("--threads", type=int, default=2)
    args = parser.parse_args()
    if not 1 <= args.batch_size <= 64 or not 1 <= args.threads <= 16:
        parser.error("Use 1–64 items per batch and 1–16 CPU threads")
    import numpy as np
    import onnxruntime as ort
    import tokenizers
    from tokenizers import Tokenizer

    model, tokenizer_file = args.model_dir / "onnx/model.onnx", args.model_dir / "tokenizer.json"
    if digest(model) != MODEL_SHA256 or digest(tokenizer_file) != TOKENIZER_SHA256:
        raise ValueError("The model/tokenizer does not match the frozen public revision")
    paths = {name: args.data_dir / name for name in ["corpus.jsonl", "queries.jsonl", "qrels/test.tsv"]}
    corpus, all_queries = read_rows(paths["corpus.jsonl"]), read_rows(paths["queries.jsonl"])
    truth = judgments(paths["qrels/test.tsv"], {row["_id"] for row in corpus}, {row["_id"] for row in all_queries})
    queries = [row for row in all_queries if row["_id"] in truth]
    if len(queries) != len(truth):
        raise ValueError("Evaluated query count changed while selecting the test split")
    args.output.mkdir(parents=True, exist_ok=True)
    settings = ort.SessionOptions()
    settings.intra_op_num_threads, settings.inter_op_num_threads = args.threads, 1
    session = ort.InferenceSession(str(model), sess_options=settings, providers=["CPUExecutionProvider"])
    tokenizer = Tokenizer.from_file(str(tokenizer_file))
    tokenizer.enable_truncation(max_length=256)
    tokenizer.enable_padding(pad_id=0, pad_token="[PAD]")
    inputs = {item.name for item in session.get_inputs()}
    if inputs != {"input_ids", "attention_mask", "token_type_ids"}:
        raise ValueError("Unexpected model input contract")

    def encode(texts: list[str]):
        encoded = tokenizer.encode_batch(texts)
        feed = {"input_ids": np.asarray([item.ids for item in encoded], dtype=np.int64),
                "attention_mask": np.asarray([item.attention_mask for item in encoded], dtype=np.int64),
                "token_type_ids": np.asarray([item.type_ids for item in encoded], dtype=np.int64)}
        started = time.perf_counter()
        tokens = session.run(["last_hidden_state"], feed)[0]
        mask = feed["attention_mask"][..., None].astype(np.float32)
        vectors = (tokens * mask).sum(axis=1) / np.maximum(mask.sum(axis=1), 1)
        norms = np.linalg.norm(vectors, axis=1, keepdims=True)
        if vectors.shape != (len(texts), 384) or not np.isfinite(vectors).all() or np.any(norms <= 0):
            raise ValueError("Model returned invalid embeddings")
        vectors = vectors / norms
        return vectors.astype("<f4"), time.perf_counter() - started, sum(bool(item.overflowing) for item in encoded)

    _, warmup_seconds, _ = encode(["Warm up the configured retrieval model."])
    timings = {}
    for kind, rows in [("documents", corpus), ("queries", queries)]:
        target = args.output / f"{kind}.f32"
        staging = target.with_suffix(".f32.partial")
        batches, truncated = [], 0
        with staging.open("wb") as stream:
            for offset in range(0, len(rows), args.batch_size):
                batch = rows[offset:offset + args.batch_size]
                texts = [(row.get("title", "") + "\n" + row["text"]).strip() if kind == "documents" else row["text"] for row in batch]
                vectors, seconds, clipped = encode(texts)
                stream.write(vectors.tobytes(order="C"))
                batches.append({"rows": len(batch), "seconds": seconds})
                truncated += clipped
                if offset == 0 or offset % (args.batch_size * 32) == 0:
                    print(json.dumps({"encoding": kind, "complete": min(offset + len(batch), len(rows)), "total": len(rows)}), flush=True)
        if staging.stat().st_size != len(rows) * 384 * 4:
            raise ValueError("Embedding output length does not match its declared rows")
        staging.replace(target)
        timings[kind] = {"rows": len(rows), "truncatedRows": truncated, "batches": batches,
                         "inferenceSeconds": sum(item["seconds"] for item in batches), "sha256": digest(target)}

    write_json(args.output / "documents.json", [{"id": row["_id"], "title": row.get("title", ""),
               "text": (row.get("title", "") + "\n" + row["text"]).strip()} for row in corpus])
    write_json(args.output / "queries.json", [{"id": row["_id"], "text": row["text"], "relevance": truth[row["_id"]]} for row in queries])
    write_json(args.output / "preparation.json", {
        "schema": "crispsorter-native-retrieval-input-v1", "completed": True, "corpus": args.corpus_name,
        "inputs": {name: {"sha256": digest(path), "bytes": path.stat().st_size} for name, path in paths.items()},
        "model": {"id": MODEL_ID, "revision": MODEL_REVISION, "sha256": MODEL_SHA256,
                  "tokenizerSha256": TOKENIZER_SHA256, "dimensions": 384, "maximumTokens": 256,
                  "pooling": "attention-masked mean, L2 normalization", "documentText": "title + newline + body",
                  "provider": "CPUExecutionProvider", "threads": args.threads, "warmupSeconds": warmup_seconds},
        "runtime": {"python": platform.python_version(), "onnxruntime": ort.__version__, "tokenizers": tokenizers.__version__,
                    "numpy": np.__version__, "platform": platform.platform(), "machine": platform.machine(), "logicalCpus": os.cpu_count()},
        "encoding": timings,
        "preparedFiles": {name: {"sha256": digest(args.output / name), "bytes": (args.output / name).stat().st_size}
                          for name in ["documents.json", "queries.json", "documents.f32", "queries.f32"]},
        "scope": "Public English scientific-abstract retrieval; not a user-corpus, OCR, multilingual, or full-desktop qualification",
    })
    print(json.dumps({"completed": True, "documents": len(corpus), "queries": len(queries), "output": str(args.output)}), flush=True)


if __name__ == "__main__":
    main()
