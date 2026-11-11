#!/usr/bin/env python3
"""Evaluate saved search reports against explicit document relevance judgments.

No indexing, inference, or network calls occur here. Input reports must contain
the displayed, unfiltered result window in relevance order. Each experiment
uses the same query set and records the exact report hashes and search recipes.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import math
from pathlib import Path
from statistics import mean


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ValueError(message)


def finite_number(value: object, name: str) -> float:
    require(isinstance(value, (int, float)) and not isinstance(value, bool), f"{name} must be numeric")
    require(math.isfinite(value) and value >= 0, f"{name} must be finite and non-negative")
    return float(value)


def percentile(values: list[float], fraction: float) -> float | None:
    """Nearest-rank percentile; small samples remain visible in the output."""
    return sorted(values)[max(0, math.ceil(len(values) * fraction) - 1)] if values else None


def evaluate_report(case: dict, report: dict, k: int) -> dict:
    require(isinstance(k, int) and not isinstance(k, bool) and 1 <= k <= 500, "k must be between 1 and 500")
    require(isinstance(report, dict), "A search report must be a JSON object")
    require(report.get("format") == "CrispSorter search report" and report.get("version") == 1, "Unsupported search report format")
    require(report.get("scope") == "displayed", "Selected-only reports cannot measure retrieval quality")
    require(report.get("sort") == "relevance", "Reports must preserve relevance order")
    require(report.get("displayTags") == [], "Clear display-tag filters before exporting evaluation reports")
    require(report.get("executedQuery") == case["query"], f"Executed query differs for {case['id']}")
    coverage = report.get("coverage")
    require(isinstance(coverage, dict), "Report coverage metadata is required")
    requested = coverage.get("requestedLimit")
    require(isinstance(requested, int) and not isinstance(requested, bool) and k <= requested <= 500, "Requested result window is smaller than k or invalid")
    recipe = report.get("recipe", {})
    require(isinstance(recipe, dict), "The search recipe must be an object")
    require(recipe.get("mode") in ("text", "hybrid", "vector"), "The search recipe needs a supported retrieval mode")
    intent = recipe.get("queryIntent", "advanced")
    require(intent in ("advanced", "natural"), "The search recipe needs a supported query intent")
    if "queryIntent" in case:
        require(case["queryIntent"] in ("advanced", "natural"), "Expected query intent must be advanced or natural")
        require(intent == case["queryIntent"], f"Query intent differs for {case['id']}")
    require(recipe.get("limit") == requested and not isinstance(recipe.get("limit"), bool), "Recipe and coverage result limits differ")
    if "filters" in case:
        require(recipe.get("filters") == case["filters"], f"Search filters differ for {case['id']}")
    if "docIdScope" in case:
        require(recipe.get("docIdScope") == case["docIdScope"], f"Document scope differs for {case['id']}")
    if "ownerId" in case:
        require(case["ownerId"] is None or isinstance(case["ownerId"], str), "ownerId must be a string or null")
        require(recipe.get("ownerId") == case["ownerId"], f"Owner scope differs for {case['id']}")
    grades = case.get("relevance", {})
    require(isinstance(grades, dict), "Relevance judgments must be an object keyed by document ID")
    require(all(isinstance(key, str) and key and isinstance(value, int) and not isinstance(value, bool) and 0 <= value <= 3 for key, value in grades.items()), "Relevance grades must be integers from 0 to 3")
    documents = report.get("documents")
    require(isinstance(documents, list), "Report documents must be a list")
    require(all(isinstance(doc, dict) for doc in documents), "Every result must be a document object")
    for key in ("returnedDocuments", "exportedDocuments"):
        count = coverage.get(key)
        require(isinstance(count, int) and not isinstance(count, bool) and count == len(documents),
                f"Incomplete result window: {key} differs from the exported documents")
    doc_ids = [doc.get("docId") for doc in documents]
    require(all(isinstance(doc_id, str) and doc_id for doc_id in doc_ids), "Every result needs a document ID")
    identity = case.get("identity", "document")
    require(identity in ("document", "owner-and-document"), "Unknown relevance identity mode")
    if identity == "owner-and-document":
        require(all(isinstance(doc.get("ownerId"), str) for doc in documents), "Owner-aware evaluation requires ownerId on every result")
        ids = [json.dumps([doc["ownerId"], doc["docId"]], ensure_ascii=False, separators=(",", ":")) for doc in documents]
        for key in grades:
            try:
                parts = json.loads(key)
            except (ValueError, TypeError):
                raise ValueError("Owner-aware judgments must use documentKey values from the report") from None
            require(isinstance(parts, list) and len(parts) == 2 and all(isinstance(value, str) for value in parts) and parts[1]
                    and json.dumps(parts, ensure_ascii=False, separators=(",", ":")) == key,
                    "Owner-aware judgments must use documentKey values from the report")
    else:
        ids = doc_ids
    require(len(set(ids)) == len(ids), "Duplicate document IDs invalidate a ranked report")
    latency = finite_number(report.get("elapsedMs"), "elapsedMs")
    top = ids[:k]
    positives = {doc_id for doc_id, grade in grades.items() if grade > 0}
    require(not case.get("expectEmpty") or not positives, "An expected-empty query cannot have positive relevance judgments")
    require(isinstance(case.get("expectEmpty", False), bool), "expectEmpty must be a boolean")
    for name in ("mustInclude", "mustExclude"):
        values = case.get(name, [])
        require(isinstance(values, list) and all(isinstance(value, str) and value for value in values), f"{name} must be a list of document IDs")
    found = positives.intersection(top)
    ideal = sorted(grades.values(), reverse=True)[:k]
    dcg = sum((2 ** grades.get(doc_id, 0) - 1) / math.log2(rank + 2) for rank, doc_id in enumerate(top))
    idcg = sum((2 ** grade - 1) / math.log2(rank + 2) for rank, grade in enumerate(ideal))
    failures = []
    missing = sorted(set(case.get("mustInclude", [])) - set(top))
    forbidden = sorted(set(case.get("mustExclude", [])).intersection(ids))
    if missing:
        failures.append({"missingFromTopK": missing})
    if forbidden:
        failures.append({"excludedDocumentsReturned": forbidden})
    if case.get("expectEmpty") and ids:
        failures.append({"expectedEmpty": True, "returned": len(ids)})
    return {
        "queryId": case["id"], "query": case["query"], "k": k, "identity": identity,
        "returnedDocuments": len(ids), "topKDocuments": len(top),
        "judgedRelevantDocuments": len(positives),
        "precisionAtK": len(found) / k if positives else None,
        "recallAtK": len(found) / len(positives) if positives else None,
        "ndcgAtK": dcg / idcg if idcg else None,
        "reciprocalRank": next((1 / (rank + 1) for rank, doc_id in enumerate(top) if doc_id in positives), 0.0) if positives else None,
        "unjudgedInTopK": sum(doc_id not in grades for doc_id in top),
        "elapsedMs": latency, "constraintFailures": failures,
        "recipe": recipe, "searchedAt": report.get("searchedAt"),
    }


def evaluate_manifest(manifest: dict, base: Path) -> dict:
    require(isinstance(manifest, dict), "The evaluation manifest must be a JSON object")
    k = manifest.get("k", 10)
    require(isinstance(k, int) and not isinstance(k, bool) and 1 <= k <= 500, "k must be between 1 and 500")
    require(isinstance(manifest.get("corpus"), str) and manifest["corpus"].strip(), "Name the judged corpus or revision")
    require(isinstance(manifest.get("environment"), str) and manifest["environment"].strip(), "Describe the run environment")
    cases, experiments = manifest.get("queries"), manifest.get("experiments")
    require(isinstance(cases, list) and cases, "At least one judged query is required")
    require(isinstance(experiments, list) and experiments, "At least one experiment is required")
    require(all(isinstance(run, dict) for run in experiments), "Experiments must be JSON objects")
    require(all(isinstance(case, dict) and isinstance(case.get("id"), str) and case["id"] and isinstance(case.get("query"), str) for case in cases), "Queries need unique IDs and exact executed query strings")
    case_ids = {case["id"] for case in cases}
    require(len(case_ids) == len(cases), "Query IDs must be unique")
    require(len({run.get("id") for run in experiments}) == len(experiments), "Experiment IDs must be unique")
    results = []
    for experiment in experiments:
        require(isinstance(experiment.get("id"), str) and experiment["id"], "Every experiment needs an ID")
        reports = experiment.get("reports", {})
        require(isinstance(reports, dict) and set(reports) == case_ids, f"Experiment {experiment['id']} must provide exactly the complete query set")
        rows = []
        for case in cases:
            path = base / reports[case["id"]]
            raw = path.read_bytes()
            row = evaluate_report(case, json.loads(raw), k)
            row["report"] = reports[case["id"]]
            row["reportSha256"] = hashlib.sha256(raw).hexdigest()
            rows.append(row)
        positive = [row for row in rows if row["judgedRelevantDocuments"] > 0]
        top_count = sum(row["topKDocuments"] for row in rows)
        summary = {
            "queries": len(rows), "queriesWithRelevantJudgments": len(positive),
            "meanPrecisionAtK": mean(row["precisionAtK"] for row in positive) if positive else None,
            "meanRecallAtK": mean(row["recallAtK"] for row in positive) if positive else None,
            "meanNdcgAtK": mean(row["ndcgAtK"] for row in positive) if positive else None,
            "meanReciprocalRank": mean(row["reciprocalRank"] for row in positive) if positive else None,
            "unjudgedFraction": sum(row["unjudgedInTopK"] for row in rows) / top_count if top_count else 0.0,
            "p50ElapsedMs": percentile([row["elapsedMs"] for row in rows], 0.50),
            "p95ElapsedMs": percentile([row["elapsedMs"] for row in rows], 0.95),
        }
        gate_failures = []
        thresholds = manifest.get("thresholds", {})
        require(isinstance(thresholds, dict), "Thresholds must be an object")
        gates = {
            "minMeanRecallAtK": ("meanRecallAtK", lambda actual, wanted: actual >= wanted),
            "minMeanNdcgAtK": ("meanNdcgAtK", lambda actual, wanted: actual >= wanted),
            "maxP95ElapsedMs": ("p95ElapsedMs", lambda actual, wanted: actual <= wanted),
            "maxUnjudgedFraction": ("unjudgedFraction", lambda actual, wanted: actual <= wanted),
        }
        require(set(thresholds) <= set(gates), "Unknown quality threshold")
        for key, value in thresholds.items():
            wanted = finite_number(value, key)
            require(key == "maxP95ElapsedMs" or wanted <= 1, f"{key} must be between 0 and 1")
            field, compare = gates[key]
            actual = summary[field]
            if actual is None or not compare(actual, wanted):
                gate_failures.append({"threshold": key, "expected": wanted, "actual": actual})
        passed = not gate_failures and all(not row["constraintFailures"] for row in rows)
        results.append({"id": experiment["id"], "passed": passed, "summary": summary, "thresholdFailures": gate_failures, "queries": rows})
    return {
        "format": "CrispSorter retrieval evaluation", "version": 1,
        "corpus": manifest["corpus"], "environment": manifest["environment"], "k": k,
        "method": "Macro averages over queries with positive judgments; unjudged documents have gain zero. Negative queries use explicit constraints. Latencies are recorded interactive requests, not a controlled throughput benchmark.",
        "passed": all(run["passed"] for run in results), "experiments": results,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("manifest", type=Path)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    try:
        raw = args.manifest.read_bytes()
        output = evaluate_manifest(json.loads(raw), args.manifest.parent)
        output["manifestSha256"] = hashlib.sha256(raw).hexdigest()
        encoded = json.dumps(output, ensure_ascii=False, indent=2, allow_nan=False) + "\n"
        if args.output:
            args.output.write_text(encoded, encoding="utf-8")
        else:
            print(encoded, end="")
        return 0 if output["passed"] else 1
    except (ValueError, KeyError, TypeError, OSError) as error:
        parser.exit(2, f"Evaluation input error: {error}\n")


if __name__ == "__main__":
    raise SystemExit(main())
