"""Runner logic that needs no native binary: python3 -m unittest discover -s bench/ocr/tests -v"""
import hashlib
import json
import os
import shutil
import stat
import sys
import tempfile
import time
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))
import runner  # noqa: E402
import summarize  # noqa: E402

FAKE = HERE / "fake_crispsorter.py"
PNG = b"\x89PNG\r\n\x1a\n" + b"fake-png-body"
JPG = b"\xff\xd8\xff\xe0" + b"fake-jpeg-body-longer"


def sha(b):
    return hashlib.sha256(b).hexdigest()


def tiny_v2():
    return dict(schema="crispembed-ocr-real-scans-v2", pages=[
        dict(id="en-prose-a", tags=["en", "prose"], priority=1, ground_truth="Hello brave new world",
             image=dict(url="https://example.invalid/a.png", sha256=sha(PNG), bytes=len(PNG)), max_tokens=64),
        dict(id="de-fraktur-b", tags=["de", "fraktur"], priority=2, ground_truth="Grüße aus der Stadt",
             image=dict(url="https://example.invalid/b.jpg", sha256=sha(JPG), bytes=len(JPG),
                        fallback_urls=["https://example.invalid/b2.jpg"]), max_tokens=64),
        dict(id="receipt-c", tags=["receipt", "photo", "id-en-mixed"], priority=1, ground_truth="TOTAL 12.50 CASH",
             image=dict(url="https://example.invalid/c.png", sha256=sha(PNG + b"c"), bytes=len(PNG) + 1), max_tokens=64),
    ])


def tiny_gold():
    return dict(schema="gold", pages=[
        dict(id="en-prose-a", tags=["en", "prose"], source="x", v2_id="en-prose-a",
             image=dict(url="https://example.invalid/a.png", sha256=sha(PNG), bytes=len(PNG)),
             blocks=[dict(order=2, text="new world"), dict(order=1, text="Hello brave")]),
    ])


def tiny_pins(v2_bytes, gold_bytes):
    pins = runner.load_pins()
    pins = json.loads(json.dumps(pins))
    pins["manifests"]["real-scans-v2"]["sha256"] = sha(v2_bytes)
    pins["manifests"]["gold-v1"]["sha256"] = sha(gold_bytes)
    return pins


class FakeFetch:
    def __init__(self, mapping):
        self.mapping, self.calls = mapping, []

    def __call__(self, url, **kw):
        self.calls.append(url)
        v = self.mapping.get(url)
        if v is None:
            raise OSError(f"404 {url}")
        if isinstance(v, list):
            return v.pop(0)
        return v


def make_fake_binary(dirpath):
    exe = Path(dirpath) / "crispsorter"
    exe.write_text(f'#!/bin/sh\nexec "{sys.executable}" "{FAKE}" "$@"\n')
    exe.chmod(exe.stat().st_mode | stat.S_IXUSR)
    return exe


FAKE_ENGINES = [
    dict(id="okeng", engine="fake_ok", devices=["cpu", "gpu"], group="g1", cpu_shards=1, timeout_s=dict(cpu=30, gpu=30), language="page"),
    dict(id="raweng", engine="fake_raw", devices=["cpu"], group="g1", cpu_shards=1, timeout_s=dict(cpu=30), language=None),
    dict(id="unsup", engine="fake_unsup", devices=["cpu"], group="g2", cpu_shards=2, timeout_s=dict(cpu=30), language=None),
    dict(id="crashy", engine="fake_crash", devices=["cpu"], group="g3", cpu_shards=1, timeout_s=dict(cpu=30), language=None, default=False),
    dict(id="sleepy", engine="fake_sleep", devices=["cpu"], group="g4", cpu_shards=1, timeout_s=dict(cpu=1), language=None, default=False),
]


class TestScoringAndPins(unittest.TestCase):
    def test_pre_pull_does_not_replace_a_custom_model_or_page_language(self):
        self.assertIsNone(runner.models_pull_argv("/tmp/cli", {"engine": "unified", "args": ["--model", "ovisocr2-f16"]}))
        self.assertIsNone(runner.models_pull_argv("/tmp/cli", {"engine": "surya", "args": ["--rec-model=custom"]}))
        self.assertIsNone(runner.models_pull_argv("/tmp/cli", {"engine": "tesseract", "language": "page"}))
        self.assertEqual(runner.models_pull_argv("/tmp/cli", {"engine": "tesseract", "language": "deu"}),
                         ["/tmp/cli", "--accept-license", "models", "pull", "tesseract", "--language", "deu"])

    def test_vendored_scorer_matches_pin_and_scores(self):
        s = runner.Scorer()
        self.assertEqual(s.identity["sha256"], runner.load_pins()["vendored"]["crispembed_score.py"]["sha256"])
        perfect = s.score("Hello  world", "Hello world")
        self.assertEqual(perfect["cer"], 0.0)
        self.assertEqual(perfect["word_recall"], 1.0)
        m = s.score("**Hello** wrld", "Hello world")  # markdown stripped, one deletion
        self.assertAlmostEqual(m["cer"], round(1 / 11, 4))
        self.assertEqual(m["word_recall"], 0.5)
        self.assertEqual(set(m), {"cer", "cer_nospace", "wer", "word_recall", "len_ratio"})
        empty = s.score("", "abc")
        self.assertEqual(empty["cer"], 1.0)

    def test_tampered_vendor_copy_is_refused(self):
        pins = json.loads(json.dumps(runner.load_pins()))
        pins["vendored"]["crispembed_score.py"]["sha256"] = "0" * 64
        with self.assertRaises(RuntimeError):
            runner.load_vendored("crispembed_score.py", pins)

    def test_primary_metric_and_language(self):
        self.assertEqual(runner.primary_metric(["receipt", "photo"]), "word_recall")
        self.assertEqual(runner.primary_metric(["en", "prose"]), "cer_nospace")
        self.assertEqual(runner.page_language(["de", "fraktur"]), "deu")
        self.assertEqual(runner.page_language(["receipt", "id-en-mixed"]), "eng")
        self.assertIsNone(runner.page_language(["table"]))


class TestEnginesAndUnits(unittest.TestCase):
    def test_shipped_engine_table_is_valid(self):
        engines = runner.load_engines()
        ids = {e["id"] for e in engines}
        for need in ["auto", "system_tesseract", "ocrs", "paddle", "dbnet_trocr", "surya", "tesseract", "tesseract_fraktur",
                     "parseq", "ppocrv6", "easyocr", "glm", "got", "qwen2vl", "qwen3vl", "internvl2", "deepseek_ocr2",
                     "pix2struct", "granite_vision", "lightonocr", "unlimited_ocr", "olmocr", "lfm2_vl"]:
            self.assertIn(need, ids)
        gpu = {e["id"] for e in runner.select_engines(engines, "default", "gpu")}
        self.assertNotIn("ocrs", gpu)  # never spend a GPU session on a CPU-only engine
        self.assertIn("olmocr", gpu)

    def test_select_engines(self):
        e = FAKE_ENGINES
        self.assertEqual([x["id"] for x in runner.select_engines(e, "default", "cpu")], ["okeng", "raweng", "unsup"])
        self.assertEqual(len(runner.select_engines(e, "all", "cpu")), 5)
        self.assertEqual([x["id"] for x in runner.select_engines(e, "group:g1", "cpu")], ["okeng", "raweng"])
        self.assertEqual([x["id"] for x in runner.select_engines(e, "raweng,okeng,okeng", "gpu")], ["okeng"])
        with self.assertRaises(ValueError):
            runner.select_engines(e, "nope", "cpu")

    def test_plan_matrix(self):
        jobs = runner.plan_matrix(FAKE_ENGINES, "cpu", "default")
        self.assertEqual([j["name"] for j in jobs], ["g1-1of1", "g2-1of2", "g2-2of2"])
        self.assertEqual(jobs[0]["engines"], "okeng,raweng")
        self.assertEqual([j["shard"] for j in jobs[1:]], ["0/2", "1/2"])
        real = runner.plan_matrix(runner.load_engines(), "cpu", "default")
        self.assertEqual(len({j["name"] for j in real}), len(real))
        covered = {x for j in real for x in j["engines"].split(",")}
        self.assertEqual(covered, {e["id"] for e in runner.select_engines(runner.load_engines(), "default", "cpu")})

    def test_enumerate_and_shard_partition(self):
        pages = [dict(id=f"p{i}", priority=(i % 3) + 1) for i in range(7)]
        units = runner.enumerate_units(FAKE_ENGINES[:3], pages, "cpu")
        self.assertEqual(len(units), 21)
        self.assertEqual([p["id"] for e, p in units[:7]], ["p0", "p3", "p6", "p1", "p4", "p2", "p5"])  # priority order
        shards = [runner.shard_units(units, f"{i}/3") for i in range(3)]
        keys = [[(e["id"], p["id"]) for e, p in s] for s in shards]
        self.assertEqual(sum(len(k) for k in keys), 21)
        self.assertEqual(len(set().union(*map(set, keys))), 21)
        for s in shards:  # every shard gets a share of every engine
            self.assertEqual({e["id"] for e, _ in s}, {"okeng", "raweng", "unsup"})
        for bad in ("3/3", "1/0", "x", "-1/2"):
            with self.assertRaises(ValueError):
                runner.parse_shard(bad)

    def test_unit_paths_and_listing(self):
        k = runner.unit_key("gpu", "got", "receipt-cord-test-4")
        self.assertEqual(runner.unit_path("r1", k), "results/r1/units/gpu/got/receipt-cord-test-4.json")
        listing = {runner.unit_path("r1", k), "results/r1/progress/gpu-shard0of1.json", "results/r2/units/gpu/x/y.json"}
        self.assertEqual(runner.done_keys_from_listing(listing, "r1"), {k})
        with self.assertRaises(ValueError):
            runner.unit_key("cpu", "../etc", "p")

    def test_parse_cli_output(self):
        self.assertEqual(runner.parse_cli_output(b'{"engine":"x","text":"abc","chars":3}')[:2], ("abc", "json"))
        self.assertEqual(runner.parse_cli_output(b'note\n{"engine":"x",\n"text":"abc"}')[:2], ("abc", "json"))
        self.assertEqual(runner.parse_cli_output(b"plain text\n")[:2], ("plain text\n", "raw"))


class TestCorpusAndImages(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp())
        self.v2b = json.dumps(tiny_v2()).encode()
        self.goldb = json.dumps(tiny_gold()).encode()
        self.pins = tiny_pins(self.v2b, self.goldb)

    def tearDown(self):
        shutil.rmtree(self.tmp, ignore_errors=True)

    def fetch_for_manifests(self):
        base = f"https://raw.githubusercontent.com/{self.pins['crispembed_repo']}/{self.pins['crispembed_commit']}/"
        return FakeFetch({base + self.pins["manifests"]["real-scans-v2"]["upstream_path"]: self.v2b,
                          base + self.pins["manifests"]["gold-v1"]["upstream_path"]: self.goldb})

    def test_load_corpus_v2_and_gold_conversion(self):
        f = self.fetch_for_manifests()
        pages, ident = runner.load_corpus("real-scans-v2", self.tmp, self.pins, f)
        self.assertEqual([p["id"] for p in pages], ["en-prose-a", "de-fraktur-b", "receipt-c"])
        self.assertEqual(pages[2]["primary_metric"], "word_recall")
        self.assertEqual(pages[1]["language"], "deu")
        gold, gident = runner.load_corpus("gold-v1", self.tmp, self.pins, f)
        self.assertEqual(gold[0]["ground_truth"], "Hello brave\nnew world")  # blocks in reading order
        self.assertEqual(gold[0]["max_tokens"], 64)  # v2 frozen cap reused
        self.assertEqual(gident["sha256"], sha(self.goldb))
        n = len(f.calls)
        runner.load_corpus("gold-v1", self.tmp, self.pins, f)
        self.assertEqual(len(f.calls), n)  # sha-verified local cache

    def test_manifest_sha_mismatch_is_fatal(self):
        pins = json.loads(json.dumps(self.pins)); pins["manifests"]["real-scans-v2"]["sha256"] = "1" * 64
        with self.assertRaises(RuntimeError):
            runner.load_corpus("real-scans-v2", self.tmp / "x", pins, self.fetch_for_manifests())

    def test_select_pages(self):
        pages, _ = runner.load_corpus("real-scans-v2", self.tmp, self.pins, self.fetch_for_manifests())
        self.assertEqual([p["id"] for p in runner.select_pages(pages, "receipt-c")], ["receipt-c"])
        self.assertEqual([p["id"] for p in runner.select_pages(pages, "all", "fraktur,receipt")], ["de-fraktur-b", "receipt-c"])
        with self.assertRaises(ValueError):
            runner.select_pages(pages, "missing")

    def test_image_provider_verifies_retries_and_caches(self):
        pages = tiny_v2()["pages"]
        store = runner.DirStore(self.tmp / "store")
        up = runner.Uploader(store, interval=999)
        fetch = FakeFetch({"https://example.invalid/a.png": PNG,
                           "https://example.invalid/b.jpg": b"drifted CDN bytes",
                           "https://example.invalid/b2.jpg": JPG})
        prov = runner.ImageProvider(self.tmp / "img", store=store, uploader=up, fetch=fetch, sleep=lambda s: None)
        a = prov.get(pages[0])
        self.assertEqual(a.suffix, ".png"); self.assertEqual(a.read_bytes(), PNG)
        b = prov.get(pages[1])  # first URL drifted, fallback verified
        self.assertEqual(b.suffix, ".jpg")
        self.assertIn(f"corpus/images/{sha(JPG)}.jpg", up.pending)
        with self.assertRaises(runner.ImageUnavailable):
            prov.get(pages[2])
        self.assertFalse(prov.receipts["receipt-c"]["sha256_ok"])
        self.assertTrue(up.flush(force=True))
        # A fresh provider (new machine) takes verified bytes from the store's corpus cache.
        prov2 = runner.ImageProvider(self.tmp / "img2", store=store, fetch=FakeFetch({}))
        self.assertEqual(prov2.get(pages[1]).read_bytes(), JPG)
        self.assertEqual(prov2.receipts["de-fraktur-b"]["source"], "store-corpus-cache")


class FlakyStore(runner.DirStore):
    def __init__(self, root, fail_times):
        super().__init__(root); self.fail_times, self.commits = fail_times, 0

    def put_many(self, items, message=""):
        if self.fail_times > 0:
            self.fail_times -= 1
            raise OSError("429 Too Many Requests")
        self.commits += 1
        super().put_many(items, message)


class TestUploader(unittest.TestCase):
    def test_failures_keep_items_queued_and_heartbeat_rides_along(self):
        tmp = Path(tempfile.mkdtemp())
        try:
            store = FlakyStore(tmp, fail_times=2)
            beats = []
            up = runner.Uploader(store, interval=999, heartbeat=lambda: beats.append(1) or ("results/r/progress/x.json", b"{}"))
            up.add("results/r/units/cpu/e/p.json", b'{"a":1}')
            self.assertFalse(up.flush(force=True))
            self.assertIn("results/r/units/cpu/e/p.json", up.pending)
            self.assertFalse(up.flush())  # still backing off: no attempt
            self.assertFalse(up.flush(force=True))
            self.assertTrue(up.flush(force=True))
            self.assertEqual(up.pending, {})
            self.assertTrue((tmp / "results/r/units/cpu/e/p.json").is_file())
            self.assertTrue((tmp / "results/r/progress/x.json").is_file())
            self.assertEqual(store.commits, 1)
            self.assertGreaterEqual(len(beats), 3)
        finally:
            shutil.rmtree(tmp, ignore_errors=True)


class TestHFStoreMocked(unittest.TestCase):
    """HFStore against a stand-in huggingface_hub module (no network, no token)."""

    def setUp(self):
        import types
        self.saved = sys.modules.get("huggingface_hub")
        hub = types.ModuleType("huggingface_hub")
        test = self

        class RepoFile:
            def __init__(self, path): self.path = path

        class RepoFolder(RepoFile):
            pass

        class EntryNotFoundError(Exception):
            pass

        class CommitOperationAdd:
            def __init__(self, path_in_repo, path_or_fileobj):
                self.path_in_repo, self.path_or_fileobj = path_in_repo, path_or_fileobj

        class HfApi:
            def __init__(self, token=None): test.token = token
            def create_repo(self, repo_id, **kw): test.created = (repo_id, kw)
            def list_repo_tree(self, repo_id, path_in_repo, recursive, repo_type):
                if path_in_repo.startswith("missing"):
                    raise EntryNotFoundError("404 Client Error: Entry Not Found")
                return [RepoFolder("results/r/units/cpu"), RepoFile("results/r/units/cpu/e/p.json")]
            def create_commit(self, repo_id, operations, commit_message, repo_type):
                test.commits.append((repo_id, [(o.path_in_repo, o.path_or_fileobj) for o in operations], commit_message, repo_type))

        hub.HfApi, hub.CommitOperationAdd = HfApi, CommitOperationAdd
        sys.modules["huggingface_hub"] = hub
        self.commits = []

    def tearDown(self):
        if self.saved is None:
            sys.modules.pop("huggingface_hub", None)
        else:
            sys.modules["huggingface_hub"] = self.saved

    def test_list_put_and_missing_prefix(self):
        st = runner.HFStore("cstr/x", token="t0k")
        self.assertEqual(self.created, ("cstr/x", dict(repo_type="dataset", private=True, exist_ok=True)))
        self.assertEqual(st.list("results/r/units/cpu"), {"results/r/units/cpu/e/p.json"})  # folders dropped
        self.assertEqual(st.list("missing/prefix"), set())
        st.put_many([("a.json", b"{}"), ("img/x.png", "/tmp/x.png")], "msg")
        repo, ops, msg, rtype = self.commits[0]
        self.assertEqual((repo, msg, rtype), ("cstr/x", "msg", "dataset"))
        self.assertEqual(ops[0][0], "a.json"); self.assertEqual(ops[0][1].read(), b"{}")
        self.assertEqual(ops[1], ("img/x.png", "/tmp/x.png"))


@unittest.skipIf(os.name != "posix", "process-group handling is POSIX")
class TestMeasuredRuns(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp())
        self.exe = make_fake_binary(self.tmp)

    def tearDown(self):
        shutil.rmtree(self.tmp, ignore_errors=True)

    def run_engine(self, engine, timeout=30):
        img = self.tmp / "x.png"; img.write_bytes(PNG)
        so, se = self.tmp / "o", self.tmp / "e"
        rr = runner.run_measured([str(self.exe), "ocr", str(img), "--engine", engine], dict(os.environ), timeout,
                                 stdout_path=so, stderr_path=se)
        return rr, runner.classify(rr, se.read_text()), so.read_bytes()

    def test_ok(self):
        rr, st, out = self.run_engine("fake_ok")
        self.assertEqual(st, "ok"); self.assertEqual(rr["exit_code"], 0)
        self.assertEqual(runner.parse_cli_output(out)[0], "hello world")
        self.assertGreater(rr["max_rss_single_mb"], 1)

    def test_timeout_kills_process_group(self):
        t = time.monotonic()
        rr, st, _ = self.run_engine("fake_sleep", timeout=1.5)
        self.assertEqual(st, "timeout"); self.assertTrue(rr["timed_out"])
        self.assertLess(time.monotonic() - t, 20)

    def test_failure_classes(self):
        self.assertEqual(self.run_engine("fake_crash")[1], "crash")
        self.assertEqual(self.run_engine("fake_unsup")[1], "unsupported")
        self.assertEqual(self.run_engine("fake_cuda")[1], "cuda_no_kernel_image")
        self.assertEqual(self.run_engine("fake_whatever")[1], "error")

    @unittest.skipUnless(os.path.isdir("/proc/self"), "tree RSS polling needs /proc")
    def test_child_rss_is_counted(self):
        rr, st, _ = self.run_engine("fake_child")
        self.assertEqual(st, "ok")
        self.assertGreater(rr["peak_rss_tree_mb"], 40)


@unittest.skipIf(os.name != "posix", "fake binary is a shell wrapper")
class TestBenchEndToEnd(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp())
        self.exe = make_fake_binary(self.tmp)
        self.calls = self.tmp / "calls.jsonl"
        os.environ["FAKE_CALL_LOG"] = str(self.calls)
        os.environ["FAKE_TEXT_DIR"] = str(self.tmp / "texts")
        (self.tmp / "texts").mkdir()
        v2 = tiny_v2()
        self.pages = [dict(id=p["id"], tags=p["tags"], ground_truth=p["ground_truth"], image=p["image"], priority=p["priority"],
                           max_tokens=64, language=runner.page_language(p["tags"]), primary_metric=runner.primary_metric(p["tags"]),
                           ground_truth_sha256=sha(p["ground_truth"].encode())) for p in v2["pages"]]
        # Pre-seed verified images so no network is needed.
        img = self.tmp / "work" / "images"; img.mkdir(parents=True)
        for p, data in zip(self.pages, (PNG, JPG, PNG + b"c")):
            (img / f"{sha(data)}.{runner.image_ext(data)}").write_bytes(data)
            (self.tmp / "texts" / f"{sha(data)}.txt").write_text(p["ground_truth"])
        self.store = runner.DirStore(self.tmp / "store")

    def tearDown(self):
        os.environ.pop("FAKE_CALL_LOG", None); os.environ.pop("FAKE_TEXT_DIR", None)
        shutil.rmtree(self.tmp, ignore_errors=True)

    def bench(self, engines, extra=()):
        args = runner.build_parser().parse_args(
            ["run", "--binary", str(self.exe), "--device", "cpu", "--run-id", "t1", "--store", f"dir:{self.tmp / 'store'}",
             "--work-dir", str(self.tmp / "work"), "--upload-interval", "999", *extra])
        sel = runner.select_engines(FAKE_ENGINES, engines, "cpu")
        return runner.Bench(args, self.store, pages=self.pages, corpus_identity=dict(name="tiny"), engines=sel)

    def ocr_calls(self):
        if not self.calls.exists():
            return []
        return [json.loads(l) for l in self.calls.read_text().splitlines() if "ocr" in json.loads(l) and "--help" not in json.loads(l)]

    def test_run_resume_and_summarize(self):
        b = self.bench("okeng,unsup,raweng")
        self.assertEqual(b.run(), "complete")
        units = sorted(self.store.list("results/t1/units"))
        self.assertEqual(len(units), 9)
        rec = json.loads((self.tmp / "store" / "results/t1/units/cpu/okeng/en-prose-a.json").read_text())
        for key in ("schema", "run_id", "unit", "device", "engine_id", "page_id", "status", "command", "settings", "input",
                    "metrics", "primary_metric", "primary_value", "timing", "memory", "output", "binary", "scorer",
                    "license_acceptance", "session", "runner_version"):
            self.assertIn(key, rec)
        self.assertEqual(rec["schema"], runner.UNIT_SCHEMA)
        self.assertEqual(rec["status"], "ok")
        self.assertEqual(rec["metrics"]["cer"], 0.0)
        self.assertEqual(rec["settings"]["requested"]["language"], "eng")
        self.assertIn("--accept-license", rec["command"])
        self.assertTrue(rec["command"][0].endswith("crispsorter"))
        self.assertTrue(all("$WORK" in c or str(self.tmp / "work") not in c for c in rec["command"]))
        self.assertEqual(rec["binary"]["sha256"], hashlib.sha256(self.exe.read_bytes()).hexdigest())
        receipt = json.loads((self.tmp / "store" / "results/t1/units/cpu/okeng/receipt-c.json").read_text())
        self.assertEqual(receipt["primary_metric"], "word_recall")
        self.assertEqual(receipt["primary_value"], 1.0)
        unsup = json.loads((self.tmp / "store" / "results/t1/units/cpu/unsup/de-fraktur-b.json").read_text())
        self.assertEqual(unsup["status"], "unsupported"); self.assertIsNone(unsup["metrics"])
        raw = json.loads((self.tmp / "store" / "results/t1/units/cpu/raweng/de-fraktur-b.json").read_text())
        self.assertEqual(raw["settings"]["effective"]["output_format"], "raw")
        self.assertTrue(self.store.list("results/t1/engines/cpu/okeng"))
        self.assertTrue(self.store.list("results/t1/sessions"))
        self.assertTrue(self.store.list("results/t1/progress"))
        n_calls = len(self.ocr_calls())
        # Resume: same run id, nothing new to do -> no OCR invocation at all (not even warm-up).
        self.assertEqual(self.bench("okeng,unsup,raweng").run(), "complete")
        self.assertEqual(len(self.ocr_calls()), n_calls)
        # Units written locally whose upload never landed are re-uploaded, not rerun.
        (self.tmp / "store" / "results/t1/units/cpu/okeng/de-fraktur-b.json").unlink()
        self.bench("okeng").run()
        self.assertTrue((self.tmp / "store" / "results/t1/units/cpu/okeng/de-fraktur-b.json").is_file())
        self.assertEqual(len(self.ocr_calls()), n_calls)
        # Summaries
        data = summarize.load_run(self.store, "t1", self.tmp / "sumcache")
        self.assertEqual(len(data["units"]), 9)
        rows, tag_rows = summarize.aggregate(data["units"], data["engines"], data["skipped"])
        by = {r["engine_id"]: r for r in rows}
        self.assertEqual(by["okeng"]["ok"], 3); self.assertEqual(by["okeng"]["mean_cer"], 0.0)
        self.assertEqual(by["unsup"]["failures"], "unsupported=3")
        self.assertEqual(by["unsup"]["penalised_cer_nospace"], 1.0); self.assertIsNone(by["unsup"]["mean_cer"])
        self.assertEqual(by["okeng"]["receipt_word_recall"], 1.0)
        self.assertTrue(any(t["tag"] == "fraktur" for t in tag_rows))
        md = summarize.to_markdown("t1", rows, tag_rows, data["sessions"])
        self.assertIn("| okeng | 3/3 |", md)
        self.assertIn("leaderboard", "leaderboard")  # csv writer smoke below
        self.assertTrue(summarize.to_csv(rows).startswith("device,engine_id"))

    def test_breaker_and_skips_are_retried(self):
        b = self.bench("sleepy", ["--breaker", "2", "--no-warmup"])
        b.run()
        self.assertEqual(len(self.store.list("results/t1/units/cpu/sleepy")), 2)  # two timeouts recorded
        self.assertEqual(len(self.store.list("results/t1/skipped/cpu/sleepy")), 1)
        self.assertEqual(b.counts.get("timeout"), 2); self.assertEqual(b.counts.get("skipped_breaker"), 1)

    def test_budget_stops_before_starting_units(self):
        b = self.bench("okeng", ["--budget-seconds", "5"])  # < 2 * 30 s timeout + margin
        self.assertEqual(b.run(), "budget")
        self.assertEqual(self.store.list("results/t1/units"), set())
        self.assertEqual(self.ocr_calls(), [])

    def test_shards_cover_everything_once(self):
        for i in range(2):
            self.bench("okeng,unsup", ["--shard", f"{i}/2", "--no-warmup"]).run()
        self.assertEqual(len(self.store.list("results/t1/units")), 6)
        self.assertEqual(len(self.ocr_calls()), 6)
        self.assertEqual(len(self.store.list("results/t1/progress")), 2)


if __name__ == "__main__":
    unittest.main()
