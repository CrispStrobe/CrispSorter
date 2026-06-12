# CrispSorter v0.5.0

## Highlights

**CrispEmbed deep integration.** Seven new modules wire every major capability
of the [CrispEmbed](https://github.com/CrispStrobe/CrispEmbed) inference engine
into CrispSorter. All features are gated behind `--features crispembed`
(with Metal/Vulkan/CUDA sub-features for GPU acceleration) and degrade gracefully
when the feature is off.

**Four-tier OCR.** A new Tier 4 tops the OCR ladder: Surya-OCR-2 (91 languages)
for text detection + Qwen2.5-VL (with German support) for recognition, all via
pure GGUF models — no ORT or Tesseract dependency needed.

**Cross-modal search.** BidirLM-Omni encodes text, audio, and images into a
shared 2048-D embedding space. Type "photo of a sunset" and get image hits
without OCR. Type "podcast about Bosnia" and get audio hits without
transcription.

**Math OCR.** Detect formula regions via RT-DETRv2 layout detection, then
recognize each to LaTeX via PP-FormulaNet-L (printed) or PosFormer
(handwritten). Formulas are injected into the indexed text wrapped in `$...$`
delimiters.

---

## New modules

### P17.1 — Layout-aware PDF extraction

`src-tauri/src/extractors/layout.rs`

RT-DETRv2 document layout detection identifies 17 region types on a page image:
text, title, table, figure, formula, header, footer, caption, reference, etc.
Used as a pre-pass before OCR to route each region to the right engine:

- Text/title/caption regions -> text OCR
- Formula regions -> math OCR (LaTeX output)
- Figure/table regions -> skip or structured extraction

Regions are returned in reading order (top-to-bottom, left-to-right).

### P17.2 — CrispEmbed OCR (Tier 4)

`src-tauri/src/extractors/ocr_crispembed.rs`

New highest-priority OCR tier in the dispatch ladder. When `--features
crispembed` is active:

- **Detection**: Surya-OCR-2 (EfficientViT, 91 languages)
- **Recognition**: Qwen2.5-VL (German support) or DBNet+TrOCR (lightweight)
- All models are GGUF — no ORT, no Tesseract, no PaddleOCR needed
- Auto-downloads from HuggingFace on first use
- Falls through to Tier 3/2/1 on failure

### P17.3 — Math formula OCR

`src-tauri/src/extractors/math_ocr.rs`

Recognizes printed and handwritten mathematical formulas:

- **PP-FormulaNet-L** (printed, 181M params, BLEU 0.90)
- **PosFormer** (handwritten, DenseNet + Transformer + ARM)
- **DeiT+TrOCR**, **BTTR**, **HMER**, **MixTex**, **Qwen2.5-VL**
- Standalone image recognition or layout-integrated crop-and-recognize
- Engine auto-detected from GGUF metadata

### P17.4 — Face detection

`src-tauri/src/images/face.rs`

Detects WHETHER and WHERE faces appear in photos:

- **YuNet** (0.2 MB, fastest) or **SCRFD** (16 MB, higher recall)
- Returns bounding boxes + confidence + facial landmarks
- Use cases: "photos with people" filter, auto-crop thumbnails

**EU AI Act compliant**: detection only — no face embeddings, no person
matching, no biometric identification.

### P17.5 — BidirLM-Omni cross-modal embeddings

`src-tauri/src/index/omni_embed.rs`

Shared 2048-D embedding space for text, audio, and images:

- `encode_text_omni` / `encode_text_omni_batch` — text encoding
- `encode_audio_omni` — raw 16 kHz mono PCM encoding
- `encode_image_omni` — image file encoding
- `encode_text_with_image_omni` — text conditioned on image

Enables a new RRF channel in search that mixes omni-vector cosine similarity
with existing FTS + dense + sparse channels. Schema migration v108 adds the
`embedding_omni` column.

### P17.6 — Decoder embedding models (GGUF-only)

5 new `EmbedderModel` variants in the 36-model registry:

| Model | Dimensions | Context | Architecture |
|---|---|---|---|
| Gemma3-Embedding 2B | 2048d | 8192 | Decoder, GeGLU, last-token pool |
| ModernBERT-base | 768d | 8192 | Pre-LN, GeGLU, per-layer RoPE |
| ModernBERT-large | 1024d | 8192 | Same |
| DeBERTa-v2-xlarge | 1536d | 512 | Disentangled attention |
| NomicBERT-MoE | 768d | 8192 | 8-expert top-2, SwiGLU, RoPE |

All quantizable to Q4_K via CrispEmbed. No ONNX Runtime dependency.

### P17.7 — ViT image embeddings

`src-tauri/src/images/vit_embed.rs`

SigLIP/CLIP image encoding for visual similarity search:

- Encodes images into a shared text-image vector space
- "Find similar images" works across crops, formats, resolutions
- L2-normalized embeddings for cosine similarity
- Schema migration v109 adds `embedding_vit` column

---

## Testing

- **35 new unit tests** covering all 7 modules (region parsing, stub
  degradation, similarity functions, feature-flag gating, serde strings)
- **15 live tests** (`#[ignore]`, require GGUF models on disk) covering
  model loading, encoding, cross-modal similarity, OCR pipelines
- **660 total tests pass**, 0 failures, 0 regressions

---

## Upgrade notes

- All new features require `--features crispembed` (or `crispembed-metal` /
  `crispembed-vulkan` / `crispembed-cuda` for GPU). Without the feature flag,
  CrispSorter works exactly as before.
- GGUF models auto-download from HuggingFace on first use (~50 MB to ~2.6 GB
  depending on model). Set `HF_HOME` to control the cache location.
- The `desktop` feature flag (introduced in v0.4.0 for mobile support) gates
  TTS, folder watcher, shell plugins, and mistral.rs. Desktop release builds
  should use `--features desktop`.
