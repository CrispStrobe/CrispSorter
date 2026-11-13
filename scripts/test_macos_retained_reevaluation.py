import ast
import copy
import json
from pathlib import Path
import tempfile
import unittest
import zipfile

import replay_macos_package_validation as replay
import reevaluate_macos_retained_validation as review

ROOT = Path(__file__).resolve().parents[1]


class RetainedReviewTests(unittest.TestCase):
    def setUp(self):
        self.doctor = json.loads(
            (ROOT / "scripts/fixtures/macos-replay/doctor.stdout.json").read_text()
        )

    def fixture(self):
        # Synthetic orchestration evidence, never a native execution claim.
        files = {}
        records = []

        def add(label, args, stdout, stderr="", exit_code=0, matched=None):
            streams = {"stdout": stdout.encode(), "stderr": stderr.encode()}
            row = {
                "label": label,
                "arguments": args,
                "exit_code": exit_code,
                **{flag: False for flag in review.FLAGS},
                "output_bytes": {name: len(raw) for name, raw in streams.items()},
                **{
                    name + "_sha256": review.digest(raw)
                    for name, raw in streams.items()
                },
            }
            for name, raw in streams.items():
                files[label + "." + name + ".txt"] = raw
            files[label + ".json"] = json.dumps(row).encode()
            records.append(
                {
                    **row,
                    **(
                        {"fixture_evidence_matched": matched}
                        if matched is not None
                        else {}
                    ),
                }
            )

        for index in range(2):
            prefix = f"package-{index}-"
            add(
                prefix + "version",
                ["--format", "json", "version"],
                json.dumps(
                    {
                        "name": "crispsorter",
                        "version": "0.15.1",
                        "target": "macos",
                        "arch": "aarch64",
                    }
                ),
            )
            add(
                prefix + "doctor",
                ["--format", "json", "doctor"],
                json.dumps(self.doctor),
            )
            add(
                prefix + "worker",
                ["--crispsorter-ocr-worker"],
                "",
                "OCR worker requires request and response paths",
                1,
            )
            args = [
                "ocr",
                "/retained/original-source/tests/quality/ocr-task.pdf",
                "--engine",
                "dbnet_trocr",
                "--cleanup",
                "false",
                "--render",
                "hocr",
                "--min-confidence",
                "0",
                "--min-chars",
                "0",
            ]
            add(
                prefix + "original-ocr",
                args,
                "",
                "missing OCR models: dbnet-det, trocr-printed",
                1,
                False,
            )
            add(
                prefix + "retry-ocr",
                args,
                "ocr_page cobalt warranty 2031",
                matched=True,
            )
        for phase, label in [("plan", "model-plan"), ("pull", "permissive-model-pull")]:
            installed = phase == "pull"
            plan = {
                "engine": "dbnet_trocr",
                "models": [
                    {
                        "name": name,
                        "license": license,
                        "requires_consent": False,
                        "installed": installed,
                    }
                    for name, license in replay.MODELS.items()
                ],
                "missing": [] if installed else list(replay.MODELS),
            }
            add(
                label,
                ["--format", "json", "models", phase, "dbnet_trocr"],
                json.dumps(plan),
            )
        files["verified-permissive-models.json"] = json.dumps(
            {
                name: {"bytes": size, "sha256": sha}
                for name, (size, sha) in replay.MODEL_PINS.items()
            }
        ).encode()
        content = {"Contents/MacOS/crispsorter": {"bytes": 1, "sha256": "a" * 64}}
        content_sha = review.digest(json.dumps(content, sort_keys=True).encode())
        obs = {
            "schema": "crispsorter-macos-existing-package-observation-v1",
            "status": "failed",
            "original_app_source_association": replay.SOURCE,
            "tooling_source": "1" * 40,
            "tooling_tree": "2" * 40,
            "packages": {
                name: {"bytes": size, "sha256": sha}
                for name, (size, sha) in replay.ASSETS.items()
            },
            "fixture_sha256": replay.FIXTURE,
            "draft": {"tag": replay.TAG, "target": replay.SOURCE, "unpublished": True},
            "cargo_lock_sha256": "cc0a1af878bec8ee7cbe6ea29a0a2981cf878fa1dc89a330cbe98092eb9c00b4",
            "runtime_lock_sha256": "b3715018bba76b5c7223736e555be78b6d4df5021cd0ff10a1f5208cc1b397e3",
            "detach_failed": True,
            "satisfies_publication_gate": False,
            "license_consent_granted": False,
            "application_identity": {
                "bundle_id": "com.crispstrobe.crispsorter",
                "version": "0.15.1",
                "files": content,
                "content_sha256": content_sha,
            },
            "commands": records,
            "permissive_models_provisioned": True,
            "original_ocr_results": {
                f"package-{index}-original-ocr": False for index in range(2)
            },
            "retry_ocr_results": {
                f"package-{index}-retry-ocr": True for index in range(2)
            },
            "error_type": "ValueError",
            "error": "Original failure retained",
        }
        order = [
            f"package-{index}-{phase}"
            for index in range(2)
            for phase in ["version", "doctor", "worker", "original-ocr"]
        ]
        order += [
            "model-plan",
            "permissive-model-pull",
            "package-0-retry-ocr",
            "package-1-retry-ocr",
        ]
        obs["commands"] = sorted(
            obs["commands"], key=lambda row: order.index(row["label"])
        )
        files["observation.json"] = json.dumps(obs).encode()
        pin = {
            "schema": "crispsorter-reviewed-macos-replay-evidence-v1",
            "original_app_source": replay.SOURCE,
            "original_tooling_source": "1" * 40,
            "original_tooling_tree": "2" * 40,
            "original_status": "failed",
            "original_detach_failed": True,
            "application_content_sha256": content_sha,
            "evidence_sha256": {
                name: review.digest(data) for name, data in files.items()
            },
        }
        return files, pin

    def test_actual_sanitized_doctor_matches_original_producer_and_strict_types(self):
        source = (ROOT / "src-tauri/src/cli/mod.rs").read_text()
        self.assertIn(
            '"lance_dir": lance_dir.as_ref().map(|p| p.display().to_string())', source
        )
        self.assertTrue(replay.doctor_pass(self.doctor))
        for field, value in [
            ("lance_dir", 42),
            ("pdf_extract_compiled_in", 1),
            ("fuse_compiled", "false"),
        ]:
            bad = {**self.doctor, field: value}
            self.assertFalse(replay.doctor_pass(bad))
        self.assertTrue(replay.doctor_pass({**self.doctor, "lance_dir": None}))
        self.assertFalse(
            replay.doctor_pass(
                {key: value for key, value in self.doctor.items() if key != "lance_dir"}
            )
        )
        self.assertFalse(replay.doctor_pass({**self.doctor, "unexpected": True}))

    def test_corrected_cli_result_preserves_original_failure_and_unresolved_cleanup(
        self,
    ):
        files, pin = self.fixture()
        result = review.evaluate(files, pin)
        self.assertEqual(result["original_observation_status"], "failed")
        self.assertTrue(result["original_detach_failed"])
        self.assertFalse(result["satisfies_publication_gate"])
        self.assertIn("Not established", result["cleanup_status"])
        self.assertTrue(all(result["retry_ocr_results"].values()))
        self.assertFalse(any(result["original_ocr_results"].values()))

    def test_changed_or_coherently_replaced_outputs_cannot_replace_pinned_evidence(
        self,
    ):
        files, pin = self.fixture()
        files["package-0-retry-ocr.stdout.txt"] = (
            b"ocr_page cobalt warranty 2031 replacement"
        )
        with self.assertRaisesRegex(ValueError, "evidence changed"):
            review.evaluate(files, pin)

    def test_invalid_doctor_fails_even_in_self_consistent_test_evidence(self):
        self.doctor["lance_dir"] = False
        files, pin = self.fixture()
        with self.assertRaisesRegex(ValueError, "doctor schema"):
            review.evaluate(files, pin)

    def test_original_failure_cleanup_source_and_package_cannot_be_relabelled(self):
        for key, value in [
            ("status", "passed"),
            ("detach_failed", False),
            ("original_app_source_association", "3" * 40),
            ("packages", {}),
        ]:
            files, pin = self.fixture()
            obs = json.loads(files["observation.json"])
            obs[key] = value
            files["observation.json"] = json.dumps(obs).encode()
            pin["evidence_sha256"]["observation.json"] = review.digest(
                files["observation.json"]
            )
            with self.assertRaises(ValueError):
                review.evaluate(files, pin)

    def test_archive_digest_and_every_retained_member_are_bound(self):
        files, pin = self.fixture()
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "evidence.zip"
            with zipfile.ZipFile(path, "w") as archive:
                for name, data in files.items():
                    archive.writestr(name, data)
            pin["archive"] = {"bytes": path.stat().st_size, "sha256": replay.sha(path)}
            self.assertEqual(review.archive_files(path, pin), files)
            changed = copy.deepcopy(pin)
            changed["archive"]["sha256"] = "0" * 64
            with self.assertRaises(ValueError):
                review.archive_files(path, changed)

    def test_offline_reviewer_cannot_launch_packages_network_downloads_or_release_writes(
        self,
    ):
        source = (ROOT / "scripts/reevaluate_macos_retained_validation.py").read_text()
        tree = ast.parse(source)
        self.assertNotIn("replay.command(", source)
        for node in ast.walk(tree):
            if (
                isinstance(node, ast.Call)
                and isinstance(node.func, ast.Attribute)
                and isinstance(node.func.value, ast.Name)
                and node.func.value.id == "subprocess"
            ):
                self.assertEqual(node.func.attr, "check_output")
                self.assertEqual(node.args[0].elts[0].value, "git")
        self.assertIn(".reevaluation.observation.json", source)
        self.assertTrue(
            any(
                isinstance(node, ast.Call)
                and isinstance(node.func, ast.Attribute)
                and node.func.attr == "open"
                and node.args
                and isinstance(node.args[0], ast.Constant)
                and node.args[0].value == "x"
                for node in ast.walk(tree)
            )
        )


if __name__ == "__main__":
    unittest.main()
