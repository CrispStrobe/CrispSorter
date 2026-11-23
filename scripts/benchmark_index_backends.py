#!/usr/bin/env python3
"""Compare isolated service costs on generated text and vectors, using Docker."""

import contextlib
import csv
import io
import json
from pathlib import Path
import random
import statistics
import subprocess
import sys
import time
from urllib.request import Request, urlopen
import uuid

from pg8000 import dbapi


IMAGES = {
    "opensearch": {
        "image": "opensearchproject/opensearch@sha256:ffa260e43be96b49144fb7ececbb135c3fd7081567adc781c49ec9232ce1e47f",
        "tag": "3.10.0", "port": 9200, "compressed_bytes": 1160663754,
    },
    "qdrant": {
        "image": "qdrant/qdrant@sha256:c5e32ee251293d2e1f4dd34890ad75eeee5a4f3821a121d6d8b7e4323448b386",
        "tag": "v1.20.2", "port": 6333, "compressed_bytes": 74318344,
    },
    "postgres": {
        "image": "pgvector/pgvector@sha256:4bdc16048580b56345bca441bffe8d9b45dd2f5d76963808c853df1661df6e00",
        "tag": "0.8.7-pg18", "port": 5432, "compressed_bytes": 165258000,
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
    password = uuid.uuid4().hex
    environment = {
        "opensearch": {
            "discovery.type": "single-node",
            "OPENSEARCH_JAVA_OPTS": "-Xms1g -Xmx1g",
            "DISABLE_INSTALL_DEMO_CONFIG": "true",
            "DISABLE_SECURITY_PLUGIN": "true",
        },
        "postgres": {"POSTGRES_PASSWORD": password},
    }.get(backend, {})
    env_args = [item for key, value in environment.items() for item in ("--env", f"{key}={value}")]
    start = time.perf_counter()
    docker("run", "--detach", "--name", name, "--memory", "3g", "--memory-swap", "3g",
           "--cpus", "2", "--shm-size", "256m", "--publish", f"127.0.0.1::{image['port']}",
           *env_args, image["image"])
    database = None
    try:
        address = docker("port", name, f"{image['port']}/tcp")
        base = f"http://{address}"
        health = "/" if backend == "opensearch" else "/collections"
        while True:
            try:
                if backend == "postgres":
                    host, port = address.rsplit(":", 1)
                    database = dbapi.connect(user="postgres", password=password, host=host, port=int(port), timeout=15)
                else:
                    call(base, health)
                break
            except (OSError, dbapi.Error):
                if time.perf_counter() - start > 120:
                    raise
                time.sleep(0.5)
        yield (database if backend == "postgres" else base), name, {
            "cold_pull_s": pull_s, "installed_image_bytes": disk_bytes,
            "startup_s": time.perf_counter() - start,
        }
    except Exception:
        print(docker("logs", "--tail", "30", name), file=sys.stderr)
        raise
    finally:
        try:
            if database is not None:
                database.close()
        finally:
            docker("rm", "--force", "--volumes", name)


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


def run_postgres(database, docs):
    with contextlib.closing(database.cursor()) as cursor:
        cursor.execute("CREATE EXTENSION vector")
        cursor.execute(f"""
            CREATE TABLE documents (
                id integer PRIMARY KEY,
                group_id integer NOT NULL,
                body text NOT NULL,
                vec vector({DIMENSIONS}) NOT NULL,
                terms tsvector GENERATED ALWAYS AS (to_tsvector('german', body)) STORED
            )
        """)
        database.commit()
        start = time.perf_counter()
        for offset in range(0, len(docs), BATCH):
            with io.StringIO(newline="") as records:
                csv.writer(records).writerows(
                    (doc["id"], doc["group"], doc["body"], json.dumps(doc["vec"]))
                    for doc in docs[offset:offset + BATCH]
                )
                records.seek(0)
                cursor.execute("COPY documents (id, group_id, body, vec) FROM STDIN WITH (FORMAT CSV)",
                               stream=records)
        database.commit()
        cursor.execute("SELECT count(*) FROM documents")
        assert cursor.fetchone()[0] == len(docs)
        visible = time.perf_counter() - start

        cursor.execute("CREATE INDEX documents_terms ON documents USING gin (terms)")
        cursor.execute("CREATE INDEX documents_group ON documents (group_id)")
        cursor.execute("CREATE INDEX documents_vec ON documents USING hnsw (vec vector_cosine_ops)")
        cursor.execute("ANALYZE documents")
        database.commit()
        indexed = time.perf_counter() - start

        def rows(statement, params):
            cursor.execute(statement, params)
            return cursor.fetchall()

        return {
            "bulk_visible_s": visible,
            "indexes_ready_s": indexed,
            "scoped_term_query": query_times(
                lambda i: rows("""
                    SELECT id FROM documents, plainto_tsquery('german', %s) AS search(query)
                    WHERE group_id = %s AND terms @@ query
                    ORDER BY ts_rank(terms, query) DESC LIMIT 10
                """, ("Sitzungsbeschluss", i % GROUPS)),
                lambda i, hits: verify_hits(hits, i, lambda hit: hit[0]),
            ),
            "vector_query": query_times(
                lambda i: rows("SELECT id FROM documents ORDER BY vec <=> CAST(%s AS vector) LIMIT 10",
                               (json.dumps(docs[i]["vec"]),)),
                lambda i, hits: verify_vector_hits(hits, i, lambda hit: hit[0]),
            ),
        }


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
        elif backend == "qdrant":
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
        else:
            result.update(run_postgres(base, docs))
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
