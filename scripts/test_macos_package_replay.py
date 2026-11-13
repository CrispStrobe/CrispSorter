import copy
import hashlib
import io
import json
import re
import subprocess
import sys
import tarfile
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch
import replay_macos_package_validation as r

ROOT = Path(__file__).resolve().parents[1]


class ReplayTests(unittest.TestCase):
    def setUp(self):
        t = tempfile.TemporaryDirectory()
        self.addCleanup(t.cleanup)
        self.root = Path(t.name)

    def test_selector(self):
        env = {
            "GITHUB_EVENT_NAME": "workflow_dispatch",
            "CRISPSORTER_RELEASE_SELECTOR": r.SELECTOR,
            "CRISPSORTER_RELEASE_PUBLISH": "false",
            "CRISPSORTER_RELEASE_MAC_APPSTORE": "false",
            "CRISPSORTER_MOBILE_RELEASE_PROFILE": "standard",
        }
        r.guard(env)
        for key in env:
            with self.subTest(key=key), self.assertRaises(ValueError):
                r.guard({**env, key: "wrong"})

    def test_draft_exact_unique_target_and_assets(self):
        good = {
            "tag_name": r.TAG,
            "draft": True,
            "target_commitish": r.SOURCE,
            "assets": [
                {"name": n, "size": b, "digest": "sha256:" + h}
                for n, (b, h) in r.ASSETS.items()
            ],
        }
        for case in [
            "valid",
            "public",
            "target",
            "duplicate",
            "missing",
            "size",
            "digest",
        ]:
            row = copy.deepcopy(good)
            if case == "public":
                row["draft"] = False
            elif case == "target":
                row["target_commitish"] = "1" * 40
            elif case == "missing":
                row["assets"].pop()
            elif case == "size":
                row["assets"][0]["size"] += 1
            elif case == "digest":
                row["assets"][0]["digest"] = "sha256:" + "0" * 64
            pages = [[row, row]] if case == "duplicate" else [[row]]
            with patch.object(r, "validate_release_target") as guard, patch.object(
                r.subprocess, "check_output", return_value=json.dumps(pages)
            ):
                if case == "valid":
                    self.assertTrue(r.draft("owner/repo")["unpublished"])
                else:
                    with self.assertRaises(ValueError):
                        r.draft("owner/repo")
                guard.assert_called_once_with("owner/repo", r.TAG, r.SOURCE)

    def test_package_actual_bytes_and_symlink(self):
        p = self.root / "p"
        p.write_bytes(b"pin")
        with patch.dict(r.ASSETS, {"p": (3, hashlib.sha256(b"pin").hexdigest())}):
            r.verify_asset(p)
            p.write_bytes(b"bad")
            with self.assertRaises(ValueError):
                r.verify_asset(p)
            p.unlink()
            (self.root / "other").write_bytes(b"pin")
            p.symlink_to("other")
            with self.assertRaises(ValueError):
                r.verify_asset(p)

    def archive(self, rows):
        p = self.root / "test.tar.gz"
        with tarfile.open(p, "w:gz") as out:
            for name, data, link in rows:
                row = tarfile.TarInfo(name)
                if link is not None:
                    row.type = tarfile.SYMTYPE
                    row.linkname = link
                    out.addfile(row)
                else:
                    row.size = len(data)
                    row.mode = 0o755
                    out.addfile(row, io.BytesIO(data))
        return p

    def test_archive_validation_before_writes(self):
        for rows in [
            [("../escape", b"x", None)],
            [("CrispSorter.app/a", b"x", None)] * 2,
            [
                ("CrispSorter.app/a", b"", "/tmp"),
                ("CrispSorter.app/a/file", b"x", None),
            ],
            [("CrispSorter.app/a", b"", "../../escape")],
        ]:
            dest = self.root / "extract"
            dest.mkdir(exist_ok=True)
            with self.assertRaises(ValueError):
                r.extract_tar(self.archive(rows), dest)
            self.assertEqual(list(dest.iterdir()), [])
        r.extract_tar(
            self.archive(
                [
                    ("CrispSorter.app/lib/native", b"elf", None),
                    ("CrispSorter.app/lib/link", b"", "native"),
                ]
            ),
            self.root / "valid",
        )
        self.assertEqual(
            (self.root / "valid/CrispSorter.app/lib/link").read_bytes(), b"elf"
        )

    def test_real_failure_sanitizes_both_streams_and_does_not_inherit_secrets(self):
        env = r.app_env(self.root / "home")
        self.assertNotIn("GH_TOKEN", env)
        self.assertNotIn("HTTPS_PROXY", env)
        code = 'import sys;print("https://alice:secret@example.test/file?token=private#fragment");print("actual model rejection",file=sys.stderr);sys.exit(7)'
        rec, raw = r.command(sys.executable, ["-c", code], env, self.root, "failure")
        self.assertEqual(rec["exit_code"], 7)
        self.assertFalse(r.completed(rec))
        self.assertIn(
            "actual model rejection", (self.root / "failure.stderr.txt").read_text()
        )
        public = (self.root / "failure.stdout.txt").read_text()
        self.assertIn("https://example.test/file", public)
        for secret in ["alice", "secret", "private", "fragment"]:
            self.assertNotIn(secret, public)

    def test_nested_and_escaped_json_credentials_are_decoded_before_redaction(self):
        value = {
            "diagnosis": "model not installed",
            "nested": [
                {
                    "access_token": "secret-one",
                    "authorization": {"bearer": "secret-two"},
                }
            ],
            "encoded": json.dumps({"password": "secret-three"}),
            "url": "https://name:secret-four@example.test/path?token=secret-five",
        }
        encoded = json.dumps(value).replace("access_token", "access_\\u0074oken")
        public, rejected = r.sanitize_output(encoded)
        self.assertFalse(rejected)
        self.assertIn("model not installed", public)
        for secret in [
            "secret-one",
            "secret-two",
            "secret-three",
            "secret-four",
            "secret-five",
        ]:
            self.assertNotIn(secret, public)
        self.assertIn("<redacted-credential>", public)
        self.assertIn("https://example.test/path", public)
        public, rejected = r.sanitize_output(
            json.dumps({r"access_\u0074oken": "double-escaped-secret"})
        )
        self.assertFalse(rejected)
        self.assertNotIn("double-escaped-secret", public)
        for encoded in [
            json.dumps(json.dumps({"password": "root-secret"})),
            json.dumps(
                "https://name:root-secret@example.test/path?token=query-secret"
            ).replace("/", r"\/"),
        ]:
            public, rejected = r.sanitize_output(encoded)
            self.assertFalse(rejected)
            self.assertNotIn("root-secret", public)
            self.assertNotIn("query-secret", public)

    def test_header_name_value_arrays_and_objects_redact_nested_escaped_credentials(
        self,
    ):
        exact = '{"headers":[["Authorization","Bearer SECRET-SENTINEL"]]}'
        values = [
            exact,
            json.dumps(
                {"nested": {"headers": [[r"Author\u0069zation", "SECRET-SENTINEL"]]}}
            ),
            json.dumps(
                {"headers": [{"name": "Authorization", "value": "SECRET-SENTINEL"}]}
            ),
            json.dumps(
                {
                    "headers": [
                        {
                            r"na\u006de": r"Author\u0069zation",
                            "values": ["SECRET-SENTINEL"],
                        }
                    ]
                }
            ),
            json.dumps(json.dumps({"headers": [["Cookie", "SECRET-SENTINEL"]]})),
            json.dumps({"pairs": [["password", {"nested": "SECRET-SENTINEL"}]]}),
        ]
        for value in values:
            with self.subTest(value=value):
                public, rejected = r.sanitize_output(value)
                self.assertFalse(rejected)
                self.assertNotIn("SECRET-SENTINEL", public)
                self.assertIn("<redacted-credential>", public)
        public, rejected = r.sanitize_output(
            '{"headers":[["User-Agent","CrispSorter"]]}'
        )
        self.assertFalse(rejected)
        self.assertIn("CrispSorter", public)

    def test_bearer_credentials_redact_without_authorization_key(self):
        for value in [
            "request failed: Bearer SECRET-SENTINEL",
            'Bearer "SECRET-SENTINEL"',
            "bearer\nSECRET-SENTINEL",
            json.dumps("Bearer SECRET-SENTINEL"),
            json.dumps({"message": "Bearer SECRET-SENTINEL"}),
            json.dumps({"message": r"Bea\u0072er SECRET-SENTINEL"}),
        ]:
            with self.subTest(value=value):
                public, rejected = r.sanitize_output(value)
                self.assertFalse(rejected)
                self.assertNotIn("SECRET-SENTINEL", public)
                self.assertIn("<redacted-bearer>", public)

    def test_real_sanitized_native_stderr_and_tensor_logs_are_not_json_errors(self):
        fixture = ROOT / "scripts/fixtures/macos-replay/native-dbnet-trocr.stderr.txt"
        text = fixture.read_text()
        self.assertIn("math_ocr: [graph]", text)
        self.assertIn("[:5]", text)
        public, rejected = r.sanitize_output(text)
        self.assertFalse(rejected)
        self.assertIn("math_ocr: [graph]", public)
        self.assertIn("[:5]", public)
        env = r.app_env(self.root / "home")
        code = "import sys;sys.stderr.write(open(sys.argv[1]).read());print('ocr_page cobalt warranty 2031')"
        rec, raw = r.command(
            sys.executable, ["-c", code, str(fixture)], env, self.root, "native-log"
        )
        self.assertTrue(r.completed(rec))
        self.assertTrue(r.ocr_pass(rec, raw))
        self.assertEqual((self.root / "native-log.stderr.txt").read_text(), public)
        for value in [
            "math_ocr: [graph] dec embed+pos+ln x[:5]=[0.1 0.2]",
            "[info] layer [0..4] x=[-1.3 0.2 1e-5]",
            'native [graph] {"diagnosis":"missing model","token":"hidden"}',
        ]:
            public, rejected = r.sanitize_output(value)
            self.assertFalse(rejected)
            self.assertNotIn("hidden", public)
        for value in [
            "[password] hidden",
            '[graph] {"password":"hidden',
            '[info] [{"authorization":"hidden"}',
        ]:
            public, rejected = r.sanitize_output(value)
            self.assertTrue(rejected)
            self.assertNotIn("hidden", public)

    def test_malformed_sensitive_structures_are_omitted_and_cannot_pass(self):
        for raw in [
            '{"password": "hidden',
            '{"to\\u006ben": "hidden",',
            'prefix [{"authorization":"hidden"}',
            '"password": "hidden',
            '{"password": "hidden\\q"}',
            "password=\nhidden",
            'password="unterminated\nhidden',
        ]:
            with self.subTest(raw=raw):
                public, rejected = r.sanitize_output(raw)
                self.assertTrue(rejected)
                self.assertNotIn("hidden", public)
        env = r.app_env(self.root / "home")
        rec, _ = r.command(
            sys.executable,
            ["-c", 'print(\'{"token": "hidden\')'],
            env,
            self.root,
            "malformed",
        )
        self.assertEqual(rec["exit_code"], 0)
        self.assertTrue(rec["diagnostics_rejected"])
        self.assertFalse(r.completed(rec))
        self.assertNotIn("hidden", (self.root / "malformed.stdout.txt").read_text())

    def test_descendant_open_pipe_preserves_partial_sanitized_failure_record(self):
        env = r.app_env(self.root / "home")
        code = (
            "import subprocess,sys;print('partial model diagnostic',flush=True);"
            'print(\'{"password":"pipe-secret"}\',file=sys.stderr,flush=True);'
            "subprocess.Popen([sys.executable,'-c','import time;time.sleep(10)'])"
        )
        rec, _ = r.command(sys.executable, ["-c", code], env, self.root, "open-pipe")
        self.assertEqual(rec["exit_code"], 0)
        self.assertTrue(rec["output_pipe_failure"])
        self.assertFalse(r.completed(rec))
        self.assertEqual(json.loads((self.root / "open-pipe.json").read_text()), rec)
        self.assertIn(
            "partial model diagnostic", (self.root / "open-pipe.stdout.txt").read_text()
        )
        self.assertNotIn(
            "pipe-secret", (self.root / "open-pipe.stderr.txt").read_text()
        )
        for stream in ["stdout", "stderr"]:
            path = self.root / ("open-pipe." + stream + ".txt")
            self.assertLessEqual(path.stat().st_size, r.OUTPUT_LIMIT)
            self.assertEqual(
                rec[stream + "_sha256"], hashlib.sha256(path.read_bytes()).hexdigest()
            )

    def test_real_timeout_and_oversized_output_never_pass(self):
        env = r.app_env(self.root / "home")
        rec, _ = r.command(
            sys.executable,
            ["-c", "import time;time.sleep(2)"],
            env,
            self.root,
            "timeout",
            0.02,
        )
        self.assertTrue(rec["timed_out"])
        self.assertFalse(r.completed(rec))
        rec, _ = r.command(
            sys.executable, ["-c", 'print("x"*300000)'], env, self.root, "oversize"
        )
        self.assertTrue(rec["output_truncated"])
        self.assertFalse(r.completed(rec))
        self.assertLessEqual(
            (self.root / "oversize.stdout.txt").stat().st_size, r.OUTPUT_LIMIT
        )

    def plan(self, installed=False):
        return {
            "engine": "dbnet_trocr",
            "models": [
                {
                    "name": n,
                    "license": lic,
                    "requires_consent": False,
                    "installed": installed,
                }
                for n, lic in r.MODELS.items()
            ],
            "missing": [] if installed else list(r.MODELS),
        }

    def test_model_gate(self):
        self.assertTrue(r.model_plan(self.plan()))
        r.model_plan(self.plan(True), installed=True)
        for case in ["qwen", "consent", "license", "duplicate", "missing"]:
            p = self.plan()
            if case == "qwen":
                p["models"][0]["name"] = "qwen2vl-3b"
            elif case == "consent":
                p["models"][0]["requires_consent"] = True
            elif case == "license":
                p["models"][0]["license"] = "restricted"
            elif case == "duplicate":
                p["models"][0] = p["models"][1]
            else:
                p["missing"] = ["qwen2vl-3b"]
            with self.subTest(case=case), self.assertRaises(ValueError):
                r.model_plan(p)

    def test_retry_preserves_both_original_failures_and_requires_real_output(self):
        labels = []

        def fake(exe, args, env, out, label, timeout=120):
            labels.append(label)
            rec = {
                "label": label,
                "exit_code": 0,
                "timed_out": False,
                "output_truncated": False,
            }
            raw = {"stdout": "{}", "stderr": ""}
            if label.endswith("-version"):
                raw["stdout"] = json.dumps(
                    {
                        "name": "crispsorter",
                        "version": "0.15.1",
                        "target": "macos",
                        "arch": "aarch64",
                    }
                )
            elif label.endswith("-doctor"):
                raw["stdout"] = json.dumps(
                    {
                        key: key == "pdf_extract_compiled_in"
                        for key in [
                            "tesseract_installed",
                            "ocrs_models_available",
                            "paddle_ocr_available",
                            "pdf_extract_compiled_in",
                            "fuse_compiled",
                            "fuse_runtime_available",
                            "embedder_model_cached",
                            "lance_dir_exists",
                        ]
                    }
                )
            elif label.endswith("-worker"):
                rec["exit_code"] = 1
                raw["stderr"] = "OCR worker requires request and response paths"
            elif "original-ocr" in label:
                rec["exit_code"] = 1
                raw["stderr"] = "missing OCR models: dbnet-det, trocr-printed"
            elif label == "model-plan":
                raw["stdout"] = json.dumps(self.plan())
            elif label == "permissive-model-pull":
                raw["stdout"] = json.dumps(self.plan(True))
            elif "retry-ocr" in label:
                raw["stdout"] = "ocr_page cobalt warranty 2031"
            return rec, raw

        with patch.object(r, "command", side_effect=fake), patch.object(
            r, "verify_models"
        ):
            passed, records, provisioned = r.probe(
                [Path("/tar.app"), Path("/dmg.app")],
                Path("/fixture.pdf"),
                {"HOME": str(self.root)},
                self.root,
            )
        self.assertTrue(passed)
        self.assertTrue(provisioned)
        self.assertLess(
            labels.index("package-1-original-ocr"),
            labels.index("permissive-model-pull"),
        )
        self.assertEqual(
            sum(x["exit_code"] == 1 and "original-ocr" in x["label"] for x in records),
            2,
        )
        rec = {"exit_code": 0, "timed_out": False, "output_truncated": False}
        self.assertFalse(r.ocr_pass(rec, {"stdout": ""}))
        self.assertFalse(r.ocr_pass(rec, {"stdout": "ocr_page cobalt warranty"}))

    def test_unrelated_original_error_never_triggers_model_download(self):
        labels = []

        def fake(exe, args, env, out, label, timeout=120):
            labels.append(label)
            rec = {
                "label": label,
                "exit_code": 0,
                "timed_out": False,
                "output_truncated": False,
            }
            raw = {"stdout": "{}", "stderr": ""}
            if label.endswith("-version"):
                raw["stdout"] = json.dumps(
                    {
                        "name": "crispsorter",
                        "version": "0.15.1",
                        "target": "macos",
                        "arch": "aarch64",
                    }
                )
            elif label.endswith("-doctor"):
                raw["stdout"] = json.dumps(
                    {
                        key: key == "pdf_extract_compiled_in"
                        for key in [
                            "tesseract_installed",
                            "ocrs_models_available",
                            "paddle_ocr_available",
                            "pdf_extract_compiled_in",
                            "fuse_compiled",
                            "fuse_runtime_available",
                            "embedder_model_cached",
                            "lance_dir_exists",
                        ]
                    }
                )
            elif label.endswith("-worker"):
                rec["exit_code"] = 1
                raw["stderr"] = "OCR worker requires request and response paths"
            elif "original-ocr" in label:
                rec["exit_code"] = 1
                raw["stderr"] = "native loader rejected the executable"
            elif label == "model-plan":
                raw["stdout"] = json.dumps(self.plan())
            return rec, raw

        with patch.object(r, "command", side_effect=fake), patch.object(
            r, "verify_models"
        ):
            passed, records, provisioned = r.probe(
                [Path("/tar.app"), Path("/dmg.app")],
                Path("/fixture.pdf"),
                {"HOME": str(self.root)},
                self.root,
            )
        self.assertFalse(passed)
        self.assertFalse(provisioned)
        self.assertNotIn("permissive-model-pull", labels)

    def test_committed_source_fixture_and_tooling_identity_fail_closed(self):
        source = self.root / "source"
        tooling = self.root / "tooling"
        source.mkdir()
        tooling.mkdir()
        (source / "tests/quality").mkdir(parents=True)
        (source / "tests/quality/ocr-task.pdf").write_bytes(b"fixture")
        (tooling / "scripts").mkdir()
        (tooling / "scripts/replay_macos_package_validation.py").write_bytes(
            b"committed"
        )
        for part in ["Cargo.lock", "scripts/crispembed-runtime-lock.json"]:
            dest = source / part
            dest.parent.mkdir(exist_ok=True)
            dest.write_bytes(b"lock")

        def fake(args, **kwargs):
            if "show" in args:
                return b"committed"
            if "status" in args:
                return ""
            return (
                r.SOURCE if args[-1] == "HEAD" and args[2] == str(source) else "1" * 40
            )

        with patch.object(r.subprocess, "check_output", side_effect=fake), patch.object(
            r, "FIXTURE", hashlib.sha256(b"fixture").hexdigest()
        ):
            self.assertEqual(
                r.source_identity(source, tooling)["original_app_source_association"],
                r.SOURCE,
            )
            (source / "tests/quality/ocr-task.pdf").write_bytes(b"changed")
            with self.assertRaisesRegex(ValueError, "fixture"):
                r.source_identity(source, tooling)

    def test_provisioned_models_require_exact_real_bytes_inside_isolated_home(self):
        home = self.root / "home"
        home.mkdir()
        (home / "model.gguf").write_bytes(b"model")
        with patch.dict(
            r.MODEL_PINS,
            {"model.gguf": (5, hashlib.sha256(b"model").hexdigest())},
            clear=True,
        ):
            self.assertEqual(r.verify_models(home, self.root)["model.gguf"]["bytes"], 5)
            (home / "model.gguf").write_bytes(b"wrong")
            with self.assertRaises(ValueError):
                r.verify_models(home, self.root)

    def test_contradictory_replay_flags_exclude_every_unrelated_job_and_fail_guard(
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
        self.assertIn("release-macos-appstore", jobs)
        for publish in [False, True]:
            for appstore in [False, True]:
                for profile in ["standard", "ocr"]:
                    values = {
                        "github.event_name": "workflow_dispatch",
                        "github.ref": "refs/heads/replay",
                        "inputs.platform": r.SELECTOR,
                        "inputs.publish": publish,
                        "inputs.mac_appstore": appstore,
                        "inputs.mobile_profile": profile,
                    }
                    selected = []
                    for name, body in jobs.items():
                        match = re.search(
                            r"^    if: \$\{\{(.*?)\}\}", body, re.M | re.S
                        )
                        self.assertIsNotNone(match, name)
                        expression = match.group(1)
                        expression = re.sub(
                            r"\b(?:github|inputs|needs)\.[A-Za-z0-9_.-]+",
                            lambda m: repr(values.get(m.group(), "skipped")),
                            expression,
                        )
                        expression = expression.replace("&&", " and ").replace(
                            "||", " or "
                        )
                        expression = re.sub(r"!(?!=)", "not ", expression)
                        expression = " ".join(expression.split())
                        if eval(
                            expression,
                            {
                                "__builtins__": {},
                                "always": lambda: True,
                                "startsWith": lambda value, prefix: value.startswith(
                                    prefix
                                ),
                            },
                        ):
                            selected.append(name)
                    self.assertEqual(selected, ["macos-package-validation-replay"])
                    env = {
                        "GITHUB_EVENT_NAME": "workflow_dispatch",
                        "CRISPSORTER_RELEASE_SELECTOR": r.SELECTOR,
                        "CRISPSORTER_RELEASE_PUBLISH": str(publish).lower(),
                        "CRISPSORTER_RELEASE_MAC_APPSTORE": str(appstore).lower(),
                        "CRISPSORTER_MOBILE_RELEASE_PROFILE": profile,
                    }
                    if publish or appstore or profile != "standard":
                        with self.assertRaises(ValueError):
                            r.guard(env)
                    else:
                        r.guard(env)
        replay_job = jobs["macos-package-validation-replay"]
        self.assertLess(
            replay_job.index("guard(os.environ)"), replay_job.index("ref: " + r.SOURCE)
        )
        self.assertLess(
            replay_job.index("guard(os.environ)"), replay_job.index("GH_TOKEN:")
        )

    def test_workflow_no_build_sign_publish_or_concurrency_changes(self):
        text = (ROOT / ".github/workflows/release.yml").read_text()
        job = text.split("  macos-package-validation-replay:", 1)[1].split(
            "  release-macos:", 1
        )[0]
        for part in [
            "inputs.platform == 'macos_validate_b739'",
            "Reject contradictory replay flags",
            "guard(os.environ)",
            "contents: read",
            "if: always()",
            "persist-credentials: false",
            r.SOURCE,
        ]:
            self.assertIn(part, job)
        for forbidden in [
            "cargo ",
            "npm ",
            "sign_macos",
            "release upload",
            "release edit",
            "concurrency:",
        ]:
            self.assertNotIn(forbidden, job)
        before = subprocess.check_output(
            [
                "git",
                "-C",
                str(ROOT),
                "show",
                "origin/main:.github/workflows/release.yml",
            ],
            text=True,
        )
        self.assertEqual(
            [line for line in text.splitlines() if "concurrency" in line],
            [line for line in before.splitlines() if "concurrency" in line],
        )


if __name__ == "__main__":
    unittest.main()
