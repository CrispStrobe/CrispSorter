#!/usr/bin/env python3
"""Download + verify + unpack a benchmark build from the HF dataset repo (bin/<sha>/<variant>.tar.gz).

Prints one JSON line: {"binary": ..., "lib_dir": ..., "build_info": ..., "sha256": ...}.
With --apt, installs the Debian packages listed in runtime-apt-packages.txt (sudo when not root),
then fails loudly when `ldd` still reports unresolved libraries (libcuda.so.1 is only checked on
GPU hosts) or when the host glibc is older than the build requires.

usage: fetch_build.py --sha GIT_SHA --variant cpu|cuda [--repo cstr/crispsorter-ocr-bench] [--dest DIR] [--apt]
"""
import argparse
import hashlib
import json
import os
import platform
import re
import shutil
import subprocess
import sys
import tarfile
from pathlib import Path


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def token():
    if os.environ.get("HF_TOKEN"):
        return os.environ["HF_TOKEN"].strip()
    import glob
    for p in ["/kaggle/input/crispasr-hf-token/hf_token.txt"] + sorted(glob.glob("/kaggle/input/datasets/*/crispasr-hf-token/hf_token.txt")):
        if Path(p).is_file():
            return Path(p).read_text().strip()
    return None


def glibc_tuple(s):
    m = re.search(r"(\d+)\.(\d+)", s or "")
    return (int(m.group(1)), int(m.group(2))) if m else None


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--sha", required=True)
    ap.add_argument("--variant", required=True, choices=["cpu", "cuda"])
    ap.add_argument("--repo", default="cstr/crispsorter-ocr-bench")
    ap.add_argument("--dest", default="ocr-bench-build")
    ap.add_argument("--apt", action="store_true", help="install runtime-apt-packages.txt")
    a = ap.parse_args()
    if not re.fullmatch(r"[0-9a-f]{40}", a.sha):
        sys.exit("--sha must be the full 40-hex commit the build workflow printed")
    from huggingface_hub import hf_hub_download
    tok = token()
    dest = Path(a.dest).resolve(); dest.mkdir(parents=True, exist_ok=True)
    tgz = hf_hub_download(a.repo, f"bin/{a.sha}/{a.variant}.tar.gz", repo_type="dataset", token=tok, local_dir=dest / "dl")
    ref = hf_hub_download(a.repo, f"bin/{a.sha}/{a.variant}.tar.gz.sha256", repo_type="dataset", token=tok, local_dir=dest / "dl")
    want = Path(ref).read_text().split()[0]
    got = sha256_file(tgz)
    if got != want:
        sys.exit(f"build tarball sha256 {got} != published {want}")
    root = dest / f"crispsorter-ocr-bench-{a.variant}"
    if root.exists():
        shutil.rmtree(root)
    with tarfile.open(tgz) as t:
        for m in t.getmembers():
            if Path(m.name).is_absolute() or ".." in Path(m.name).parts:
                sys.exit(f"unsafe tar member {m.name}")
        t.extractall(dest)
    info = json.loads((root / "BUILD-INFO.json").read_text())
    if info.get("git_sha") != a.sha or info.get("variant") != a.variant:
        sys.exit(f"BUILD-INFO mismatch: {info.get('git_sha')} {info.get('variant')}")
    exe = root / "bin" / "crispsorter"
    exe.chmod(0o755)
    host = glibc_tuple(" ".join(platform.libc_ver()))
    need = glibc_tuple(info.get("glibc_required"))
    print(f"glibc host {host} build requires {need}", file=sys.stderr)
    if host and need and host < need:
        print("!" * 100 + f"\nHOST GLIBC {host} < REQUIRED {need}: the binary/libraries will not load here\n" + "!" * 100,
              file=sys.stderr, flush=True)
        sys.exit(3)
    if a.apt:
        pkgs = [p for p in (root / "runtime-apt-packages.txt").read_text().split() if p]
        if pkgs:
            pre = [] if os.geteuid() == 0 else ["sudo"]
            env = dict(os.environ, DEBIAN_FRONTEND="noninteractive")
            subprocess.run(pre + ["apt-get", "update", "-qq"], check=False, env=env)
            r = subprocess.run(pre + ["apt-get", "install", "-y", "-qq", "--no-install-recommends"] + pkgs, env=env)
            if r.returncode != 0:  # one renamed package must not block the rest
                for p in pkgs:
                    subprocess.run(pre + ["apt-get", "install", "-y", "-qq", "--no-install-recommends", p], env=env)
    env = dict(os.environ, LD_LIBRARY_PATH=str(root / "lib"))
    ldd = subprocess.run(["ldd", str(exe)], capture_output=True, text=True, env=env).stdout
    missing = [l.split()[0] for l in ldd.splitlines() if "not found" in l]
    gpu_host = shutil.which("nvidia-smi") is not None
    fatal = [m for m in missing if gpu_host or not m.startswith("libcuda.so")]
    if fatal:
        print("!" * 100 + f"\nUNRESOLVED SHARED LIBRARIES: {fatal}\n{ldd}\n" + "!" * 100, file=sys.stderr, flush=True)
        sys.exit(4)
    print(json.dumps(dict(binary=str(exe), lib_dir=str(root / "lib"), build_info=str(root / "BUILD-INFO.json"),
                          sha256=got, git_sha=info["git_sha"], variant=a.variant, glibc_required=info.get("glibc_required"))))


if __name__ == "__main__":
    main()
