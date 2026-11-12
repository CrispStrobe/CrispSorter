"""Static wiring of the benchmark: pins, workflows, Kaggle kernel rendering. No network."""
import json
import re
import runpy
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

BENCH = Path(__file__).resolve().parents[1]
ROOT = BENCH.parents[1]
LOCK = json.loads((ROOT / "scripts/crispembed-runtime-lock.json").read_text())
PINS = json.loads((BENCH / "vendor/PINS.json").read_text())


class Wiring(unittest.TestCase):
    def test_scorer_pin_follows_the_runtime_lock(self):
        self.assertEqual(PINS["crispembed_commit"], LOCK["source_commit"])

    def test_build_workflow_pins_and_verifies_crispembed(self):
        s = (ROOT / ".github/workflows/ocr-bench-build.yml").read_text()
        self.assertRegex(s, rf"(?m)^  CRISPEMBED_REF: v{re.escape(LOCK['version'])}\b")
        self.assertRegex(s, rf"(?m)^  CRISPEMBED_COMMIT: {LOCK['source_commit']}\b")
        self.assertIn("scripts/verify_crispembed_bundle.py", s)
        self.assertNotIn("releases/latest", s)

    def test_bench_workflows_are_manual_only_and_never_print_the_token(self):
        for name in ("ocr-bench-build.yml", "ocr-bench-cpu.yml"):
            s = (ROOT / ".github/workflows" / name).read_text()
            on = s.split("\non:", 1)[1].split("\n\n", 1)[0]
            self.assertIn("workflow_dispatch", on, name)
            for trigger in ("push:", "pull_request", "schedule:"):
                self.assertNotIn(trigger, on, name)
            self.assertNotRegex(s, r"echo[^\n]*\$HF_TOKEN\b|echo[^\n]*secrets\.HF_TOKEN", name)

    def test_no_kaggle_account_or_token_committed(self):
        for p in BENCH.rglob("*"):
            if p.is_file() and p.suffix in (".py", ".json", ".md", ".yml") and p.name != "test_wiring.py":
                t = p.read_text()
                # Account names live only in the environment: dataset mount paths use a glob or the placeholder.
                for owner in re.findall(r"/kaggle/input/datasets/([^/\s\"']+)/", t):
                    self.assertIn(owner, ("*", "{KAGGLE_ACCOUNT}", "<owner>"), str(p))
                self.assertNotRegex(t, r"hf_[A-Za-z0-9]{30,}", str(p))

    def test_kernel_metadata(self):
        m = runpy.run_path(str(BENCH / "kaggle/kpush.py"))["DEFAULT_METADATA"]
        self.assertTrue(m["id"].startswith("${KAGGLE_ACCOUNT}/"))
        self.assertEqual(m["dataset_sources"], ["${KAGGLE_ACCOUNT}/crispasr-hf-token"])
        for k in ("enable_gpu", "enable_internet", "is_private"):
            self.assertEqual(m[k], "true", k)  # strings, not booleans
        self.assertEqual(m["code_file"], "kernel.py")

    def test_kpush_works_without_local_metadata(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            for name in ("kpush.py", "kernel.py"):
                shutil.copy(BENCH / "kaggle" / name, root / name)
            output = root / "rendered"
            result = subprocess.run([sys.executable, str(root / "kpush.py"), "--run-id", "r1",
                                     "--binary-sha", "a" * 40, "--dry-run", str(output)],
                                    capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            metadata = json.loads((output / "kernel-metadata.json").read_text())
            self.assertEqual(metadata["code_file"], "kernel.py")
            self.assertEqual(metadata["enable_gpu"], "true")

    def test_kpush_renders_every_placeholder(self):
        with tempfile.TemporaryDirectory() as d:
            r = subprocess.run([sys.executable, str(BENCH / "kaggle/kpush.py"), "--run-id", "r1", "--binary-sha", "a" * 40,
                                "--slot", "b", "--shard", "1/2", "--dry-run", d], capture_output=True, text=True)
            self.assertEqual(r.returncode, 0, r.stderr)
            code = (Path(d) / "kernel.py").read_text()
            meta = json.loads((Path(d) / "kernel-metadata.json").read_text())
            self.assertNotRegex(code, r"@[A-Z_]+@")
            self.assertNotIn("${KAGGLE_ACCOUNT}", code + json.dumps(meta))
            self.assertIn('RUN_ID = "r1"', code)
            self.assertIn('SHARD = "1/2"', code)
            self.assertTrue(meta["id"].endswith("/crispsorter-ocr-bench-gpu-b"))
            self.assertEqual(meta["title"], meta["id"].split("/", 1)[1])  # Kaggle derives the slug from the title
            compile(code, "kernel.py", "exec")
            # Every bench file the kernel downloads exists at this commit.
            files = re.search(r"BENCH_FILES = \[(.*?)\]", code, re.S).group(1)
            for rel in re.findall(r'"([^"]+)"', files):
                self.assertTrue((BENCH / rel).is_file(), rel)

    def test_ovis_comparison_is_explicit_and_gpu_fails_closed(self):
        engines = {e["id"]: e for e in json.loads((BENCH / "engines.json").read_text())["engines"]}
        for name in ("ovisocr2_cpu", "ovisocr2_cpu_legacy", "ovisocr2_gpu"):
            arm = engines[name]
            self.assertFalse(arm["default"])
            self.assertEqual(arm["engine"], "unified")
            self.assertEqual(arm["args"], ["--model", "ovisocr2-f16", "--cleanup=false"])
            for gate in ("CRISPEMBED_QWEN35_OCR", "CRISPEMBED_OVISOCR2_FRONTEND", "CRISPEMBED_OVISOCR2_INFERENCE"):
                self.assertEqual(arm["env"][gate], "1")
        self.assertEqual(engines["ovisocr2_cpu_legacy"]["env"]["CRISPEMBED_QWEN35_OCR_GRAPH_DECODER"], "0")
        self.assertEqual(engines["ovisocr2_gpu"]["env"]["CRISPEMBED_QWEN35_OCR_GPU_STRICT"], "1")
        self.assertEqual(engines["ovisocr2_gpu"]["devices"], ["gpu"])

    def test_kpush_rejects_short_sha(self):
        r = subprocess.run([sys.executable, str(BENCH / "kaggle/kpush.py"), "--run-id", "r1", "--binary-sha", "abc",
                            "--dry-run", tempfile.gettempdir()], capture_output=True, text=True)
        self.assertNotEqual(r.returncode, 0)


if __name__ == "__main__":
    unittest.main()
