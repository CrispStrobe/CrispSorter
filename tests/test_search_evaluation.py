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
    return {"format": "CrispSorter search report", "version": 1, "scope": "displayed", "sort": "relevance", "displayTags": [], "executedQuery": "needle", "coverage": {"requestedLimit": 20, "returnedDocuments": len(ids), "exportedDocuments": len(ids)}, "recipe": {"mode": "text", "filters": {}, "limit": 20, "docIdScope": []}, "documents": [{"docId": doc_id} for doc_id in ids], "elapsedMs": 40}


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

    def test_incomplete_or_contradictory_report_windows_are_rejected(self):
        case = {"id": "q", "query": "needle", "relevance": {"a": 1}}
        for change in [{"returnedDocuments": 2}, {"exportedDocuments": 0}, {"returnedDocuments": True},
                       {"requestedLimit": 10}, {"requestedLimit": 501}]:
            saved = report(["a"])
            saved["coverage"].update(change)
            with self.subTest(change=change), self.assertRaises(ValueError):
                evaluate.evaluate_report(case, saved, 10)
        for key, value in [("coverage", None), ("recipe", []), ("documents", ["a"]),
                           ("coverage", {"requestedLimit": 20})]:
            with self.subTest(key=key), self.assertRaises(ValueError):
                evaluate.evaluate_report(case, {**report(["a"]), key: value}, 10)

    def test_optional_document_scope_is_checked_without_rejecting_additive_view_state(self):
        case = {"id": "q", "query": "needle", "relevance": {"a": 1}, "docIdScope": ["a"]}
        saved = report(["a"])
        with self.assertRaisesRegex(ValueError, "Document scope"):
            evaluate.evaluate_report(case, saved, 10)
        saved["recipe"].update(version=3, docIdScope=["a"], view={"compact": True})
        self.assertEqual(evaluate.evaluate_report(case, saved, 10)["recallAtK"], 1)

    def test_fraction_thresholds_and_boolean_empty_expectations_are_validated(self):
        with self.assertRaisesRegex(ValueError, "boolean"):
            evaluate.evaluate_report({"id": "q", "query": "needle", "expectEmpty": "false"}, report([]), 10)
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            (base / "run.json").write_text(json.dumps(report(["a"])), encoding="utf-8")
            manifest = {"corpus": "fixture", "environment": "unit fixture", "queries": [{"id": "q", "query": "needle", "relevance": {"a": 1}}], "experiments": [{"id": "baseline", "reports": {"q": "run.json"}}], "thresholds": {"minMeanRecallAtK": 1.01}}
            with self.assertRaisesRegex(ValueError, "between 0 and 1"):
                evaluate.evaluate_manifest(manifest, base)

    def test_owner_aware_reports_do_not_merge_equal_document_ids(self):
        saved = report(["shared", "shared"])
        saved["documents"][0]["ownerId"] = "alice"
        saved["documents"][1]["ownerId"] = "bob"
        case = {"id": "q", "query": "needle", "identity": "owner-and-document",
                "relevance": {'["alice","shared"]': 0, '["bob","shared"]': 3},
                "mustInclude": ['["bob","shared"]']}
        result = evaluate.evaluate_report(case, saved, 2)
        self.assertEqual(result["recallAtK"], 1)
        self.assertEqual(result["reciprocalRank"], 0.5)
        self.assertEqual(result["constraintFailures"], [])
        with self.assertRaisesRegex(ValueError, "Duplicate document"):
            evaluate.evaluate_report({**case, "identity": "document"}, saved, 2)
        saved["documents"][1]["ownerId"] = "alice"
        with self.assertRaisesRegex(ValueError, "Duplicate document"):
            evaluate.evaluate_report(case, saved, 2)
        with self.assertRaisesRegex(ValueError, "documentKey"):
            evaluate.evaluate_report({**case, "relevance": {"shared": 3}}, saved, 2)

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
