import ast
import copy
import hashlib
import json
from pathlib import Path
import re
import subprocess
import struct
import sys
import tempfile
import unittest
from unittest.mock import patch
import zipfile

import replay_windows_retained_package as replay

ROOT = Path(__file__).resolve().parents[1]


class WindowsReplayTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        self.pin = json.loads((ROOT / replay.PIN).read_text())

    def test_reviewed_original_preflight_has_separate_app_and_workflow_identity(self):
        raw = (ROOT / replay.PREFLIGHT).read_bytes()
        proof = json.loads(raw)
        self.assertEqual(hashlib.sha256(raw).hexdigest(), self.pin["preflight_sha256"])
        self.assertEqual(proof["app_source"], replay.controls.SOURCE)
        self.assertEqual(proof["workflow_source"], self.pin["original_workflow_source"])
        self.assertNotEqual(proof["app_source"], proof["workflow_source"])
        for name, digest in proof["original_inputs"].items():
            data = subprocess.check_output(
                ["git", "-C", str(ROOT), "show", replay.controls.SOURCE + ":" + name]
            )
            self.assertEqual(hashlib.sha256(data).hexdigest(), digest)

    def test_replay_code_must_match_actual_workflow_definition_commit(self):
        with patch.object(
            replay.controls,
            "source_identity",
            return_value={"tooling_source": "a" * 40},
        ), patch.dict(replay.os.environ, {"GITHUB_WORKFLOW_SHA": "b" * 40}):
            with self.assertRaisesRegex(ValueError, "workflow-definition"):
                replay.source_identity(self.root, self.root, self.pin, self.root)

    def test_selector_and_every_unrelated_workflow_job_fail_closed_under_contradictory_flags(
        self,
    ):
        text = (
            (ROOT / ".github/workflows/release.yml").read_text().split("jobs:\n", 1)[1]
        )
        jobs = dict(
            re.findall(
                r"^  ([A-Za-z0-9_-]+):\n(.*?)(?=^  [A-Za-z0-9_-]+:|\Z)",
                text,
                re.M | re.S,
            )
        )
        for publish in [False, True]:
            for store in [False, True]:
                for profile in ["standard", "ocr"]:
                    values = {
                        "github.event_name": "workflow_dispatch",
                        "github.ref": "refs/heads/replay",
                        "inputs.platform": replay.SELECTOR,
                        "inputs.publish": publish,
                        "inputs.mac_appstore": store,
                        "inputs.mobile_profile": profile,
                    }
                    selected = []
                    for name, body in jobs.items():
                        condition = re.search(
                            r"^    if: \$\{\{(.*?)\}\}", body, re.M | re.S
                        ).group(1)
                        expression = re.sub(
                            r"\b(?:github|inputs|needs)\.[A-Za-z0-9_.-]+",
                            lambda m: repr(values.get(m.group(), "skipped")),
                            condition,
                        )
                        expression = re.sub(
                            r"!(?!=)",
                            "not ",
                            expression.replace("&&", " and ").replace("||", " or "),
                        )
                        if eval(
                            " ".join(expression.split()),
                            {
                                "__builtins__": {},
                                "always": lambda: True,
                                "startsWith": lambda v, p: v.startswith(p),
                            },
                        ):
                            selected.append(name)
                    self.assertEqual(selected, ["windows-package-validation-replay"])
                    env = {
                        "GITHUB_EVENT_NAME": "workflow_dispatch",
                        "CRISPSORTER_RELEASE_SELECTOR": replay.SELECTOR,
                        "CRISPSORTER_RELEASE_PUBLISH": str(publish).lower(),
                        "CRISPSORTER_RELEASE_MAC_APPSTORE": str(store).lower(),
                        "CRISPSORTER_MOBILE_RELEASE_PROFILE": profile,
                    }
                    if publish or store or profile != "standard":
                        with self.assertRaises(ValueError):
                            replay.guard(env)
                    else:
                        replay.guard(env)

    def test_job_has_exact_checkouts_draft_visibility_and_no_build_sign_or_release_writes(
        self,
    ):
        text = (ROOT / ".github/workflows/release.yml").read_text()
        job = text.split("  windows-package-validation-replay:", 1)[1].split(
            "  macos-package-validation-replay:", 1
        )[0]
        for value in [
            "contents: write",
            "if: always()",
            replay.controls.SOURCE,
            self.pin["original_workflow_source"],
        ]:
            self.assertIn(value, job)
        self.assertEqual(job.count("persist-credentials: false"), 3)
        self.assertIn("ref: ${{ github.workflow_sha }}", job)
        self.assertLess(job.index("core.autocrlf false"), job.index("actions/checkout"))
        self.assertLess(
            job.index("guard(os.environ)"), job.index("path: original-source")
        )
        for value in [
            "cargo ",
            "npm ",
            "release upload",
            "release edit",
            "sign_windows",
            "concurrency:",
        ]:
            self.assertNotIn(value, job)
        tree = ast.parse(
            (ROOT / "scripts/replay_windows_retained_package.py").read_text()
        )
        for node in ast.walk(tree):
            if (
                isinstance(node, ast.Call)
                and node.args
                and isinstance(node.args[0], ast.List)
            ):
                values = [
                    x.value for x in node.args[0].elts if isinstance(x, ast.Constant)
                ]
                if values and values[0] == "gh":
                    self.assertTrue(
                        values[:2] == ["gh", "api"]
                        or values[:3] == ["gh", "release", "download"]
                    )
                    self.assertFalse(
                        set(values) & {"-X", "--method", "-f", "-F", "--input"}
                    )

    def test_dynamic_draft_requires_exact_asset_unique_target_and_digest(self):
        package = self.pin["package"]
        row = {
            "tag_name": replay.controls.TAG,
            "draft": True,
            "target_commitish": replay.controls.SOURCE,
            "assets": [
                {
                    "name": package["filename"],
                    "size": package["bytes"],
                    "digest": "sha256:" + package["sha256"],
                }
            ],
        }
        for case in ["valid", "duplicate", "public", "target", "asset"]:
            value = copy.deepcopy(row)
            if case == "public":
                value["draft"] = False
            elif case == "target":
                value["target_commitish"] = "0" * 40
            elif case == "asset":
                value["assets"][0]["digest"] = "sha256:" + "0" * 64
            pages = [[value, value]] if case == "duplicate" else [[value]]
            with patch.object(replay, "validate_release_target"), patch.object(
                replay.subprocess, "check_output", return_value=json.dumps(pages)
            ):
                if case == "valid":
                    self.assertTrue(replay.draft("owner/repo", self.pin)["unpublished"])
                else:
                    with self.assertRaises(ValueError):
                        replay.draft("owner/repo", self.pin)

    def test_zip_windows_collisions_traversal_ads_and_devices_are_rejected_before_writes(
        self,
    ):
        for names in [
            ["../bad"],
            ["/bad"],
            ["a.exe", "A.EXE"],
            ["file:stream"],
            ["NUL.txt"],
            ["bad."],
            ["dir\\file"],
        ]:
            archive = self.root / "archive.zip"
            with zipfile.ZipFile(archive, "w") as out:
                for name in names:
                    out.writestr(name, b"fixture")
            dest = self.root / "extract"
            dest.mkdir(exist_ok=True)
            with self.assertRaises(ValueError):
                replay.extract_package(archive, dest)
            self.assertEqual(list(dest.iterdir()), [])
        with zipfile.ZipFile(archive, "w") as out:
            header = bytearray(88)
            header[:2] = b"MZ"
            struct.pack_into("<I", header, 60, 64)
            header[64:68] = b"PE\0\0"
            struct.pack_into("<H", header, 68, 0x8664)
            out.writestr("crispsorter.exe", header)
        self.assertEqual(
            replay.extract_package(archive, dest), dest / "crispsorter.exe"
        )

    def test_actual_zip_bytes_are_checked(self):
        path = self.root / "package.zip"
        path.write_bytes(b"fixture")
        pin = {
            "package": {
                "filename": path.name,
                "bytes": 7,
                "sha256": hashlib.sha256(b"fixture").hexdigest(),
            }
        }
        replay.verify_package(path, pin)
        path.write_bytes(b"changed")
        with self.assertRaises(ValueError):
            replay.verify_package(path, pin)

    def test_environment_isolates_cache_profile_and_never_inherits_credentials(self):
        with patch.dict(
            replay.os.environ,
            {
                "GH_TOKEN": "private",
                "HTTPS_PROXY": "private",
                "CRISPEMBED_ACCEPT_LICENSE": "private",
            },
        ):
            env = replay.windows_env(self.root / "home", str(self.root / "windows"))
        for key in ["GH_TOKEN", "HTTPS_PROXY", "CRISPEMBED_ACCEPT_LICENSE"]:
            self.assertNotIn(key, env)
        self.assertEqual(env["USERPROFILE"], str(self.root / "home"))
        self.assertTrue(
            Path(env["CRISPEMBED_CACHE_DIR"]).is_relative_to(self.root / "home")
        )

    def test_real_process_failure_preserves_bounded_sanitized_both_streams(self):
        env = replay.windows_env(self.root / "home", str(self.root / "windows"))
        code = 'import sys;print(\'{"headers":[["Authorization","Bearer SECRET-SENTINEL"]]}\');print("actual missing model rejection",file=sys.stderr);sys.exit(1)'
        rec, raw = replay.command(
            sys.executable, ["-c", code], env, self.root, "failure"
        )
        self.assertEqual(rec["exit_code"], 1)
        self.assertFalse(replay.controls.completed(rec))
        self.assertNotIn(
            "SECRET-SENTINEL", (self.root / "failure.stdout.txt").read_text()
        )
        self.assertIn(
            "actual missing model rejection",
            (self.root / "failure.stderr.txt").read_text(),
        )
        for name in ["stdout", "stderr"]:
            self.assertEqual(
                rec[name + "_sha256"],
                replay.controls.sha(self.root / ("failure." + name + ".txt")),
            )

    def test_timeout_and_excess_capture_fail_closed(self):
        env = replay.windows_env(self.root / "home", str(self.root / "windows"))
        rec, _ = replay.command(
            sys.executable,
            ["-c", "import time;time.sleep(2)"],
            env,
            self.root,
            "timeout",
            0.02,
        )
        self.assertTrue(rec["timed_out"])
        self.assertFalse(replay.controls.completed(rec))
        rec, _ = replay.command(
            sys.executable, ["-c", 'print("x"*300000)'], env, self.root, "large"
        )
        self.assertTrue(rec["output_truncated"])
        self.assertFalse(replay.controls.completed(rec))
        self.assertLess(
            (self.root / "large.stdout.txt").stat().st_size,
            replay.controls.OUTPUT_LIMIT,
        )

    def test_shared_control_invokes_real_windows_consumer_path_and_preserves_original_rejection(
        self,
    ):
        calls = []
        doctor = json.loads(
            (ROOT / "scripts/fixtures/macos-replay/doctor.stdout.json").read_text()
        )

        def run(exe, args, env, out, label, timeout=120):
            self.assertEqual(exe, Path("/package/crispsorter.exe"))
            calls.append((label, args))
            rec = {"label": label, "exit_code": 0}
            raw = {"stdout": "{}", "stderr": ""}
            if label.endswith("-version"):
                raw["stdout"] = json.dumps(
                    {
                        "name": "crispsorter",
                        "version": "0.15.1",
                        "target": "windows",
                        "arch": "x86_64",
                    }
                )
            elif label.endswith("-doctor"):
                raw["stdout"] = json.dumps(doctor)
            elif label.endswith("-worker"):
                rec["exit_code"] = 1
                raw["stderr"] = "OCR worker requires request and response paths"
            elif label.endswith("original-ocr"):
                rec["exit_code"] = 1
                raw["stderr"] = "missing OCR models: dbnet-det, trocr-printed"
            elif label in ["model-plan", "permissive-model-pull"]:
                installed = label == "permissive-model-pull"
                raw["stdout"] = json.dumps(
                    {
                        "engine": "dbnet_trocr",
                        "models": [
                            {
                                "name": n,
                                "license": license_id,
                                "requires_consent": False,
                                "installed": installed,
                            }
                            for n, license_id in replay.controls.MODELS.items()
                        ],
                        "missing": [] if installed else list(replay.controls.MODELS),
                    }
                )
            elif label.endswith("retry-ocr"):
                raw["stdout"] = "ocr_page cobalt warranty 2031"
            return rec, raw

        with patch.object(replay.controls, "verify_models"):
            passed, records, provisioned = replay.controls.probe_executables(
                [Path("/package/crispsorter.exe")],
                Path("/source/ocr-task.pdf"),
                {"HOME": str(self.root)},
                self.root,
                target="windows",
                arch="x86_64",
                runner=run,
            )
        self.assertTrue(passed)
        self.assertTrue(provisioned)
        self.assertLess(
            [label for label, _ in calls].index("package-0-original-ocr"),
            [label for label, _ in calls].index("permissive-model-pull"),
        )
        original = next(r for r in records if r["label"] == "package-0-original-ocr")
        self.assertEqual(original["exit_code"], 1)
        self.assertEqual(
            next(args for label, args in calls if label == "package-0-retry-ocr")[0],
            "ocr",
        )


if __name__ == "__main__":
    unittest.main()
