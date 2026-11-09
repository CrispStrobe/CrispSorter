"""Receipt rejection controls only; these do not execute a desktop application."""
import importlib.util
import json
from pathlib import Path
import platform
import struct
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[2] / "scripts/desktop_ocr_startup_smoke.py"
SPEC = importlib.util.spec_from_file_location("desktop_ocr_startup_smoke", SCRIPT)
assert SPEC and SPEC.loader
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


def result(stdout="", stderr="", returncode=0):
    return {"argv": ["qualification-fixture"], "stdout": stdout, "stderr": stderr, "returncode": returncode}


def valid_cli():
    return {"help": result("Usage: crispsorter [OPTIONS]\n  ocr  Recognise a document\n"),
            "version_flag": result("crispsorter 0.15.0\n"),
            "version_json": result(json.dumps({"name": "crispsorter", "version": "0.15.0",
                                               "target": "linux", "arch": platform.machine()})),
            "invalid_argument": result(stderr="error: unexpected argument '--definitely-not-supported'", returncode=2)}


class DesktopStartupControls(unittest.TestCase):
    def test_linker_requires_exact_paired_runtime(self):
        native = Path("/qualified/libcrispembed.so")
        MODULE.validate_linker_output("  libcrispembed.so => /qualified/libcrispembed.so (0x123)\n", native)
        for output in ("", "libcrispembed.so => /old/libcrispembed.so (0x123)",
                       "libcrispembed.so => not found", "undefined symbol: ocr_run",
                       "libcrispembed.so => /qualified/libcrispembed.so (0x123)\nother.so => not found"):
            with self.assertRaises(ValueError):
                MODULE.validate_linker_output(output, native)

    def test_mapping_rejects_absent_deleted_and_extra_native_libraries(self):
        native = Path("/qualified/libcrispembed.so")
        line = "7f00-7fff r-xp 0000 08:01 1 /qualified/libcrispembed.so\n"
        self.assertEqual(MODULE.validate_mapping(line + line, native), [str(native)])
        for maps in ("", line.replace("/qualified/", "/old/"), line.rstrip() + " (deleted)",
                     line + line.replace("/qualified/", "/old/")):
            with self.assertRaises(ValueError):
                MODULE.validate_mapping(maps, native)

    def test_cli_checks_version_and_rejects_invalid_argument(self):
        self.assertEqual(MODULE.validate_cli(valid_cli(), "0.15.0")["name"], "crispsorter")
        for key, replacement in (("help", result("Usage: crispsorter")),
                                 ("version_flag", result("crispsorter 0.14.0")),
                                 ("version_json", result('{"version":"0.15.0"}')),
                                 ("invalid_argument", result()),
                                 ("invalid_argument", result(stderr="unexpected argument", returncode=1))):
            values = valid_cli()
            values[key] = replacement
            with self.assertRaises(ValueError):
                MODULE.validate_cli(values, "0.15.0")

    def test_cli_fatal_stderr_cannot_pass_on_zero_exit(self):
        values = valid_cli()
        values["help"]["stderr"] = "symbol lookup error: undefined symbol: ocr_run"
        with self.assertRaisesRegex(ValueError, "Fatal"):
            MODULE.validate_cli(values, "0.15.0")

    def test_frontend_requires_successful_get_for_html_and_both_entries(self):
        paths = ["/", "/_app/immutable/entry/start.ABC.js", "/_app/immutable/entry/app.XYZ.js"]
        requests = [{"method": "GET", "path": path, "status": 200} for path in paths]
        self.assertEqual(len(MODULE.frontend_evidence(requests)), 2)
        for candidate in (requests[:1], requests[1:], requests[:-1],
                          [{**item, "status": 404} for item in requests],
                          [{**item, "method": "HEAD"} for item in requests]):
            with self.assertRaises(ValueError):
                MODULE.frontend_evidence(candidate)

    def test_frontend_start_entry_duplicates_do_not_substitute_for_app_entry(self):
        requests = [{"method": "GET", "path": path, "status": 200} for path in
                    ("/", "/_app/immutable/entry/start.A.js", "/_app/immutable/entry/start.B.js")]
        with self.assertRaises(ValueError):
            MODULE.frontend_evidence(requests)

    def test_frontend_manifest_binds_real_files_and_rejects_symlinks(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with self.assertRaisesRegex(ValueError, "index.html"):
                MODULE.frontend_manifest(root)
            (root / "index.html").write_text("<html>qualification fixture only</html>")
            files = MODULE.frontend_manifest(root)
            self.assertEqual(files[0]["sha256"], MODULE.sha256(root / "index.html"))
            (root / "copy.html").symlink_to(root / "index.html")
            with self.assertRaisesRegex(ValueError, "symbolic"):
                MODULE.frontend_manifest(root)

    def test_png_capture_rejects_missing_or_implausible_header(self):
        data = b"\x89PNG\r\n\x1a\n" + struct.pack(">I", 13) + b"IHDR" + struct.pack(">II", 800, 600) + b"\0" * 9
        self.assertEqual(MODULE.png_dimensions(data), {"width": 800, "height": 600})
        for invalid in (b"", b"not a screenshot", data[:16] + struct.pack(">II", 1, 600) + data[24:]):
            with self.assertRaises(ValueError):
                MODULE.png_dimensions(invalid)


if __name__ == "__main__":
    unittest.main()
