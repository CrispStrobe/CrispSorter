# CrispSorter v0.2.0

## Highlights

**Document translation arrives.** A new **Translate** tab in the main nav drives an end-to-end `.docx` → `.docx` translation flow against any of 13 LLM backends (12 cloud + 1 offline NMT). Paragraph styles, sections, bookmarks, and footnote references are preserved at the OOXML level; intra-paragraph bold / italic spans survive translation via an opt-in alignment encoder.

**OS-keychain credential storage.** API keys for every LLM provider now live in the platform's native credential vault (macOS Keychain / Windows Credential Manager / Linux Secret Service) instead of `settings.json`. Existing keys migrate transparently on first launch.

---

## Translate tab

### Cloud LLM providers (12)

OpenAI, Anthropic, Groq, OpenRouter, Together, Cerebras, Mistral, Nebius, Scaleway, Poe, Google (Gemini), and any other OpenAI-compatible host via a custom base URL.

### Offline / local

- **Ollama** — HTTP server, no API key
- **CrispASR NMT** — fully offline GGUF backends (m2m100 / wmt21 / madlad / gemma4-e2b). No network calls; loads a GGUF file from disk

### Format preservation (opt-in)

A checkbox in the Translate tab enables **alignment-driven format reconstruction**: per paragraph, source and translated words are matched via [CrispEmbed](https://github.com/CrispStrobe/CrispEmbed)'s multilingual encoder, then each source run's bold / italic / rStyle is redistributed onto the matching target word range. Word-order reordering survives — the German "schläft" correctly inherits the bold from the English "sleeping" even though it's in a different position.

Verified live on the Vielfalt cs15.docx test corpus: 64 italic + 44 bold runs in the source, all carried through into the German translation.

### UX

- Native file pickers for input `.docx` and output path
- Auto-suggested output filename (`<stem>.<target-lang>.docx`)
- Live preview of extracted paragraphs before any tokens are spent
- Provider key status pill — `🔑 ready` (green) vs `no key` (amber)
- Progress bar with elapsed seconds, paragraphs-per-second, and projected ETA
- Form state persists across launches (provider, model, language pair, concurrency, format-preservation, alignment-model path)
- Pre-submit validation blocks at the form gate rather than mid-translation

---

## OS-keychain secret storage

API keys for any LLM provider now use the platform's native secret vault under the service name `CrispSorter.LLM`:

- macOS: visible in Keychain Access under `login`
- Windows: visible in Credential Manager → Generic Credentials
- Linux: visible in `seahorse` / `kwalletmanager`

The Settings tab transparently migrates pre-existing plaintext keys on first load, replacing them with `@keyring/llm-provider:<id>` sentinels in `settings.json`. New keys typed into the form migrate on save. The `LLMClient` resolves sentinels back to real values via `resolveSecret` so every call site (Chat, BatchReview, benchmarking, Translate) is automatically safe.

---

## What ships in this release

**macOS arm64 (Apple Silicon)** — full feature set:
- All v0.1.x features (Stapel, Chat, Catalog, search, audio + image + translation indexing, etc.)
- Translate tab with all 12 cloud providers + Ollama + offline NMT (CrispASR m2m100/wmt21)
- Intra-paragraph format preservation via CrispEmbed
- OS keychain credential storage

**Linux x86_64** — same as macOS, in `.deb` form.

**Windows** — feature-less in this release (DLL co-bundling for crispembed / crispasr deferred; tracked in PLAN.md). Translation is functional via cloud providers; offline NMT and format preservation are not enabled.

---

## Under the hood

The translator pipeline is a new sibling workspace, [`crisp-docx`](https://github.com/CrispStrobe/crisp-docx) — a 6-crate Rust workspace covering OOXML primitives, LLM HTTP clients, SimAlign-driven word alignment, and an end-to-end CLI. CrispSorter consumes it as path deps; the same crates can be `cargo install`-ed standalone.

Three Python bugs in the predecessor [`CrispTranslator`](https://github.com/CrispStrobe/CrispTranslator) were found and fixed upstream via the Rust port: spurious nested-bold from pandoc's RTF reader; `cmd_check`'s bookmark allow-list / `_rels/.rels` base-path resolution / optional `settings.xml`.

---

## Upgrading

Drop-in. Settings, batch sessions, catalogs, and indexes all migrate. The keychain migration happens on first launch of the Settings tab — no user action required.

Anything else in your install (custom prompts, watch folders, cloud-drive registrations, etc.) is untouched.

---

## Acknowledgements

The translation pipeline draws on three sibling projects in the Crisp ecosystem:

- [CrispEmbed](https://github.com/CrispStrobe/CrispEmbed) for the multilingual encoder powering SimAlign-style word alignment
- [CrispASR](https://github.com/CrispStrobe/CrispASR) for the offline NMT backends (m2m100 / wmt21 / madlad)
- [CrispTranslator](https://github.com/CrispStrobe/CrispTranslator) — Python ancestor whose OOXML primitives the new Rust workspace is a port of
