#!/usr/bin/env python3
"""Stage pinned official mobile ONNX Runtime artifacts without compiling them.

The app's existing dense/sparse ONNX indexing uses ort 2.0.0-rc.11 (API 23),
whose automatic binary catalogue has no Android x86_64 distribution. Supply
real supported libraries; never disable linking or substitute a host library.
"""
from __future__ import annotations

import argparse
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path, PurePosixPath
import plistlib
import re
import stat
import struct
import urllib.parse
import urllib.request
import zipfile


ROOT = Path(__file__).resolve().parents[1]
TARGETS = {"android-arm64": ("android", "arm64-v8a"), "android-x86_64": ("android", "x86_64"),
           "ios-sim-arm64": ("ios", "ios-arm64_x86_64-simulator"), "ios-arm64": ("ios", "ios-arm64")}
SYSTEM_LIBRARIES = {"libc.so", "libm.so", "libdl.so", "liblog.so", "libandroid.so"}
MAX_ARCHIVE_BYTES = 256 * 1024 * 1024


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def check_bytes(data: bytes, pin: dict, label: str) -> None:
    if len(data) != pin["bytes"] or sha256(data) != pin["sha256"]:
        raise ValueError(f"{label} differs from its committed length/SHA-256 pin")


class HttpsRedirects(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        if urllib.parse.urlsplit(newurl).scheme != "https":
            raise ValueError("refusing a non-HTTPS runtime download redirect")
        return super().redirect_request(req, fp, code, msg, headers, newurl)


def download(pin: dict) -> bytes:
    if not 0 < pin["bytes"] <= MAX_ARCHIVE_BYTES or not re.fullmatch(r"[0-9a-f]{64}", pin["sha256"]):
        raise ValueError("invalid committed runtime download bounds")
    if urllib.parse.urlsplit(pin["url"]).scheme != "https":
        raise ValueError("runtime downloads require HTTPS")
    request = urllib.request.Request(pin["url"], headers={"User-Agent": "CrispSorter-mobile-runtime-qualification"})
    with urllib.request.build_opener(HttpsRedirects).open(request, timeout=60) as response:
        if response.status != 200:
            raise ValueError("runtime download did not return HTTP 200")
        data = response.read(pin["bytes"] + 1)
    check_bytes(data, pin, pin["url"])
    return data


def members(archive: zipfile.ZipFile) -> dict:
    result = {}
    for info in archive.infolist():
        parts = PurePosixPath(info.filename)
        if parts.is_absolute() or ".." in parts.parts or "\\" in info.filename or "\0" in info.filename:
            raise ValueError("unsafe runtime archive path")
        if info.filename in result or info.file_size > MAX_ARCHIVE_BYTES:
            raise ValueError("duplicate or oversized runtime archive member")
        result[info.filename] = info
    if len(result) > 5000:
        raise ValueError("runtime archive has too many entries")
    return result


def read_member(archive: zipfile.ZipFile, entries: dict, name: str) -> bytes:
    info = entries.get(name)
    if info is None or info.is_dir() or stat.S_ISLNK(info.external_attr >> 16):
        raise ValueError(f"runtime archive is missing a regular file: {name}")
    return archive.read(info)


def android_details(data: bytes, abi: str) -> dict:
    if len(data) < 64 or data[:6] != b"\x7fELF\x02\x01":
        raise ValueError("ONNX Runtime must be little-endian ELF64")
    header = struct.unpack_from("<16sHHIQQQIHHHHHH", data)
    if header[1] != 3 or header[2] != {"arm64-v8a": 183, "x86_64": 62}.get(abi):
        raise ValueError("ONNX Runtime ELF target differs from the selected Android ABI")
    offset, entry_size, count = header[5], header[9], header[10]
    if entry_size != 56 or not 0 < count <= 128 or offset + count * entry_size > len(data):
        raise ValueError("invalid runtime ELF program headers")
    segments = [struct.unpack_from("<IIQQQQQQ", data, offset + index * entry_size) for index in range(count)]
    loads = [segment for segment in segments if segment[0] == 1]
    if not loads or any(segment[7] < 16384 or segment[2] % segment[7] != segment[3] % segment[7] for segment in loads):
        raise ValueError("Android ONNX Runtime does not support 16 KiB load alignment")
    dynamic = [segment for segment in segments if segment[0] == 2]
    if len(dynamic) != 1:
        raise ValueError("runtime ELF has no unique dynamic table")
    start, size = dynamic[0][2], dynamic[0][5]
    if start + size > len(data) or size % 16:
        raise ValueError("truncated runtime ELF dynamic table")
    values = [struct.unpack_from("<qQ", data, at) for at in range(start, start + size, 16)]
    strtabs, strsizes = [value for tag, value in values if tag == 5], [value for tag, value in values if tag == 10]
    if len(strtabs) != 1 or len(strsizes) != 1:
        raise ValueError("runtime ELF has no unique dynamic string table")
    strings = None
    for segment in loads:
        delta = strtabs[0] - segment[3]
        if 0 <= delta and delta + strsizes[0] <= segment[5]:
            begin = segment[2] + delta
            if begin + strsizes[0] > len(data):
                raise ValueError("truncated runtime ELF strings")
            strings = data[begin:begin + strsizes[0]]
            break
    if strings is None:
        raise ValueError("runtime ELF strings are outside loaded segments")

    def string(at):
        end = strings.find(b"\0", at)
        if at >= len(strings) or end < 0:
            raise ValueError("invalid runtime dependency name")
        return strings[at:end].decode("ascii")

    needed = sorted(string(value) for tag, value in values if tag == 1)
    if set(needed) - SYSTEM_LIBRARIES:
        raise ValueError("unbundled ONNX Runtime dependency: " + ", ".join(sorted(set(needed) - SYSTEM_LIBRARIES)))
    if [string(value) for tag, value in values if tag == 14] != ["libonnxruntime.so"]:
        raise ValueError("unexpected ONNX Runtime SONAME")
    return {"abi": abi, "dependencies": needed, "minimum_load_alignment": min(segment[7] for segment in loads)}


def static_framework(data: bytes) -> list[str]:
    if data[:4] != b"\xca\xfe\xba\xbe" or len(data) < 8:
        raise ValueError("expected the pinned universal static iOS framework")
    count = struct.unpack_from(">I", data, 4)[0]
    if not 1 <= count <= 2 or 8 + 20 * count > len(data):
        raise ValueError("invalid iOS framework slice table")
    cpus, ranges = [], []
    for index in range(count):
        cpu, _, start, size, _ = struct.unpack_from(">IIIII", data, 8 + 20 * index)
        if cpu not in (0x1000007, 0x100000C) or start < 8 + 20 * count or size < 8 or start + size > len(data):
            raise ValueError("invalid iOS static framework slice")
        if data[start:start + 8] != b"!<arch>\n":
            raise ValueError("iOS framework slice is not a static archive")
        if cpu in cpus or any(start < other_end and other_start < start + size for other_start, other_end in ranges):
            raise ValueError("duplicate or overlapping iOS framework slices")
        cpus.append(cpu)
        ranges.append((start, start + size))
    if 0x100000C not in cpus:
        raise ValueError("iOS framework has no ARM64 slice")
    return ["arm64" if cpu == 0x100000C else "x86_64" for cpu in cpus]


def extract(archive_path: Path, output: Path, target: str, lock: dict) -> dict:
    platform, identifier = TARGETS[target]
    pin = lock[platform]
    check_bytes(archive_path.read_bytes(), pin, "runtime archive")
    if output.exists() or output.is_symlink():
        raise ValueError("choose a new empty output path for runtime staging")
    files = {}
    with zipfile.ZipFile(archive_path) as archive:
        entries = members(archive)
        header_prefix = "headers/" if platform == "android" else "Headers/"
        header = read_member(archive, entries, header_prefix + "onnxruntime_c_api.h")
        if sha256(header) != lock["header_sha256"] or not re.search(rb"#define\s+ORT_API_VERSION\s+23\b", header):
            raise ValueError("ONNX header does not match the pinned API-23 binding")
        for name in entries:
            if name.startswith(header_prefix) and name.endswith(".h") and name.count("/") == 1:
                files["include/" + PurePosixPath(name).name] = read_member(archive, entries, name)
        if platform == "android":
            binary = read_member(archive, entries, f"jni/{identifier}/libonnxruntime.so")
            check_bytes(binary, pin["libraries"][identifier], "Android ONNX Runtime")
            details = android_details(binary, identifier)
            files["lib/libonnxruntime.so"] = binary
        else:
            manifest = plistlib.loads(read_member(archive, entries, "onnxruntime.xcframework/Info.plist"))
            libraries = [item for item in manifest["AvailableLibraries"] if item["LibraryIdentifier"] == identifier]
            if len(libraries) != 1 or libraries[0]["SupportedPlatform"] != "ios" or "arm64" not in libraries[0]["SupportedArchitectures"]:
                raise ValueError("iOS framework does not contain the intended platform and architecture")
            if libraries[0].get("SupportedPlatformVariant") != ("simulator" if target == "ios-sim-arm64" else None):
                raise ValueError("iOS device/simulator framework mismatch")
            prefix = "onnxruntime.xcframework/" + identifier + "/"
            for name, info in entries.items():
                if name.startswith(prefix) and not info.is_dir():
                    files[name] = read_member(archive, entries, name)
            binary_name = prefix + "onnxruntime.framework/onnxruntime"
            binary = files[binary_name]
            check_bytes(binary, pin["libraries"][identifier], "iOS ONNX Runtime")
            architectures = static_framework(binary)
            info = plistlib.loads(files[prefix + "onnxruntime.framework/Info.plist"])
            if info.get("CFBundleVersion") != pin["version"]:
                raise ValueError("iOS framework version differs from its pinned package")
            manifest["AvailableLibraries"] = libraries
            files["onnxruntime.xcframework/Info.plist"] = plistlib.dumps(manifest, sort_keys=True)
            details = {"framework_identifier": identifier, "architectures": architectures, "static": True,
                       "minimum_os_version": info.get("MinimumOSVersion")}
    if sum(len(data) for data in files.values()) > MAX_ARCHIVE_BYTES:
        raise ValueError("staged runtime exceeds its total size limit")
    output.mkdir(parents=True)
    for name, data in files.items():
        destination = output / name
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes(data)
    return {"schema_version": 1, "kind": "ocr-mobile-ort-runtime", "target": target,
            "version": pin["version"], "api_version": lock["api_version"], "rust_binding": lock["rust_binding"],
            "archive": {key: pin[key] for key in ("url", "filename", "bytes", "sha256")},
            "details": details, "files": {name: {"bytes": len(data), "sha256": sha256(data)} for name, data in files.items()}}


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", choices=TARGETS, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--lock", type=Path, default=ROOT / "scripts/ocr-mobile-ort-lock.json")
    parser.add_argument("--archive", type=Path, help="existing archive; its committed SHA-256 is still mandatory")
    parser.add_argument("--notices-dir", type=Path, help="existing version-prefixed notice files; pins remain mandatory")
    args = parser.parse_args()
    lock = json.loads(args.lock.read_text())
    platform, _ = TARGETS[args.target]
    pin = lock[platform]
    output = args.output.resolve()
    if output.exists():
        parser.error("choose a new output directory")
    output.parent.mkdir(parents=True, exist_ok=True)
    if args.archive:
        archive_path = args.archive
        if archive_path.is_symlink() or archive_path.stat().st_size != pin["bytes"]:
            parser.error("runtime archive path or length differs from the committed input")
    else:
        archive_path = output.parent / (output.name + "-" + pin["filename"])
        with archive_path.open("xb") as handle:
            handle.write(download(pin))
    notices = {}
    for notice in lock["notices"]:
        data = ((args.notices_dir / (pin["version"] + "-" + notice["filename"])).read_bytes() if args.notices_dir
                else download({**notice, "url": f"https://raw.githubusercontent.com/microsoft/onnxruntime/v{pin['version']}/{notice['filename']}"}))
        check_bytes(data, notice, "ONNX Runtime notice")
        notices[notice["filename"]] = data
    receipt = extract(archive_path, output, args.target, lock)
    for name, data in notices.items():
        destination = output / "notices" / name
        destination.parent.mkdir(exist_ok=True)
        destination.write_bytes(data)
        receipt["files"]["notices/" + name] = {"bytes": len(data), "sha256": sha256(data)}
    receipt["lock_sha256"] = sha256(args.lock.read_bytes())
    receipt["recorded_at"] = datetime.now(timezone.utc).isoformat()
    (output / "ort-runtime.json").write_text(json.dumps(receipt, indent=2) + "\n")
    print(json.dumps({"target": args.target, "version": receipt["version"], "archive_sha256": pin["sha256"], "details": receipt["details"]}))


if __name__ == "__main__":
    main()
