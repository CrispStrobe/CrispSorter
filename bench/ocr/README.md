# OCR benchmark

Every OCR engine of the `crispsorter ocr` CLI, run against a frozen, sha256-pinned corpus,
on CPU (GitHub Actions) and GPU (Kaggle). Measures CER, WER, word recall, speed, peak RAM
and GPU memory. Results stream to the **private** HF dataset repo
[`cstr/crispsorter-ocr-bench`](https://huggingface.co/datasets/cstr/crispsorter-ocr-bench)
one JSON per finished unit, so a hang, crash or session limit loses at most the unit in
flight, and a rerun with the same run id resumes.

| File | Role |
|---|---|
| `runner.py` | the runner (`run`, `plan`, `engines`); stdlib + `huggingface_hub` (+ `rapidfuzz`, `pyarrow` optional) |
| `summarize.py` | leaderboard (markdown + CSV) for a run, uploaded next to the results |
| `engines.json` | engine configurations: CLI engine, extra args/env, devices, timeouts, CPU job grouping |
| `vendor/` | CrispEmbed's scorer and gold-corpus converter, verbatim at `c364feb9` (v0.17.19), sha256-pinned in `PINS.json` |
| `package_build.py` / `fetch_build.py` | package a build with a CrispEmbed runtime / download + verify + unpack it |
| `kaggle/` | GPU kernel (`kernel.py`) and `kpush.py`, which renders submission metadata locally |
| `tests/` | `python3 -m unittest discover -s bench/ocr/tests -v` (no binary, no network, HF mocked) |

Workflows: `.github/workflows/ocr-bench-build.yml` (build + upload binaries) and
`.github/workflows/ocr-bench-cpu.yml` (CPU arm). Both are `workflow_dispatch` only.

## What a unit is

One *unit* = (device, engine configuration, page). For each unit the runner calls

```
crispsorter --format json --accept-license ocr <page image> --engine <engine> --data-dir <fresh profile> [--language eng|deu|fra] [engine args]
```

with `CRISPEMBED_CACHE_DIR` and the profile inside the work dir and
`CRISPSORTER_ACCEPT_INTENDED_PURPOSE=1`. The CLI's JSON envelope
(`{file, engine, render, chars, text}`) is the machine-readable output; the CLI has no
timing/memory output, so everything is measured from outside:

- **wall time**: monotonic clock around the process; the process gets its own process group
  and the whole group is killed at the engine's timeout (`timeout_s` in `engines.json`).
- **peak RAM**: peak of the summed `VmRSS` of the whole process tree (`/proc` polled every
  0.25 s; OCR workers such as the Qwen2-VL CPU-budget worker are child processes), plus the
  kernel's `ru_maxrss` for the direct child (`max_rss_single_mb`).
- **GPU memory** (GPU arm): `nvidia-smi` every second; peak memory of the processes of the
  tree (`peak_process_mb`), device-wide peak and delta over the pre-unit baseline, and peak
  utilisation. A successful GPU unit with no GPU memory attributed to it gets a `gpu_note`
  (the engine most likely ran on the CPU).
- **model download** is measured once per engine and session, outside the scored units:
  `crispsorter models pull <engine>` when the CLI has it (detected from `--help`), then an
  unscored **warm-up** run on the smallest selected page (with auto-download that run carries
  the download; on GPU it also carries the PTX JIT). Recorded in
  `results/<run>/engines/<device>/<engine>/<session>.json` with the new model files, sizes and
  sha256. If `ocr --help` offers `--download-models` / `--yes`, the flag is passed.

Statuses: `ok`, `timeout`, `oom_or_killed` (SIGKILL without a timeout), `crash`,
`cuda_no_kernel_image`, `unsupported`, `license_refused`, `model_unavailable`, `error`
(final: never rerun), and `skipped_breaker` (after `--breaker` consecutive
timeouts/crashes the engine's remaining pages are skipped for this session; skips live under
`skipped/` and ARE retried on resume). Failures are results, never a reason to stop the run.

## Corpus and metrics

`--corpus gold-v1` (default, 57 pages: English/German/French prose, Fraktur, tables,
multi-column, old print, receipts, photos) or `real-scans-v2` (20 pages). Manifests are
fetched from CrispEmbed at the pinned commit and checked against `vendor/PINS.json`;
`gold-v1` is rendered to page text with the vendored `to_text_manifest.py` (blocks in reading
order). Page images are verified against their pinned sha256 (Wikimedia thumbnails retried
over the pinned fallback URLs; CORD receipts read from the pinned HF parquet row, which needs
`pyarrow`) and then cached in the bench repo under `corpus/images/<sha256>.<ext>`, so later
sessions do not depend on Wikimedia's CDN.

Scoring is CrispEmbed's `score.py` (normalisation: markup stripped, NFKC, long s, quotes,
dashes, soft hyphen, line-end hyphenation, whitespace):

| metric | definition |
|---|---|
| `cer` | Levenshtein(pred, gt) / len(gt), normalised text, case-sensitive |
| `cer_nospace` | same with all whitespace removed (insensitive to column/cell separators) |
| `wer` | word-level Levenshtein / gt words (lower-cased, punctuation stripped) |
| `word_recall` | multiset word overlap / gt words |
| `len_ratio` | len(pred) / len(gt) |

**Primary metric** (CrispSorter panel convention, `tests/quality/ocr-real-panel.json`):
`word_recall` on receipts (their ground truth is partial: an engine that reads more than the
annotated fields is not wrong), `cer_nospace` everywhere else. The leaderboard shows means
over successful units *and* a failure-penalised mean (failed unit = CER 1, recall 0), since
averaging only successes rewards engines that fail on hard pages.

Each unit JSON records the exact argv, requested/effective settings, input sha256 + ground
truth sha256, binary sha256 + git sha, scorer sha256, host, licence acceptance, timing,
memory, GPU and the full recognised text (so it can be re-scored later).

## Optional OvisOCR2 comparison

The explicit `ovisocr2_cpu`, `ovisocr2_cpu_legacy`, and `ovisocr2_gpu` arms use
CrispEmbed 0.17.19's pinned Apache-2.0 OvisOCR2 F16 model through the app's
`unified` GGUF route. They are excluded from `default`, require the listed
experimental native gates, and disable cleanup so the same input reaches each
arm. The legacy CPU arm disables the graph decoder and threaded attention; the
GPU arm requires the native strict-GPU gate so CPU fallback is not a GPU result.
Use the same frozen pages, runtime lock and model hash for all arms, retain every
failure and timeout, and compare both text/quality and resource measurements.
Upstream's 11-case parity does not establish app-level or general OCR quality.
The GPU comparison must run on an actual supported GPU worker.

## Store layout (`cstr/crispsorter-ocr-bench`)

```
bin/<git_sha>/{cpu,cuda}.tar.gz(.sha256), {cpu,cuda}.build-info.json, cuda-archs.txt
corpus/images/<sha256>.<ext>
results/<run_id>/units/<device>/<engine_id>/<page_id>.json
results/<run_id>/skipped/<device>/<engine_id>/<page_id>.json
results/<run_id>/engines/<device>/<engine_id>/<session>.json
results/<run_id>/sessions/<session>.json        host, GPU, glibc, binary, CLI probe, args
results/<run_id>/progress/<device>-shard<i>of<n>.json, gpu-kernel-<slot>.json   heartbeats
results/<run_id>/summary/{leaderboard.md,leaderboard.csv,by_tag.csv}
```

Uploads are batched into one commit per `--upload-interval` (default 60 s, 120 s in the CPU
workflow) to stay below the Hub's per-repo commit rate limit; every record is written to the
local work dir first and a failed commit is retried with backoff. The heartbeat
(`progress/…json`: elapsed, budget, counts per status, current unit, pending uploads) rides
along in every commit. On Kaggle it is the only live view of a running session.

## How to run

The HF token comes from the repository secret **`HF_TOKEN`** (GitHub) or the private Kaggle
dataset `crispasr-hf-token` (`hf_token.txt`). Nothing prints it.

**(a) Build + upload binaries**

```sh
gh workflow run ocr-bench-build.yml -R CrispStrobe/CrispSorter --ref feat/ocr-benchmark
# defaults: features desktop,sidecars,crispembed,crispembed-license-api,paddle-ocr; CUDA = -cuda-bundled
gh run watch -R CrispStrobe/CrispSorter   # the job summary prints the binary sha (= the commit built)
```

One release compile on Ubuntu 22.04, packaged twice (`cpu`, `cuda`: same binary, different
CrispEmbed shared libraries; the backend is chosen at runtime by ggml). `BUILD-INFO.json`
records features, CrispEmbed asset digests, the glibc floor, the `ldd` report and the apt
packages the binary needs on the run host; `cuda-archs.txt` lists the CUDA targets found in
`libggml-cuda`.

**(b) CPU run** (GitHub Actions; CPU work does not go to Kaggle)

```sh
gh workflow run ocr-bench-cpu.yml -R CrispStrobe/CrispSorter --ref feat/ocr-benchmark \
  -f run_id=ocr-2026-11-12 -f binary_sha=<40-hex sha> [-f engines=default] [-f max_parallel=4]
```

`runner.py plan` turns `engines.json` into the matrix: the fast engines share two jobs
(`classic`, `detrec`), each vision model gets `cpu_shards` jobs that split its pages. Each job
stops *starting* units at 5.4 h (`budget_hours`), well inside the 6 h job limit; the
`summarize` job then writes the leaderboard.

**(c) GPU run** (Kaggle, account from the environment, never committed)

```sh
export KAGGLE_ACCOUNT=<account>   # KAGGLE_TOKEN optional: else the kaggle CLI's stored credentials
python3 bench/ocr/kaggle/kpush.py --run-id ocr-2026-11-12 --binary-sha <sha> [--runner-sha <sha>]
# two concurrent sessions (Kaggle max): --slot a --shard 0/2  and  --slot b --shard 1/2
# render only: --dry-run /tmp/kernel
```

The kernel downloads `bench/ocr/*` from GitHub at `--runner-sha` (default: the binary sha;
the commit must be pushed) and `bin/<sha>/cuda.tar.gz` from HF, installs the runtime apt
packages, checks `ldd` and glibc loudly, records `nvidia-smi` + compute capability, and runs
`runner.py run --device gpu` with a budget so the last unit ends by ~11.5 h. CPU-only engines
(`system_tesseract`, `ocrs`, `paddle`) are not run on the GPU arm.

**(d) Resume**: dispatch / push again with the *same* `run_id`. Stored units are skipped
(listing of `results/<run_id>/units/` at start, plus units written locally whose upload never
landed). To retry a failed engine after a fix, use a new run id (final statuses are never
rerun) or delete its unit files from the repo.

**(e) Summarize** (also run automatically by both arms)

```sh
HF_TOKEN=... python3 bench/ocr/summarize.py --run-id ocr-2026-11-12 [--no-upload] [--out DIR]
```

Local dry run without HF (any `crispsorter` binary, or `tests/fake_crispsorter.py` behind a
wrapper): `runner.py run --store dir:/tmp/bench-store ...` then
`summarize.py --store dir:/tmp/bench-store ...`.

## Caveats

- **Kaggle GPUs are effectively always P100 (sm_60).** CrispEmbed's CUDA build has no sm_60
  SASS; ggml's default target list includes `compute_50` PTX, which the driver JIT-compiles
  (slow first load, cached in the session's `CUDA_CACHE_PATH`). If a unit still reports
  `no kernel image is available`, it is recorded as `cuda_no_kernel_image` with a loud banner.
  Below compute capability 6.1 ggml has no integer-MMQ path, so quantized models run as
  dequant + cuBLAS: P100 numbers are valid for P100 only. No torch is involved.
- **glibc**: the CrispEmbed CUDA archives are built on `ubuntu-latest` (24.04, glibc 2.39),
  unlike the manylinux CPU archive. `fetch_build.py` exits loudly (code 3) when the host's
  glibc is older than `BUILD-INFO.json`'s `glibc_required`.
- **Receipts**: partial ground truth; use word recall (the primary metric there), not CER.
- **CPU runners** (public repo): 4 vCPU / 16 GB. 7B vision models may time out or be killed
  (`oom_or_killed`); that is a result, not an infrastructure error.
- **Licences**: `--accept-license` is passed on every call (internal evaluation of e.g. GOT
  (non-commercial), surya-det (OpenRAIL-M)); every unit records it.
- **Language**: `--language` is the page's language (`en`→`eng`, `de`→`deu`, `fr`→`fra`), as
  a user who picked the document language would. Engines that ignore it only warn; for
  Qwen2-VL/InternVL2/Granite/Qwen3-VL it becomes a prompt hint.
- The CORD receipt images need `pyarrow` the first time (before they are in the bench repo's
  corpus cache).

### Real-page comparison follow-up (2026-11-12)

A source-pinned eight-page comparison at application revision `82c5da324043`
processed seven pages per arm; the remaining page failed its pinned input hash
on every attempt. Later independent read-only fetches recovered the exact
original pinned bytes; stage those bytes and verify the pin before rerunning.
No replacement or repin is needed.
The Ovis CPU/current, CPU/legacy and strict-GPU arms returned empty text for
all seven processed pages. Direct native diagnostics identify rejection before
inference because those images exceeded the native patch bound; the app
incorrectly recorded empty success. These are failure results, not measured
Ovis quality or GPU throughput. The GPU initialized and a separate small
synthetic positive executed on CUDA, but that does not qualify real-page GPU
inference.

For the seven admitted pages, PP-OCRv6 CPU reported 75.76% character error and
2.79% word recall. Tesseract had one TSV/TXT disagreement; its
failure-penalized character error was 32.59% and word recall 61.34%. These
figures are specific to this incomplete sample and its recorded settings.
Preserve failures in the denominator. First propagate the native rejection as
an app failure, then design and qualify bounded resizing or tiling against the
unchanged real-page references. Do not promote a new default or claim a GPU
speedup from the rejection timings.
