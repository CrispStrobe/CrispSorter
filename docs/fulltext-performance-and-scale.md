# Full-text indexing performance and scale

The desktop index uses LanceDB for document rows and Tantivy for lexical search.
PDFium is the default PDF text extractor when the bundled library is available.
The Rust parser remains available for missing PDFium, extraction failures and empty
text layers. OCR decisions retain the original page boundaries.

## Windows measurements

All full-index measurements used local input, fresh indexes unless marked as
refreshes, and disabled vectors and OCR. The build used the same release profile
for both sides of each comparison.

| Workload | Previous | Candidate |
| --- | ---: | ---: |
| 30 PDFs, native extraction with metadata | 12.9 s | 4.9 s with PDFium |
| 30 PDFs, full indexing | 15.5 s with Rust extraction | 7.0 s with PDFium |
| 512 synthetic text files, already using the batched writer | 6.37 s | 0.94 s with the optimized pipeline |
| Same synthetic text files, refresh | 9.04 s | 1.16 s |
| 30 PDFs, full indexing with PDFium on both sides | 5.51 s | 4.81 s |

PDFium recovered searchable text in 27 of 30 PDFs, compared with 22 for the
Rust parser. Page counts and blank-page positions agreed on all 25 files both
parsers opened. Across documents both opened, PDFium recovered 99.3% of the
distinct normalized terms the Rust parser produced. The pipeline benchmark
preserved source identities through fresh indexing and refreshes.

The CLI batches identity checks, reuses one source read for native PDF hashing
and extraction, and overlaps bounded preparation with commits. Both LanceDB
and Tantivy remain covered by the durable intent. The writer re-verifies source
content under its lease. PDFium document lifetimes are serialized and the
preparation queue remains concurrent. Supported settings are
`--prepare-jobs` and `--text-batch-size`. The current defaults are four workers
and 128 documents per commit with a 16 MiB text cap. A 256-document batch took
0.71 s on the small text fixture. The Windows CLI contract and 58 targeted
ingest tests passed. One direct Tantivy test setup hit an intermittent Windows
file-open error and passed on rerun.

## Scaling on limited hardware

- Put mutable catalogs, journals and active scratch on local or attached block
  storage. Serve archive bytes from cold storage. Take consistent snapshots
  and transfer immutable archives with checksums.
- Build metadata and native full text before scheduling OCR. Limit active OCR
  by page geometry, available memory and measured accuracy. Generate vectors
  for collections that benefit from semantic search.
- Budget separately for raw files, extracted text, lexical indexes, vectors,
  scratch and backups. Measure by document and page distribution as well as
  by source bytes. One million 1,024-dimensional float32 vectors alone occupy
  about 3.8 GiB before graphs, rows, scratch and replicas.
- Route queries by owner and collection. Split unusually large collections
  into bounded leaf partitions. Evaluate broad queries separately because
  they require more shard work.

## Backend qualification

A durable SQLite FTS5 prototype indexed the 512 small text files in a median
0.16 s, with two source reads, SHA-256 verification and term, phrase and prefix
checks. CrispSorter also tracks identity collisions, reviews, archives,
permissions, ranking and crash recovery. Match these contracts when comparing
production candidates.

| Engine | Candidate workload | Qualification focus |
| --- | --- | --- |
| LanceDB plus Tantivy | Embedded hybrid search | Manifest and fragment growth, compaction, filtered search, writer recovery |
| SQLite plus Tantivy or FTS5 | Embedded text-only catalog | Atomic metadata updates, German search parity, incremental refresh |
| OpenSearch | Multi-node lexical service | Image and JVM cost, indexing throughput, replica and merge overhead, scoped and broad query p95 |
| Qdrant | Vector-heavy service | Image size, on-disk vectors, quantization recall, payload filters and snapshot recovery |

Compare compressed image size by platform, installed size, peak resident
memory, WAL and index size, warm and cold p95, and update throughput on the
same corpus and hardware. A small synthetic run does not predict multi-terabyte
performance. See [SQLite FTS5](https://www.sqlite.org/fts5.html),
[OpenSearch Docker](https://docs.opensearch.org/latest/install-and-configure/install-opensearch/docker/)
and [Qdrant capacity planning](https://qdrant.tech/documentation/capacity-planning/).

### Container image and service measurements

Compressed layer sizes from the Linux OCI manifests in MiB:

| Image | Linux amd64 | Linux arm64 |
| --- | ---: | ---: |
| OpenSearch 3.10.0 | 1,107 | 845 |
| Qdrant 1.20.2 | 71 | 69 |
| PostgreSQL 18 with pgvector 0.8.7 | 158 | 156 |

The [hosted synthetic run](https://github.com/CrispStrobe/CrispSorter/actions/runs/45260934512)
loaded 4,096 records with 128-dimensional vectors and eight groups. It capped
each Linux amd64 service at two CPU cores and 3 GiB RAM and verified scoped
results and exact self-vector hits. OpenSearch used a 1 GiB JVM heap.

| Measurement | OpenSearch | Qdrant |
| --- | ---: | ---: |
| Installed image | 1,615 MiB | 188 MiB |
| Service startup | 18.44 s | 0.15 s |
| Bulk load until visible | 3.03 s | 0.22 s |
| Warm scoped term request p95 | 5.94 ms | 0.69 ms |
| Warm vector request p95 | 7.52 ms | 0.88 ms |
| Memory after queries | 1.39 GiB | 51.9 MiB |

OpenSearch ranked the term results. Qdrant filtered by term and group and
finished building all vector indexes within 0.98 s of starting the load.
Memory is a post-query sample. The fixture does not establish peak RAM,
long-run maintenance cost or equal durability and relevance. It supports
testing Qdrant first for a lean vector service. OpenSearch remains a candidate
if ranked lexical queries, distributed replicas and search features justify
the heavier image and working set.

## OCR qualification

Use the same rendered pages and reference transcriptions for OCRS, CrispEmbed
GGML Tesseract and PP-OCR. Compare German character and word error rates,
missing pages, cold and warm wall time, peak RAM and VRAM, model bytes and
raster bytes at several DPIs. Include clean scans, difficult scans, tables
and mixed digital/scanned PDFs. Test CPU first, then the available GPU or
Metal backend. Promote a model or DPI default only on measurements for the
device tier and document class.
