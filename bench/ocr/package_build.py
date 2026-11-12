#!/usr/bin/env python3
"""Package a built `crispsorter` binary with one CrispEmbed runtime variant for the OCR benchmark.

Layout of <out>/<variant>.tar.gz (top-level dir crispsorter-ocr-bench-<variant>/):
  bin/crispsorter              RPATH $ORIGIN/../lib
  lib/libcrispembed.so*, lib/libggml*.so* (+ bundled CUDA runtime for the cuda variant)
  BUILD-INFO.json              git sha, features, CrispEmbed asset identity, glibc floor, ldd report
  runtime-apt-packages.txt     Debian/Ubuntu packages providing the remaining shared libraries
                               (webkit/gtk/... that the Tauri binary links), for `apt-get install`

Writes <out>/<variant>.tar.gz, <variant>.tar.gz.sha256 and <variant>.build-info.json; the
ocr-bench-build workflow uploads them to the HF dataset repo as bin/<git_sha>/<name>.

usage: package_build.py --binary target/release/crispsorter --variant cpu|cuda
           --crispembed-archive crispembed-linux-x86_64.tar.gz --crispembed-asset NAME
           --git-sha SHA --features F [--out dist]
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
import tempfile
from pathlib import Path

BASE_PACKAGES = {"libc6", "libgcc-s1", "libstdc++6", "libgcc1"}


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def sh(*args, check=True):
    return subprocess.run(list(args), capture_output=True, text=True, check=check)


def max_glibc(paths):
    """Highest GLIBC_x.y symbol version any of the ELF files requires (objdump -T)."""
    best = (0, 0)
    for p in paths:
        r = sh("objdump", "-T", str(p), check=False)
        for m in re.finditer(r"GLIBC_(\d+)\.(\d+)", r.stdout):
            best = max(best, (int(m.group(1)), int(m.group(2))))
    return f"{best[0]}.{best[1]}" if best != (0, 0) else None


def ldd_report(binary, libdir):
    env = dict(os.environ, LD_LIBRARY_PATH=str(libdir))
    r = subprocess.run(["ldd", str(binary)], capture_output=True, text=True, env=env)
    resolved, missing = {}, []
    for line in r.stdout.splitlines():
        m = re.match(r"\s*(\S+)\s+=>\s+(\S+)", line)
        if not m:
            continue
        name, path = m.groups()
        if path == "not":
            missing.append(name)
        else:
            resolved[name] = path
    return resolved, missing, r.stdout


def apt_packages(resolved, libdir):
    pkgs = set()
    for name, path in resolved.items():
        if str(Path(path).resolve()).startswith(str(Path(libdir).resolve())):
            continue
        r = sh("dpkg", "-S", str(Path(path).resolve()), check=False)
        if r.returncode != 0:
            r = sh("dpkg", "-S", path, check=False)
        if r.returncode == 0 and r.stdout:
            pkg = r.stdout.split(":", 1)[0].strip()
            if pkg not in BASE_PACKAGES:
                pkgs.add(pkg)
    return sorted(pkgs)


def package(a):
    out = Path(a.out); out.mkdir(parents=True, exist_ok=True)
    top = f"crispsorter-ocr-bench-{a.variant}"
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp) / top
        (root / "bin").mkdir(parents=True); (root / "lib").mkdir()
        exe = root / "bin" / "crispsorter"
        shutil.copy2(a.binary, exe)
        ext = Path(tmp) / "ce"; ext.mkdir()
        with tarfile.open(a.crispembed_archive) as t:
            for m in t.getmembers():
                if Path(m.name).is_absolute() or ".." in Path(m.name).parts:
                    raise SystemExit(f"unsafe member in {a.crispembed_archive}: {m.name}")
            t.extractall(ext)
        n = 0
        for f in sorted(ext.rglob("*")):
            if ".so" in f.name and (f.is_file() or f.is_symlink()):
                dst = root / "lib" / f.name
                if f.is_symlink():
                    os.symlink(os.readlink(f), dst)
                else:
                    shutil.copy2(f, dst)
                n += 1
        if not any(p.name.startswith("libcrispembed.so") for p in (root / "lib").iterdir()):
            raise SystemExit("no libcrispembed.so in the CrispEmbed archive")
        sh("patchelf", "--set-rpath", "$ORIGIN/../lib", str(exe))
        for lic in ("LICENSE", "NOTICE.marker", "NVIDIA-NOTICE.txt", "NVIDIA-EULA.txt", "Apache-2.0.txt"):
            for f in ext.rglob(lic):
                shutil.copy2(f, root / "lib" / f"crispembed-{f.name}")
        resolved, missing, ldd_text = ldd_report(exe, root / "lib")
        elfs = [exe] + [p for p in (root / "lib").iterdir() if p.is_file() and not p.is_symlink() and ".so" in p.name]
        pkgs = apt_packages(resolved, root / "lib")
        # libcuda.so.1 is the driver: provided by the GPU host, never bundled.
        missing_ok = [m for m in missing if not m.startswith("libcuda.so")]
        info = dict(
            schema="crispsorter-ocr-bench-build-v1", variant=a.variant, git_sha=a.git_sha, git_ref=a.git_ref,
            features=a.features, cargo_profile="release", binary_sha256=sha256_file(exe),
            crispembed=dict(asset=a.crispembed_asset, archive_sha256=sha256_file(a.crispembed_archive),
                            ref=a.crispembed_ref, commit=a.crispembed_commit, libs=sorted(p.name for p in (root / "lib").iterdir())),
            libs_copied=n, glibc_required=max_glibc(elfs), build_host=dict(platform=platform.platform(),
            glibc=" ".join(platform.libc_ver()), rustc=sh("rustc", "--version", check=False).stdout.strip()),
            ldd_missing_at_build=missing, runtime_apt_packages=pkgs, ldd=ldd_text,
            github=dict(run_id=os.environ.get("GITHUB_RUN_ID"), workflow=os.environ.get("GITHUB_WORKFLOW"),
                        repository=os.environ.get("GITHUB_REPOSITORY")),
        )
        if missing_ok:
            raise SystemExit(f"unresolved shared libraries at build time: {missing_ok}\n{ldd_text}")
        (root / "BUILD-INFO.json").write_text(json.dumps(info, indent=1) + "\n")
        (root / "runtime-apt-packages.txt").write_text("\n".join(pkgs) + "\n")
        tar_path = out / f"{a.variant}.tar.gz"
        with tarfile.open(tar_path, "w:gz") as t:
            t.add(root, arcname=top)
    digest = sha256_file(tar_path)
    (out / f"{a.variant}.tar.gz.sha256").write_text(f"{digest}  {a.variant}.tar.gz\n")
    (out / f"{a.variant}.build-info.json").write_text(json.dumps(info, indent=1) + "\n")
    print(json.dumps(dict(tarball=str(tar_path), sha256=digest, bytes=tar_path.stat().st_size,
                          glibc_required=info["glibc_required"], runtime_apt_packages=pkgs)))
    return tar_path, info


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--binary", required=True)
    ap.add_argument("--variant", required=True, choices=["cpu", "cuda"])
    ap.add_argument("--crispembed-archive", required=True)
    ap.add_argument("--crispembed-asset", required=True)
    ap.add_argument("--crispembed-ref", default="")
    ap.add_argument("--crispembed-commit", default="")
    ap.add_argument("--git-sha", required=True)
    ap.add_argument("--git-ref", default="")
    ap.add_argument("--features", required=True)
    ap.add_argument("--out", default="dist")
    a = ap.parse_args()
    if not re.fullmatch(r"[0-9a-f]{40}", a.git_sha):
        sys.exit("--git-sha must be a full 40-hex commit")
    package(a)


if __name__ == "__main__":
    main()
