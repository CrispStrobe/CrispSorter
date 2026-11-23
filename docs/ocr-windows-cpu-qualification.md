# Windows CPU OCR qualification

The test used the pinned CrispEmbed 0.17.20 Windows CPU bundle and four
generated German A4 layouts: ordinary printed text, dense small type, a
faint tilted scan and a table. Each layout was rendered at 140, 200 and
300 DPI. Unicode NFC and whitespace normalization were applied before
computing character error rate (CER) and word error rate (WER).

| DPI | Grayscale decoded bytes per A4 page | Average PNG size on the fixture |
| --- | ---: | ---: |
| 140 | 1.89 MB | 38.4 kB |
| 200 | 3.87 MB | 67.4 kB |
| 300 | 8.70 MB | 115.8 kB |

Reported page medians use four layouts at each DPI. CrispEmbed reused the
OCR server over loopback and counted HTTP request wall time after warmup.
OCRS started a fresh CLI process per page, with four Rayon workers and
cleanup off. This describes two operating modes. Comparable cold and warm
measurements for both engines are required for a performance ranking.

| Engine and models | Threads | Model bytes | 200 DPI median / CER / WER | 300 DPI median / CER / WER |
| --- | ---: | ---: | ---: | ---: |
| CrispEmbed Tesseract German + DBNet | 2 | 13.47 MB | 6.72 s / 12.69% / 36.26% | 10.23 s / 1.14% / 6.52% |
| CrispEmbed Tesseract German + PP-OCRv6 tiny detector | 2 | 1.39 MB | 6.79 s / 2.44% / 17.28% | 13.85 s / 0.99% / 7.08% |
| CrispEmbed PP-OCRv6 tiny detector + tiny recognizer | 2 | 5.45 MB | 7.10 s / 76.27% / 100% | 8.80 s / 77.06% / 100% |
| OCRS | 4 | 12.21 MB | 9.24 s / 5.64% / 36.54% | 6.80 s / 6.23% / 39.09% |

All completed profiles produced output for all 12 pages. The small PP-OCRv6
pair timed out on its first full-page 200 DPI request at the 60 s bound on
this CPU path. Four CrispEmbed threads preserved accuracy and delivered
mixed timing results. On the constrained CPU start with two threads.

**Candidate settings for printed German.** A first pass at 200 DPI using
`tesseract-deu` with the PP-OCRv6 tiny detector limits model download and
decoded pixels. For dense copy or poor recognition, the 300 DPI DBNet
and Tesseract German pairing gave substantially better word accuracy.
Apply the higher DPI selectively to pages that need it. Treat the native
PP-OCRv6 tiny recognizer as a failed German qualification for this bundle.

The current CrispSorter renderer defaults to 200 DPI when source DPI is
unavailable. With the embedded source-DPI probe, the current range is
150–400 DPI. The fixture supports testing a 200 DPI first pass and a
bounded 300 DPI retry. Apply a production default after real scan
transcriptions and tests on the CUDA/Vulkan and Metal builds. See
[the pinned model registry](https://github.com/CrispStrobe/CrispEmbed/blob/d842065436fddae00a89bbc89db56b92293a1b23/examples/cli/model_mgr.cpp).
