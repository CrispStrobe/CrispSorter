#!/usr/bin/env python3
"""Render and push the OCR-bench GPU kernel without committing an account name or token.

Modeled on CrispEmbed's tools/kaggle/kpush.py: The built-in defaults and kernel.py carry the
placeholder ${KAGGLE_ACCOUNT}; this renders a temporary copy with the account from the
environment plus the run parameters (@RUN_ID@, @BINARY_SHA@, ...) and pushes it. Kaggle uploads
only the code_file, so everything the kernel needs is baked into the rendered kernel.py.

    KAGGLE_ACCOUNT=<username> [KAGGLE_TOKEN=<api token>] python3 bench/ocr/kaggle/kpush.py \\
        --run-id ocr-2026-11-12 --binary-sha <40-hex> [--runner-sha <40-hex>] \\
        [--engines default] [--shard 0/1] [--slot a] [--corpus gold-v1] [--budget-hours 11.5] [--dry-run DIR]

KAGGLE_TOKEN is optional: without it the kaggle CLI uses its own stored credentials
(~/.kaggle/access_token). Kaggle allows at most two concurrent GPU sessions per account; use
--slot a / --slot b (distinct kernel slugs) with --shard 0/2 / 1/2 to run two at once. Pushing
a slot again starts a new version of that kernel (and replaces its log; save it first).
One Kaggle account per person (Kaggle Terms of Use): never point this at a second account.
"""
import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
# Public defaults are account-neutral; local metadata overrides stay ignored.
DEFAULT_METADATA = {'id': '${KAGGLE_ACCOUNT}/crispsorter-ocr-bench-gpu-@SLOT@',
 'title': 'crispsorter-ocr-bench-gpu-@SLOT@',
 'code_file': 'kernel.py',
 'language': 'python',
 'kernel_type': 'script',
 'is_private': 'true',
 'enable_gpu': 'true',
 'enable_tpu': 'false',
 'enable_internet': 'true',
 'competition_sources': [],
 'dataset_sources': ['${KAGGLE_ACCOUNT}/crispasr-hf-token'],
 'kernel_sources': [],
 'model_sources': []}
ACCOUNT = "${KAGGLE_ACCOUNT}"


def render(text, params):
    for k, v in params.items():
        text = text.replace(k, v)
    return text


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--run-id", required=True)
    ap.add_argument("--binary-sha", required=True, help="build to benchmark (bin/<sha>/cuda.tar.gz on HF)")
    ap.add_argument("--runner-sha", help="CrispSorter commit whose bench/ocr scripts the kernel downloads (default: --binary-sha)")
    ap.add_argument("--engines", default="default")
    ap.add_argument("--shard", default="0/1")
    ap.add_argument("--slot", default="a", choices=["a", "b"])
    ap.add_argument("--corpus", default="gold-v1", choices=["gold-v1", "real-scans-v2"])
    ap.add_argument("--pages", default="all")
    ap.add_argument("--budget-hours", type=float, default=11.5)
    ap.add_argument("--dry-run", metavar="DIR", help="write the rendered kernel dir here instead of pushing")
    a, extra = ap.parse_known_args(argv)
    for name in ("binary_sha", "runner_sha"):
        v = getattr(a, name)
        if v and not re.fullmatch(r"[0-9a-f]{40}", v):
            sys.exit(f"--{name.replace('_', '-')} must be a full 40-hex commit")
    if not re.fullmatch(r"[A-Za-z0-9._-]{1,128}", a.run_id):
        sys.exit("bad --run-id")
    if not 0.5 <= a.budget_hours <= 11.75:
        sys.exit("--budget-hours must leave room inside Kaggle's 12 h session limit")
    account = os.environ.get("KAGGLE_ACCOUNT", "").strip()
    if not account and not a.dry_run:
        sys.exit("kpush: set KAGGLE_ACCOUNT (and KAGGLE_TOKEN, or rely on the kaggle CLI's stored credentials)")
    params = {ACCOUNT: account or "ACCOUNT", "@RUN_ID@": a.run_id, "@BINARY_SHA@": a.binary_sha,
              "@RUNNER_SHA@": a.runner_sha or a.binary_sha, "@ENGINES@": a.engines, "@SHARD@": a.shard, "@SLOT@": a.slot,
              "@CORPUS@": a.corpus, "@PAGES@": a.pages, "@BUDGET_HOURS@": str(a.budget_hours)}
    metadata_path = HERE / "kernel-metadata.json"
    metadata_text = metadata_path.read_text() if metadata_path.exists() else json.dumps(DEFAULT_METADATA)
    meta = json.loads(render(metadata_text, params))
    code = render((HERE / meta["code_file"]).read_text(), params)
    leftovers = [k for k in params if k in code or k in json.dumps(meta)]
    if leftovers:
        sys.exit(f"kpush: unrendered placeholders {leftovers}")
    with tempfile.TemporaryDirectory(prefix="kpush-") as tmp:
        dst = Path(a.dry_run) if a.dry_run else Path(tmp) / "kernel"
        dst.mkdir(parents=True, exist_ok=True)
        (dst / "kernel-metadata.json").write_text(json.dumps(meta, indent=2) + "\n")
        (dst / meta["code_file"]).write_text(code)
        print(f"rendered {meta['id']} (run {a.run_id}, binary {a.binary_sha[:12]}, engines {a.engines}, shard {a.shard})")
        if a.dry_run:
            return 0
        env = dict(os.environ)
        if os.environ.get("KAGGLE_TOKEN"):
            env["KAGGLE_API_TOKEN"] = os.environ["KAGGLE_TOKEN"]
        kaggle = [shutil.which("kaggle")] if shutil.which("kaggle") else [sys.executable, "-m", "kaggle"]
        return subprocess.call(kaggle + ["kernels", "push", "-p", str(dst)] + extra, env=env)


if __name__ == "__main__":
    sys.exit(main())
