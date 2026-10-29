# Selected-document retrieval and citation audit

Selected-document chat now applies Unicode NFKC normalization for lexical
matching, preserving the original source text and Unicode-character anchors.
It indexes adjacent Chinese, Japanese and Korean characters so questions need
not contain whitespace. Latin and Arabic terms remain word matches. This is
lexical retrieval: it does not translate cross-language questions or establish
semantic equivalence. Library-wide retrieval continues to use the native index.

The author-written [selected retrieval panel](../tests/quality/selected-retrieval-panel.json)
contains eight English, German, French, Spanish, Chinese, Japanese, Korean and
Arabic examples. Each answer occurs beyond 9,000 characters and is paired with
a near-miss document. All eight examples passed in the targeted 20-test evidence
suite, alongside explicit no-evidence, Unicode-normalization, page-anchor,
prompt-budget and citation checks. The panel uses fictional text and measures
lexical retrieval only; it is not a representative quality estimate for personal
libraries, paraphrases or semantic search.

Each answer now records which supplied sources it actually referenced, including
bracketed groups such as `[S1, S2]`. The per-question list labels cited and
uncited passages and preserves warnings for missing or nonexistent citations.
Citation presence is a formatting check, not proof that an answer is supported.
The viewer opens the actual source and displays its known excerpt and anchor;
unknown page numbers remain unknown.

The app still pins [CrispEmbed v0.17.12](https://github.com/CrispStrobe/CrispEmbed/releases/tag/v0.17.12).
At upstream development commit [364f92199584](https://github.com/CrispStrobe/CrispEmbed/commit/364f9219958493bc34d3dfa6228e3037f242a835),
MoEdern and UltraBERT continuations remain under active qualification; the
newly registered workflows are not themselves passing model results or a new
released ABI. The existing independent MoEdern comparison remains the basis for
keeping it optional: 27M matched the small dense baseline, while 1.5M and 115M
missed the predeclared gate. See [the recorded evaluation](library-model-evaluation.md).
No model weights were downloaded and no model inference ran on the VPS for this
selected-document lexical panel.
