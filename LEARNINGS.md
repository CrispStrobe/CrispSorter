# CrispSorter — Learnings & Key Insights

Critical things we've learned that are easy to forget when returning to this codebase.

---

## CI release refs must use tagged releases, not commit SHAs

The release workflow downloads pre-built native libs (libcrispembed,
libcrispasr) from GitHub Releases.  The download URL is:
`github.com/<repo>/releases/download/<REF>/lib*.tar.gz`.  This only
works when `<REF>` is a release tag (e.g. `v0.13.0`), not a raw
commit SHA — raw SHAs return 404.

**Rule:** Only bump `CRISPEMBED_REF` / `CRISPASR_REF` in
`release.yml` to tagged releases.  If you need features from HEAD,
either tag a new release on the sibling repo first, or stub the new
API calls so the code compiles against the older tagged version.

## TOML target-specific dependency sections (gotcha)

A `[target.'cfg(...)'.dependencies]` section in Cargo.toml extends
until the next `[section]` header.  If placed mid-file among regular
`[dependencies]` entries, **all subsequent deps are swallowed into
the target gate**.  This caused a v0.9.0 Android build failure where
`rusqlite`, `sha2`, `image`, and 590+ other crates were silently
excluded from the Android build because a `[target.'cfg(not(android))'
.dependencies]` section for `arboard` was placed in the middle of the
dependency list.

**Rule:** Always place target-specific dependency sections at the
bottom of Cargo.toml, grouped with other target sections — never
inline them between regular deps.

## lopdf 0.38 API notes

- `as_dict()` returns `Result`, not `Option` — chain with `.ok()`
- No `merge_document()` method — manual object copying with ID
  remapping required for PDF merging
- ~~`CryptFilter` is `pub(crate)`, so V2 (RC4-128) is the only handler~~ —
  **stale**. 0.38 ships `EncryptionVersion::V5` + `Aes256CryptFilter`, and
  AES-256 is now our default. RC4 remains only behind `--legacy-rc4`.
- `EncryptionVersion::V2` requires `/ID` in the trailer
- **`Document::load` decrypts eagerly, and only with the empty password.**
  There is no public way to pass one. Given a real user password you get a
  1-object document, `decrypt()` then reports success having decrypted
  nothing, and `save` writes ~170 bytes with no `/Root`. Detect both cases;
  do not report success (`pdf_ops::decrypt_pdf`).
- **`Stream::compress()` returns `Ok(())` without compressing** in two
  cases: a `/Filter` is already set, or deflate would not save at least 19
  bytes — which is the normal outcome for a one-line page's content stream.
  Count the `/Filter` the call leaves behind, never the call: counting calls
  made `pdf compress` report 12 streams deflated on a file it had left in
  plaintext. Also skip `/Type/XRef` and `/Type/ObjStm` streams — the writer
  rebuilds them, so work there never reaches the file.
- **Object streams need xref streams.** Packing objects into an `/ObjStm`
  and then saving with a classic xref table makes them unreachable (page
  count went 8 → 0). Use `save_with_options` with `use_object_streams(true)`
  *and* `use_xref_streams(true)`, and let lopdf do the packing.

---

## PDF structure gotchas (P32)

Independent of lopdf — these are things the format does, not the crate.

### `/AcroForm` may be a *direct* dictionary in the catalog

Handling only the indirect-reference form made MuPDF-authored forms
invisible: `read_fields` returned nothing and `flatten` did nothing while
reporting success. Resolve both shapes (`pdf_forms::acroform_dict`).
Related: a checkbox's "on" value is whatever its widget's `/AP` says, not
necessarily `/Yes` — a German form uses `/Ja`.

### Base-14 AFM metrics are Adobe-Standard-encoded

The AFMs' own `C` codes are *not* WinAnsi. Indexing the width tables by
Latin-1 code while declaring `/WinAnsiEncoding` made every accented
character (`Ü ä ß ï`) report as unsupported by fonts that plainly have it.
Key the tables by **glyph name** through the Adobe Glyph List
(`pdf_base14.rs`); the header comment carries the regeneration snippet.

### Text extraction has two different jobs

`pdf text` reads the text layer; `ocr` reads pixels. A scan gives the first
one nothing, so it says which command the caller wanted rather than
printing an empty string that looks like a failure.

---

## clap: a subcommand that shadows a global arg panics at *runtime*

Not at build time, and not in `--help` — the subcommand's own help renders
fine, so it survives review and blows up when someone invokes it. This
crate hit it twice (`--format` and a `--out`/`format` field-name clash).
The field *name* is what collides, not the long flag. `cli::global_arg_collision_tests`
now walks the whole command tree and fails on any shadowing; it was verified
to fire by injecting a collision. Rename the field
(`pdf export-annotations --to`, `pdf number --style`).

---

## "The code is real" is not "the code is right" — and tolerant readers hide it

`pdf_oxide`'s linearizer was a 696-line **no-op** whose own docs said
"reserved". The lesson drawn from that was *read the source*. zpdf 0.11.0
then taught the follow-up: its linearizer is 449 lines that genuinely walk
the object graph, emit a `/Linearized` dict and patch `/L /H /O /E /T`
offsets in a second pass — and the file it produces is malformed.

On a 4-page fixture: the cross-reference table omits objects 11–13 that the
page tree references (`MuPDF: object out of range (11 0 R); xref size 11`,
hundreds of times), the object count disagrees with the highest object
number, and `/N` disagrees with the page count.

What makes this worth writing down is **how nearly it passed**: MuPDF and
poppler both recover the page text from the broken file, so "extract the
text and compare it" — the check that caught pdf_oxide — says *pass* here.
Only `qpdf --check-linearization`, a check specific to the claim being made,
names the `/N` defect. Generalisation: verify the *property you claim*, not
just that the output is still readable; readers are built to paper over
damage, which is exactly what makes them poor judges of structural
correctness.

Our own page-count/catalog guard did reject it, which is why
`pdf linearize` fails loudly instead of writing a plausible file.

---

## A verification guard can skip exactly the case it exists for

`pdf_ops::verify_decrypted` is the guard that caught `pdf_oxide` writing
still-encrypted streams: it re-reads the output and requires text if the
source had text. But its reference is the **source** —

```rust
let src_text = pdf_extract::extract_text(src_path).unwrap_or_default();
if src_text.trim().is_empty() { return Ok(()); }   // nothing to compare
```

— and `pdf_extract` cannot read a file protected by a non-empty **user**
password. So for the hardest case, the one where a decryptor is most likely
to hand back ciphertext, `src_text` comes back empty and the content check
*silently returns Ok*. It works for owner-password-only files (which
pdf_extract can open) and skips for the rest. That is why the zpdf path takes
its reference from the in-memory decrypted document instead, before writing.

Generalisation: when a guard needs a "before" value, check that the before
value is *obtainable* in the case that matters. A guard whose premise fails
open is worse than no guard, because it reports a pass.

---

## An embedder's width is not a constant — and a lying exit code hides it

`crispsorter index ingest` hardcoded `LocalIndex::open_or_create(&data_dir,
1024)` — bge-m3's width — for **every** `--model`. Any narrower model
(MiniLM and both e5-small variants are 384, nomic is 768) built a 1024-wide
LanceDB table and then **panicked inside arrow** on the first write:
`Length of the child array (384) must be the multiple of the value length
(1024)`. The panic killed the writer task, so every later file failed with
"Writer task has stopped — index may need re-init".

Two lessons, and the second is the dangerous one:

1. Derive the width from the model (`Embedder::dims()`), and for a table that
   already exists read the width **off the table** rather than trusting the
   caller — `open_or_create` used the caller's guess to build the schema, so
   a wrong guess silently produced mismatched record batches. There is now a
   guard in `chunks_to_record_batch` that refuses a mismatched embedding with
   both numbers in the message instead of letting arrow panic.
2. **The command printed "0 ingested, 4 errors" and exited 0.** Every file had
   failed and the exit status said success — so the live harness recorded a
   pass for ingest and only the *search* assertions failed, which sent the
   investigation to the wrong place. Any command that counts failures must
   fold them into its exit status. This is the same family as the compression
   report that counted attempts instead of results: the code knew, and did
   not say.

---

## A GUI/CLI dispatcher fails *silently* — guard the verb list with a test

`main.rs` decides CLI-vs-GUI by scanning argv for a name in
`cli::SUBCOMMANDS`, a hand-written list, and falls through to the GUI on no
match. So a verb missing from that list does not produce "unknown command" —
**the app opens its window and ignores the command line**, while the verb's
own `crispsorter <verb> --help` renders perfectly (clap is reached in that
path). Three whole families sat like that: `docx`, `zone` and `search`, each
fully implemented, CLI-documented, and unreachable. The symptom when I ran
one was a process hanging for ten minutes; `sample <pid>` showed
`NSApplication run` at the bottom of the stack, which is the tell.

Fixed by `cli_mode_detection_tests`, which derives the real names (plus
aliases) from `Cli::command()` and fails in both directions — a verb absent
from the list, or a stale name left after a rename. Any hand-maintained
mirror of a derivable set wants exactly this kind of test; the same applies
to the Tauri `invoke_handler` list, which is also written out by hand.

---

## Binary test fixtures can usually be authored instead of committed

The P30 DOCX tests sat deferred for months behind "needs test .docx
fixtures in the repo". But a `.docx` is a zip of a few XML parts, so the
fixtures can be *written*: `docx_tools::fixtures` builds them in memory with
the `zip` crate (`[Content_Types].xml` + `word/document.xml` +
`word/_rels/document.xml.rels` is enough for `crisp_docx_core::open`), and
each fixture is a readable string showing exactly the shape its test depends
on. No binaries in git, no external corpus, and a reviewer can see why a
test passes. `crisp-docx`'s own integration tests do the same. The same
trick applies to any zip-container format (EPUB, ODF, XLSX) and to PDF.

Corollary for the tests themselves: serde round-trip tests on a result
struct pass just as well when the command returns an empty struct. Assert on
behaviour — that transplant kept the *blueprint's* page size and dropped the
blueprint's body text, that quote normalisation produces *different* bytes
for German and English (a normaliser ignoring its style argument would
otherwise pass), that footnote injection removed the literal `[1]`.

---

## Verification: assert the premise, or the check cannot fail

`scripts/verify_pdf_independent.py` produced two false passes, both from
trusting a fixture instead of interrogating it:

- A hard-coded rectangle redacted **empty space** (MuPDF's `insert_text` is
  y-down from the top, and the pages are A4, not Letter), so every "the text
  is gone" assertion held trivially. Ask the reader where the text is
  (`page.search_for`) and derive the rectangle from that.
- "Streams are deflated" passed because **MuPDF's own save had already
  deflated them**. The compression input is now built with
  `qpdf --stream-data=uncompress`, and the harness asserts *no* `/Filter`
  exists — and that the streams exceed 2 kB, since lopdf correctly declines
  to deflate a short one — before testing the claim. That premise check
  immediately caught the next mistake: PyMuPDF appends a separate content
  stream per `insert_text` call, so 40 calls give 40 short streams; pass the
  lines as one sequence instead.

The general rule: a claim about a change needs both sides asserted (changed
here, unchanged there), and any state the claim depends on has to be
verified rather than assumed. Cross-check the tool's own report against what
an independent reader sees — that is what caught the compression
mis-reporting that every in-process unit test had missed.

---

## CrispEmbed integration (P17)

### CrispEmbed structs are `Send` but not `Sync`

All CrispEmbed types (`CrispEmbed`, `CrispFace`, `MathOcr`, `OcrPipeline`,
`CrispVit`, `CrispLayout`) wrap a raw C context pointer and implement `Send`
but not `Sync`.  That means you can move them between threads but NOT share
references across threads.  The integration pattern used throughout P17 is:

```rust
static INSTANCE: OnceLock<Mutex<crispembed::SomeType>> = OnceLock::new();
```

`OnceLock` handles lazy init; `Mutex` serialises access.  This trades
concurrency for zero repeated model-load overhead.  If throughput matters
(e.g. batch ingest), create per-thread instances instead.

### GGUF-only models need `None` in `to_model_spec()`

When adding `EmbedderModel` variants that have no ONNX path (only GGUF via
CrispEmbed), they must return `None` from `to_model_spec()` AND have an
entry in `gguf_registry_name()`.  The init path in `Embedder::new()` checks
`backend == Gguf` first, then `is_native()`, then `to_model_spec()` — if a
GGUF-only model falls through to the ONNX path it will fail with a confusing
"no model spec" error.

### EU AI Act: face detection vs recognition

Face **detection** (is there a face? where?) is fine.  Face **recognition**
(who is it? face embeddings for person matching) falls under biometric
identification and triggers high-risk classification.  P17.4 deliberately
exposes only `detect_faces` / `count_faces` — no `recognize_faces`, no
`encode_face`, no face embeddings.  The CrispEmbed API supports recognition
but we don't call it.

### Two separate engine-int namespaces: pipeline stages vs VLM escalation

CrispEmbed has **two different integer mappings** for OCR engines:

1. **`crispembed_ocr_stage.engine`** — the per-stage pipeline engine ID
   (0=dbnet_trocr, 1=surya, 2=got, ..., 12=qwen3vl). Mapped by
   `engine_id()` in `extractors/mod.rs`.

2. **`crispembed_ocr_pipeline_params.vlm_engine`** — the VLM escalation
   engine for simple mode (0=GOT, 1=GLM, 2=Qwen2-VL, 3=InternVL2,
   4=Qwen3-VL). Mapped by `vlm_engine_id()` in `ocr_crispembed.rs`.

These are **not the same numbering**. GOT is engine 2 in the stage
namespace but 0 in the VLM namespace. Adding a new engine to one doesn't
automatically add it to the other — check both `engine_id()` and
`vlm_engine_id()`, plus the CLI value_parsers for `--engine` and
`--vlm-ocr-engine`, plus both Settings UI dropdowns (stage builder and
simple-mode VLM escalation).

### `isVlmEngine` in Settings.svelte must track det+rec engines

The frontend helper `isVlmEngine(e)` controls whether the stage builder
shows two model fields (det+rec) or one (VLM single-model). Any engine
that uses the DBNet detector + a separate recognizer (dbnet_trocr, surya,
tesseract, parseq) must return `false`. PARSeq was missed initially
because its name doesn't suggest a det+rec split, but it's engine 7 =
DBNet detect + PARSeq recognize.

### Feature-gated imports and constants cause warnings in default builds

Many modules (ocr_crispembed, layout, math_ocr, face, vit_embed,
omni_embed, audio, ocr_paddle) import `anyhow::Context`, `std::sync::Mutex`,
or define constants like `DEFAULT_DET_MODEL` that are ONLY used inside
`#[cfg(feature = "crispembed")]` (or `crispasr`, `paddle-ocr`) blocks.
Without a matching `#[cfg]` on the import/constant itself, `cargo check`
in default-feature builds (which is what CI and most devs run) emits
"unused import" / "never used" warnings.

Pattern: every import, constant, or helper function that's only consumed
inside a `#[cfg(feature = "X")]` block needs the same gate on its
declaration. For functions also used in `#[cfg(test)]` (like `engine_id`
and `source_type_id`), use `#[cfg(any(feature = "crispembed", test))]`.

---

## Build & CI

### `notify` event handlers run on a non-tokio thread

`notify::recommended_watcher`'s callback runs on a notify-internal thread,
not a tokio worker. That means: no `await`, no tokio-mutex `lock().await`
directly. Two clean patterns:

1. Pass an `Arc<Mutex<…>>` into the closure and `tokio::spawn` an async
   block from inside the callback — the spawn returns immediately, the
   async work runs on the runtime, and emit-to-frontend stays well-formed.
2. Use a sync `std::sync::Mutex` instead of `tokio::sync::Mutex` for
   small bookkeeping inside the callback.

Pattern (1) is what `watcher::handle_event` uses for the dedup map +
`AppHandle::emit` call. Pattern (2) would be slightly faster but mixes
sync/async primitives in a way that's harder to reason about — every
async caller would need to use `blocking_lock` to coordinate with the
sync callback.

### `onMount` cleanup with async work needs a sync wrapper

Svelte's `onMount` accepts a sync function returning either nothing or
a sync cleanup. An `async () => …` returning a cleanup typechecks as
`Promise<() => void>`, which is *not* the same as the expected
`() => void`. Pattern that works in Svelte 5 + TypeScript:

```ts
onMount(() => {
    let cleanup = () => {};
    (async () => {
        // ... await work ...
        cleanup = () => unlisten();
    })();
    return () => cleanup();
});
```

The IIFE runs as a side-effect; the cleanup closure is captured by
reference and gets the real implementation once the async setup
finishes. If `onMount`'s return runs *before* the IIFE assigns
`cleanup`, the no-op default fires, which is harmless.

### Native TTS over stdin avoids argv-quoting nightmares

`say` (macOS), `espeak`/`spd-say` (Linux), and PowerShell SAPI (Windows)
all read text from stdin when invoked correctly. Piping the text instead
of argv-passing dodges the quoting fan-out that arbitrary chat content
would otherwise require — code blocks, smart quotes, embedded newlines,
single + double quotes side by side, etc. The Windows path uses a
PowerShell one-liner that calls `[Console]::OpenStandardInput().ReadToEnd()`
and feeds the result into the SAPI synthesizer; never embed user text in
the script string itself or you reintroduce the quoting problem.

`AppState.tts_process` holds the spawned `tokio::process::Child`. Both
`tts_speak` (when called while another utterance is in flight) and
`tts_stop` call `Child::kill().await` then `wait().await` to ensure the
synth process is reaped before returning — otherwise rapid speak/stop
cycles leave zombies. The frontend doesn't track utterance lifetime
precisely (the synth runs detached); `ttsSpeaking` flips back to
`false` on a 500ms timer after the spawn returns, which is good enough
to keep the Mute button visible during plausible speech without holding
it forever after a 1-word utterance finishes.

### Two sibling path deps now: CrispEmbed AND CrispASR

The `crispembed` and `crispasr` optional path-deps both live as siblings
of the CrispSorter checkout (`../../CrispEmbed/crispembed` and
`../../CrispASR/crispasr` from `src-tauri/`). Cargo metadata resolves
both at every build, so a missing checkout breaks even default-feature
builds. `release.yml` now checks out both via the `actions/checkout`
sibling pattern under `_sibling/CrispEmbed` and `_sibling/CrispASR`,
and a single Python rewrite step retargets both path deps to the
`_sibling/...` layout. Adding a third sibling dep in the future means
extending that rewrite — keep the regex anchors tight (`\.\./\.\./X/y`)
so unrelated `path = "..."` strings in Cargo.toml aren't accidentally
rewritten.

### `crispembed` optional path dep still needs to resolve
`src-tauri/Cargo.toml` has `crispembed` as an optional dep at path `../../CrispEmbed/crispembed`.
Cargo resolves ALL path deps (even optional ones) during `cargo metadata`, so if the sibling repo
doesn't have the Rust crate, the build fails even when the `crispembed` feature is not enabled.

**Local dev fix:** A minimal stub crate lives at `<crispembed-dir>`.
**CI fix:** The release workflow checks out `CrispStrobe/CrispEmbed` and rewrites the Cargo.toml path.

In CI/release, both repos live inside `$GITHUB_WORKSPACE`. A Python regex rewrites the path dep
from `../../CrispEmbed/crispembed` to `../_sibling/CrispEmbed/crispembed` at build time. This is
**fragile** — if the path format changes in Cargo.toml, the regex silently fails and `cargo metadata`
breaks. Always verify the rewrite output in CI logs.

### Release workflow: CrispEmbed checkout is load-bearing for `npm prebuild`
The `generate-licenses.js` prebuild hook runs `cargo-license --json`, which calls `cargo metadata`.
That resolves ALL path dependencies in Cargo.toml — including the optional `crispembed` dep.
Without the sibling checkout + path rewrite, the build fails at the **npm prebuild** step, not at
Rust compilation. The error message ("failed to resolve patches") doesn't obviously point to
CrispEmbed.

### llama-server sidecar binary naming
Tauri sidecar binaries must be named `{name}-{target_triple}[.exe]` and placed in `src-tauri/bin/`.
The release workflow downloads pre-built llama-server binaries from the ggml-org/llama.cpp
releases and renames them to match. On macOS/Linux, shared libraries (`.dylib`/`.so`) from the
tarball are also copied to `src-tauri/bin/` so they're bundled alongside the sidecar.

### serde `rename_all = "kebab-case"` and digit boundaries — pin with a test
Heck (the kebab-case algorithm serde uses) splits between letter↔digit, but the
exact split is not always intuitive. Examples produced by serde for our enum:
- `BgeSmallEnV15` → `bge-small-en-v15`  *(no dash before `15`; `V` and `15` stay together)*
- `AllMiniLmL6V2` → `all-mini-lm-l6-v2`  *(splits `Mini`+`Lm`, `L6`+`V2`)*
- `EmbeddingGemma300M` → `embedding-gemma300-m`  *(`Gemma300` is one token; trailing `M` splits off)*
- `MxbaiEmbedLargeV1` → `mxbai-embed-large-v1`

The frontend `Settings.svelte::indexEmbedderToRust` hand-writes these strings.
A mismatch silently falls back to `bge-m3` (the `?? 'bge-m3'` default). Always
add new variants to the `embedder_model_serde_strings` test in `embedder.rs`
to lock the wire format the frontend has to match.

### `src-tauri/target` is a symlink to `<external-volume>`
The Cargo build dir is symlinked to an external volume. If that volume is not mounted, `cargo` fails
with "Not a directory". Fix: `rm src-tauri/target && mkdir src-tauri/target`.

`scripts/build.sh` re-establishes the symlink automatically on every run when the volume is mounted
and the symlink is missing or pointing somewhere else. It also falls through to a local
`src-tauri/target/` when the volume isn't mounted (so disconnected-laptop builds Just Work, just
without the disk-saving aspect). Override the destination via the
`CRISPSORTER_TARGET_VOLUME` env var, or set it to `""` to skip the dance entirely.

### Debug binary launch (`cargo run`) needs vite running — white screen otherwise
Tauri 2 reads `devUrl: http://localhost:1420` from `tauri.conf.json` for **debug** builds and
loads the webview from there. Release builds use `frontendDist: "../build"` instead. So:

* **`npm run tauri dev`** is the right dev command — it spawns vite on :1420 and cargo together.
* **`cargo run` alone** points at `:1420` and gets nothing → white screen if vite isn't up.
* **`cargo build --release && ./target/release/tauri-app`** uses the static `build/` and runs
  standalone. Useful for testing the bundled-frontend path without the full `cargo tauri build` cycle.

`scripts/build.sh --release` does the static-frontend + release-binary combo in one shot.

### macOS 13 (Intel) GitHub runner is chronically slow to provision
macOS 13 runners are often queued for 1+ hours on GitHub-hosted Actions. The release workflow now
has a separate `publish` job with `if: always()` that publishes the draft as soon as all *other*
matrix jobs finish — macOS 13 can catch up later or be skipped without blocking the release.

### AppImage requires `APPIMAGE_EXTRACT_AND_RUN=1` on GitHub runners
Linux AppImage tools (`linuxdeploy`, `linuxdeploy-plugin-appimage`, `linuxdeploy-plugin-gtk`) are
themselves AppImages. GitHub Actions ubuntu-24.04 runners have no FUSE, so the AppImage bundling
step fails with the unhelpful error `failed to run linuxdeploy`. Set `APPIMAGE_EXTRACT_AND_RUN=1`
in the workflow to make AppImage tools extract to a temp dir instead of using FUSE. Also useful:
`NO_STRIP=true` to prevent `strip` from failing on unusual binaries. The `.deb` bundle is
unaffected — it doesn't use linuxdeploy.

### Bundling native wrappers (libcrispasr / libcrispembed) into the .app

Five-step recipe that finally got `cargo tauri build --features
crispasr-metal` to produce a self-contained `.app` (PLAN P3.5 phase 1):

1. **Sibling-repo source-of-truth.** `crispasr-sys` (and `crispembed-sys`)
   are direct optional cargo deps with `links = "crispasr"`. Their
   `build.rs` runs cmake on the parent repo (or finds a pre-built tree
   under `../<repo>/build*` — `build-flutter-bundle` matches what
   CrisperWeaver already produces) and emits `cargo:LIB_DIR=<absolute
   path>` so consuming crates' `build.rs` see `DEP_<NAME>_LIB_DIR`.
2. **Direct-dep is mandatory.** Cargo only forwards `links` metadata to
   *immediate* dependents — a transitive path drops the `cargo:LIB_DIR=`
   silently. So `crispasr-sys` has to be in `[dependencies.crispasr-sys]`
   alongside the safe `crispasr` wrapper, even though we don't import
   it from Rust.
3. **rpath has to be emitted from the consumer's build.rs.**
   `cargo:rustc-link-arg=-Wl,-rpath,…` from a transitive lib's build
   script is silently dropped. Read `DEP_CRISPASR_LIB_DIR` in the
   consumer's `build.rs` and emit four entries on macOS: the absolute
   build dir + `<build>/ggml/src` (so `cargo run` works out of the
   workspace), plus `@executable_path/../Frameworks` + `@loader_path/
   ../Frameworks` (so the bundled `.app` resolves them post-bundling).
4. **The .app must be patched after `tauri build` finishes.** Tauri 2
   has no hook between "create .app" and "create .dmg", so post-process:
   copy `lib*.dylib` from `<build>/src/` and recursively from `<build>/
   ggml/src/` (per-backend subdirs `ggml-metal/`, `ggml-blas/`, …) into
   `Contents/Frameworks/` with the SOVERSION-1 symlink alias
   (`libcrispasr.1.dylib → libcrispasr.dylib`); recursively bundle every
   `/opt/homebrew/...` / `/usr/local/...` transitive (espeak-ng pulls
   pcaudiolib on kokoro builds) with their `LC_ID_DYLIB` rewritten to
   `@rpath/<basename>`; **delete every absolute `LC_RPATH` entry from
   libcrispasr** (cmake bakes `/opt/homebrew/Cellar/espeak-ng/...` and
   the build-tree path in, both leak the dev machine and crowd out the
   bundled `@loader_path` lookup) and add `@loader_path/.` as the only
   rpath; ad-hoc codesign; finally `hdiutil create` a fresh .dmg from
   the patched .app and `gh release upload --clobber` the new .dmg
   *and* `.app.tar.gz` over what `tauri-action` already pushed.
5. **Verify with a clean-machine simulation.** `mv build-flutter-bundle
   build-flutter-bundle.HIDDEN; DYLD_PRINT_LIBRARIES=1 …app/Contents/
   MacOS/<bin>` should show *every* dylib loading from `…app/Contents/
   Frameworks/…`. If you see `/opt/homebrew/...` paths in the trace,
   you missed an `LC_RPATH` cleanup or a transitive walk.

The `links` metadata channel is the keystone — without it you have no
clean way to discover the cmake build dir at the consumer's build time,
and `cargo:rustc-link-arg` from the wrong place is a silent no-op. The
debugging path goes: `cargo build -vv | grep "tauri_app .*--crate-type
bin"` → look for `-C link-arg=-Wl,-rpath,...` in the final rustc
invocation, then `otool -l <bin> | grep -A2 LC_RPATH` on the result.

### CrispASR ships per-platform `libcrispasr-{arch}.tar.gz` bundles

Mirroring `llama.cpp`'s release-asset layout: CrispASR's `release.yml`
publishes a tarball per (platform, GGML-backend) combo containing
`libcrispasr.{dylib,so,dll}` + `libggml*.{dylib,so,dll}` + headers under
the same directory shape that `crispasr-sys`'s `try_existing_build`
walks (`<bundle>/src/lib*` + `<bundle>/ggml/src/libggml*`). CrispSorter's
release.yml downloads + extracts the matching tarball into
`_sibling/CrispASR/build-flutter-bundle/` and `crispasr-sys`'s build
script then bypasses its cmake fallback. Saves ~10 min per CrispSorter
release per platform vs. building from source in our own CI; reusable
for CrisperWeaver / Python wrapper / future Node-Go bindings (one
upstream lib build feeds many consumer apps).

---

## Frontend (Svelte 5 + Tauri)

### Svelte 5 rune mode: `$state` inside class fields
`BatchManager` uses `$state<BatchItem[]>([])` as class fields.  This works, but mutations must
happen through direct assignment (`this.items = [...]`) or array mutations (`this.items.push(...)`)
— reactive updates propagate correctly in both cases in Svelte 5.

### `@tauri-apps/plugin-http` fetch supports `AbortSignal`
The Tauri HTTP plugin's `fetch(input, init)` extends `RequestInit`, so `signal: AbortController.signal`
works as expected.  This is NOT documented prominently — it just falls through to the underlying
`reqwest` cancel token.

### `invoke()` calls cannot be cancelled mid-execution
Tauri's `invoke()` is a one-shot IPC call.  Once the Rust handler starts, there is no way to cancel
it from JS.  Use `Promise.race([invokePromise, timeoutPromise, abortPromise])` to make the **JS side**
responsive while the Rust handler runs to completion in the background.

### Stop button only works between async boundaries
`BatchManager.stopRequested` is checked at the top of each loop iteration and after each `await`.
If a single `await` takes a long time (LLM query, Rust extraction), the stop button has no effect
until that `await` resolves.  The fix is to pass `AbortSignal` into the awaited call so it can
terminate early.

### Svelte store subscriptions in rune-mode components
In a `$state`-based (rune-mode) component, subscribe to a Svelte writable store using `$effect`:
```svelte
$effect(() => {
    const unsub = myStore.subscribe(v => { localState = v; });
    return unsub; // cleanup
});
```
Or use `import { get } from 'svelte/store'` for one-shot reads.

---

## Search / FTS

### Model cache directory is configurable, shared across all download paths

Five different code paths download model weights into the same dir:
fastembed native (ONNX), fastembed UserDefined (ONNX), OrtPath (external
ONNX data), CrispEmbed GGUF embedder, CrispEmbed GGUF reranker. The single
`resolve_model_cache_dir(config, data_dir)` helper picks the path in this
order:

1. `CRISPSORTER_MODEL_CACHE_DIR` env var (machine-wide override; useful
   for CI runners and shared multi-user installs).
2. `IndexConfig.model_cache_dir` (Settings UI override; persisted via
   the standard `saveSetting` flow as `indexModelCacheDir`).
3. `{data_dir}/models/` (default).

The directory is created eagerly on resolve so hf-hub / fastembed don't
fail on the very first download. Pointing this at an external volume
(e.g. `<external-volume>/ai/crispsorter-models`) lets the cache survive app
re-installs and be shared with `CrispEmbed` CLI when both use the same
hf-hub layout (`models--<repo>--<sha>/...`). Note: fastembed-rs's own
`fastembed_cache/` layout is *not* the same as hf-hub's, so the cache is
only fully shareable with hf-hub-using tools — fastembed will re-download
into its own subtree under the chosen dir.

**The default eats your boot drive on macOS.** `<data_dir>/models/`
resolves to `~/Library/Application Support/com.crispstrobe.crispsorter/models/`
which lives on the boot volume. Five embedder model variants run ~5-10 GB;
add a few rerankers + ASR models and you're routinely over 15 GB. Users
with tight boot drives should set the cache dir to an external volume on
first run via Settings → Search Index → "Model cache directory". The
canonical user HF cache (`~/.cache/huggingface/hub/` on Linux,
`~/Library/Caches/huggingface/hub/` on macOS — though many users override
to a backup drive via `HF_HOME`) is also a reasonable target if you want
free reuse with Python `transformers` / `huggingface_hub` / CLI tools.

**Symlink trick when downloads already happened in the wrong dir.** The
HF hub layout is content-addressed (`snapshots/<sha>/<file>` referencing
`blobs/<hash>` via hardlink), so an `rsync -a --ignore-existing src/ dst/
&& rm -rf src && ln -s dst src` on each `models--*` subdir is safe — the
rsync is a no-op when both sides have the same blobs (the common case
when both came from the same `hf_hub` download flow), and a merge when
one side is missing blobs (e.g. fresh CrispSorter cache vs years-old
shared user HF cache). Today's session reclaimed ~6.9 GB by symlinking 7
recently-downloaded model dirs to the canonical
`<external-volume>/ai/huggingface-hub/` mirror.

### Matryoshka truncation is GGUF-only and pinned by index schema

`CrispEmbed::set_dim(N)` truncates the model output vector to N dims;
`fastembed-rs` exposes no equivalent hook. So `EmbedderConfig.matryoshka_dim`
is honored only when the active backend is `EmbedderBackend::Gguf` — ONNX
paths silently fall back to the model's nominal dim regardless of the
config. The Settings UI gate (`indexEmbedderBackend === 'gguf'`) keeps
the field invisible elsewhere so users don't set it expecting an ONNX
effect.

The LanceDB `embedding` column is `FixedSizeList<Float32>[N]` — a single
fixed width per index. `EmbedderConfig::effective_dim()` is the source of
truth: it clamps `matryoshka_dim` to `model.dims()` (so a too-large value
is silently corrected) and treats `Some(0)` as `None` (model default,
common UI sentinel). `tauri_commands::init_index` reads it once and
passes to both `Embedder::new` *and* `LocalIndex::open_or_create` so the
two stay in lockstep.

Changing `matryoshka_dim` on an existing index is a schema-incompatible
migration: LanceDB cannot resize a `FixedSizeList` column in place. The
UI hint warns about re-ingestion; the runtime would fail at write time
with a column-width mismatch otherwise. Quality also depends on the
model — only MRL-trained models (BGE-M3, Snowflake Arctic L v2,
PIXIE-Rune) preserve relative similarity at smaller dims; non-MRL models
will silently produce poorer embeddings.

### Sparse retrieval is a 3rd RRF channel, not a primary modality

LanceDB has no native sparse-vector ANN, and an inverted index over
`embedding_sparse` would duplicate work Tantivy already does for term
matching. Instead `LocalIndex::search_sparse_in_pool` scores the union of
FTS+ANN candidates by sparse dot product against the query's sparse vector,
and `SearchEngine::search_hybrid` fuses the result as a third RRF channel
via the generalized `rrf_merge_n`. Trade-offs:

- **No corpus-wide sparse retrieval** — chunks that didn't show up in
  either FTS or dense ANN can't be promoted by sparse alone. For an
  academic-doc corpus where dense and sparse usually agree on candidate
  selection, this is a feature: it stops sparse from amplifying lexical
  noise. For a true SPLADE-as-primary-modality use case, a separate
  inverted index is needed.
- **Two-pointer merge when both inputs are sorted** (the common case for
  BGE-M3 / SPLADE outputs) — O(|a|+|b|), zero allocation. Hash-join
  fallback for unsorted inputs. `is_sorted_ascending` is the gate.
- **Sparse ingestion path was already wired** before this — every chunk
  has an `embedding_sparse` JSON column when the active embedder has a
  sparse head. The new query-side retrieval just *reads* that column.

### Cross-encoder reranking: lazy-load handle + NaN-safe fallback
Rerankers double the search-side memory (separate `crispembed::CrispEmbed`
instance with its own GGUF) and add ~100–500ms of first-query latency for
the model load + download. Two patterns in `index/reranker.rs` keep this
manageable:

1. **`RerankerHandle`** is the only thing `SearchEngine` carries — a
   `Clone`-able struct with `(model, cache_dir, Arc<Mutex<Option<Reranker>>>)`.
   First `score_batch` call performs the load (HF download via `hf-hub`,
   GGUF open via `crispembed::CrispEmbed::new`, `is_reranker()` sanity
   check); subsequent calls reuse the cached `Reranker`. Users who never
   issue a query don't pay the load cost.
2. **NaN-safe fallback**: `RerankerHandle::score_batch` returns
   `vec![f32::NAN; docs.len()]` on load failure, and `Reranker::score`
   returns `f32::NAN` if the underlying CrispEmbed call fails. The
   `SearchEngine::maybe_rerank` sort treats NaN entries as "stay in original
   RRF order at the back of the list" — a misconfigured reranker degrades
   to plain RRF ranking instead of erroring the entire query.

The cross-encoder scores are *raw logits* (not probabilities), so absolute
score values aren't comparable across reranker models. Only the relative
ordering matters — don't expose the score in UI as a confidence percentage.

Bi-encoder reranking (`CrispEmbed::rerank_biencoder`) is intentionally not
wired in: it's just better dense retrieval over the candidate set, which the
existing dense ANN already does. Cross-encoders are the only path that adds
real signal beyond what we already have.

### `rerank_top_n` interacts with the inner `*2` RRF over-fetch
`search_hybrid` already over-fetches by 2x on each side (`limit*2`) so RRF
has slack. When reranking, the over-fetch is `inner_limit*2` where
`inner_limit = max(rerank_top_n, limit)`. With default `top_n=50`, that's
100 candidates per side hitting Tantivy/Lance — fine for the typical
academic corpus, but worth profiling before raising `top_n` over 100.

### Tantivy ASCII-folding must happen on both sides
Query-side folding alone (via `fts_query::fold_accents`) is not enough — the
indexed terms also have to be folded, otherwise `München` (indexed as
`münchen`, since the `default` tokenizer only lowercases) never matches a
folded query `munchen`. The fix is a custom tokenizer
`ascii_folding = SimpleTokenizer + RemoveLong(40) + LowerCaser + AsciiFoldingFilter`
registered on the index in `register_tokenizers()` and used by the schema for
`title`, `headings`, `body`. The query-side `fold_accents` (`deunicode +
to_lowercase`) covers substitutions Tantivy's `AsciiFoldingFilter` does not
(e.g. `ø` → `o`), so keep both.

**Breaking change for existing FTS dirs**: indexes written before this fix
have `münchen`-flavored terms that won't match queries passing through the
new tokenizer. Re-ingestion is required — there is no in-place migration
because Tantivy stores tokens, not raw text. Document this when bumping the
release that ships the fix.

---

## Embedder / Model registry

### Rust enum match arms and non-existent variants

In a `match` on a Rust enum, if you write a variant name that doesn't exist in the enum, Rust treats it as a **variable binding** (catches everything), not a compile error for a missing variant. Combined with `|` (or) patterns this produces E0408 "variable not bound in all patterns" — confusing unless you know the root cause.

Example that fails:
```rust
fn gguf_registry_name(&self) -> Option<&'static str> {
    use EmbedderModel::*;
    Some(match self {
        // BgeSmallEnV15 doesn't exist in the enum yet — Rust binds it as a variable!
        BgeSmallEnV15 | BgeSmallEnV15Q => "bge-small-en-v1.5",
        _ => return None,
    })
}
```

**Rule**: Never add match arms for model variants that don't exist in the enum yet. The wildcard `_ => return None` handles them. Add enum variants first, then add match arms.

### `GGUF_CAPABLE_MODELS` must mirror the `EmbedderModel` enum

The `GGUF_CAPABLE_MODELS` set in `Settings.svelte` gates the ONNX/GGUF backend toggle in the UI. Entries that don't correspond to actual `<option>` values in the model dropdown are dead weight — they'll never match. Keep the set in sync with models that have both:
1. An `EmbedderModel` enum variant (Rust side)
2. An `<option>` in the Settings dropdown (Svelte side)

### OrtPath vs Fastembed backend selection

Two conditions force the OrtPath backend (bypassing fastembed's `UserDefined`):
1. **External ONNX data** — `.onnx_data` companion files that ORT must resolve by relative path (loading from bytes breaks this).
2. **No config.json** — fastembed's `UserDefinedEmbeddingModel` requires a `config.json`; repos like Octen don't have one.

Models with KV-cache (Qwen3-Embedding) also use OrtPath because they need custom input tensor construction (empty `past_key_values` tensors).

### Decoder model pooling strategies

Different decoder-based embedding models use different pooling:
- **Qwen3-Embedding (KV-cache ONNX)**: last-token pooling on the EOS position. Empty KV-cache tensors `[batch, kv_heads, 0, head_dim]` are passed — ndarray supports zero-sized dims but ort's raw-data path does not, so use `ndarray::Array4::zeros()`.
- **Octen-0.6B (no KV-cache ONNX)**: also last-token pooling, but the ONNX export has no `past_key_values` inputs — set `force_last_token_pool()`.
- **electroglyph uint8 export**: pre-pooled uint8 output requires dequantization: `f32 = (u8 - zero_point) * scale`.

### Snowflake Arctic L v2.0 ONNX variants

Same `Snowflake/snowflake-arctic-embed-l-v2.0` HF repo, all under `onnx/`:
- `model_quantized.onnx` (INT8, default — smallest practical)
- `model_int8.onnx` (INT8 via different quantization)
- `model_fp16.onnx`, `model_q4.onnx`, `model_q4f16.onnx`, `model_O4.onnx`
- `model.onnx` + `model.onnx_data` (FP32 reference, ~1.7 GB, needs OrtPath)

Only the FP32 variant has external data — the quantized ones are self-contained and can use the fastembed UserDefined path.

### Asymmetric retrieval prefixes are model-specific and silent on mistakes

Different model families train with different (or no) prefixes. The mapping
lives in `EmbedderModel::prefix(EmbedRole)`:

| Model family | Query prefix | Passage prefix |
|---|---|---|
| E5 (multilingual + English) | `query: ` | `passage: ` |
| Nomic v1.5 | `search_query: ` | `search_document: ` |
| BGE en-v1.5 + Mxbai | `Represent this sentence for searching relevant passages: ` | (none) |
| Jina v5 (Small + Nano) | `Query: ` | `Document: ` |
| EmbeddingGemma 300M | `task: search result \| query: ` | `title: none \| text: ` |
| BGE-M3, Qwen3, Octen, PIXIE-Rune, Snowflake Arctic-L v2, GTE en-v1.5, Jina v2/v3, MiniLM, BERT bases | (none) | (none) |

A wrong prefix degrades retrieval quality silently — vectors stay valid but
score against the wrong manifold. When in doubt, default to no prefix; it's
a smaller hit than picking the wrong one. The `prefix_table` test in
`embedder.rs` pins the mapping so changes are explicit.

The CrispEmbed (GGUF) backend has a native `set_prefix` that applies the
prefix inside tokenization (no max-token competition with chunk text). The
fastembed and OrtPath backends prepend in Rust — so a long prefix (BGE's is
~14 tokens) does eat into the chunker's `max_tokens` budget; truncation
silently drops the chunk tail. Acceptable for typical 256–512-token chunks
but worth re-checking for very small windows.

Sparse models (BGE-M3, SPLADE++) are trained without prefixes — `embed_sparse`
passes texts through unprefixed.

### Adding a new model to CrispSorter — checklist

1. Add variant to `EmbedderModel` enum in `embedder.rs`.
2. Add `display_name()`, `dims()`, `max_tokens()` match arms.
3. Add `to_model_spec()` with HF repo + ONNX filename + any special config — or return `None` if it's a native fastembed model.
4. Update `is_native()` (native fastembed) and `to_fastembed_dense()` / `to_fastembed_sparse()` mappings as applicable.
5. If GGUF equivalent exists: add match arm to `gguf_registry_name()`. The decoder vs encoder branch in `to_gguf_spec()` already picks the right `<name>-q8_0.gguf` vs `<name>.gguf` filename.
6. Add the new variant to the `embedder_model_serde_strings` test so the kebab-case wire format is locked.
7. Add `<option value="...">` to the model dropdown in `Settings.svelte`. If GGUF-capable, add the option value to `GGUF_CAPABLE_MODELS`. Add the entry to `indexEmbedderToRust` matching the kebab-case from the test.
8. Add i18n labels (`model_*`) in `src/lib/i18n.svelte.ts` for both `en` and `de` blocks.

---

## Processing Pipeline

### Session resume leaves items in mid-flight statuses
When the app closes mid-batch and resumes, items can be stuck in `extracting` or `analyzing`.
These statuses mean "actively running" to the UI, which is wrong after a restart.
**Pattern:** Always sanitize loaded session items: reset `extracting`/`analyzing` to a safe
"interrupted" status in `resumeLastSession()`.

### Extract-then-analyze is the right two-phase approach
Running extraction and LLM analysis interleaved (per item) means a rate-limited LLM also blocks
all extraction. Better: extract ALL items in pass 1, then analyze ALL in pass 2. Both passes
respect `stopRequested` and can be run independently.

### Per-page watchdog is better than flat timeout for extraction
A 5-minute flat timeout is too generous for small files and too tight for large scanned PDFs.
The right signal is "no page progress in N seconds" — wire the `onProgress` callback to a
watchdog timer that resets on every page event and fires after 30 s of silence.

---

## Rate Limits

### Remote provider 429s can cascade
Rate limits on Groq, OpenRouter, etc. cause the retry loop to eat into `MAX_RL_RETRIES` (currently 6)
before giving up.  During a large batch, this can mean many minutes of dead time.
**Better:** Detect 429 early and switch to a fallback provider (round-robin) rather than retrying
the same one.  Reset the provider index at the start of each `processAll`.

---

## Release

### Version must be bumped in three places
- `package.json` → `"version"`
- `src-tauri/Cargo.toml` → `version = "..."`
- `src-tauri/tauri.conf.json` → `"version"`

All three must match or the Tauri build will fail / produce inconsistent binaries.

### `releaseDraft: true` + separate publish job is the right pattern
`tauri-apps/tauri-action` uploads artifacts to the draft release as each platform job completes.
A separate `publish` job with `needs: [release]` and `if: always()` converts the draft to live
once all matrix jobs have settled — regardless of individual failures.

## Catalog (.caf)

### `.caf` is per-file metadata + dir tree + volume header — not just filenames

Easy to under-describe `.caf` because the user-visible payoff is "filename
search across drives." But the on-disk format (Cathy 1.x → Catfish v8)
carries materially more, and our reader parses every field. What we *use*
is a different question.

What `.caf` actually contains (per `src-tauri/src/catalog/caf.rs::read_file`):

* **Volume header** — device path, volume label, alias, serial number,
  comment (v ≥ 4), free space (v ≥ 1), archive flag (v ≥ 6), creation date.
* **Directory tree** — per-directory aggregates `(file_count, total_size)`
  for v ≥ 3, names, parent pointers.
* **Per-file ELM entries** — `(mtime, size, parent_id, name)` with version-
  dependent struct widths:
  * v ≤ 6: size as `<i32>`, parent_id as `<u16>`
  * v = 7:  size as `<i64>`, parent_id as `<u16>`
  * v = 8:  size as `<i64>`, parent_id as `<u32>` (current Catfish writes this)
* `size < 0` encodes a directory: dir ID = `-size` (v > 6) or 1-based
  positional index (v ≤ 6 quirk).
* **Hashes are NOT stored** — Cathy/Catfish design choice; we recompute
  on demand for dedup.

What we *keep* (per `catalog/index.rs::FileEntry` + `catalog/lance.rs::build_schema`):

| Field | Parsed | Surfaced to `FileIndex` | Lance `catalog_entries` row |
|---|---|---|---|
| path (reconstructed from dir tree) | ✓ | ✓ | ✓ as `entry_path` |
| size (per-file bytes) | ✓ | ✓ | ✓ as `Int64` |
| mtime (unix seconds) | ✓ | ✓ | ✓ as `Int64` |
| filename | denormalized | implicit | ✓ |
| hash (computed locally, not from .caf) | n/a | ✓ | ✓ nullable |
| device / root path | ✓ | ✓ as `root_path` | implicit via `catalog_path` |
| is_windows_path heuristic | ✓ | ✓ | — |
| volume label / alias / serial / comment | ✓ in `CafMetadata` | ✓ since v0.1.36 follow-up | — (still UI-only) |
| free space / archive flag / creation date | ✓ in `CafMetadata` | ✓ since v0.1.36 follow-up | — (still UI-only) |
| dir aggregates (file_count / total_size per dir) | ✗ skipped (`skip(12)`) | ✗ | — |

The dir aggregates are pre-computed values that would let us render a
folder tree without iterating all entries. Skipping them is a deliberate
simplification; revisit if the Catalog UI ever wants per-folder size
breakdowns without a full scan.

### Round-trip is semantic, not byte-identical

The `round_trip_v8_preserves_files` test asserts `(file_name, size, mtime)`
tuples match across a load → save → load cycle. It does NOT assert
byte-for-byte equality of the `.caf` output, and earlier docs that
implied "bit-identical round-trip" were too strong. Byte equality
would fail because:

1. We always **write v8**, even when reading v1–v7. Deliberate — bumps
   format and avoids version-specific writer code paths.
2. Volume label / alias / serial / comment / freesize / archive flag
   are parsed into `CafMetadata` but (until the v0.1.36 follow-up) were
   discarded by `FileIndex`, so a load → save → load cycle dropped them.
3. Hashes never round-trip (the format never carried them).

Practically, this means: re-saving a Cathy v6 catalog from 2008 emits a
v8 file readable by Catfish (Cathy's Python successor) but not by the
original Cathy.exe binary that produced it. Acceptable trade — Catfish
is our canonical reference.

### v ≤ 6 zero-size clamp matches Catfish behaviour

`caf.rs:421-429` substitutes `1024` bytes for any v ≤ 6 entry whose
`<i32>` size field is exactly 0, and clamps v > 6 zero-size entries to
`1` byte. Both clamps exist so the `size_index` bucket lookup works (it
keys on size). The v ≤ 6 fallback of 1024 specifically matches what
Catfish does — older Cathy.exe versions wrote 0 for indeterminate sizes
on Win9x. The downside: a genuine zero-byte file in a v ≤ 6 catalog
will report as 1024 bytes here. Real zero-byte files in v ≥ 7 catalogs
report as 1 byte instead of 0. Both are documented quirks, not bugs.

---

## Cloud drives (P11 Pillar 5)

### WebDAV servers post-check `exists()` after DELETE — your provider must invalidate caches

wsgidav's `do_DELETE` calls `child_res.delete()`, then immediately
re-asks `provider.exists(path, environ)`.  If the answer is still
`True`, wsgidav raises `DAVError(HTTP_INTERNAL_ERROR,
"Resource could not be deleted.")` — even though your underlying
storage call succeeded.  This bit filen-python (10-minute folder/file
listing cache wasn't cleared on `trash_item`/`delete_permanent`) and
will bite any provider whose `exists()` reads through a TTL cache that
mutations don't invalidate.  Whenever you write a custom WebDAV
provider on top of an API with cached listings: invalidate the parent
folder's cache (or the path-resolution cache) on every mutation, not
just the entry being deleted.

Symptom to watch for during e2e: `curl -X DELETE` returns 500 with the
exact string "Resource could not be deleted" and the file remains
visible in `PROPFIND` immediately after.  The fix is always at the
provider's `exists()`/`get_resource_inst()` cache, not the `delete()`
implementation.

### Internxt `cli.py` and Filen `cli.py` need `--json` patches to be useful from Rust

Both Python CLIs default to emoji-decorated text output.  Scraping that
from Rust is brittle (kaomojis, table widths, decorations vary across
versions).  The right move is to upstream a `--json` flag on the
read-side commands (`whoami`, `ls`/`list-path`, `resolve`, `trash`)
and parse with `serde_json`.  Patches live in:

* `internxt-python/995a543` — `whoami` / `list-path` / `resolve`
* `filen-python/1162aa0`    — `whoami` / `ls` / `resolve` / `trash`
                              (also added the missing `handle_trash`)

Both CLIs use argparse-style flag-before-subcommand vs flag-after-
subcommand semantics; the patches expose `--json` *both* on the parent
parser and on the patched subparsers so either invocation form works.

### `unittest.mock.patch` is not thread-safe

If two threads each enter their own `with patch(...)` block,
`__enter__` saves and `__exit__` restores the original attribute with
no locking.  Races between t1's `__exit__` and t2's `__enter__` (or
vice versa) leave the real attribute callable for a brief window —
which on CI without credentials triggers exceptions in unrelated
threads, leaving asserted dict slots empty and the failure message
surfacing as a generic `KeyError` instead of the real cause.

When testing thread-locality of state, hoist all `patch()` calls
*outside* the threaded function so a single mock surface wraps the
whole join window.  The threads never see an unpatched value.  Pattern
in `tests/test_webdav_misc.py:test_isolated_session_separate_threads…`
(internxt-python/`9128c3d`).

### `# type: ignore[<code>, unused-ignore]` for cross-platform mypy ignores

Linux's `os.stat_result` stub doesn't expose `st_birthtime` (macOS-only).
Bare `# type: ignore[attr-defined]` is needed on Linux but flagged
"Unused" on macOS.  Mypy's own `unused-ignore` code handles the
meta-warning: `# type: ignore[attr-defined, unused-ignore]` works on
both platforms.  Same trick for libraries with conditional stub
availability (e.g. `waitress`, `cheroot`):
`# type: ignore[import-untyped, unused-ignore]`.  Avoids platform-
specific `if sys.platform == ...:` guards in source code and avoids a
Linux-only mypy CI lane.

---

## Performance patterns (P28)

### `to_uppercase()` for operator detection is a hidden allocation

`token.to_uppercase() == "AND"` allocates a new String per token.
`token.eq_ignore_ascii_case("AND")` is zero-alloc and handles the same
cases.  Found in `fts_query.rs`, `synonyms.rs`, and `fuzzify_query`.

### Byte-slicing ASCII prefixes on UTF-8 strings panics

`word[..2].eq_ignore_ascii_case("W/")` panics when `word` starts with
a multi-byte char (e.g. `ü` is 2 bytes in UTF-8, so `word[..2]` lands
inside the char).  Guard with `word.is_ascii()` first, since `W/` and
`PRE/` are inherently ASCII.

### `chars().take(N).collect::<String>()` is an allocation you usually don't need

For LID sampling, snippet generation, etc. where the consumer just
reads the slice — use `char_indices().nth(N)` to find the byte boundary
and slice the original `&str`.  The `String` collect allocates a copy
on the heap that's immediately discarded after `.trim()` / `.len()`.

### `Vec::with_capacity` matters for Arrow RecordBatch iteration

Functions that iterate `Vec<RecordBatch>` and push results should
pre-compute `batches.iter().map(|b| b.num_rows()).sum()` and pass it
to `Vec::with_capacity`.  Without it, the Vec re-allocates 3–5 times
for a typical 1000-row result set.

---

## Local index & search

### `chunk_index = 0` is required for local search visibility

The local LanceDB query helpers in
`src-tauri/src/index/local_index.rs` filter their representative-row
SELECT with `chunk_index = 0` — see `fetch_by_doc_ids` and
`fetch_by_doc_ids_filtered`.  The comment block at the top of
`schema.rs` says "L1 metadata + L3 representative rows use
`chunk_index <= 0`", which is *almost* right — but the actual
filter is `= 0`, not `<= 0`.  So a row ingested with
`chunk_index = -1` lives in LanceDB, gets indexed by Tantivy,
returns hits at the FTS layer — and then vanishes in the
metadata-join phase of `crispsorter index search`, producing the
mysterious "0 result(s)" with a non-empty FTS hit set.

**Rule:** any DocumentChunk that needs to be findable via
`crispsorter index search` (or browseable in Übersicht) must use
`chunk_index: 0, chunk_total: 1`.  Reserve `-1` only for rows that
are *intentionally* invisible to local search (none today; the
original use case turned out to be a foot-gun).

Got bit by this during the v107 follow-on work for L1-aware local
search: pulled cb-api rows ingested with `chunk_index = -1` (matching
the "manifest-only" convention).  Tantivy reported `num_docs = 30`;
`crispsorter index search` returned 0 of them.  Switched the pull
path to `chunk_index = 0, chunk_total = 1` and search worked
immediately.  See `cli/mod.rs` around the
`CloudBackupCmd::Pull` handler for the live example.

### `cargo build` on this workspace takes 3–9 min cold, 1–3 min warm

The default Rust dev build of `crispsorter` pulls in a non-trivial
chunk of the Tauri + LanceDB + Tantivy + Arrow stack.  Plan around
it when iterating on something that requires a recompile to test
manually — e.g. don't do six small edits and rebuild between each;
batch the edits, rebuild once.  Use `cargo check --package
crispsorter` (~30 s warm) to catch type errors mid-edit; only build
the binary when you actually need to exercise behavior.

### `crispsorter sync cloud-backup pull` writes BOTH LanceDB and Tantivy

The Pull command's chunk-ingest path is two-write: each pulled L1
chunk goes into the local LanceDB documents table via
`local.ingest_batch`, AND each chunk with `full_text` populated
gets added to the local Tantivy FTS via a delete-then-add by doc_id.
Soft-fails on the Tantivy side so a write-protected `fts/` dir
doesn't break the pull.  Emits `[sync] indexed N L1 row(s) into
local Tantivy` on success.

Without the Tantivy write, pulled rows would live in LanceDB but
not be findable via `crispsorter index search`, defeating the
offline-search story.  See `cli/mod.rs` around the
`CloudBackupCmd::Pull` arm.

### `#[serde(default = "…")]` does NOT feed `#[derive(Default)]`

`PageSpec` carries `#[serde(default = "default_page_limit")]` (→ 200)
on its `limit` field, but `PageSpec::default()` still yields
`limit: 0` — the serde attribute only supplies a value when a field is
*absent during deserialization*; Rust's derived `Default` ignores it
entirely and uses `u32::default()`.  `query_documents` then clamps
`limit` to `[1, 1000]`, so a `PageSpec::default()` page returns exactly
**one** row.

This burned ~two 19-minute build cycles chasing a phantom
"`array_has` drops rows from the tag-filtered browse" bug: `count_rows`
(no limit) reported the correct count while the scanner page returned a
single row.  `array_has` + the `lance::Scanner` filter/order/limit
pipeline were fine all along — the test built its page via
`PageSpec::default()`.  The real frontend always sends
`page: { limit: 200, … }`, so the app path was never affected.

**Rule:** in tests/back-end callers, construct `PageSpec { limit: N,
cursor: None }` explicitly; never trust `PageSpec::default()` to carry
the serde default.  (Same trap applies to any struct that mixes
`#[serde(default = "…")]` with `#[derive(Default)]`.)  If a single-row
result smells wrong, check the page limit before suspecting the query
engine.

### `#[serde(default)]` does NOT cover an explicit `null` on the wire

`#[serde(default)]` supplies a value only when a key is **absent**.  When
the key is present with an explicit `null`, serde still tries to
deserialize `null` into the target type — and for a non-`Option` like
`Vec<String>` that **fails the whole struct**.  cb-api's Pydantic models
emit `Optional[List[str]]` → `"tags": null` for a row with no tags, so
`cloud_backup::SearchHit { #[serde(default)] tags: Vec<String> }` parsed
fine for tagged rows but blew up the *entire* federated response the
moment one tagless row appeared ("search: parse body").  Local-only and
mock tests never caught it — only a live search over the wallabag corpus
(one untagged article) tripped it.

**Fix:** a tolerant deserializer that maps `null`/absent → default:
```rust
fn de_tags_null_as_empty<'de, D>(de: D) -> Result<Vec<String>, D::Error>
where D: serde::Deserializer<'de> {
    Ok(Option::<Vec<String>>::deserialize(de)?.unwrap_or_default())
}
// #[serde(default, deserialize_with = "de_tags_null_as_empty")]
```
Apply it to every non-`Option` field that a `null`-emitting backend can
send (here: `ManifestRow`/`PullRow`/`SearchHit::tags`).  Rule of thumb:
if the producer's schema says `Optional[...]` but the Rust side isn't
`Option<...>`, you need this — `#[serde(default)]` alone is a latent
"one bad row poisons the batch" bug.

### The crispsorter target dir can live on a slow external volume

On this machine the Cargo target dir resolves to
`<external-volume>/code/crispsorter-target` — a cold `cargo test`
build of the `crispsorter` lib took **~19 min**.  Budget for it: batch
all edits, run ONE build, and make probe tests *comprehensive* (test
every hypothesis in a single binary) rather than iterating one
spelling/stage per build.  `cargo check` is still the fast
inner-loop for type errors.

### Bucket bulk corpora across `collection_id` — never one giant collection

The cb-api backend shards its Lance index by `collection_id` (a collection
routes to a single shard for topical locality, falling back to a content-hash
prefix when unset).  That means a **5-figure corpus pushed under one
`collection_id` collapses onto one hotspot shard** — a multi-minute first FTS
build, no fanout parallelism.  Bucket it instead: `SyncManager`'s `partition.rs`
(Stage N) already assigns volume-proportional `<root>/<group>/<k>`
sub-collection-ids so a corpus spreads across many shards.  Route bulk ingests
through that path; don't hand-set a single `ManifestRow.collection_id` for a
large set.  Wire-wise nothing changes — `collection_id` flows through
`ManifestRow`/`PullRow`/`HybridSearchHit` either way, and the server's search
fanout is topology-aware (skips empty shards, narrows to the queried
collection's shards) so a well-bucketed corpus searches fast.  Server-side
detail lives in the cloud-backup repo.

**Scope federated search to LEAF bucket ids for the fast server path.** The
cb-api resolves an *exact* (leaf) `collection_id` like `<root>/<k>` straight to
its one shard with no index scan; a bare *parent* id like `<root>` is correct
but forces the server to scan every shard's collection set to find the buckets
(a multi-second cost over network storage at scale).  So when the federated
client (`sync cloud-backup hybrid-search --collection-ids …` /
`SyncManager`'s search push-down) knows the concrete `<root>/<k>` ids a corpus
was bucketed into, pass those rather than the parent — same results, far less
server latency.  The leaf ids are exactly what `partition.rs` emits at ingest,
so they're already available client-side.  (Server fix: cloud-backup
`target_prefixes` deterministic leaf-routing, 2026-05-31.)

**FTS freshness (server-side, for consistency):** the cb-api search index is a
LanceDB FTS index. A LanceDB FTS index does **not** auto-update on writes, and a
**stale** index (e.g. after a compaction that rewrites data fragments) silently
degrades to a full-table-scan per query — the single biggest server-side search
footgun. The cb-api rebuilds the FTS index after compaction/bulk-ingest; if
federated search ever feels suddenly slow, a stale FTS index is the first
suspect (server-side detail in the cloud-backup repo). Storage placement of the
index is *not* the lever — a live inverted index reads only small postings.
