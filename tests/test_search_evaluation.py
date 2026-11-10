"""Hand-calculated retrieval and evaluation-input regressions."""
import importlib.util
import json
import math
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("evaluate", Path(__file__).resolve().parents[1] / "scripts/evaluate_search_reports.py")
evaluate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(evaluate)


def report(ids):
    return {"format": "CrispSorter search report", "version": 1, "scope": "displayed", "sort": "relevance", "displayTags": [], "executedQuery": "needle", "coverage": {"requestedLimit": 20}, "recipe": {"mode": "text", "filters": {}}, "documents": [{"docId": doc_id} for doc_id in ids], "elapsedMs": 40}


class SearchEvaluationTests(unittest.TestCase):
    def test_graded_metrics_have_known_values(self):
        case = {"id": "q", "query": "needle", "relevance": {"a": 3, "b": 1, "c": 0}}
        metrics = evaluate.evaluate_report(case, report(["c", "a"]), 2)
        self.assertEqual(metrics["precisionAtK"], 0.5)
        self.assertEqual(metrics["recallAtK"], 0.5)
        self.assertEqual(metrics["reciprocalRank"], 0.5)
        self.assertAlmostEqual(metrics["ndcgAtK"], (7 / math.log2(3)) / (7 + 1 / math.log2(3)))

    def test_negative_queries_do_not_inflate_macro_recall(self):
        metrics = evaluate.evaluate_report({"id": "q", "query": "needle", "relevance": {}, "expectEmpty": True}, report([]), 10)
        self.assertIsNone(metrics["recallAtK"])
        self.assertIsNone(metrics["ndcgAtK"])
        self.assertEqual(metrics["constraintFailures"], [])

    def test_excluded_documents_are_checked_beyond_metric_cutoff(self):
        metrics = evaluate.evaluate_report({"id": "q", "query": "needle", "relevance": {"a": 1}, "mustExclude": ["forbidden"]}, report(["a", "forbidden"]), 1)
        self.assertEqual(metrics["recallAtK"], 1)
        self.assertTrue(metrics["constraintFailures"])

    def test_selection_display_filters_and_reordering_are_rejected(self):
        case = {"id": "q", "query": "needle", "relevance": {"a": 1}}
        for key, value in [("scope", "selected"), ("sort", "year-desc"), ("displayTags", ["reviewed"]), ("executedQuery", "different"), ("elapsedMs", float("nan"))]:
            with self.subTest(key=key), self.assertRaises(ValueError):
                evaluate.evaluate_report(case, {**report(["a"]), key: value}, 10)

    def test_duplicate_ids_and_insufficient_windows_are_rejected(self):
        case = {"id": "q", "query": "needle", "relevance": {"a": 1}}
        for saved, k in [(report(["a", "a"]), 10), (report(["a"]), 50)]:
            with self.assertRaises(ValueError):
                evaluate.evaluate_report(case, saved, k)

    def test_unjudged_results_are_counted_and_zero_gain(self):
        metrics = evaluate.evaluate_report({"id": "q", "query": "needle", "relevance": {"a": 1}}, report(["unknown", "a"]), 2)
        self.assertEqual(metrics["unjudgedInTopK"], 1)
        self.assertEqual(metrics["precisionAtK"], 0.5)

    def test_complete_query_set_and_thresholds_are_required(self):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            (base / "run.json").write_text(json.dumps(report(["unknown"])), encoding="utf-8")
            manifest = {"corpus": "fixture-v1", "environment": "unit fixture", "queries": [{"id": "q", "query": "needle", "relevance": {"a": 1}}], "experiments": [{"id": "baseline", "reports": {"q": "run.json"}}], "thresholds": {"minMeanRecallAtK": 1}}
            result = evaluate.evaluate_manifest(manifest, base)
            self.assertFalse(result["passed"])
            self.assertEqual(len(result["experiments"][0]["queries"][0]["reportSha256"]), 64)
            manifest["experiments"][0]["reports"] = {}
            with self.assertRaises(ValueError):
                evaluate.evaluate_manifest(manifest, base)


if __name__ == "__main__":
    unittest.main()
