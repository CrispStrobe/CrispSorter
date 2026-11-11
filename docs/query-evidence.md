# Query evidence and source spans

Indexed text inspection includes **Query evidence** for the executed search recipe.
The result includes the original query, its explicit interpretation, the actual
expanded lexical expression, and the exact owner, document, passage, source
fingerprint and indexing timestamp. Literal **Find in passage** remains an
independent control.

## What the explanation proves

For Full text and Hybrid, each leaf clause is evaluated against the current
document's real full-text postings using the same query compiler as retrieval.
The returned tree evaluates every AND, OR, NOT and field scope. Phrase,
wildcard, fuzzy and ordered or unordered proximity clauses keep their actual
query semantics. Unscoped leaf clauses identify which indexed fields matched:
title, body, headings or translation. Document and owner identity are resolved
together; missing or ambiguous full-text entries require a refresh.

Source highlights are separate, bounded witnesses in the selected **stored
passage**. They use the index's tokenizer and its original UTF-8 offsets, so
accent folding does not alter the displayed source. Ordinary fuzzy witnesses
use the same Levenshtein automaton as the index. Positional witnesses use the
same indexed-word positions, including removed-word gaps, phrase operands,
nonoverlap and direction. A proximity highlight covers the complete witnessed
interval, including intervening words.

Only successful positive branches that contribute to a true complete
expression receive highlights. A false AND branch inside a successful OR does
not contribute isolated term highlights. Excluded terms do not become positive
evidence; double negation is evaluated correctly. A true negative condition can
have no source span because it describes absence. A clause can also match a
title, a translation or another passage while having no body span here.

The index and source passage are inspected under one consistent read lease.
The interface rejects an explanation if navigation, the executed recipe, the
passage text or its source receipt changed while the request was running.
The explanation describes the current index. It does not reconstruct a past
ranking score, certify that the original file is still unchanged, or rerun
structured metadata filters.

## Semantic evidence

Semantic search returns the selected stored passage as evidence, with no
lexical truth claim, clause tree or highlighted word attribution. A model's
passage score is not a proof about individual words. Hybrid explains its
lexical component and identifies semantic evidence as a property of the mode;
it does not claim that every returned document was found by a dense channel.

## Boundaries and limits

Clause truth covers the complete indexed document and expression. Source
spans cover the returned passage only; the explanation does not invent missing
cross-passage text. A phrase or proximity match spanning two stored passages
can therefore be true without a complete interval in the current view.

At most 256 source witnesses are returned. Additional omitted witnesses are
reported explicitly without changing clause truth. Stored passage text is
limited to 256 KiB. Tokens touching an unknown beginning or ending are omitted
conservatively: cutting a chunk inside `alphabet` must not create a whole-word
`alpha` witness. Interior complete tokens remain usable. The source notes
disclose these omissions and passage truncation.

The existing query byte, token, nesting, dictionary and proximity-expansion
limits still apply. Explanation additionally limits source matching work,
including wildcard pattern length and fuzzy edit-distance work. A bound is an
explicit failure, never an incomplete expression labeled fully explained.

Explanation latency on large indexes has not been qualified. The source-work
limit is not a wall-clock deadline for index scoring: a native positional or
phrase scorer can inspect other postings before reaching the selected document.
Retrieval benchmark timings do not measure this inspection path.

## Local and remote protocol

The desktop command is `index_explain_passage`. It accepts the selected
identity, executed query and mode, `queryIntent`, `fuzzy` and `synonyms`. The
shared query planner derives the exact lexical expression before inspection.

Remote servers advertise `query_explanation_version: 1` independently of the
existing stored-passage capability. `POST /v1/passages/explain` requires the
normal bearer authentication and recomputes the original query's plan before
accepting its effective lexical expression. Each response confirms the version
in `x-crisp-query-explanation-version` and echoes the exact query receipts.
Clients bound the response and validate identity, tree truth, allowed fields,
Unicode byte boundaries, semantic restrictions and evidence counts. An older
server can continue serving ordinary stored passages while explaining matches
remains unavailable until that server is upgraded.

The standalone query and server contract suites exercise actual index postings,
Unicode/fuzzy/positional source witnesses, Boolean branch suppression, owner
isolation, cross-passage truth, clipped-token safeguards, work bounds, capability
negotiation, authentication and tampered receipt rejection.
