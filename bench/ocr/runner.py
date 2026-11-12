#!/usr/bin/env python3
"""Resumable OCR benchmark for the `crispsorter ocr` CLI.

Runs every configured engine (bench/ocr/engines.json) against a frozen, sha256-pinned
corpus (CrispEmbed docs/ocr-gold-corpus-v1.json or docs/ocr-real-scans-v2.json), one
*unit* = (device, engine configuration, page) at a time, and streams one small JSON per
unit to a results store (a private Hugging Face dataset repo by default) as soon as it
finishes. A rerun with the same --run-id lists what is already stored and skips it, so a
hung, killed or out-of-time session loses at most the unit in flight.

Per unit: CER / CER without spaces / WER / word recall / length ratio, scored with the
vendored CrispEmbed scorer (vendor/crispembed_score.py, sha256-checked against
vendor/PINS.json); wall time; peak RSS of the whole process tree (OCR workers are child
processes) by /proc polling plus the kernel's ru_maxrss; on GPU, peak device memory and
utilisation by nvidia-smi polling. Model download is measured once per engine and
session, separately from the scored runs (a `models pull` step when the CLI has one,
then an unscored warm-up page).

Stores: --store hf:<repo_id> (token from HF_TOKEN or --hf-token-file) or dir:<path>.
Layout inside the store (all paths relative to the repo root):
  results/<run_id>/units/<device>/<engine_id>/<page_id>.json     final unit records
  results/<run_id>/skipped/<device>/<engine_id>/<page_id>.json   circuit-breaker skips (retried on resume)
  results/<run_id>/engines/<device>/<engine_id>/<session>.json   pull + warm-up per session
  results/<run_id>/sessions/<session>.json                       host/binary/CLI identity
  results/<run_id>/progress/<device>-<shard>.json                heartbeat (overwritten)
  corpus/images/<sha256>.<ext>                                   verified page images (cache)

Dependencies: Python >= 3.9 stdlib; huggingface_hub for hf: stores; rapidfuzz (optional,
the scorer has a pure-Python fallback); pyarrow only for the CORD receipt pages when the
image is not yet in the store's corpus cache.

usage:
  runner.py run  --binary PATH --device cpu|gpu --run-id ID [--engines default|all|id,...|group:G]
                 [--pages all|id,...] [--tags t1,t2] [--shard i/n] [--budget-seconds S] ...
  runner.py plan --device cpu [--engines ...]          # GitHub Actions matrix as JSON
  runner.py engines [--device cpu]                      # list engine configurations
"""
from __future__ import annotations

import argparse
import hashlib
import importlib.util
import io
import json
import os
import platform
import re
import shutil
import signal
import socket
import subprocess
import sys
import threading
import time
import urllib.request
import uuid
from pathlib import Path

RUNNER_VERSION = "ocr-bench-runner-2026-11-12.1"
UNIT_SCHEMA = "crispsorter-ocr-bench-unit-v1"
ENGINE_SCHEMA = "crispsorter-ocr-bench-engine-v1"
SESSION_SCHEMA = "crispsorter-ocr-bench-session-v1"
PROGRESS_SCHEMA = "crispsorter-ocr-bench-progress-v1"

HERE = Path(__file__).resolve().parent
VENDOR = HERE / "vendor"
DEFAULT_ENGINES = HERE / "engines.json"
DEFAULT_REPO = "cstr/crispsorter-ocr-bench"
UA = {"User-Agent": "CrispSorter-OCR-bench/1.0 (https://github.com/CrispStrobe/CrispSorter)"}
LANG_BY_TAG = {"en": "eng", "de": "deu", "fr": "fra", "id-en-mixed": "eng"}
MAX_TEXT_CHARS = 200_000
# Unit statuses. Everything except the skipped_* family is final: stored under units/ and
# never rerun on resume. Skips are informational and are retried by the next session.
FINAL_STATUSES = ("ok", "timeout", "oom_or_killed", "crash", "cuda_no_kernel_image", "unsupported",
                  "license_refused", "model_unavailable", "error")
BREAKER_STATUSES = ("timeout", "oom_or_killed", "crash", "cuda_no_kernel_image")


def log(*a):
    print(time.strftime("[%H:%M:%S]"), *a, flush=True)


def banner(msg):
    line = "!" * 100
    print(f"{line}\n{msg}\n{line}", flush=True)


def now_iso():
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def sha256_file(path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def safe_name(s: str) -> str:
    if not re.fullmatch(r"[A-Za-z0-9._-]{1,128}", s or "") or s in (".", ".."):
        raise ValueError(f"unsafe identifier for a store path: {s!r}")
    return s


# ─────────────────────────────── pins + vendored scorer ───────────────────────────────

def load_pins():
    return json.loads((VENDOR / "PINS.json").read_text())


def load_vendored(name: str, pins=None):
    """Import a vendored CrispEmbed module after checking its sha256 against PINS.json."""
    pins = pins or load_pins()
    path = VENDOR / name
    want = pins["vendored"][name]["sha256"]
    got = sha256_file(path)
    if got != want:
        raise RuntimeError(f"vendored {name} sha256 {got} != pinned {want}; refusing to score with a modified copy")
    spec = importlib.util.spec_from_file_location(f"_bench_vendor_{path.stem}", path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


class Scorer:
    def __init__(self, pins=None):
        pins = pins or load_pins()
        self.mod = load_vendored("crispembed_score.py", pins)
        self.identity = dict(path="bench/ocr/vendor/crispembed_score.py",
                             upstream_path=pins["vendored"]["crispembed_score.py"]["upstream_path"],
                             sha256=pins["vendored"]["crispembed_score.py"]["sha256"],
                             crispembed_commit=pins["crispembed_commit"])
        try:
            import rapidfuzz  # noqa: F401
            self.identity["levenshtein"] = "rapidfuzz"
        except Exception:
            self.identity["levenshtein"] = "pure-python"

    def score(self, pred: str, gt: str) -> dict:
        return self.mod.score(pred or "", gt or "")


def primary_metric(tags) -> str:
    """CrispSorter panel convention: receipts have partial GT -> word recall; else CER w/o spaces."""
    return "word_recall" if "receipt" in (tags or ()) else "cer_nospace"


def page_language(tags):
    for t in tags or ():
        if t in LANG_BY_TAG:
            return LANG_BY_TAG[t]
    return None


# ─────────────────────────────────────── stores ───────────────────────────────────────

class DirStore:
    """Filesystem mirror of the HF layout (dry runs, tests, offline)."""

    def __init__(self, root):
        self.root = Path(root)
        self.root.mkdir(parents=True, exist_ok=True)
        self.name = f"dir:{self.root}"

    def list(self, prefix: str) -> set:
        base = self.root / prefix
        if not base.exists():
            return set()
        return {p.relative_to(self.root).as_posix() for p in base.rglob("*") if p.is_file()}

    def get(self, path: str, dest) -> bool:
        src = self.root / path
        if not src.is_file():
            return False
        Path(dest).parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(src, dest)
        return True

    def fetch_prefix(self, prefix: str, dest) -> Path:
        """Mirror every file under prefix into dest (same relative paths)."""
        for rel in self.list(prefix):
            self.get(rel, Path(dest) / rel)
        return Path(dest)

    def put_many(self, items, message: str = ""):
        for path, data in items:
            dst = self.root / path
            dst.parent.mkdir(parents=True, exist_ok=True)
            if isinstance(data, (bytes, bytearray)):
                dst.write_bytes(data)
            else:
                shutil.copyfile(data, dst)


class HFStore:
    def __init__(self, repo_id: str, token=None, repo_type="dataset", create=True):
        from huggingface_hub import HfApi  # imported lazily: tests and plan need no HF
        self.api = HfApi(token=token)
        self.repo_id, self.repo_type, self.token = repo_id, repo_type, token
        self.name = f"hf:{repo_id}"
        if create:
            self.api.create_repo(repo_id, repo_type=repo_type, private=True, exist_ok=True)

    def list(self, prefix: str) -> set:
        try:
            return {e.path for e in self.api.list_repo_tree(self.repo_id, path_in_repo=prefix, recursive=True,
                                                            repo_type=self.repo_type)
                    if e.__class__.__name__ != "RepoFolder"}
        except Exception as e:  # missing folder = nothing stored yet
            if "404" in str(e) or e.__class__.__name__ in ("EntryNotFoundError", "RepositoryNotFoundError"):
                return set()
            raise

    def get(self, path: str, dest) -> bool:
        from huggingface_hub import hf_hub_download
        try:
            tmp = hf_hub_download(self.repo_id, path, repo_type=self.repo_type, token=self.token)
        except Exception as e:
            if "404" in str(e) or e.__class__.__name__ in ("EntryNotFoundError",):
                return False
            raise
        Path(dest).parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(tmp, dest)
        return True

    def fetch_prefix(self, prefix: str, dest) -> Path:
        """One snapshot download of everything under prefix (thousands of unit files)."""
        from huggingface_hub import snapshot_download
        snapshot_download(self.repo_id, repo_type=self.repo_type, token=self.token, local_dir=str(dest),
                          allow_patterns=[f"{prefix}/**"])
        return Path(dest)

    def put_many(self, items, message: str = ""):
        from huggingface_hub import CommitOperationAdd
        ops = [CommitOperationAdd(path_in_repo=p, path_or_fileobj=(io.BytesIO(d) if isinstance(d, (bytes, bytearray)) else str(d)))
               for p, d in items]
        self.api.create_commit(self.repo_id, operations=ops, commit_message=message or "ocr-bench", repo_type=self.repo_type)


def open_store(spec: str, token=None):
    if spec.startswith("dir:"):
        return DirStore(spec[4:])
    if spec.startswith("hf:"):
        return HFStore(spec[3:], token=token)
    raise ValueError(f"unknown --store {spec!r} (want hf:<repo> or dir:<path>)")


def read_token(args_token_file=None):
    if os.environ.get("HF_TOKEN"):
        return os.environ["HF_TOKEN"].strip()
    import glob
    # The Kaggle token dataset mounts at /kaggle/input/crispasr-hf-token/ or, in newer images,
    # /kaggle/input/datasets/<owner>/crispasr-hf-token/ (no account name is committed here).
    cands = [args_token_file, "/kaggle/input/crispasr-hf-token/hf_token.txt"] + sorted(
        glob.glob("/kaggle/input/datasets/*/crispasr-hf-token/hf_token.txt"))
    for p in cands:
        if p and Path(p).is_file():
            return Path(p).read_text().strip()
    return None


class Uploader:
    """Queue of (path_in_repo -> bytes | local file) committed in small batches.

    Files are always written locally first; the queue only decides *when* they reach the
    store. A flush happens every `interval` seconds (one commit with everything pending,
    including the heartbeat), so the store never lags by more than ~interval and Hub
    commit rate limits are respected. Failures keep the items queued with backoff.
    """

    def __init__(self, store, interval=60.0, heartbeat=None, label="ocr-bench"):
        self.store, self.interval, self.heartbeat, self.label = store, float(interval), heartbeat, label
        self.pending = {}
        self.lock = threading.Lock()
        self.failures = 0
        self.next_try = 0.0
        self.last_flush = 0.0
        self.uploaded = 0
        self.last_error = None
        self._stop = threading.Event()
        self._thread = None

    def add(self, path: str, data):
        with self.lock:
            self.pending[path] = data

    def flush(self, force=False) -> bool:
        if not force and time.time() < self.next_try:
            return False
        if self.heartbeat:
            try:
                hp, hd = self.heartbeat()
                self.add(hp, hd)
            except Exception as e:
                log("heartbeat build failed:", repr(e)[:200])
        with self.lock:
            batch = dict(self.pending)
        if not batch:
            self.last_flush = time.time()
            return True
        units = sum(1 for p in batch if "/units/" in p)
        try:
            self.store.put_many(sorted(batch.items()), f"{self.label}: {units} unit(s), {len(batch)} file(s)")
        except Exception as e:
            self.failures += 1
            self.last_error = repr(e)[:300]
            delay = min(600.0, 15.0 * (2 ** min(self.failures, 6)))
            self.next_try = time.time() + delay
            log(f"upload of {len(batch)} file(s) failed ({self.last_error}); kept queued, retry in {delay:.0f}s")
            return False
        with self.lock:
            for p, d in batch.items():
                if self.pending.get(p) is d:
                    del self.pending[p]
        self.failures, self.next_try, self.last_flush = 0, 0.0, time.time()
        self.uploaded += units
        return True

    def _loop(self):
        while not self._stop.wait(1.0):
            if time.time() - self.last_flush >= self.interval:
                self.flush()

    def start(self):
        self.last_flush = time.time()
        self._thread = threading.Thread(target=self._loop, name="uploader", daemon=True)
        self._thread.start()

    def close(self, attempts=8):
        self._stop.set()
        if self._thread:
            self._thread.join(timeout=30)
        for i in range(attempts):
            self.next_try = 0.0
            if self.flush(force=True) and not self.pending:
                return True
            time.sleep(min(120, 10 * (i + 1)))
        banner(f"UPLOAD INCOMPLETE: {len(self.pending)} file(s) not in the store; they remain in the work dir "
               f"and the next session with the same --run-id re-uploads them. Last error: {self.last_error}")
        return False


# ─────────────────────────────────────── engines ──────────────────────────────────────

def load_engines(path=DEFAULT_ENGINES):
    data = json.loads(Path(path).read_text())
    seen = set()
    for e in data["engines"]:
        safe_name(e["id"])
        if e["id"] in seen:
            raise ValueError(f"duplicate engine id {e['id']}")
        seen.add(e["id"])
        if not set(e["devices"]) <= {"cpu", "gpu"}:
            raise ValueError(f"bad devices for {e['id']}")
        for d in e["devices"]:
            if d not in e.get("timeout_s", {}):
                raise ValueError(f"{e['id']}: no timeout_s for device {d}")
    return data["engines"]


def select_engines(engines, spec: str, device: str):
    spec = (spec or "default").strip()
    on_device = [e for e in engines if device in e["devices"]]
    if spec == "all":
        return on_device
    if spec == "default":
        return [e for e in on_device if e.get("default", True)]
    picked, by_id = [], {e["id"]: e for e in engines}
    for tok in [t.strip() for t in spec.split(",") if t.strip()]:
        if tok.startswith("group:"):
            grp = [e for e in on_device if e.get("group") == tok[6:]]
            if not grp:
                raise ValueError(f"no engines in {tok} for device {device}")
            picked += grp
        elif tok in by_id:
            if device not in by_id[tok]["devices"]:
                log(f"engine {tok} is not benchmarked on {device}; skipped")
                continue
            picked.append(by_id[tok])
        else:
            raise ValueError(f"unknown engine id {tok!r}")
    out, seen = [], set()
    for e in picked:
        if e["id"] not in seen:
            seen.add(e["id"]); out.append(e)
    return out


def plan_matrix(engines, device="cpu", spec="default"):
    """GitHub Actions matrix entries: shared-group jobs, and n page-shards for heavy engines."""
    sel = select_engines(engines, spec, device)
    groups, order = {}, []
    for e in sel:
        g = e.get("group") or e["id"]
        if g not in groups:
            groups[g] = []; order.append(g)
        groups[g].append(e)
    jobs = []
    for g in order:
        members = groups[g]
        n = max(int(e.get("cpu_shards", 1)) for e in members) if device == "cpu" else 1
        for i in range(n):
            jobs.append(dict(name=f"{g}-{i + 1}of{n}", engines=",".join(e["id"] for e in members), shard=f"{i}/{n}"))
    return jobs


# ─────────────────────────────────────── corpus ───────────────────────────────────────

def fetch_url(url, timeout=300, attempts=5):
    err = None
    for i in range(attempts):
        try:
            with urllib.request.urlopen(urllib.request.Request(url, headers=UA), timeout=timeout) as r:
                return r.read()
        except Exception as e:  # noqa: PERF203
            err = e
            time.sleep(min(60, 5 * (i + 1)))
    raise err


def raw_github(repo, commit, path):
    return f"https://raw.githubusercontent.com/{repo}/{commit}/{path}"


def load_manifest_bytes(name, cache_dir, pins, fetch=fetch_url):
    meta = pins["manifests"][name]
    cache = Path(cache_dir) / f"{meta['sha256']}.json"
    if cache.is_file() and sha256_file(cache) == meta["sha256"]:
        return cache.read_bytes()
    data = fetch(raw_github(pins["crispembed_repo"], pins["crispembed_commit"], meta["upstream_path"]))
    got = sha256_bytes(data)
    if got != meta["sha256"]:
        raise RuntimeError(f"manifest {name} sha256 {got} != pinned {meta['sha256']}")
    cache.parent.mkdir(parents=True, exist_ok=True)
    cache.write_bytes(data)
    return data


def load_corpus(name, cache_dir, pins=None, fetch=fetch_url):
    """Return (pages, identity). pages: id, tags, ground_truth, image pin, language, primary metric."""
    pins = pins or load_pins()
    if name not in pins["manifests"]:
        raise ValueError(f"unknown corpus {name!r}; known: {sorted(pins['manifests'])}")
    v2 = json.loads(load_manifest_bytes("real-scans-v2", cache_dir, pins, fetch))
    if name == "gold-v1":
        conv = load_vendored("to_text_manifest.py", pins)
        manifest = conv.convert(json.loads(load_manifest_bytes("gold-v1", cache_dir, pins, fetch)), v2)
    else:
        manifest = v2
    pages = []
    for p in manifest["pages"]:
        safe_name(p["id"])
        pages.append(dict(id=p["id"], tags=list(p.get("tags") or []), ground_truth=p["ground_truth"],
                          image=p["image"], max_tokens=p.get("max_tokens"), priority=p.get("priority", 9),
                          language=page_language(p.get("tags")), primary_metric=primary_metric(p.get("tags")),
                          ground_truth_sha256=sha256_bytes(p["ground_truth"].encode())))
    ids = [p["id"] for p in pages]
    if len(set(ids)) != len(ids):
        raise ValueError("duplicate page id in corpus")
    identity = dict(name=name, upstream_path=pins["manifests"][name]["upstream_path"],
                    sha256=pins["manifests"][name]["sha256"], crispembed_commit=pins["crispembed_commit"], pages=len(pages))
    return pages, identity


def select_pages(pages, ids_spec="all", tags_spec=""):
    out = pages
    if ids_spec and ids_spec != "all":
        want = [t.strip() for t in ids_spec.split(",") if t.strip()]
        known = {p["id"] for p in pages}
        unknown = [w for w in want if w not in known]
        if unknown:
            raise ValueError(f"unknown page id(s): {unknown}")
        out = [p for p in out if p["id"] in want]
    if tags_spec:
        tags = {t.strip() for t in tags_spec.split(",") if t.strip()}
        out = [p for p in out if tags & set(p["tags"])]
    return out


def image_ext(data: bytes) -> str:
    if data[:8] == b"\x89PNG\r\n\x1a\n":
        return "png"
    if data[:3] == b"\xff\xd8\xff":
        return "jpg"
    if data[:4] in (b"II*\x00", b"MM\x00*"):
        return "tif"
    if data[:4] == b"RIFF" and data[8:12] == b"WEBP":
        return "webp"
    return "jpg"


class ImageUnavailable(Exception):
    pass


class ImageProvider:
    """Verified page bytes: local cache -> store corpus cache -> pinned URL(s) / pinned parquet row."""

    def __init__(self, cache_dir, store=None, uploader=None, fetch=fetch_url, sleep=time.sleep):
        self.sleep = sleep
        self.dir = Path(cache_dir)
        self.dir.mkdir(parents=True, exist_ok=True)
        self.store, self.uploader, self.fetch = store, uploader, fetch
        self.remote = None
        self.receipts = {}

    def _remote_listing(self):
        if self.remote is None:
            try:
                self.remote = self.store.list("corpus/images") if self.store else set()
            except Exception as e:
                log("corpus cache listing failed:", repr(e)[:200]); self.remote = set()
        return self.remote

    def _local(self, sha):
        for p in self.dir.glob(f"{sha}.*"):
            if p.suffix != ".part" and sha256_file(p) == sha:
                return p
        return None

    def get(self, page) -> Path:
        pin = page["image"]; sha = pin["sha256"]
        hit = self._local(sha)
        if hit:
            self.receipts.setdefault(page["id"], dict(source="local-cache", sha256=sha, sha256_ok=True))
            return hit
        rec = dict(expected_sha256=sha, attempts=[])
        for rp in sorted(p for p in self._remote_listing() if p.startswith(f"corpus/images/{sha}.")):
            tmp = self.dir / f"{sha}.part"
            try:
                if self.store.get(rp, tmp) and sha256_file(tmp) == sha:
                    dst = self.dir / Path(rp).name; tmp.replace(dst)
                    self.receipts[page["id"]] = dict(source="store-corpus-cache", sha256=sha, sha256_ok=True)
                    return dst
            except Exception as e:
                rec["attempts"].append(dict(store=rp, error=repr(e)[:200]))
        data = None
        if "hf_parquet" in pin:
            data = self._from_parquet(pin["hf_parquet"], rec)
        else:
            urls = [pin["url"]] + list(pin.get("fallback_urls", []))
            # Wikimedia's CDN intermittently serves other bytes for a pinned thumbnail URL (CrispEmbed
            # gold run 2026-11-07): retry a few rounds; never change the pin.
            for rnd in range(3):
                for url in urls:
                    try:
                        d = self.fetch(url)
                    except Exception as e:
                        rec["attempts"].append(dict(url=url, error=repr(e)[:200])); continue
                    h = sha256_bytes(d); rec["attempts"].append(dict(url=url, sha256=h))
                    if h == sha:
                        data = d; break
                if data is not None:
                    break
                self.sleep(10 * (rnd + 1))
        if data is None or sha256_bytes(data) != sha or ("bytes" in pin and len(data) != pin["bytes"]):
            self.receipts[page["id"]] = dict(rec, sha256_ok=False)
            raise ImageUnavailable(f"no verified bytes for {page['id']} (expected sha256 {sha})")
        dst = self.dir / f"{sha}.{image_ext(data)}"
        dst.write_bytes(data)
        self.receipts[page["id"]] = dict(rec, source="pinned-source", sha256=sha, sha256_ok=True)
        if self.uploader is not None:
            self.uploader.add(f"corpus/images/{dst.name}", dst)
        return dst

    def _from_parquet(self, hp, rec):
        pq_path = self.dir / f"parquet-{hp['sha256']}.parquet"
        if not (pq_path.is_file() and sha256_file(pq_path) == hp["sha256"]):
            try:
                d = self.fetch(hp["url"])
            except Exception as e:
                rec["attempts"].append(dict(url=hp["url"], error=repr(e)[:200])); return None
            if sha256_bytes(d) != hp["sha256"]:
                rec["attempts"].append(dict(url=hp["url"], error="parquet sha256 mismatch")); return None
            pq_path.write_bytes(d)
        try:
            import pyarrow.parquet as pq
        except Exception:
            rec["attempts"].append(dict(error="pyarrow not installed; cannot read the pinned CORD parquet row"))
            return None
        row = pq.read_table(str(pq_path), columns=["image"]).slice(int(hp["row"]), 1).to_pylist()[0]["image"]
        return row["bytes"]


# ─────────────────────────────── process measurement ──────────────────────────────────

def _proc_children_map():
    kids = {}
    for d in os.listdir("/proc"):
        if not d.isdigit():
            continue
        try:
            with open(f"/proc/{d}/stat") as f:
                s = f.read()
            ppid = int(s[s.rfind(")") + 2:].split()[1])
            kids.setdefault(ppid, []).append(int(d))
        except (OSError, ValueError, IndexError):
            pass
    return kids


def proc_tree(pid):
    kids, out, todo = _proc_children_map(), [], [pid]
    while todo:
        p = todo.pop(); out.append(p); todo += kids.get(p, [])
    return out


def rss_kb(pid):
    try:
        with open(f"/proc/{pid}/status") as f:
            for line in f:
                if line.startswith("VmRSS:"):
                    return int(line.split()[1])
    except (OSError, ValueError):
        pass
    return 0


def nvidia_smi(query, apps=False):
    flag = "--query-compute-apps" if apps else "--query-gpu"
    try:
        r = subprocess.run(["nvidia-smi", f"{flag}={query}", "--format=csv,noheader,nounits"],
                           capture_output=True, text=True, timeout=15)
        if r.returncode != 0:
            return None
        return [[c.strip() for c in line.split(",")] for line in r.stdout.strip().splitlines() if line.strip()]
    except Exception:
        return None


def gpu_used_mb():
    rows = nvidia_smi("memory.used,utilization.gpu")
    if not rows:
        return None, None
    try:
        return sum(float(r[0]) for r in rows), max(float(r[1]) for r in rows)
    except (ValueError, IndexError):
        return None, None


class RunResult(dict):
    pass


def run_measured(argv, env, timeout, cwd=None, gpu=False, poll=0.25, gpu_poll=1.0, stdout_path=None, stderr_path=None):
    """Run argv in its own process group; kill the whole group at the deadline.

    Peak RSS: max over samples of the summed VmRSS of the process tree (Linux /proc), and
    the kernel's ru_maxrss for the direct child (which covers waited-for descendants)."""
    linux = os.path.isdir("/proc/self")
    so = open(stdout_path, "wb") if stdout_path else subprocess.DEVNULL
    se = open(stderr_path, "wb") if stderr_path else subprocess.DEVNULL
    gpu_base = gpu_used_mb()[0] if gpu else None
    t0 = time.monotonic()
    p = subprocess.Popen(argv, env=env, cwd=cwd, stdout=so, stderr=se, stdin=subprocess.DEVNULL, start_new_session=True)
    state = dict(peak_tree_kb=0, timed_out=False, gpu_peak=None, gpu_util=None, gpu_proc_peak=None, samples=0)
    done = threading.Event()

    def kill_group(sig):
        try:
            os.killpg(p.pid, sig)
        except (ProcessLookupError, PermissionError, OSError):
            pass

    def monitor():
        last_gpu = 0.0
        while not done.wait(poll):
            if linux:
                try:
                    tree = proc_tree(p.pid)
                    total = sum(rss_kb(x) for x in tree)
                    state["peak_tree_kb"] = max(state["peak_tree_kb"], total)
                    state["samples"] += 1
                except Exception:
                    tree = [p.pid]
            else:
                tree = [p.pid]
            if gpu and time.monotonic() - last_gpu >= gpu_poll:
                last_gpu = time.monotonic()
                used, util = gpu_used_mb()
                if used is not None:
                    state["gpu_peak"] = max(state["gpu_peak"] or 0.0, used)
                    state["gpu_util"] = max(state["gpu_util"] or 0.0, util or 0.0)
                rows = nvidia_smi("pid,used_memory", apps=True) or []
                mine = 0.0
                for r in rows:
                    try:
                        if int(r[0]) in tree:
                            mine += float(r[1])
                    except (ValueError, IndexError):
                        pass
                if rows:
                    state["gpu_proc_peak"] = max(state["gpu_proc_peak"] or 0.0, mine)
            if time.monotonic() - t0 > timeout and not state["timed_out"]:
                state["timed_out"] = True
                kill_group(signal.SIGTERM)
                # Never p.poll() here: it would reap the child before the main thread's wait4.
                if not done.wait(10):
                    kill_group(signal.SIGKILL)

    th = threading.Thread(target=monitor, name="monitor", daemon=True)
    th.start()
    try:
        _, status, ru = os.wait4(p.pid, 0)
    finally:
        wall = time.monotonic() - t0
        done.set(); th.join(timeout=30)
        kill_group(signal.SIGKILL)  # orphaned workers must not outlive their unit
        for f in (so, se):
            if hasattr(f, "close"):
                f.close()
    p.returncode = os.waitstatus_to_exitcode(status) if hasattr(os, "waitstatus_to_exitcode") else (status >> 8)
    rc = p.returncode
    maxrss = ru.ru_maxrss / (1024 * 1024 if sys.platform == "darwin" else 1024)
    rr = RunResult(exit_code=rc if rc >= 0 else None, signal=-rc if rc < 0 else None, timed_out=state["timed_out"],
                   wall_seconds=round(wall, 3), peak_rss_tree_mb=round(state["peak_tree_kb"] / 1024, 1) if linux else None,
                   max_rss_single_mb=round(maxrss, 1), rss_samples=state["samples"])
    if gpu:
        rr["gpu"] = dict(baseline_mb=gpu_base, peak_used_mb=state["gpu_peak"],
                         peak_delta_mb=(round(state["gpu_peak"] - gpu_base, 1) if state["gpu_peak"] is not None and gpu_base is not None else None),
                         peak_process_mb=state["gpu_proc_peak"], max_util_pct=state["gpu_util"])
    return rr


def tail(path, n=3000):
    try:
        data = Path(path).read_bytes()
    except OSError:
        return ""
    return data[-n:].decode("utf-8", "replace")


ERROR_PATTERNS = [
    ("cuda_no_kernel_image", re.compile(r"no kernel image is available", re.I)),
    ("license_refused", re.compile(r"licen[cs]e|consent|non-commercial", re.I)),
    ("unsupported", re.compile(r"unsupported ocr engine|not available in this build|requires a build|not compiled|"
                               r"feature is not enabled|requires an embedded|unavailable explicit", re.I)),
    ("model_unavailable", re.compile(r"download|no such model|model .*not found|unknown model|sha-?256|checksum|404|"
                                     r"registry|could not (?:resolve|load) model", re.I)),
]


def classify(rr, stderr_text):
    """Map a finished process to a unit status (see FINAL_STATUSES)."""
    if rr["timed_out"]:
        return "timeout"
    if re.search(r"no kernel image is available", stderr_text or "", re.I):
        return "cuda_no_kernel_image"
    if rr.get("signal"):
        return "oom_or_killed" if rr["signal"] == signal.SIGKILL else "crash"
    if rr.get("exit_code") not in (0, None):
        for status, rx in ERROR_PATTERNS:
            if rx.search(stderr_text or ""):
                return status
        return "error"
    return "ok"


def parse_cli_output(stdout_bytes):
    """`crispsorter -f json ocr` prints {file, engine, render, chars, text}; fall back to raw text.

    Diagnostics that precede the payload on stdout are tolerated: the last parseable JSON
    object that starts at a line beginning and carries "text" wins."""
    s = stdout_bytes.decode("utf-8", "replace")
    st = s.strip()
    starts = [0] + [i + 1 for i in range(len(st)) if st[i] == "\n" and st[i + 1:i + 2] == "{"]
    for pos in reversed(starts):
        if st[pos:pos + 1] != "{":
            continue
        try:
            d = json.loads(st[pos:])
        except json.JSONDecodeError:
            continue
        if isinstance(d, dict) and "text" in d:
            return d.get("text") or "", "json", {k: v for k, v in d.items() if k != "text"}
    return s, "raw", {}


# ─────────────────────────────────────── units ────────────────────────────────────────

def unit_key(device, engine_id, page_id):
    return f"{safe_name(device)}/{safe_name(engine_id)}/{safe_name(page_id)}"


def unit_path(run_id, key):
    return f"results/{safe_name(run_id)}/units/{key}.json"


def skipped_path(run_id, key):
    return f"results/{safe_name(run_id)}/skipped/{key}.json"


def enumerate_units(engines, pages, device):
    """Engine-major (one model load/download per engine), pages in manifest priority order."""
    ordered = sorted(pages, key=lambda p: (p.get("priority") or 9, p["id"]))
    return [(e, p) for e in engines for p in ordered]


def parse_shard(spec):
    if not spec:
        return 0, 1
    m = re.fullmatch(r"(\d+)/(\d+)", spec.strip())
    if not m or int(m.group(2)) < 1 or int(m.group(1)) >= int(m.group(2)):
        raise ValueError(f"bad --shard {spec!r} (want i/n with 0 <= i < n)")
    return int(m.group(1)), int(m.group(2))


def shard_units(units, spec):
    """Round-robin within each engine, so every shard gets a share of every engine's pages."""
    i, n = parse_shard(spec)
    if n == 1:
        return list(units)
    out, per_engine = [], {}
    for e, p in units:
        k = per_engine.get(e["id"], 0); per_engine[e["id"]] = k + 1
        if k % n == i:
            out.append((e, p))
    return out


def done_keys_from_listing(paths, run_id):
    prefix = f"results/{run_id}/units/"
    return {p[len(prefix):-5] for p in paths if p.startswith(prefix) and p.endswith(".json")}


def local_final_units(local_root, run_id):
    """Locally written final records (may predate an upload that never landed)."""
    base = Path(local_root) / "results" / run_id / "units"
    out = {}
    if base.is_dir():
        for f in base.rglob("*.json"):
            try:
                rec = json.loads(f.read_text())
            except (OSError, json.JSONDecodeError):
                continue
            if rec.get("status") in FINAL_STATUSES:
                out[f.relative_to(base).as_posix()[:-5]] = f
    return out


# ─────────────────────────────────────── CLI probe ────────────────────────────────────

def probe_cli(binary, env, timeout=60):
    """Detect optional CLI surfaces from help text (never runs a bare/unknown subcommand,
    which would start the GUI)."""
    def run(args):
        try:
            r = subprocess.run([binary] + args, env=env, capture_output=True, text=True, timeout=timeout,
                               stdin=subprocess.DEVNULL)
            return r.returncode, r.stdout + r.stderr
        except Exception as e:
            return None, repr(e)

    caps = dict(models_pull=False, ocr_download_flag=None, version=None, doctor=None)
    rc, top = run(["--help"])
    caps["help_ok"] = rc == 0
    rc, ocr_help = run(["ocr", "--help"])
    for flag in ("--download-models", "--yes"):
        if re.search(rf"(^|\s){re.escape(flag)}\b", ocr_help or ""):
            caps["ocr_download_flag"] = flag
            break
    if re.search(r"^\s+models\s", top or "", re.M):
        rc, mh = run(["models", "pull", "--help"])
        caps["models_pull"] = rc == 0
        caps["models_pull_help"] = (mh or "")[:2000]
    rc, out = run(["-f", "json", "version"])
    try:
        caps["version"] = json.loads(out.strip().splitlines()[-1]) if rc == 0 else None
    except Exception:
        caps["version"] = (out or "")[:500]
    rc, out = run(["-f", "json", "doctor"])
    try:
        caps["doctor"] = json.loads(out) if rc == 0 else (out or "")[:1000]
    except Exception:
        caps["doctor"] = (out or "")[:1000]
    return caps


def host_info(device):
    info = dict(hostname=socket.gethostname(), platform=platform.platform(), python=sys.version.split()[0],
                cpu_count=os.cpu_count(), machine=platform.machine())
    try:
        info["cpu_model"] = next(l.split(":", 1)[1].strip() for l in open("/proc/cpuinfo") if l.startswith("model name"))
        info["mem_total_gb"] = round(int(next(l.split()[1] for l in open("/proc/meminfo") if l.startswith("MemTotal"))) / 2**20, 1)
    except Exception:
        pass
    try:
        info["glibc"] = " ".join(platform.libc_ver())
    except Exception:
        pass
    for k in ("GITHUB_RUN_ID", "GITHUB_RUN_ATTEMPT", "GITHUB_JOB", "GITHUB_SHA", "KAGGLE_KERNEL_RUN_TYPE", "KAGGLE_URL_BASE"):
        if os.environ.get(k):
            info[k.lower()] = os.environ[k]
    if device == "gpu":
        rows = nvidia_smi("name,compute_cap,driver_version,memory.total")
        info["gpus"] = [dict(name=r[0], compute_cap=r[1], driver=r[2], memory_total_mb=r[3]) for r in rows or [] if len(r) >= 4]
    return info


def find_build_info(binary, explicit=None):
    cands = [explicit] if explicit else []
    b = Path(binary).resolve()
    cands += [b.parent / "BUILD-INFO.json", b.parent.parent / "BUILD-INFO.json"]
    for c in cands:
        if c and Path(c).is_file():
            try:
                return json.loads(Path(c).read_text())
            except json.JSONDecodeError:
                pass
    return None


def dir_snapshot(paths):
    snap = {}
    for root in paths:
        root = Path(root)
        if root.is_dir():
            for f in root.rglob("*"):
                if f.is_file():
                    try:
                        snap[str(f)] = f.stat().st_size
                    except OSError:
                        pass
    return snap


def free_gb(path):
    try:
        return shutil.disk_usage(path).free / 2**30
    except OSError:
        return None


# ─────────────────────────────────────── runner ───────────────────────────────────────

class Bench:
    def __init__(self, args, store, scorer=None, pages=None, corpus_identity=None, engines=None, fetch=fetch_url):
        self.a = args
        self.store = store
        self.t_start = time.time()
        self.session = args.session_id or f"{time.strftime('%Y%m%dT%H%M%S', time.gmtime())}-{uuid.uuid4().hex[:6]}"
        safe_name(self.session); safe_name(args.run_id)
        self.work = Path(args.work_dir).resolve()
        self.local = self.work / "store-mirror"
        self.cache = Path(args.model_cache).resolve() if args.model_cache else self.work / "models"
        self.profile = self.work / "profile"
        for d in (self.work, self.local, self.cache, self.profile, self.work / "logs", self.work / "cuda-cache"):
            d.mkdir(parents=True, exist_ok=True)
        self.scorer = scorer or Scorer()
        self.engines = engines
        self.pages, self.corpus_identity = pages, corpus_identity
        self.counts = {}
        self.current = None
        self.total = self.done_now = 0
        self.stop_reason = None
        self.last_status = None
        self.binary_sha = None
        shard = parse_shard(args.shard)
        self.shard_label = f"shard{shard[0]}of{shard[1]}"
        self.uploader = Uploader(store, interval=args.upload_interval, heartbeat=self.heartbeat,
                                 label=f"{args.run_id}/{args.device}/{self.shard_label}")
        self.images = ImageProvider(self.work / "images", store=store, uploader=self.uploader, fetch=fetch)
        self.caps = {}
        self.build_info = None

    # -- store helpers: everything is written locally first, then queued
    def put(self, path, obj):
        data = (json.dumps(obj, indent=1, ensure_ascii=False) + "\n").encode()
        dst = self.local / path
        dst.parent.mkdir(parents=True, exist_ok=True)
        dst.write_bytes(data)
        self.uploader.add(path, data)

    def elapsed(self):
        return time.time() - self.t_start

    def heartbeat(self):
        path = f"results/{self.a.run_id}/progress/{self.a.device}-{self.shard_label}.json"
        return path, (json.dumps(dict(
            schema=PROGRESS_SCHEMA, run_id=self.a.run_id, device=self.a.device, shard=self.a.shard or "0/1",
            session=self.session, runner_version=RUNNER_VERSION, updated_at=now_iso(),
            elapsed_seconds=round(self.elapsed(), 1), budget_seconds=self.a.budget_seconds or None,
            units_total=self.total, units_done_this_session=self.done_now, current=self.current,
            status_counts=self.counts, last_status=self.last_status, stop_reason=self.stop_reason,
            uploads_pending=len(self.uploader.pending), upload_failures=self.uploader.failures,
            binary_sha256=self.binary_sha, binary_git_sha=(self.build_info or {}).get("git_sha"),
            host=socket.gethostname(),
        ), indent=1) + "\n").encode()

    def base_env(self):
        env = dict(os.environ)
        for k in ("HF_TOKEN", "HUGGING_FACE_HUB_TOKEN", "KAGGLE_TOKEN", "KAGGLE_API_TOKEN", "GH_TOKEN", "GITHUB_TOKEN"):
            env.pop(k, None)  # the OCR CLI never needs our credentials
        libdirs = [d for d in (self.a.lib_dir, str(Path(self.a.binary).resolve().parent.parent / "lib")) if d and Path(d).is_dir()]
        if libdirs:
            env["LD_LIBRARY_PATH"] = ":".join(libdirs + ([env["LD_LIBRARY_PATH"]] if env.get("LD_LIBRARY_PATH") else []))
        env.update(CRISPEMBED_CACHE_DIR=str(self.cache), CRISPSORTER_ACCEPT_INTENDED_PURPOSE="1",
                   CUDA_CACHE_PATH=str(self.work / "cuda-cache"), CUDA_CACHE_MAXSIZE="4294967296",
                   RUST_BACKTRACE="1")
        if self.a.device == "cpu":
            env["CRISPEMBED_FORCE_CPU"] = "1"
            env["CUDA_VISIBLE_DEVICES"] = ""
        env.pop("DISPLAY", None); env.pop("WAYLAND_DISPLAY", None)  # never let a mis-parsed call open the GUI
        return env

    def ocr_argv(self, engine, page, image):
        argv = [str(Path(self.a.binary).resolve()), "--format", "json", "--accept-license", "ocr", str(image),
                "--engine", engine["engine"], "--data-dir", str(self.profile)]
        lang = page.get("language") if engine.get("language") == "page" else engine.get("language")
        if lang:
            argv += ["--language", lang]
        argv += list(engine.get("args", []))
        if self.caps.get("ocr_download_flag"):
            argv.append(self.caps["ocr_download_flag"])
        return argv, lang

    def redact(self, argv):
        w = str(self.work)
        return [a.replace(w, "$WORK") for a in argv]

    def engine_env(self, engine):
        env = self.base_env()
        env.update(engine.get("env", {}))
        return env

    # -- per-engine pull + warm-up (measured once per engine and session)
    def prepare_engine(self, engine, pages):
        rec = dict(schema=ENGINE_SCHEMA, run_id=self.a.run_id, device=self.a.device, engine_id=engine["id"],
                   engine=engine["engine"], session=self.session, started_at=now_iso(), runner_version=RUNNER_VERSION,
                   license_acceptance=dict(flag="--accept-license", accepted=True, license_gated=bool(engine.get("license_gated")),
                                           reason="internal evaluation benchmark"))
        before = dir_snapshot([self.cache, self.profile / "models"])
        env = self.engine_env(engine)
        if self.caps.get("models_pull"):
            argv = [str(Path(self.a.binary).resolve()), "--accept-license", "models", "pull", engine["engine"]]
            left = self.budget_left()
            pto = self.a.pull_timeout if left is None else max(60.0, min(self.a.pull_timeout, left / 2))
            rr = run_measured(argv, env, pto, stdout_path=self.work / "logs" / "pull.out",
                              stderr_path=self.work / "logs" / "pull.err")
            rec["pull"] = dict(method="models pull", command=self.redact(argv), exit_code=rr["exit_code"],
                               timed_out=rr["timed_out"], seconds=rr["wall_seconds"],
                               stderr_tail=tail(self.work / "logs" / "pull.err", 1500))
        else:
            rec["pull"] = dict(method="auto-download during warm-up (no `models pull` in this CLI)")
        warm = None
        if not self.a.no_warmup and pages:
            # Smallest selected page whose image is obtainable.
            for p in sorted(pages, key=lambda p: p["image"].get("bytes") or 0):
                try:
                    img = self.images.get(p); warm = (p, img); break
                except ImageUnavailable:
                    continue
        if warm:
            p, img = warm
            argv, _ = self.ocr_argv(engine, p, img)
            to = max(self.a.pull_timeout, self.unit_timeout(engine))
            left = self.budget_left()
            if left is not None:
                to = max(60.0, min(to, left - 120))
            rr = run_measured(argv, env, to, gpu=self.a.device == "gpu", stdout_path=self.work / "logs" / "warm.out",
                              stderr_path=self.work / "logs" / "warm.err")
            err = tail(self.work / "logs" / "warm.err")
            rec["warmup"] = dict(page=p["id"], command=self.redact(argv), status=classify(rr, err), **rr,
                                 stderr_tail=err[-1500:],
                                 note="first run: includes model download (if not pulled), model load and, on GPU, PTX JIT; not scored")
            if rec["warmup"]["status"] == "cuda_no_kernel_image":
                banner(f"CUDA 'no kernel image is available' for engine {engine['id']}: the CrispEmbed CUDA build "
                       f"has no kernel for this GPU's compute capability. Units will record cuda_no_kernel_image.")
        after = dir_snapshot([self.cache, self.profile / "models"])
        new = {f: s for f, s in after.items() if before.get(f) != s}
        rec["models"] = [dict(file=f.replace(str(self.work), "$WORK"), bytes=s,
                              sha256=(sha256_file(f) if self.a.hash_models else None)) for f, s in sorted(new.items())]
        rec["model_bytes_added"] = sum(new.values())
        rec["finished_at"] = now_iso()
        self.put(f"results/{self.a.run_id}/engines/{self.a.device}/{engine['id']}/{self.session}.json", rec)
        return rec

    def purge_models(self):
        for d in (self.cache, self.profile / "models"):
            if d.is_dir():
                shutil.rmtree(d, ignore_errors=True)
        self.cache.mkdir(parents=True, exist_ok=True)

    def unit_timeout(self, engine):
        t = engine["timeout_s"][self.a.device]
        return min(t, self.a.unit_timeout) if self.a.unit_timeout else t

    def run_unit(self, engine, page, key):
        img = self.images.get(page)
        argv, lang = self.ocr_argv(engine, page, img)
        so, se = self.work / "logs" / "unit.out", self.work / "logs" / "unit.err"
        started = now_iso()
        rr = run_measured(argv, self.engine_env(engine), self.unit_timeout(engine), gpu=self.a.device == "gpu",
                          stdout_path=so, stderr_path=se)
        err = tail(se, 4000)
        full_err = tail(se, 2_000_000)
        out_bytes = so.read_bytes() if so.exists() else b""
        status = classify(rr, full_err)
        text, fmt, payload = parse_cli_output(out_bytes) if status == "ok" else ("", None, {})
        metrics = self.scorer.score(text, page["ground_truth"]) if status == "ok" else None
        pm = page["primary_metric"]
        gpu = rr.pop("gpu", None)
        rec = dict(
            schema=UNIT_SCHEMA, run_id=self.a.run_id, unit=key, device=self.a.device, engine_id=engine["id"],
            engine=engine["engine"], page_id=page["id"], corpus=self.corpus_identity, status=status,
            exit_code=rr["exit_code"], signal=rr["signal"], timed_out=rr["timed_out"],
            timeout_seconds=self.unit_timeout(engine), stderr_tail=err[-2500:] if status != "ok" else err[-400:],
            command=self.redact(argv), env_overrides=engine.get("env", {}),
            settings=dict(requested=dict(engine=engine["engine"], language=lang, args=engine.get("args", []),
                                         download_flag=self.caps.get("ocr_download_flag")),
                          effective=dict(payload, output_format=fmt)),
            input=dict(id=page["id"], sha256=page["image"]["sha256"], bytes=page["image"].get("bytes"),
                       width=page["image"].get("width"), height=page["image"].get("height"), tags=page["tags"],
                       ground_truth_sha256=page["ground_truth_sha256"], max_tokens=page.get("max_tokens")),
            metrics=metrics, primary_metric=pm, primary_value=(metrics or {}).get(pm),
            timing=dict(started_at=started, finished_at=now_iso(), wall_seconds=rr["wall_seconds"],
                        elapsed_ms=int(rr["wall_seconds"] * 1000)),
            memory=dict(peak_rss_tree_mb=rr["peak_rss_tree_mb"], max_rss_single_mb=rr["max_rss_single_mb"],
                        rss_samples=rr["rss_samples"]),
            gpu=gpu,
            output=dict(text=text[:MAX_TEXT_CHARS], text_truncated=len(text) > MAX_TEXT_CHARS, chars=len(text),
                        text_sha256=sha256_bytes(text.encode()), stdout_sha256=sha256_bytes(out_bytes),
                        stderr_sha256=sha256_file(se) if se.exists() else None),
            binary=dict(sha256=self.binary_sha, git_sha=(self.build_info or {}).get("git_sha"),
                        variant=(self.build_info or {}).get("variant"), version=self.caps.get("version")),
            scorer=self.scorer.identity, session=self.session, host=socket.gethostname(),
            license_acceptance=dict(flag="--accept-license", accepted=True, license_gated=bool(engine.get("license_gated")),
                                    reason="internal evaluation benchmark"),
            runner_version=RUNNER_VERSION, recorded_at=now_iso(),
        )
        rec["stderr_markers"] = dict(
            cuda=bool(re.search(r"ggml_cuda|CUDA0|cuda backend", full_err, re.I)),
            cpu_fallback=bool(re.search(r"fall(?:ing)? ?back to (?:the )?cpu", full_err, re.I)),
            empty_output=status == "ok" and not text.strip())
        if gpu and status == "ok" and not (gpu.get("peak_process_mb") or 0) and not (gpu.get("peak_delta_mb") or 0) > 64:
            rec["gpu_note"] = "no GPU memory attributed to the process tree: this engine probably ran on the CPU"
        return rec

    def write_session(self, extra=None):
        rec = dict(schema=SESSION_SCHEMA, run_id=self.a.run_id, session=self.session, device=self.a.device,
                   shard=self.a.shard or "0/1", runner_version=RUNNER_VERSION, started_at=time.strftime(
                       "%Y-%m-%dT%H:%M:%SZ", time.gmtime(self.t_start)), host=host_info(self.a.device),
                   binary=dict(path=str(self.a.binary), sha256=self.binary_sha), build_info=self.build_info,
                   cli=self.caps, corpus=self.corpus_identity, scorer=self.scorer.identity,
                   args={k: v for k, v in vars(self.a).items() if k not in ("hf_token_file",)},
                   image_receipts=self.images.receipts)
        rec.update(extra or {})
        self.put(f"results/{self.a.run_id}/sessions/{self.session}.json", rec)
        return rec

    def run(self):
        a = self.a
        self.binary_sha = sha256_file(a.binary)
        self.build_info = find_build_info(a.binary, a.build_info)
        log(f"runner {RUNNER_VERSION} | run {a.run_id} | device {a.device} | shard {a.shard or '0/1'} | session {self.session}")
        log(f"binary {a.binary} sha256 {self.binary_sha} git_sha {(self.build_info or {}).get('git_sha')} "
            f"variant {(self.build_info or {}).get('variant')}")
        self.caps = probe_cli(a.binary, self.base_env())
        log("cli:", json.dumps({k: v for k, v in self.caps.items() if k not in ("doctor", "models_pull_help")}))
        units = shard_units(enumerate_units(self.engines, self.pages, a.device), a.shard)
        listing = self.store.list(f"results/{a.run_id}/units/{a.device}")
        remote_done = done_keys_from_listing(listing, a.run_id)
        local_done = local_final_units(self.local, a.run_id)
        for k, f in local_done.items():
            if k not in remote_done:  # finished earlier, upload never landed
                self.uploader.add(unit_path(a.run_id, k), f.read_bytes())
        done = remote_done | set(local_done)
        todo = [(e, p) for e, p in units if unit_key(a.device, e["id"], p["id"]) not in done]
        self.total = len(units)
        self.counts = {"already_done": len(units) - len(todo)}
        log(f"{len(units)} unit(s) in this shard; {len(units) - len(todo)} already stored; {len(todo)} to run")
        self.write_session()
        self.uploader.start()
        try:
            self._loop(todo)
        except KeyboardInterrupt:
            self.stop_reason = self.stop_reason or "interrupted"
        finally:
            self.current = None
            self.stop_reason = self.stop_reason or "complete"
            self.write_session(dict(finished_at=now_iso(), stop_reason=self.stop_reason, status_counts=self.counts,
                                    elapsed_seconds=round(self.elapsed(), 1)))
            ok = self.uploader.close()
            log(f"finished: {self.stop_reason}; counts {self.counts}; all uploads landed: {ok}")
        return self.stop_reason

    def _loop(self, todo):
        a = self.a
        by_engine = {}
        for e, p in todo:
            by_engine.setdefault(e["id"], (e, []))[1].append(p)
        for eid, (engine, pages) in by_engine.items():
            # Room for a warm-up and at least one unit, each bounded by the unit timeout.
            if self.budget_left() is not None and self.budget_left() < 2 * self.unit_timeout(engine) + 180:
                self.stop_reason = "budget"
                log(f"budget: not starting engine {eid}"); return
            fg = free_gb(self.cache)
            if fg is not None and fg < a.min_free_gb:
                log(f"only {fg:.1f} GB free: purging the model cache before {eid}")
                self.purge_models()
            log(f"== engine {eid} ({engine['engine']}): {len(pages)} unit(s)")
            self.current = dict(engine=eid, phase="prepare")
            prep = self.prepare_engine(engine, pages)
            w = prep.get("warmup") or {}
            log(f"   prepare: pull={prep['pull'].get('seconds')} warmup={w.get('status')} {w.get('wall_seconds')}s "
                f"models +{prep['model_bytes_added'] / 2**20:.0f} MB")
            consecutive = 1 if w.get("status") in BREAKER_STATUSES else 0
            for page in pages:
                key = unit_key(a.device, eid, page["id"])
                left = self.budget_left()
                if left is not None and left < self.unit_timeout(engine) + 30:
                    self.stop_reason = "budget"
                    log(f"budget: {left:.0f}s left < unit timeout {self.unit_timeout(engine)}s; stopping"); return
                if consecutive >= a.breaker:
                    self.put(skipped_path(a.run_id, key), dict(schema=UNIT_SCHEMA, run_id=a.run_id, unit=key, device=a.device,
                             engine_id=eid, page_id=page["id"], status="skipped_breaker", recorded_at=now_iso(),
                             reason=f"{consecutive} consecutive {sorted(BREAKER_STATUSES)} for this engine in session {self.session}"))
                    self.counts["skipped_breaker"] = self.counts.get("skipped_breaker", 0) + 1
                    continue
                self.current = dict(engine=eid, page=page["id"], started_at=now_iso())
                try:
                    rec = self.run_unit(engine, page, key)
                except ImageUnavailable as e:
                    banner(f"IMAGE UNAVAILABLE {page['id']}: {e}")
                    self.counts["skipped_image"] = self.counts.get("skipped_image", 0) + 1
                    continue
                self.put(unit_path(a.run_id, key), rec)
                st = rec["status"]
                self.counts[st] = self.counts.get(st, 0) + 1
                self.done_now += 1
                self.last_status = dict(unit=key, status=st, wall=rec["timing"]["wall_seconds"])
                consecutive = consecutive + 1 if st in BREAKER_STATUSES else 0
                m = rec["metrics"] or {}
                log(f"   {key}: {st} {rec['timing']['wall_seconds']}s rss={rec['memory']['peak_rss_tree_mb']}MB "
                    f"cer={m.get('cer')} cer_ns={m.get('cer_nospace')} wer={m.get('wer')} rec={m.get('word_recall')}"
                    + (f" gpu={rec['gpu'].get('peak_process_mb')}MB" if rec.get("gpu") else ""))
                if st == "cuda_no_kernel_image":
                    banner("CUDA: no kernel image is available for this GPU (see stderr_tail in the unit JSON)")
            if a.purge_models:
                self.purge_models()

    def budget_left(self):
        if not self.a.budget_seconds:
            return None
        return self.a.budget_seconds - self.elapsed()


# ──────────────────────────────────────── main ────────────────────────────────────────

def build_parser():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("run", help="run (or resume) benchmark units")
    r.add_argument("--binary", required=True, help="crispsorter executable")
    r.add_argument("--lib-dir", help="directory with libcrispembed/libggml (added to LD_LIBRARY_PATH)")
    r.add_argument("--build-info", help="BUILD-INFO.json of the binary tarball (default: next to the binary)")
    r.add_argument("--device", choices=["cpu", "gpu"], required=True)
    r.add_argument("--run-id", required=True, help="results/<run-id>/; reuse the same id to resume")
    r.add_argument("--store", default=f"hf:{DEFAULT_REPO}", help="hf:<repo_id> (private dataset) or dir:<path>")
    r.add_argument("--hf-token-file", help="file holding the HF token (else $HF_TOKEN or the Kaggle dataset mount)")
    r.add_argument("--corpus", default="gold-v1", choices=["gold-v1", "real-scans-v2"])
    r.add_argument("--engines", default="default", help="default | all | id,id,... | group:<name>")
    r.add_argument("--engines-file", default=str(DEFAULT_ENGINES))
    r.add_argument("--pages", default="all", help="all | page id list")
    r.add_argument("--tags", default="", help="only pages carrying any of these tags")
    r.add_argument("--shard", default="0/1", help="i/n: this process runs every n-th page of each engine, from i")
    r.add_argument("--budget-seconds", type=float, default=0, help="stop starting units that could end after this (0 = none)")
    r.add_argument("--unit-timeout", type=float, default=0, help="cap on the per-engine page timeout (0 = engines.json)")
    r.add_argument("--pull-timeout", type=float, default=3600, help="timeout for models pull / the warm-up run")
    r.add_argument("--breaker", type=int, default=3, help="consecutive timeouts/crashes before an engine's remaining pages are skipped")
    r.add_argument("--upload-interval", type=float, default=60, help="seconds between store commits (also the heartbeat)")
    r.add_argument("--work-dir", default="ocr-bench-work")
    r.add_argument("--model-cache", help="CRISPEMBED_CACHE_DIR (default <work>/models)")
    r.add_argument("--purge-models", action="store_true", help="delete downloaded models after each engine (disk-limited hosts)")
    r.add_argument("--min-free-gb", type=float, default=8, help="purge the model cache before an engine below this")
    r.add_argument("--no-warmup", action="store_true", help="skip the unscored warm-up page (download time then lands in the first unit)")
    r.add_argument("--hash-models", action=argparse.BooleanOptionalAction, default=True, help="sha256 every downloaded model file")
    r.add_argument("--session-id", help="override the generated session id")
    p = sub.add_parser("plan", help="print the CPU/GPU job matrix as JSON")
    p.add_argument("--device", choices=["cpu", "gpu"], default="cpu")
    p.add_argument("--engines", default="default")
    p.add_argument("--engines-file", default=str(DEFAULT_ENGINES))
    e = sub.add_parser("engines", help="list engine configurations")
    e.add_argument("--device", choices=["cpu", "gpu"])
    e.add_argument("--engines-file", default=str(DEFAULT_ENGINES))
    return ap


def main(argv=None):
    a = build_parser().parse_args(argv)
    if a.cmd == "plan":
        jobs = plan_matrix(load_engines(a.engines_file), a.device, a.engines)
        print(json.dumps(dict(include=jobs)))
        return 0
    if a.cmd == "engines":
        for e in load_engines(a.engines_file):
            if a.device and a.device not in e["devices"]:
                continue
            print(f"{e['id']:20s} --engine {e['engine']:16s} {','.join(e['devices']):8s} group={e.get('group')} "
                  f"default={e.get('default', True)} {' '.join(e.get('args', []))}")
        return 0
    if not Path(a.binary).is_file():
        sys.exit(f"binary not found: {a.binary}")
    work = Path(a.work_dir); work.mkdir(parents=True, exist_ok=True)
    pages, ident = load_corpus(a.corpus, work / "manifests")
    pages = select_pages(pages, a.pages, a.tags)
    engines = select_engines(load_engines(a.engines_file), a.engines, a.device)
    if not pages or not engines:
        sys.exit(f"nothing to run: {len(pages)} page(s), {len(engines)} engine(s)")
    store = open_store(a.store, token=read_token(a.hf_token_file))
    bench = Bench(a, store, pages=pages, corpus_identity=ident, engines=engines)

    def on_term(signum, frame):
        bench.stop_reason = f"signal {signum}"
        raise KeyboardInterrupt
    signal.signal(signal.SIGTERM, on_term)
    reason = bench.run()
    return 0 if reason in ("complete", "budget") else 1


if __name__ == "__main__":
    sys.exit(main())
