# EU AI Act — what this app does, and on what basis

Record of every AI capability CrispSorter ships, its classification under
Regulation (EU) 2024/1689, and the reasoning. Written so that a question from a
customer, an auditor or a market-surveillance authority is answered from a
document rather than re-derived under time pressure.

**Not legal advice.** This is an engineering inventory plus a reading of the
regulation. Classification decisions and the declarations that follow from them
belong to the provider of record.

- **Audited:** 2026-08-01, against the source tree, not from memory.
- **Timing:** Article 50 (transparency) and the Annex III high-risk regime apply
  from **2 August 2026**. Prohibitions (Art 5) and AI literacy (Art 4) have
  applied since 2 February 2025; the GPAI chapter since 2 August 2025.
- **Role:** CrispSorter is a *provider* of an AI system — it is placed on the
  market under our name. It is **not** a provider of a general-purpose AI
  *model*: we train nothing, we load third-party GGUFs, so Chapter V's
  documentation, copyright-policy and training-data-summary duties sit with the
  model publishers, not with us.
- **Licence:** AGPL-3.0-or-later. Art 2(12) exempts free-and-open-source AI
  systems from much of the regime — but **explicitly not** where the system is
  prohibited, high-risk, or subject to Art 50. Being open source therefore does
  not dispose of the two obligations that actually bite here.

## Inventory

| Capability | Where | Classification |
|---|---|---|
| LLM document classification → folder moves | `index/`, `batch_session/`, chat | Minimal risk |
| OCR (multi-engine), table extraction, KIE | `extractors/` | Minimal risk |
| ASR transcription | `asr/` | Minimal risk |
| **TTS synthesis** | `asr/mod.rs`, `tts/` | **Art 50(2)** — synthetic audio |
| NMT translation, summaries, chat answers | `index/translate_commands.rs`, chat | **Art 50(2)** — synthetic text |
| Embeddings, reranking, NER, dedup, LID | `index/` | Minimal risk |
| Face **detection** (local) | `images/face.rs` | Out of scope by design — see below |
| Face recognition (optional external service) | default-off feature; absent from shipped builds | **Annex III(1)** — see below |

## Established positions

### Synthetic audio is marked (Art 50(2)) — enforced by a test

CrispASR is built around this obligation: `Session::synthesize` watermarks by
default and needs no attestation, while `synthesize_raw` emits **unmarked**
audio and refuses unless `accept_marking_responsibility()` was called first.

CrispSorter calls `.synthesize()` (`asr/mod.rs:307`, `:678`) and contains **zero**
occurrences of `synthesize_raw` or `accept_marking_responsibility`. So the
obligation is met by default rather than by vigilance.

Because that is a property a single future line could reverse without breaking
anything, it is now a test: `compliance.rs::tts_never_bypasses_the_synthetic_audio_watermark`
scans the tree and fails on either identifier. A second test asserts the scan
actually reaches the sources, so it cannot pass vacuously.

### Local face detection is deliberately not biometric

`images/face.rs` produces bounding boxes and confidence only, and says so:
*"no biometric data (embeddings, identity, age, gender, emotion)"*. Detecting
**that** a face is present is not biometric identification, and no sensitive
attribute (age, gender, emotion, ethnicity) is inferred anywhere — so neither
Annex III(1)'s biometric-categorisation limb nor its emotion-recognition limb is
engaged by the local path.

**Keep this line.** Adding embeddings or attribute inference to this module
moves it into a different regime entirely.

### No prohibited practice (Art 5) — and one boundary to preserve

Art 5(1)(e) prohibits untargeted scraping of facial images to build or expand
facial-recognition databases. It does not apply: there is **no path** from
automatic ingest (RSS, URL, watchfolders) into face enrolment — verified by
inspection — and the optional external-service integration is off by default, requires an
explicit login, and uploads only through a user-initiated action.

This is a property of the current wiring, **not a permanent one**. Connecting
watchfolder ingest to automatic face enrolment is precisely the change that
would make Art 5(1)(e) a live question. Treat that wiring as a compliance
decision, not a feature.

### Model licences (adjacent, already handled)

`index/embedder.rs:2118` gates non-commercial and use-restricted models behind
an explicit consent step — Jina v3/v5 are CC-BY-NC-4.0, EmbeddingGemma carries
the Gemma Terms of Use. Not an AI Act obligation, but the same class of
question and worth knowing it is covered.

## Open items

### 1. Face recognition — a deferred feature, absent from shipped artifacts

The honest account first: CrispSorter has a **partially built, deferred**
integration with an external image-management service. It was never completed
and completing it is not planned. It is not a capability that was built and then
withdrawn for compliance reasons — it is unfinished work that will not ship in
its current form.

Consequently it sits behind a cargo feature that is **off by default** and named
in no release or CI build recipe: a shipped artifact contains no client for it,
and the UI does not offer it. That is primarily a product decision about not
shipping unfinished work; the compliance benefit is a consequence, not the
motive.

Within that integration, the calls that would ask *who* is in a picture — an
identity list, and per-image identity assignment — sit behind a **second,
separate** feature, likewise named in no build recipe. Matching a probe face
against a gallery of N identities is the Annex III(1) reading we do not intend to
argue with, so that boundary is enforced mechanically rather than left to
judgement, whatever happens to the rest of the deferred work.

Enforced, not merely documented:

* `compliance.rs::no_build_recipe_enables_face_identification` fails the build if
  either workflow's `--features` / `tauri_args` lines, or `default`, ever name
  the identification feature — the realistic mistake being someone appending it
  while chasing a build error months from now.
* The runtime probe reports the two capabilities separately, so the UI hides the
  identity views instead of invoking commands the build does not register.

**Scope note.** The external service is a separate product with its own
provider obligations; its classification is not ours to assert and is not
recorded here. What this document states is narrower and is what we control:
**no artifact CrispSorter ships can perform or request face identification.**

**Local face detection is a different thing and stays.** `images/face.rs`
returns bounding boxes and confidence only — no embeddings, no identity, no
inferred attributes — so it engages neither the identification nor the
categorisation limb of Annex III(1). Keep that line; adding embeddings or
attribute inference there changes the regime.

Direction of travel: PLAN.md § P35 takes the deferred client out of the tree
altogether, re-expressed against a general plugin surface if and when it is ever
picked up again. Its design notes have been moved out of the public repository,
since publishing a design for something deliberately not built invites people to
rely on it.

### 2. Synthetic text — marked in the UI, not yet in exports

Chat answers, summaries and machine translation are synthetic text. As of
2026-08-01 they are labelled: one shared `AiGeneratedBadge` component (rather
than a copied span per view, which is what gets forgotten when the next
generative surface lands), with `aiDisclosure` strings in en + de. Wired to the
Translate output banner, and to the Chat panel header — panel-level there
because answers render inside the `deep-chat` web component, so per-bubble
marking would mean reaching into its shadow DOM, and a persistent notice on the
surface that produces them cannot drift out of sync with the messages.

**The mark travels, for the artifact that leaves the app.** Art 50(2) attaches to
the *content*, not to the window it was shown in — a badge tells the person at
the keyboard and nobody who receives the file. So `translate_docx` stamps its
output: `src-tauri/src/ai_provenance.rs` writes `cp:contentStatus` and
`dc:description` into `docProps/core.xml`, which is machine-readable and survives
mailing the file on.

The implementation refuses the easy version. "Patch `docProps/core.xml` if
present" silently does nothing for packages that lack the part, so when it is
missing the stamper creates it *properly* — the part, its `[Content_Types]`
override, and its package relationship — because a marker that applies to only
some documents invites the belief that it applies to all of them. Tested for: the
no-core-part case, patching without clobbering an existing `dc:title`,
idempotence under re-stamping, body preservation across the package rewrite, and
a non-zip input erroring instead of passing silently.

A stamping failure is logged rather than propagated: it must not discard a
translation the user waited for, but it is loud, because a quietly unmarked
artifact is the outcome to avoid.

**Still open:** chat answers the user copies out, and ASR/OCR exports — the
latter deliberately, since transcription and OCR render real input rather than
generate content.

The Art 50 carve-out for AI performing "an assistive function for standard
editing" without substantially altering input data is arguable for translation
and weak for chat and summarisation. Treating all three as in scope is the
cheaper position.

### 2b. Every generative surface, and whether it is marked

Enumerated 2026-08-01 by searching for generative paths rather than listing the
ones already known, because the risk is a surface nobody audited:

| Surface | Output | Marked? |
|---|---|---|
| Chat answers | synthetic text | ✅ panel-level badge |
| Machine translation | synthetic text (+ a written `.docx`) | ✅ badge on the output banner; ❌ not in the file |
| Batch metadata suggestions — `suggested_title` / `_author` / `_year`, and the `target_path` derived from them | machine-inferred text that renames and moves the user's files | ✅ section-level badge (added 2026-08-01; previously the largest unmarked surface) |
| TTS speech | synthetic audio | ✅ watermarked in the signal by CrispASR, test-enforced |
| ASR transcription | rendering of real audio, not generated content | n/a — Art 50(2) does not reach transcription |
| OCR text | rendering of pixels, not generated content | n/a |
| NER tags, embeddings, dedup, LID | inferences/scores, not content | n/a |
| Camera `ImageDescription` (EXIF) | written by the camera, not by us | n/a |

Checked and **not** present: no VLM image captioning, no image generation, no
LLM rewriting of OCR output (the one "proofread" mention is a comment about
confidence scores, not a rewrite path).

### 2c. Human oversight is the app's strongest position — record it

Batch classification does not act autonomously: suggestions land in
`BatchReview`, where the user edits title/author/year and sees the computed
target path, and **files move only on explicit confirmation**. That is a
meaningful human-in-the-loop design and it is the substantive basis for the
Art 50(1) position in §3 — worth stating rather than leaving to be rediscovered,
because a future change that auto-applies suggestions would remove it silently.

### 2d. Intended purpose, and why acknowledgement gates output

`src-tauri/src/intended_purpose.rs` holds the intended-purpose statement, names
the excluded uses specifically (employment screening, creditworthiness,
educational assessment, law enforcement, inferring identity/emotions/protected
characteristics, anything needing certified accuracy), and states that stepping
outside them makes the *deployer* the provider of a high-risk system under
Art 25(1)(c).

Acknowledgement is a **precondition for producing output**, not a line in a
README. The consequence is the useful part: an artifact this app produced is an
artifact whose operator was shown the statement, so the output carries the
notice. That is stronger than a consent record nobody can produce — and it is
*also* recorded on disk with the statement version and a timestamp, because:

**Where the inference stops.** CrispSorter is AGPL with public source, so anyone
can build a copy with the gate removed. "Output exists ⟹ acknowledged" holds for
builds *we* publish and proves nothing about an arbitrary third-party build. The
on-disk record (`intended-purpose-ack.json`: version, `accepted_at_unix`, `via`)
is what makes it producible rather than merely inferable. A superseded version, a
corrupt file, or no file at all all count as not acknowledged — tested.

Acknowledgement routes: the in-app prompt, `--accept-intended-purpose` on the
CLI, or `CRISPSORTER_ACCEPT_INTENDED_PURPOSE=1` for unattended runs. Inspect or
withdraw it with `crispsorter intended-purpose show` / `reset` — a notice you
cannot inspect or withdraw is a poor notice, and `show` prints the *recorded*
version alongside the current one so a stale record does not read as
"acknowledged". A gate that
breaks headless automation with an unactionable error would just get patched out.

Coverage — every path that produces AI output, and the UI surface that can
satisfy the gate so nobody meets a raw error string:

| Command | Gated in | UI surface with the prompt |
|---|---|---|
| `execute_batch` | Rust (`lib.rs`) | `BatchReview` (inline) |
| `translate_text` | Rust (`index/translate_commands.rs`) | `IndexSearch` doc-tools strip (inline) |
| `translate_docx` | Rust (`translate/tauri_commands.rs`, via `app.state()`) | `Translate` actions row (inline) |
| `tts_speak` | Rust (`lib.rs`) | `Chat` (blocking overlay) |
| chat completion | **frontend only — see below** | `Chat` (blocking overlay) |

All four Rust sites go through one `ensure_intended_purpose(&state, op)` helper
rather than four copied blocks: the gate's value is that *every* output path uses
it, and copies are chances to forget one.

**Chat is the exception, by necessity.** Its completions never reach Rust —
`deep-chat` calls the provider directly, and `Chat.svelte` invokes only
`tts_speak` / `tts_stop` / `asr_transcribe`. A Rust gate there would be
decorative, so the blocking overlay *is* the enforcement point for that surface.
If chat ever moves behind a Tauri command, gate it in Rust too.

The gate component shares **one** status probe across instances: it renders once
per search-result row, and N copies each doing their own IPC call for one
process-wide fact is waste.

Deliberately **not** gated: reading, indexing, search, OCR export, `--version`,
`doctor`. The notice is about what the system *produces*; blocking ordinary use
would punish everyone to no benefit, and a `doctor` that fails on a fresh install
makes support questions unanswerable.

### 3. Art 50(1) — telling the user they are dealing with an AI

Satisfied by obviousness for a chat panel. Less obvious for automatic
classification suggestions, which look like ordinary application behaviour.

### 4. Art 4 — AI literacy

An organisational duty on providers and deployers since February 2025. Nothing
to implement in code; recorded here so it is not mistaken for a code gap.

## Not covered by this audit

- **AIToolkit** ships image generation (`ui/image_gen_tab.py`) — Art 50(2)
  synthetic images and potentially Art 50(4) deep-fake disclosure. A larger
  surface than anything in CrispSorter, and a separate audit.
- **The external image service** is out of scope: separate product, separate
  provider, separate conformity work.
- **Deployment context.** Nothing here is Annex III on its own, but a *deployer*
  who applies document classification to CV screening, credit files or exam
  material enters the high-risk regime themselves. The README says so.
