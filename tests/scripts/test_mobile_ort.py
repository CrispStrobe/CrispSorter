"""Archive, platform and linkage guards; structural fixtures are never executed."""
from __future__ import annotations

import hashlib
import importlib.util
import io
import json
import sys
from pathlib import Path
import stat
import struct
import tempfile
import unittest
import zipfile
from unittest.mock import patch


SCRIPT = Path(__file__).resolve().parents[2] / "scripts/prepare_mobile_ort.py"
sys.path.insert(0, str(SCRIPT.parent))
SPEC = importlib.util.spec_from_file_location("prepare_mobile_ort", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


def elf(*, machine=62, alignment=16384, dependency="libc.so"):
    data = bytearray(512)
    struct.pack_into("<16sHHIQQQIHHHHHH", data, 0, b"\x7fELF\x02\x01\x01" + b"\0" * 9,
                     3, machine, 1, 0, 64, 0, 0, 64, 56, 2, 0, 0, 0)
    strings = ("\0libonnxruntime.so\0" + dependency + "\0").encode()
    data[256:256 + len(strings)] = strings
    dynamic = [(5, 256), (10, len(strings)), (14, 1), (1, strings.index(dependency.encode())), (0, 0)]
    for index, entry in enumerate(dynamic):
        struct.pack_into("<qQ", data, 320 + index * 16, *entry)
    struct.pack_into("<IIQQQQQQ", data, 64, 1, 5, 0, 0, 0, len(data), len(data), alignment)
    struct.pack_into("<IIQQQQQQ", data, 120, 2, 4, 320, 320, 320, len(dynamic) * 16, len(dynamic) * 16, 8)
    return bytes(data)


class MobileOrtTests(unittest.TestCase):
    def runtime_fixture(self, root):
        archive = root / "fixture.aar"
        header = b"#define ORT_API_VERSION 23\n"
        binary = elf()
        with zipfile.ZipFile(archive, "w") as bundle:
            bundle.writestr("headers/onnxruntime_c_api.h", header)
            bundle.writestr("jni/x86_64/libonnxruntime.so", binary)
        data = archive.read_bytes()
        lock = {"android": {"url":"https://fixture.invalid/runtime.aar", "filename":"runtime.aar", "version":"fixture",
                "bytes":len(data), "sha256":MODULE.sha256(data),
                "libraries":{"x86_64":{"bytes":len(binary), "sha256":MODULE.sha256(binary)}}},
                "header_sha256":MODULE.sha256(header), "api_version":23, "rust_binding":"fixture",
                "notices":[{"filename":"LICENSE", "bytes":7, "sha256":MODULE.sha256(b"license")}]}
        return archive, lock

    def test_normal_rerun_repairs_outputs_and_partial_cache_without_deleting_unrelated_files(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); archive, lock = self.runtime_fixture(root)
            output = root / "ort"; lock_path = root / "lock.json"; lock_path.write_text(json.dumps(lock))
            argv = ["prepare", "--target", "android-x86_64", "--output", str(output), "--lock", str(lock_path)]
            def download(pin): return archive.read_bytes() if pin.get("filename") == "runtime.aar" else b"license"
            with patch.object(sys, 'argv', argv), patch.object(MODULE, 'download', side_effect=download): MODULE.main()
            marker = output / "keep.txt"; marker.write_text("unrelated")
            (output / "lib/libonnxruntime.so").write_bytes(b"partial generated library")
            with patch.object(sys, 'argv', argv), patch.object(MODULE, 'download', side_effect=AssertionError("cache should suffice")):
                MODULE.main()
            self.assertEqual((output / "lib/libonnxruntime.so").read_bytes(), elf())
            cache = root / "ort-runtime.aar"; cache.write_bytes(b"partial cached download")
            with patch.object(sys, 'argv', argv), patch.object(MODULE, 'download', side_effect=download) as fetch:
                MODULE.main(); self.assertEqual(fetch.call_count, 1)
            self.assertEqual(marker.read_text(), "unrelated")
            self.assertEqual(cache.read_bytes(), archive.read_bytes())
            self.assertEqual(json.loads((output / "ort-runtime.json").read_text())["target"], "android-x86_64")

    def test_rerun_refuses_symlinked_generated_paths_without_changing_referent(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); archive, lock = self.runtime_fixture(root)
            output = root / "ort"; (output / "lib").mkdir(parents=True)
            sentinel = root / "sentinel"; sentinel.write_bytes(b"unrelated")
            (output / "lib/libonnxruntime.so").symlink_to(sentinel)
            with self.assertRaisesRegex(ValueError, "regular file"):
                MODULE.extract(archive, output, "android-x86_64", lock)
            self.assertEqual(sentinel.read_bytes(), b"unrelated")

    def test_android_rejects_wrong_abi_alignment_and_unbundled_cpp_runtime(self):
        details = MODULE.android_details(elf(), "x86_64")
        self.assertEqual(details["dependencies"], ["libc.so"])
        self.assertEqual(details["minimum_load_alignment"], 16384)
        for value in (elf(machine=183), elf(alignment=4096), elf(dependency="libc++_shared.so"), elf()[:100]):
            with self.subTest(size=len(value)), self.assertRaises(ValueError):
                MODULE.android_details(value, "x86_64")

    def test_ios_rejects_device_binary_substitution_and_missing_arm64(self):
        value = struct.pack(">IIIIIII", 0xCAFEBABE, 1, 0x100000C, 0, 28, 8, 0) + b"!<arch>\n"
        self.assertEqual(MODULE.static_framework(value), ["arm64"])
        for bad in (value[:-1], value[:-8] + b"dyldstub", value.replace(b"\x01\0\0\x0c", b"\x01\0\0\x07"), b"!<arch>\n"):
            with self.assertRaises(ValueError):
                MODULE.static_framework(bad)

    def test_archive_paths_duplicate_members_and_symlinks_are_rejected(self):
        for unsafe in ("../escape.so", "/absolute.so", "jni\\escape.so"):
            with self.subTest(unsafe=unsafe), io.BytesIO() as data:
                with zipfile.ZipFile(data, "w") as archive:
                    archive.writestr(unsafe, b"data")
                with zipfile.ZipFile(data) as archive, self.assertRaisesRegex(ValueError, "unsafe"):
                    MODULE.members(archive)
        with io.BytesIO() as data:
            with zipfile.ZipFile(data, "w") as archive:
                entry = zipfile.ZipInfo("libonnxruntime.so")
                entry.external_attr = (stat.S_IFLNK | 0o777) << 16
                archive.writestr(entry, b"/outside")
            with zipfile.ZipFile(data) as archive, self.assertRaisesRegex(ValueError, "regular file"):
                MODULE.read_member(archive, MODULE.members(archive), "libonnxruntime.so")

    def test_modified_archive_is_rejected_before_any_output_is_created(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "runtime.aar"
            source.write_bytes(b"modified")
            lock = {"android": {"bytes": 8, "sha256": hashlib.sha256(b"original").hexdigest()}}
            with self.assertRaisesRegex(ValueError, "SHA-256"):
                MODULE.extract(source, root / "output", "android-x86_64", lock)
            self.assertFalse((root / "output").exists())

    def test_bad_header_cannot_stage_a_version_label_as_compatibility_proof(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "runtime.aar"
            header = b"#define ORT_API_VERSION 22\n"
            with zipfile.ZipFile(source, "w") as archive:
                archive.writestr("headers/onnxruntime_c_api.h", header)
            data = source.read_bytes()
            lock = {"android": {"bytes": len(data), "sha256": MODULE.sha256(data)}, "header_sha256": MODULE.sha256(header)}
            with self.assertRaisesRegex(ValueError, "API-23"):
                MODULE.extract(source, root / "output", "android-x86_64", lock)
            self.assertFalse((root / "output").exists())

    def test_insecure_download_or_redirect_is_rejected_before_fetch(self):
        with self.assertRaisesRegex(ValueError, "HTTPS"):
            MODULE.download({"url": "http://invalid/runtime.aar", "bytes": 1, "sha256": "0" * 64})
        with self.assertRaisesRegex(ValueError, "non-HTTPS"):
            MODULE.HttpsRedirects().redirect_request(None, None, 302, "", {}, "http://invalid/runtime.aar")


if __name__ == "__main__":
    unittest.main()
