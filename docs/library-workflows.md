# Library workflows in 0.13

Start in **Inbox** to extract and analyze incoming files. Choose Fast, Balanced
or Quality to set a bounded amount of text for analysis, or Advanced to use your
saved settings. **Review** contains analyzed files awaiting your decision.
Acceptance and applying moves remain explicit actions. **Library** contains
indexed documents and the existing search, catalog and advanced tools.

The work summary links to incoming files, unaccepted suggestions, processing
errors and files indexed by filename alone. A filename-only entry needs content
extraction before it can support a document answer.

## Questions with evidence

In Chat, choose **Selected documents** and select files from the sidebar, or
choose **Library** to retrieve indexed passages. Selected-file retrieval searches
every extracted-text chunk, including persisted text; library retrieval uses the
configured hybrid search index. Conversation mode retains ordinary model chat.

Answers cite numbered passages. Open a source button to read the retained
excerpt and its original document. PDF navigation uses a page number only when
the source actually supplies one. Plain indexed text instead reports its chunk
and UTF-8 byte offsets; selected-text anchors count Unicode characters. No page
number is inferred from chunk position.

The prompt, source labels and question are counted with a bundled tokenizer.
The app reserves answer space and an additional margin. The displayed count is
a cl100k estimate for models with another tokenizer. A question without retrieved
evidence returns an explicit message without calling the answer model. Citations
let you inspect support; they do not establish that every model claim is correct.

The reserved answer budget is also passed to generation. OpenAI Chat Completions
uses [`max_completion_tokens`](https://developers.openai.com/api/reference/resources/chat/subresources/completions/methods/create),
which includes reasoning tokens; other compatible servers and local engines use
their corresponding output-token limit.

## OCR recovery

Open a supported file's OCR action, or open it in the OCR workbench. Task presets
are temporary and preserve your advanced configuration:

| Task | Released engine and behavior |
| --- | --- |
| Quick scan | DBNet detection and TrOCR printed-text recognition |
| Multilingual book | Complete Qwen2.5-VL OCR stage |
| Tables and receipts | Qwen2.5-VL with a table-preserving transcription prompt |
| Difficult page | Qwen2.5-VL after deskew, border crop, whitening and Sauvola binarization |
| Advanced settings | Your configured pipeline |

Retry weak pages skips proofread pages and manually corrected text. Focus a text
region to retry its crop. A crop with no recognized text retains the previous
result; Undo OCR restores earlier page results, including edits. Changes to a task
or advanced configuration rebuild the native pipeline and release its previous
weights before loading the replacement.

These presets use the pinned 0.17.12 runtime. Newer published OCR model files do
not imply that this runtime supports or qualifies their engines. OvisOCR2,
UnlimitedOCR2 and MiniCPM-OCR remain outside the shipped engine selector pending
runtime, quality and interactive-performance qualification.

## Fields with source evidence

Enable named-entity extraction and reindex a document to retain structured fields.
Open its Library preview and expand **Extracted fields**. The panel keeps the
original quotation, normalized editable value, model score, review status and
source anchor. An exact first-match anchor is labeled as such; absent anchors and
page numbers remain unavailable.

Approve a value, correct it or reject it. **Find matching documents** uses the
exact field tag, including a corrected normalized value that differs from the
original text. Export JSON for the full evidence or CSV for a spreadsheet.

**Use approved fields for sorting** stages values for the batch sorter. Use a
token such as `{Field:vendor}` in your path template. Only approved or corrected
values are eligible. The file's SHA-256 must still match its indexed source;
otherwise reindex and review its current contents. Review the resulting target
and accept the file before applying a move.

## Optional newer CrispEmbed services

Settings → Search Index contains an explicit experimental-service connection.
The bundled native runtime stays pinned. A separately operated newer
`crispembed-server` can expose MoEdern reranking and SystemOne suggestions over
its HTTP API. Probe its capabilities before enabling either option. Passages are
sent to the configured endpoint, so choose an endpoint you intend to receive
document text. Remote endpoints require HTTPS; loopback HTTP is supported.

MoEdern encodes questions and document passages with their distinct roles and
original scoring masks. It reranks at most 100 full-text candidates without
replacing your dense index. Failure retains the original search results and
displays a notice. SystemOne returns bounded category, collection or review
suggestions from the displayed excerpt. A model score is not a calibrated
probability of correctness. Suggestions never accept or move a file.

The tested service source, model hashes and frozen evaluation panel are in
`tests/quality/model-lock.json` and `tests/quality/library-corpus.json`. Neither
integration is enabled by default, and a small panel cannot establish quality on
your entire library. The evaluation uses explicit upstream experimental gates;
the server operator must opt into the corresponding public routes.

## Build qualification

`scripts/crispembed-runtime-lock.json` records the immutable released source and
archive hashes. Native downloads are verified before extraction. Release jobs
run native regressions with the platform's shipped features, then inspect final
packages and emit per-platform `.receipt.json` and `.sha256` files. Desktop smoke
checks execute the packaged program with sibling build libraries hidden. Mobile
archive inspection is recorded separately from execution on a physical device.
