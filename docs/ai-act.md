# EU AI Act — what this app does, and on what basis

Record of every AI capability CrispSorter ships, its classification under
Regulation (EU) 2024/1689, and the reasoning. Written so that a question from a
customer, an auditor or a market-surveillance authority is answered from a
document rather than re-derived under time pressure.

**Not legal advice.** This is an engineering inventory plus a reading of the
regulation. Classification decisions and the declarations that follow from them
belong to the provider of record.

- **Audited:** 2026-08-01, against the source tree, not from memory. **Re-audited
  the same day** after the first pass missed the AIToolkit panels entirely — see
  §5. Two capabilities this document had recorded as *absent* were fully wired.
  **Re-audited 2026-08-02** — four gaps, all of them places where the code fell
  short of what *this document* asserted rather than of a contested reading. See
  §6.
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
| **Remote image generation** | `AIToolkitCapability.svelte` → `/api/images/generate` | **Art 50(2)** — synthetic image; **Art 50(4)** if used for deep fakes |
| **Remote speech synthesis** | `AIToolkitCapability.svelte` → `/api/tts/synthesize` | **Art 50(2)** — synthetic audio, **not** watermarked |
| **Remote chat / translate / captioning** | `AIToolkitCapability.svelte`, `AIToolkitView.svelte` | **Art 50(2)** — synthetic text |

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
| Machine translation — `Translate` panel | synthetic text (+ a written `.docx`) | ✅ badge on the output banner; ✅ stamped in the file |
| Machine translation — `IndexSearch` doc-tools strip | synthetic text | ✅ badge (added 2026-08-02; §6) |
| Settings: connection test + provider benchmark | synthetic text (free-form prompt in the benchmark) | ✅ badge + gate (added 2026-08-02; §6) |
| `crispsorter chat transcribe --translate-to` | synthetic text written to a file | ✅ `ai_generated` in the JSON envelope; txt/SRT/VTT warn that they carry no marking (§6) |
| Batch metadata suggestions — `suggested_title` / `_author` / `_year`, and the `target_path` derived from them | machine-inferred text that renames and moves the user's files | ✅ section-level badge (added 2026-08-01; previously the largest unmarked surface) |
| TTS speech | synthetic audio | ✅ watermarked in the signal by CrispASR, test-enforced |
| ASR transcription | rendering of real audio, not generated content | n/a — Art 50(2) does not reach transcription |
| OCR text | rendering of pixels, not generated content | n/a |
| NER tags, embeddings, dedup, LID | inferences/scores, not content | n/a |
| Camera `ImageDescription` (EXIF) | written by the camera, not by us | n/a |
| AIToolkit chat / translate | synthetic text | ✅ panel badge + gate (added 2026-08-01, §5) |
| AIToolkit vision ("Describe this image") | synthetic text | ✅ panel badge + gate (§5) |
| AIToolkit images | synthetic **image** | ✅ backend embeds an XMP/iTXt assertion; client now uses the marked copy (§5) |
| AIToolkit TTS | synthetic **audio** | ✅ backend embeds XMP (WAV) / ID3v2 (MP3); marking travels in the bytes (§5) |

Checked and **not** present *in the Rust/local surfaces*: no LLM rewriting of OCR
output (the one "proofread" mention is a comment about confidence scores, not a
rewrite path).

⚠️ **This section previously claimed "no VLM image captioning, no image
generation" full stop. That was wrong** — see §5. The enumeration was done by
searching the Rust tree, and every surface it missed is TypeScript calling a
remote HTTP backend. The lesson is recorded there rather than quietly fixed
here, because the *method* is what failed, not the conclusion about `src-tauri/`.

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
| `crispsorter chat query` | Rust (`cli/mod.rs`) | CLI flag / env var |
| `crispsorter chat tts` | Rust (`cli/mod.rs`) | CLI flag / env var |
| `crispsorter chat transcribe --translate-to` | Rust (`cli/mod.rs`) | CLI flag / env var |
| `crispsorter batch process` | Rust (`cli/mod.rs`) — **added 2026-08-02**, §6 | CLI flag / env var |
| `crispsorter batch apply` | Rust (`cli/mod.rs`) — **added 2026-08-02**, §6 | CLI flag / env var |
| AIToolkit generative panels | **frontend only** (remote backend) | `AIToolkit*` (blocking overlay) |

All four Tauri sites go through one `ensure_intended_purpose(&state, op)` helper
rather than four copied blocks: the gate's value is that *every* output path uses
it, and copies are chances to forget one. The CLI sites go through the sibling
`ensure_intended_purpose_cli(op)`, which differs only in resolving the data dir
from disk instead of from `AppState`.

**The CLI was missed on the first pass** (added 2026-08-01): `chat query` printed
a completion to stdout with nothing on record, while this table asserted the gate
covered every output path. `--accept-intended-purpose` existed but only *wrote* an
acknowledgement — nothing ever *required* one outside the GUI.

**And the fix for that miss was itself incomplete** (2026-08-02): it gated
`ChatCmd` and stopped there. `batch process` — which calls an LLM to infer
title/author/year — and `batch apply` — which moves the user's files on the
result — stayed open for another day, while this table again asserted full
coverage. The lesson is narrower than "audit the CLI": when a class of surface
is found unguarded, the fix has to enumerate the class, not patch the instance
that was noticed.

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

### 5. Remote generative surfaces — the AIToolkit panels

**What the first pass got wrong.** This document recorded image generation and
image captioning as *not present*, and treated AIToolkit as somebody else's
audit. Both were mistakes of the same kind: the enumeration searched
`src-tauri/**/*.rs`, and these surfaces are `.svelte` files calling a remote HTTP
backend. Nothing in Rust mentions them.

They are not hypothetical or half-built. `src/lib/components/AIToolkitCapability.svelte`
is bundled unconditionally, rendered from `+page.svelte`, and reachable as
first-class nav tabs (`src/lib/tabs.ts`) as soon as a connected backend
advertises the capability:

| Capability | Endpoint | Output |
|---|---|---|
| `images` | `/api/images/generate` | synthetic image, rendered inline |
| `tts` | `/api/tts/synthesize` | synthetic audio, played inline |
| `chat` | `/api/chat/completions` | synthetic text |
| `translate` | `/api/translate/text` | synthetic text |
| `vision` | `/api/vision/analyze` (`"Describe this image."`) | synthetic text |

**Why this was worse than an inventory gap.** `AiGeneratedBadge.svelte` says in a
comment that audio needs no badge because *"CrispASR watermarks that in the signal
itself"*. True of every CrispASR path — and false here. The AIToolkit TTS panel is
a second synthesis path that CrispASR never touches, so the app had an unmarked
synthetic-audio surface behind a guarantee that read as absolute.

**Why the existing guard could not have caught it.**
`compliance.rs::tts_never_bypasses_the_synthetic_audio_watermark` greps Rust for
two CrispASR identifiers. A TypeScript `fetch` to `/api/tts/synthesize` contains
neither. The guard was not weak, it was **scoped to the wrong tree** — and its
passing was being read as a property of the app rather than of `src-tauri/`.

**The guards were not running at all.** Worth stating separately, because it
undercuts every "enforced by a test" claim in this document: on this branch
`cargo test --package crispsorter --lib` did not **compile** (six errors in
`sync/`, `translate/` and `cli/` — a `super::` that meant `self::`, a missing
`?`, an uninferrable `None`, an unboxed recursive `async fn`, and a `PathBuf`
formatted without `.display()` — the last one inside the AI-provenance failure
path itself). A test that cannot build cannot fail, so the watermark and
face-identification guards had silently stopped protecting anything, and a
long-broken assertion in `intended_purpose` (`"not intended for"` vs the
statement's `"NOT intended for"`) had never once run. Fixed 2026-08-01; the
compliance suite compiles and passes.

The general lesson: **"enforced by a test" is a claim about CI, not about the
test file.** If the suite is red or unbuildable, the invariants are documentation
again — which is exactly the state this document described as unacceptable.

**Now enforced** (`compliance.rs`):

* `every_generative_frontend_surface_discloses_and_gates` — any `.svelte` file
  calling a generative client method must also carry `AiGeneratedBadge` **and**
  `IntendedPurposeGate`. No exemption list; the fix is to mark the surface.
* `the_aitoolkit_client_exposes_no_unclassified_endpoint` — the client's `/api/…`
  set is pinned to the twelve endpoints reviewed here. A thirteenth fails the
  build until somebody decides whether it generates content. This is the guard
  that would have caught the original miss, because it fails on *arrival* of a
  capability rather than on somebody remembering to re-audit.
* The non-vacuity test now also asserts the `.svelte` scan finds the tree, finds
  `AIToolkitCapability.svelte` specifically, and that at least one view still
  matches a generative needle — so a rename of the client's methods surfaces as
  a failure instead of a silently empty scan.

**The artifacts ARE marked — and CrispSorter was throwing the marking away.**

A first version of this section claimed the image and audio artifacts carried no
embedded marking, on the reasoning that a file generated remotely arrives already
encoded. That reasoning was never checked against the backend, and it was wrong.
`crossplatform/pybackend/routers/` marks both:

| Path | Marking |
|---|---|
| `tts.py` | writes the assertion **into the bytes** — an XMP `_PMX` chunk for WAV, ID3v2.4 `TXXX` frames (`DigitalSourceType`) for MP3 — and returns `X-AI-Marked`. Its own comment notes response headers "are gone the moment the client saves the file", so headers are only a hint. |
| `images.py` | marks every returned image in place; for URL-only provider responses it **fetches the image through the SSRF guard and returns marked base64**, so that "the client never has to be trusted to mark on download". Ships `marked` per image plus `ai_generated`, `digital_source_type` and the Art 50(4) `disclosure` text. |

Both are metadata assertions, not watermarks — `ai_act.py` says so itself: weaker
than a watermark, does not survive re-encoding, but machine-readable and
detectable, "the difference between marked and entirely unmarked output".

**The real defect was on our side, and it was invisible.** The backend hands back
the provider's original `url` *and* a marked `b64_json`. CrispSorter did:

```js
imgUrl = img?.url ?? (img?.b64_json ? `data:image/png;base64,${img.b64_json}` : '');
```

`url` wins. So the app displayed and saved the **unmarked original** and discarded
the marked copy the backend had gone out of its way to produce — defeating, from
the client, a duty the server had already discharged. Nothing about that line
reads as a compliance bug; it reads as "prefer a URL over a data blob". That is
what makes it worth a test rather than a note.

Fixed: `markedImageSrc` (`src/lib/aitoolkit.ts`) encodes the preference once —
`b64_json` always wins, MIME sniffed from the base64 prefix — and
`generated_images_are_taken_from_the_marked_copy` fails the build if a view
generates images without it, or reaches for `.url ??` again. TTS now reads
`X-AI-Marked` and reports the real state instead of assuming either way.

**Where that leaves Art 50(2).** Satisfied for the text panels (badge at the
surface) and, for images and audio, satisfied *in the artifact* by the backend's
in-band assertion — which is the layer the article actually cares about. The
residual limits are stated in the UI rather than papered over: the panel says the
marking lives in metadata and that re-encoding can strip it
(`aiDisclosure.markedArtifact`), and shows `unmarkedArtifact` when the backend
reports `marked: false` for a format it could not handle. Neither state is
inferred; both are read from the response.

**Update 2026-08-02 — the watermark path now exists.** The strengthening above
was described as belonging to the backend. It did belong there, and it was
already built there: AIToolkit's CrispASR path applies an AudioSeal watermark in
the signal plus C2PA Content Credentials, and `assert_no_marking_opt_out` makes
disabling it a hard error. It was simply **not wired into the HTTP sidecar** —
`routers/tts.py` reached only remote OpenAI-audio providers, its docstring
calling the local transport "stubbed", so every response fell back to the
metadata assertion even where the strong path was installed. Fixed in AIToolkit
(`COMPLIANCE.md` § 11); the sidecar now routes `provider == "CrispASR"` through
the binary.

So there are two marking strengths, and the difference is the one Art 50(2)
readers care about — whether the mark survives re-encoding:

| Transport | Marking | Survives re-encode |
|---|---|---|
| `X-AI-Marking-Path: crispasr` | AudioSeal watermark + C2PA | **yes** |
| `X-AI-Marking-Path: provider-metadata` | XMP / ID3 assertion | no |

CrispSorter reads that header and says which one the user has
(`aiDisclosure.watermarkedArtifact` vs `markedArtifact`, en + de) rather than
describing every marked artifact with the weaker sentence. For third-party
OpenAI-audio providers the metadata floor remains the ceiling — those hand back
finished bytes and there is no encoder to reach.

**Worth keeping:** this gap survived four internal AIToolkit audits and was
found from the client side, in one question — *does the audio I receive actually
carry a watermark?* An internal audit reads the capability and confirms it
exists; a client can only see what arrives on the wire. That asymmetry is the
argument for auditing across the integration boundary rather than per-repo.

**Art 50(4).** A prompt-driven image generator can produce deep fakes, and that
disclosure duty falls on the *deployer*. The backend ships the wording in
`disclosure` precisely so clients neither invent nor omit one; CrispSorter was
discarding that field too, and now renders it beneath the image.

### 6. The 2026-08-02 re-audit — the guards were looking at the wrong client

Four gaps, found by re-deriving the surface list from the call graph instead of
from this document. None needed a contested legal reading; each was a place
where the code did less than the text above claimed.

**6.1 `batch process` / `batch apply` were ungated.** See § 2d. Fixed.

**6.2 The search panel showed machine translation with no badge.**
`IndexSearch.svelte` carried `IntendedPurposeGate` but never imported
`AiGeneratedBadge`, and rendered `translated_text` unmarked. § 2b claimed
translation was badged — true of `Translate.svelte`, false of the
higher-traffic surface. Fixed.

**6.3 The disclosure guard could not see the local LLM at all.** This is the
finding that explains 6.2 and 6.4, and it is § 5's lesson recurring one layer
in. `GENERATIVE_CLIENT_CALLS` listed five `AIToolkitClient` methods. But the
app's primary generative call is `llmClient.query()`
(`src/lib/llm/client.ts` → `POST /chat/completions`), and **`Chat.svelte`
matched none of the five needles** — the flagship generative surface was
reviewed by nothing. Its badge and gate were correct, by hand; deleting either
would have passed CI.

The first version of the guard was scoped to the wrong *tree* (Rust, not
Svelte). This one was scoped to the wrong *client* (remote, not local). Both
times the guard passed, and both times its passing was read as a property of
the app. **A green guard is evidence about what it looks at, and nothing else.**

Fixed, and the non-vacuity test now names `Chat.svelte` specifically rather
than settling for "at least one view matched" — the weak floor that let this
survive. `no_unreviewed_module_generates_text` additionally pins the `.ts`
callers, since a module has no badge to carry and the view-level guard cannot
express the invariant for it.

**6.4 `Settings.svelte` was an unmarked, ungated generative surface.** The
provider benchmark runs a **user-authored free-form prompt** and displays the
model's answer; the connection test displays a response too. Low traffic, but
the rule this document sets is that there is no exemption list. Fixed — and it
was 6.3 that let it sit there, since neither call matched a needle.

**Also hardened, none of them live defects:**

* The watermark guard scanned `src-tauri/src` only, silently assuming synthesis
  could never live in one of the workspace's other eight members. Now scans the
  workspace.
* The face-identification guard read a hardcoded `["ci.yml", "release.yml"]`.
  Now enumerates `.github/workflows/*`, and fails if it finds none.
* **A new limb, not previously guarded:** `crisplens_protocol::Face` carries
  `estimated_age` / `estimated_gender`, and those arrive with the *parent*
  `images-crisplens` feature — which is legitimately allowed to ship, since the
  rest of that surface identifies nobody. Inferring age or gender from a face is
  Annex III(1)(b) biometric **categorisation**, a different limb from the
  identification one § 1 covers. CrispSorter reads neither field today;
  `no_inferred_biometric_attribute_is_ever_read` keeps it that way.
* The badge tooltip claimed summaries are AI-generated. `index/summary.rs` is
  extractive sentence-slicing and the command is called from no view. Corrected
  in en + de — overstating the badge is its own kind of inaccurate disclosure.

**Still open after this pass.** Synthetic *text* has no in-band marking except
`translate_docx`. Chat answers copied out of the app carry nothing, and
`chat transcribe --translate-to` marks the JSON envelope
(`translation.ai_generated`) but cannot mark txt/SRT/VTT — prepending a banner
to a subtitle file is not a marking, it is a broken subtitle, so those formats
warn on stderr instead. That is a floor, not a solution; a real one needs a text
provenance convention this project does not get to invent alone.

## Not covered by this audit

- **AIToolkit's own UI** (`ui/image_gen_tab.py` and the rest of that product's
  surface) remains a separate audit with its own provider obligations. What
  changed on 2026-08-01 is that *CrispSorter's client for it* is no longer
  treated as out of scope — see §5. Shipping the UI for a capability makes it
  ours to disclose, whoever runs the model.
- **The external image service** is out of scope: separate product, separate
  provider, separate conformity work.
- **Deployment context.** Nothing here is Annex III on its own, but a *deployer*
  who applies document classification to CV screening, credit files or exam
  material enters the high-risk regime themselves. The README says so.
