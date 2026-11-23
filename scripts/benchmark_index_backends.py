#!/usr/bin/env python3
"""Compare isolated service costs on generated text and vectors, using Docker."""

import contextlib
import json
from pathlib import Path
import random
import statistics
import subprocess
import sys
import time
from urllib.request import Request, urlopen
import uuid


IMAGES = {
    "opensearch": {
        "image": "opensearchproject/opensearch@sha256:ffa260e43be96b49144fb7ececbb135c3fd7081567adc781c49ec9232ce1e47f",
        "tag": "3.10.0", "port": 9200, "compressed_bytes": 1160663754,
    },
    "qdrant": {
        "image": "qdrant/qdrant@sha256:c5e32ee251293d2e1f4dd34890ad75eeee5a4f3821a121d6d8b7e4323448b386",
        "tag": "v1.20.2", "port": 6333, "compressed_bytes": 74318344,
    },
}
COUNT = 4096
DIMENSIONS = 128
GROUPS = 8
BATCH = 256
REPETITIONS = 30


def docker(*arguments):
    return subprocess.check_output(["docker", *arguments], text=True, timeout=360).strip()


def call(base, endpoint, data=None, *, method=None, kind="application/json"):
    if data is not None and not isinstance(data, bytes):
        data = json.dumps(data, separators=(",", ":")).encode()
    request = Request(base + endpoint, data=data, method=method,
                      headers={"Content-Type": kind})
    with urlopen(request, timeout=30) as response:
        return json.load(response)


@contextlib.contextmanager
def service(backend):
    image = IMAGES[backend]
    start = time.perf_counter()
    docker("pull", image["image"])
    pull_s = time.perf_counter() - start
    disk_bytes = int(docker("image", "inspect", "--format", "{{.Size}}", image["image"]))
    name = "index-bench-" + uuid.uuid4().hex[:12]
    environment = {
        "discovery.type": "single-node",
        "OPENSEARCH_JAVA_OPTS": "-Xms1g -Xmx1g",
        "DISABLE_INSTALL_DEMO_CONFIG": "true",
        "DISABLE_SECURITY_PLUGIN": "true",
    } if backend == "opensearch" else {}
    env_args = [item for key, value in environment.items() for item in ("--env", f"{key}={value}")]
    start = time.perf_counter()
    docker("run", "--detach", "--name", name, "--memory", "3g", "--memory-swap", "3g",
           "--cpus", "2", "--publish", f"127.0.0.1::{image['port']}", *env_args, image["image"])
    try:
        address = docker("port", name, f"{image['port']}/tcp")
        base = f"http://{address}"
        health = "/" if backend == "opensearch" else "/collections"
        while True:
            try:
                call(base, health)
                break
            except OSError:
                if time.perf_counter() - start > 120:
                    raise
                time.sleep(0.5)
        yield base, name, {"cold_pull_s": pull_s, "installed_image_bytes": disk_bytes,
                           "startup_s": time.perf_counter() - start}
    except Exception:
        print(docker("logs", "--tail", "30", name), file=sys.stderr)
        raise
    finally:
        docker("rm", "--force", name)


def generate():
    generator = random.Random(1967)
    return [{
        "id": i,
        "group": i % GROUPS,
        "body": f"Unterlagen zum Sitzungsbeschluss Nummer {i}. Der Ausschuss prüft die Vorschläge.",
        "vec": [round(generator.uniform(-1, 1), 5) for _ in range(DIMENSIONS)],
    } for i in range(COUNT)]


def load_opensearch(base, docs):
    call(base, "/documents", {
        "settings": {"number_of_shards": 1, "number_of_replicas": 0,
                     "refresh_interval": "-1", "index.knn": True},
        "mappings": {"properties": {
            "group": {"type": "integer"},
            "body": {"type": "text"},
            "vec": {"type": "knn_vector", "dimension": DIMENSIONS,
                    "method": {"name": "hnsw", "space_type": "cosinesimil", "engine": "faiss"}},
        }},
    }, method="PUT")
    start = time.perf_counter()
    for offset in range(0, len(docs), BATCH):
        lines = []
        for doc in docs[offset:offset + BATCH]:
            lines.append(json.dumps({"index": {"_id": str(doc["id"])}}))
            lines.append(json.dumps(doc, separators=(",", ":")))
        response = call(base, "/documents/_bulk", ("\n".join(lines) + "\n").encode(),
                        method="POST", kind="application/x-ndjson")
        assert not response["errors"], response
    call(base, "/documents/_refresh", {}, method="POST")
    assert call(base, "/documents/_count")["count"] == len(docs)
    return time.perf_counter() - start


def load_qdrant(base, docs):
    call(base, "/collections/documents", {
        "vectors": {"size": DIMENSIONS, "distance": "Cosine", "on_disk": True},
        "on_disk_payload": True,
        "optimizers_config": {"indexing_threshold": 128},
    }, method="PUT")
    call(base, "/collections/documents/index?wait=true", {
        "field_name": "body", "field_schema": {"type": "text", "tokenizer": "word", "lowercase": True},
    }, method="PUT")
    call(base, "/collections/documents/index?wait=true", {
        "field_name": "group", "field_schema": "integer",
    }, method="PUT")
    start = time.perf_counter()
    for offset in range(0, len(docs), BATCH):
        points = [{"id": d["id"], "vector": d["vec"],
                   "payload": {"body": d["body"], "group": d["group"]}}
                  for d in docs[offset:offset + BATCH]]
        call(base, "/collections/documents/points?wait=true", {"points": points}, method="PUT")
    count = call(base, "/collections/documents/points/count", {"exact": True})
    assert count["result"]["count"] == len(docs)
    write_s = time.perf_counter() - start
    while True:
        status = call(base, "/collections/documents")["result"]
        if status["status"] == "green":
            return write_s, time.perf_counter() - start, status.get("indexed_vectors_count", 0)
        if time.perf_counter() - start > 90:
            raise RuntimeError("Qdrant optimizer did not become idle")
        time.sleep(0.25)


def query_times(run_query, validate):
    for i in range(3):
        validate(i, run_query(i))
    durations = []
    for i in range(REPETITIONS):
        start = time.perf_counter()
        result = run_query(i)
        durations.append((time.perf_counter() - start) * 1000)
        validate(i, result)
    p95 = sorted(durations)[(95 * len(durations) + 99) // 100 - 1]
    return {"median_ms": statistics.median(durations), "p95_ms": p95}


def run(backend, docs):
    with service(backend) as (base, name, result):
        if backend == "opensearch":
            result["bulk_visible_s"] = load_opensearch(base, docs)
            result["scoped_term_query"] = query_times(
                lambda i: call(base, "/documents/_search", {"size": 10, "_source": False, "query": {
                    "bool": {"must": [{"match": {"body": "Sitzungsbeschluss"}}],
                             "filter": [{"term": {"group": i % GROUPS}}]}}})["hits"]["hits"],
                lambda i, hits: verify_hits(hits, i, lambda hit: int(hit["_id"])),
            )
            result["vector_query"] = query_times(
                lambda i: call(base, "/documents/_search", {"size": 10, "_source": False, "query": {
                    "knn": {"vec": {"vector": docs[i]["vec"], "k": 10}}}})["hits"]["hits"],
                lambda i, hits: verify_vector_hits(hits, i, lambda hit: int(hit["_id"])),
            )
        else:
            visible, settled, vectors = load_qdrant(base, docs)
            result.update(bulk_visible_s=visible, optimizer_idle_s=settled, indexed_vectors=vectors)
            result["scoped_term_filter"] = query_times(
                lambda i: call(base, "/collections/documents/points/scroll", {"limit": 10,
                    "with_payload": False, "filter": {"must": [
                        {"key": "body", "match": {"text": "Sitzungsbeschluss"}},
                        {"key": "group", "match": {"value": i % GROUPS}}]}})["result"]["points"],
                lambda i, hits: verify_hits(hits, i, lambda hit: hit["id"]),
            )
            result["vector_query"] = query_times(
                lambda i: call(base, "/collections/documents/points/query", {
                    "query": docs[i]["vec"], "limit": 10})["result"]["points"],
                lambda i, hits: verify_vector_hits(hits, i, lambda hit: hit["id"]),
            )
        result["memory_after_queries"] = docker("stats", "--no-stream", "--format", "{{.MemUsage}}", name)
        return result


def verify_hits(hits, index, identity):
    assert len(hits) == 10 and all(identity(hit) % GROUPS == index % GROUPS for hit in hits)


def verify_vector_hits(hits, index, identity):
    assert len(hits) == 10 and identity(hits[0]) == index


def main():
    destination = Path(sys.argv[1])
    result = {"documents": COUNT, "dimensions": DIMENSIONS, "group_count": GROUPS,
              "cpu_limit": 2, "memory_limit_gib": 3, "engines": {}}
    documents = generate()
    for backend in IMAGES:
        result["engines"][backend] = {**IMAGES[backend], **run(backend, documents)}
        destination.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
