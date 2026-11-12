#!/usr/bin/env python3
"""Print the crisp-cloud-rs revision CrispSorter depends on.

Single source of truth for CI: `src-tauri/Cargo.toml` pins `crisp-filen` and
`crisp-internxt` to a git `rev` of https://github.com/CrispStrobe/crisp-cloud-rs.
This script fails unless both manifest entries name the same full commit and
`Cargo.lock` resolved both packages to exactly that commit, then prints it.
With `--github-output` it also writes `rev=<sha>` to `$GITHUB_OUTPUT`, so the
conformance workflows check out the very commit the app builds against.
"""

import argparse
import os
import re
import sys
from pathlib import Path

REPO = "https://github.com/CrispStrobe/crisp-cloud-rs"
PACKAGES = ("crisp-filen", "crisp-internxt")


def manifest_revs(text):
    revs = {}
    for package in PACKAGES:
        match = re.search(rf'^{package}\s*=\s*\{{(?P<body>[^}}]*)\}}', text, re.M)
        if not match:
            raise SystemExit(f"src-tauri/Cargo.toml: no `{package} = {{ ... }}` dependency")
        body = match.group("body")
        git = re.search(r'git\s*=\s*"([^"]+)"', body)
        rev = re.search(r'rev\s*=\s*"([0-9a-f]{40})"', body)
        if not git or git.group(1).rstrip("/").removesuffix(".git") != REPO:
            raise SystemExit(f"src-tauri/Cargo.toml: {package} must be a git dependency on {REPO}")
        if not rev:
            raise SystemExit(f"src-tauri/Cargo.toml: {package} must pin a full 40-hex `rev`")
        revs[package] = rev.group(1)
    return revs


def lock_revs(text):
    revs = {}
    for block in text.split("[[package]]"):
        name = re.search(r'^name = "([^"]+)"', block, re.M)
        if not name or name.group(1) not in PACKAGES:
            continue
        source = re.search(r'^source = "([^"]+)"', block, re.M)
        if not source:
            raise SystemExit(f"Cargo.lock: {name.group(1)} has no source (still a path dependency?)")
        match = re.fullmatch(rf'git\+{re.escape(REPO)}\?rev=([0-9a-f]+)#([0-9a-f]{{40}})', source.group(1))
        if not match:
            raise SystemExit(f"Cargo.lock: unexpected source for {name.group(1)}: {source.group(1)}")
        if match.group(1) != match.group(2):
            raise SystemExit(f"Cargo.lock: requested and resolved revisions differ for {name.group(1)}")
        revs[name.group(1)] = match.group(2)
    missing = [package for package in PACKAGES if package not in revs]
    if missing:
        raise SystemExit(f"Cargo.lock: no entry for {', '.join(missing)}")
    return revs


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", default=Path(__file__).resolve().parent.parent, type=Path)
    parser.add_argument("--github-output", action="store_true")
    args = parser.parse_args()
    manifest = manifest_revs((args.root / "src-tauri/Cargo.toml").read_text(encoding="utf-8"))
    lock = lock_revs((args.root / "Cargo.lock").read_text(encoding="utf-8"))
    revs = set(manifest.values()) | set(lock.values())
    if len(revs) != 1:
        raise SystemExit(f"crisp-cloud-rs revisions disagree: manifest={manifest} lock={lock}")
    rev = revs.pop()
    print(rev)
    if args.github_output:
        with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as output:
            output.write(f"rev={rev}\n")


if __name__ == "__main__":
    sys.exit(main())
