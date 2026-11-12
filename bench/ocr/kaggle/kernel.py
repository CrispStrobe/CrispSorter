#!/usr/bin/env python3
"""Kaggle GPU kernel: GPU arm of the CrispSorter OCR benchmark (bench/ocr/README.md).

Self-contained (Kaggle uploads only this file). Rendered by bench/ocr/kaggle/kpush.py, which
fills the @...@ parameters and ${KAGGLE_ACCOUNT}. Steps:
  1. print script version + parameters; record nvidia-smi and compute_cap (fail fast without a GPU);
  2. HF token from the attached private dataset crispasr-hf-token (never printed);
  3. pip install huggingface_hub + rapidfuzz (+ pyarrow if missing);
  4. bench/ocr scripts from GitHub at RUNNER_SHA; the CUDA build bin/<BINARY_SHA>/cuda.tar.gz from HF
     (sha256-verified, runtime apt packages installed, ldd/glibc checked loudly);
  5. runner.py run --device gpu with a budget so no unit can end after ~11.5 h; every finished unit is
     uploaded at once (Kaggle has no mid-flight log access: results/<run_id>/progress/ on HF is the
     only live view); 6. summarize.py.
No torch is used, so a P100 (sm_60) draw is usable: ggml's CUDA build carries compute_50 PTX that the
driver JIT-compiles for sm_60 (first load is slower; the JIT cache is kept per session). Below compute
capability 6.1 ggml has no integer MMQ path (dequant + cuBLAS instead): P100 timings are not
representative of newer GPUs, and the units record the GPU they ran on.
"""
import glob
import json
import os
import shutil
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

T0 = time.time()
SCRIPT_VERSION = "crispsorter-ocr-bench-kernel-2026-11-12.1"
RUN_ID = "@RUN_ID@"
BINARY_SHA = "@BINARY_SHA@"
RUNNER_SHA = "@RUNNER_SHA@"
ENGINES = "@ENGINES@"
SHARD = "@SHARD@"
SLOT = "@SLOT@"
CORPUS = "@CORPUS@"
PAGES = "@PAGES@"
BUDGET_HOURS = float("@BUDGET_HOURS@")
KAGGLE_ACCOUNT = "${KAGGLE_ACCOUNT}"
HF_REPO = "cstr/crispsorter-ocr-bench"
GH_RAW = "https://raw.githubusercontent.com/CrispStrobe/CrispSorter"
BENCH_FILES = ["runner.py", "summarize.py", "fetch_build.py", "engines.json", "vendor/PINS.json",
               "vendor/crispembed_score.py", "vendor/to_text_manifest.py"]
TMP = Path("/kaggle/temp") if Path("/kaggle/temp").is_dir() else Path("/kaggle/working/tmp")
OUT = Path("/kaggle/working")
TMP.mkdir(parents=True, exist_ok=True)


def log(*a):
    print(time.strftime("[%H:%M:%S]"), f"+{(time.time() - T0) / 60:.1f}m", *a, flush=True)


def banner(msg):
    print("!" * 100 + "\n" + msg + "\n" + "!" * 100, flush=True)


def sh(*args, check=True, **kw):
    log("+", " ".join(args))
    return subprocess.run(list(args), check=check, **kw)


log(SCRIPT_VERSION, json.dumps(dict(run_id=RUN_ID, binary_sha=BINARY_SHA, runner_sha=RUNNER_SHA, engines=ENGINES,
                                    shard=SHARD, slot=SLOT, corpus=CORPUS, pages=PAGES, budget_hours=BUDGET_HOURS)))
for v in (RUN_ID, BINARY_SHA, RUNNER_SHA):
    if v.startswith("@"):
        sys.exit("unrendered parameter: push this kernel with bench/ocr/kaggle/kpush.py")

# 1. GPU identity (fail fast: never hold a GPU session without a GPU)
smi = subprocess.run(["nvidia-smi", "--query-gpu=name,compute_cap,driver_version,memory.total", "--format=csv,noheader"],
                     capture_output=True, text=True)
print(subprocess.run(["nvidia-smi"], capture_output=True, text=True).stdout, flush=True)
if smi.returncode != 0 or not smi.stdout.strip():
    banner("NO GPU VISIBLE (nvidia-smi failed): exiting without running the benchmark"); sys.exit(2)
name, cap, driver, mem = [c.strip() for c in smi.stdout.strip().splitlines()[0].split(",")]
log(f"GPU {name} compute_cap {cap} driver {driver} memory {mem}")
cap_f = float(cap)
if cap_f < 5.0:
    banner(f"compute capability {cap} is below ggml's lowest PTX target (compute_50): exiting"); sys.exit(2)
if cap_f < 6.1:
    banner(f"compute capability {cap} (P100 = sm_60): ggml has no integer-MMQ path below 6.1; quantized matmuls "
           "run as dequant + cuBLAS. Results are valid for this GPU only.")
gpu_info = dict(name=name, compute_cap=cap, driver=driver, memory_total=mem)

# 2. token (dataset mount; the value is never printed)
tok_files = ["/kaggle/input/crispasr-hf-token/hf_token.txt",
             f"/kaggle/input/datasets/{KAGGLE_ACCOUNT}/crispasr-hf-token/hf_token.txt"] + \
    sorted(glob.glob("/kaggle/input/datasets/*/crispasr-hf-token/hf_token.txt"))
tok = next((Path(p).read_text().strip() for p in tok_files if Path(p).is_file()), "")
if not tok:
    banner("HF token not found: attach the private dataset crispasr-hf-token (dataset_sources)"); sys.exit(2)
os.environ["HF_TOKEN"] = tok

# 3. internet + deps
try:
    urllib.request.urlopen("https://huggingface.co/api/whoami-v2", timeout=30)
except urllib.error.HTTPError:
    pass  # 401 without auth header still proves connectivity
except Exception as e:
    banner(f"NO INTERNET on this worker ({e!r}): the benchmark needs HF + GitHub; exiting"); sys.exit(2)
sh(sys.executable, "-m", "pip", "install", "-q", "huggingface_hub>=0.25", "rapidfuzz>=3")
try:
    import pyarrow  # noqa: F401  (preinstalled on Kaggle; needed only for CORD receipt pages not yet cached on HF)
except Exception:
    sh(sys.executable, "-m", "pip", "install", "-q", "pyarrow>=14", check=False)

# 4a. bench scripts at the pinned commit
BENCH = TMP / "bench" / "ocr"
for rel in BENCH_FILES:
    dst = BENCH / rel
    dst.parent.mkdir(parents=True, exist_ok=True)
    for attempt in range(5):
        try:
            dst.write_bytes(urllib.request.urlopen(f"{GH_RAW}/{RUNNER_SHA}/bench/ocr/{rel}", timeout=120).read()); break
        except Exception as e:
            log(f"fetch {rel} failed ({e!r}); retry"); time.sleep(10 * (attempt + 1))
    else:
        banner(f"could not fetch bench/ocr/{rel} at {RUNNER_SHA}"); sys.exit(2)
log("bench scripts at", RUNNER_SHA)

# Early heartbeat: proves the session is alive before the (long) build download.
sys.path.insert(0, str(BENCH))
import runner  # noqa: E402
store = runner.HFStore(HF_REPO, token=tok)
hb_path = f"results/{RUN_ID}/progress/gpu-kernel-{SLOT}.json"


def kernel_heartbeat(phase, **extra):
    try:
        store.put_many([(hb_path, (json.dumps(dict(schema="crispsorter-ocr-bench-kernel-v1", run_id=RUN_ID, slot=SLOT,
                                                    phase=phase, script_version=SCRIPT_VERSION, binary_sha=BINARY_SHA,
                                                    runner_sha=RUNNER_SHA, gpu=gpu_info, updated_at=runner.now_iso(),
                                                    elapsed_seconds=round(time.time() - T0), **extra), indent=1) + "\n").encode())],
                       f"kernel {SLOT}: {phase}")
    except Exception as e:
        log("kernel heartbeat failed:", repr(e)[:200])


kernel_heartbeat("setup")

# 4b. CUDA build
r = subprocess.run([sys.executable, str(BENCH / "fetch_build.py"), "--repo", HF_REPO, "--sha", BINARY_SHA, "--variant", "cuda",
                    "--dest", str(TMP / "build"), "--apt"], capture_output=True, text=True)
print(r.stderr[-6000:], flush=True)
if r.returncode != 0:
    banner(f"BUILD FETCH/CHECK FAILED (exit {r.returncode}); see above")
    kernel_heartbeat("failed: build fetch", exit=r.returncode, stderr_tail=r.stderr[-2000:])
    sys.exit(3)
build = json.loads(r.stdout.strip().splitlines()[-1])
log("build", build)

# 5. benchmark, budgeted so the last unit (bounded by its timeout) ends before ~BUDGET_HOURS
margin = 15 * 60  # final uploads + summary
budget = int(BUDGET_HOURS * 3600 - (time.time() - T0) - margin)
log(f"runner budget {budget / 3600:.2f} h")
kernel_heartbeat("running", runner_budget_seconds=budget)
args = [sys.executable, str(BENCH / "runner.py"), "run", "--binary", build["binary"], "--lib-dir", build["lib_dir"],
        "--build-info", build["build_info"], "--device", "gpu", "--run-id", RUN_ID, "--store", f"hf:{HF_REPO}",
        "--corpus", CORPUS, "--pages", PAGES, "--engines", ENGINES, "--shard", SHARD, "--budget-seconds", str(budget),
        "--upload-interval", "60", "--work-dir", str(TMP / "work"), "--purge-models", "--min-free-gb", "6",
        "--session-id", f"kaggle-{SLOT}-{time.strftime('%Y%m%dT%H%M%S', time.gmtime())}"]
rc = subprocess.run(args).returncode
log("runner exit", rc)

# 6. summary of everything stored for this run so far (all sessions, CPU + GPU)
s = subprocess.run([sys.executable, str(BENCH / "summarize.py"), "--run-id", RUN_ID, "--store", f"hf:{HF_REPO}",
                    "--out", str(TMP / "summary")], capture_output=True, text=True)
print(s.stdout[-20000:], s.stderr[-3000:], flush=True)
for f in (TMP / "summary" / "summary").glob("*"):
    shutil.copy2(f, OUT / f.name)
kernel_heartbeat("finished", runner_exit=rc, summary_exit=s.returncode)
log("done")
