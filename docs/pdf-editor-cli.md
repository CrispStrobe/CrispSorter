# PDF editor and CLI

The consolidated PDF editor uses the same native document operations as the CLI.
The GUI keeps an undoable session; the CLI can apply that session's serializable
edit operations as one recipe and publish a complete result. OCR settings remain
shared through the [OCR policy](ocr-engine-options.md).

## Export the existing text layer

```sh
crispsorter pdf export input.pdf --to text --out output.txt
crispsorter pdf export input.pdf --to markdown --out output.md
crispsorter pdf export input.pdf --to docx --out output.docx
```

These are the editor's text, Markdown and Word exports. They read the PDF text
layer and preserve page boundaries, with paragraphs reconstructed from wrapped
lines. They do not infer the original Word layout or recognize scanned images.
For scans, use the OCR Workbench or `crispsorter ocr` with the shared saved policy
or `--ocr-config`. A build without `ocr-office` returns an explicit Word export
capability error before writing anything.

The default CLI result is one JSON object containing `pages`, `format`, `output`
and `source: "text_layer"`. `--format text` selects a readable receipt. Operation
diagnostics go to stderr and the native app log, never into the JSON result.

## Apply editor operations

```sh
crispsorter pdf apply input.pdf --operations edits.json --out edited.pdf
```

`edits.json` is an ordered array of the editor's `EditOp` objects. For example:

```json
[
  {"kind": "rotate", "pages": [0], "degrees": 90},
  {
    "kind": "annotate",
    "annotations": [{
      "page": 0,
      "x": 72, "y": 600, "w": 220, "h": 40,
      "ann_type": "rectangle",
      "text": "Review this paragraph",
      "color": "#ff0000",
      "author": null,
      "quads": []
    }]
  }
]
```

Recipe page indices are **zero-based**, matching the editor API. Existing
one-shot commands such as `pdf rotate --pages 1` retain their documented
one-based page arguments. Annotation coordinates use unrotated PDF points from
the bottom-left corner of the page.

The recipe supports the editor's existing operation families:

| Family | Operation kinds |
| --- | --- |
| Page selection and order | `reorder`, `remove_pages`, `keep_pages` |
| Geometry | `rotate`, `crop` |
| Insertion | `insert_blank`, `insert_from` |
| Content | `page_numbers`, `watermark`, `text_box`, `metadata` |
| Markup | `annotate`, `erase_annotation` |

Each operation addresses the document produced by earlier operations. The
authoritative field definitions are in `src-tauri/src/pdf_session.rs`. An empty
`insert_from.pages` explicitly means all source pages. A recipe must contain
1–1,024 operations and fit within 1 MiB; malformed or unsupported operations
fail before publication.

The input is loaded once. Every edit must succeed before the output is staged,
flushed, synced and published. A rejected later edit preserves the previous
destination. The default JSON result reports `pages`, `operations` and `output`.
An output path equal to the input deliberately replaces the input only after
the entire recipe succeeds.

## Operation receipts and tests

Native `pdf.export_text`, `pdf.export_docx` and `pdf.recipe` receipts identify
validation, extraction/application, rendering and publication stages. Shared
`output.publish` receipts report the staged write boundary. Session actions in
the GUI additionally identify opening, applying, undoing, redoing, saving and
snapshot creation. Logs use fixed operation names and counts; document paths,
annotation contents and passwords stay out of these receipts.

Native unit tests cover recipe bounds, actual annotation/rotation output,
failure preserving existing files, saved/dirty state, snapshot ownership and
safe receipts. `scripts/verify_cli_operations.py` also runs actual PDF export
and recipe commands against the built CLI in desktop CI, including JSON output,
file readback and failure preservation. Native and browser results must be
assessed on the exact integrated revision; test source alone is not a pass.
