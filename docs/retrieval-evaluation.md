# Reproducible retrieval evaluation

Use a judged corpus and exported search reports to compare retrieval settings.
The evaluator records exact query recipes, input hashes, relevance metrics,
constraint failures, and observed request latency. It does not substitute a
synthetic fixture for a measured model result.

## Collect a run

1. Freeze a corpus revision and assign document relevance grades for each
   query: 0 is irrelevant, 1 is marginal, 2 is relevant, and 3 is highly relevant.
   Include hard negatives, no-match queries, and documents outside the first
   page of the corpus. Include the languages and source formats you actually use.
2. Run each query in CrispSorter with the chosen retrieval settings. Keep the
   relevance sort, clear display-tag filters, and request at least the evaluation
   cutoff. Export the displayed results as JSON. Selected-only reports cannot
   measure retrieval quality because they omit returned results.
3. Repeat the complete query set for each experiment. Record application/model
   versions, hardware, indexing settings, and cache conditions in the environment
   description. Preserve the exact executed query and its recorded
   `queryIntent`; Natural language keeps the original text while the shared
   planner derives its lexical expression.
4. Create a manifest beside the reports and run the evaluator:

```sh
python3 scripts/evaluate_search_reports.py evaluation.json --output evaluation-results.json
```

Example manifest, with illustrative IDs and thresholds:

```json
{
  "corpus": "reviewed-corpus-revision-1",
  "environment": "Record app, model, index, hardware and cache conditions here",
  "k": 10,
  "thresholds": {
    "minMeanRecallAtK": 0.8,
    "minMeanNdcgAtK": 0.8,
    "maxUnjudgedFraction": 0.1
  },
  "queries": [
    {
      "id": "warranty-term",
      "query": "warranty AND NOT draft",
      "relevance": {"final-document-id": 3, "draft-document-id": 0},
      "mustInclude": ["final-document-id"],
      "mustExclude": ["draft-document-id"]
    },
    {
      "id": "absent-term",
      "query": "unique-absent-term",
      "relevance": {},
      "expectEmpty": true
    }
  ],
  "experiments": [
    {
      "id": "text",
      "reports": {"warranty-term": "text-warranty.json", "absent-term": "text-absent.json"}
    },
    {
      "id": "hybrid",
      "reports": {"warranty-term": "hybrid-warranty.json", "absent-term": "hybrid-absent.json"}
    }
  ]
}
```

Copy a report's complete `recipe.filters` into a query's optional `filters`
property to require an exact filter match. Every experiment must supply the
entire query set; missing reports fail validation. Duplicate result IDs, edited
queries, selected scopes, display filtering, and reordered results are rejected.
Coverage counts must also agree with the exported document list and the recipe's
requested limit. A manually shortened report cannot qualify as the complete
returned window. An optional `docIdScope` on a judged query requires the exact
document scope from its recipe. Additive view preferences in newer recipes do
not change retrieval metrics. An optional `ownerId` requires the exact owner
restriction recorded in the recipe, including an explicit empty owner.

A query may specify `"queryIntent": "advanced"` or `"queryIntent": "natural"`
to require that exact interpretation. Omit it when deliberately comparing
interpretations of the same original text. Recipe versions before 4 default to
Advanced syntax; unknown interpretation values are rejected. This receipt
keeps a changed interpretation visible rather than attributing it to a model.

Reports retain `ownerId` and a collision-free `documentKey` for each result. When
several owners can use the same document ID, set a judged query's `identity` to
`"owner-and-document"` and use the report's exact `documentKey` strings as the
keys in `relevance` and entries in `mustInclude` / `mustExclude`. The default
`"document"` mode remains compatible with older single-owner reports and rejects
duplicate IDs instead of silently merging their relevance judgments.

## Interpret the output

- Precision@k counts relevant documents in the first k positions, divided by k.
- Recall@k divides retrieved relevant documents by all positively judged documents.
- nDCG@k uses graded gain of `2^grade - 1` with logarithmic rank discount.
- Reciprocal rank is the inverse rank of the first positively judged document.
- Unjudged documents have zero gain and are counted explicitly; a high unjudged
  fraction means judgments need work before comparing model quality.
- Macro averages include queries with positive judgments. Negative queries are
  assessed through their explicit empty-result and exclusion constraints.
- Exclusion constraints inspect the entire returned window. Required documents
  must occur within the metric cutoff. A quality threshold or constraint failure
  produces exit status 1; invalid or incomplete input produces exit status 2.
- p50 and p95 use the nearest-rank definition over the recorded interactive
  requests. A few requests or mixed machines do not establish throughput or
  production latency. Repeated controlled runs are needed for those conclusions.

The report window is bounded. Evaluation does not claim to enumerate all corpus
matches, measure OCR recognition accuracy, or prove that generated answers are
supported by their citations. Native query and transaction contracts qualify
their respective correctness properties separately.
