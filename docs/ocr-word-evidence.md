# Native OCR word evidence contract

Status: scoped for OCR-01 in the [implementation roadmap](ocr-roadmap.md).
This document defines the boundary; a schema alone is not engine support.

## First provider

The first implementation is an explicitly selected **System Tesseract** engine
(`system_tesseract`) in desktop builds that permit subprocesses. It uses the
installed Tesseract executable and installed language packs, independently of
the embedded CrispEmbed `tesseract` engine. The app must report actual installed
availability. It must never silently change the selected engine or substitute a
missing language pack. Sandboxed/mobile builds keep this capability unavailable.

One invocation writes TSV and TXT into a private temporary directory. Word rows
and text from that same invocation must align exactly, skipping only whitespace
between words. A disagreement is an error, not permission to approximate spans.
Paragraphs retain interior TXT spacing and line breaks. Page-wide TXT and
paragraph separators may be retained in provenance; current region exports use
their documented canonical separators and do not promise an exact TXT round trip.

System Tesseract recognizes the current working image. Image preparation is an
explicit separate operation. This preserves the displayed image/box contract
without claiming that embedded-model cleanup settings ran in the system engine.

## Recognition envelope

An optional `word_evidence` member on each native recognition region describes
its immutable original text:

```json
{
  "version": 1,
  "offset_unit": "utf16",
  "source": "tesseract_tsv",
  "words": [
    {
      "text": "Inv0ice",
      "start": 3,
      "end": 10,
      "x": 80,
      "y": 270,
      "w": 110,
      "h": 40,
      "confidence": 0.42,
      "confidence_basis": "recognition"
    }
  ]
}
```

For example, offsets `3..10` select `Inv0ice` in `😀 Inv0ice NS-2026-1042`.
Offsets are half-open UTF-16 code-unit offsets, matching JavaScript slicing and
textarea selection. They are not bytes, Unicode codepoints, or model tokens.

Each supplied span must be ordered, nonoverlapping, on Unicode boundaries, and
match the exact corresponding original substring. Coordinates are actual
provider evidence in **full-page working-image pixels**. A crop's origin is added
to parent and word boxes exactly once. Incoming boxes must be finite, positive,
and contained in their region and page. Region geometry edits never rescale
original word boxes: the source pixels have not moved. Replacing/transforming the
working image invalidates recognition through the existing image-change path.

Providers may supply only part of the text. Uncovered words remain visible with
their own available evidence or as unscored; no word boxes are inferred from the
region's dimensions. Invalid supplied evidence is rejected at the boundary.

`confidence` is either a recognizer word score in `[0,1]` or `null`. Tesseract TSV
scores are normalized from `[0,100]`; its unavailable sentinel becomes `null`.
The score is a review signal, not a calibrated probability of correctness. It
must not be described as a character mean/minimum. Direct word evidence takes
precedence over character-derived evidence for that supplied span, including a
direct `null` score. Region confidence, dictionary membership, and detector
confidence never fill missing word scores.

Optional provider alternatives have the shape
`alternatives: [{ text: string, confidence: number | null }]`. They must be
explicit provider output, not fabricated to populate an empty suggestion list.
The first System Tesseract TSV provider does not supply alternatives.

## Saved projects and exports

The frontend stores the validated envelope as `OcrRegion.wordEvidence`. This is
an additive, independently versioned extension to existing project version 1;
older projects without the member load with no native word evidence. Original
text, offsets, geometry, and score provenance survive editing, autosave, undo,
and JSON export.

The first implementation conservatively marks changed current text as edited.
Original evidence remains inspectable, with no stale offsets used to edit the
changed text. A later scoped correction-mapping increment may preserve evidence
for provably unchanged spans, but cannot infer alignment from repeated text.

Export uses `original_word_evidence` for immutable source evidence. Current
`word_evidence` is included only when the current text has verified alignment
with that envelope. For the initial implementation, this means exact equality
with original text; corrections omit the current envelope. Structured renderer
upgrades must explicitly consume this evidence before advertising word-level
geometry in every output format.

## Acceptance evidence

- Parser fixtures cover Unicode, adjacent-script tokens, punctuation, missing
  scores, malformed TSV, invalid geometry, and disagreement between TXT and TSV.
- A real installed Tesseract run produces boxes and scores; fixture UI behavior
  alone does not establish native support or OCR accuracy.
- Language preflight rejects a partially missing multilingual request.
- Subprocess timeouts kill and reap the process; output limits and private files
  bound work without pipe deadlocks.
- Frontend tests reject contradictory metadata, stale text offsets, and invalid
  boxes, and round-trip original evidence through save/load and JSON export.
- Browser checks verify actual word selection, original/current score wording,
  source highlighting, correction, undo, and exports.
- Native compilation and platform gates are recorded separately from parser or
  executable smoke checks; scanner/model/corpus qualification is not implied.
