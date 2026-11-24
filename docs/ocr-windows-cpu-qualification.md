# Windows OCR qualification

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
mixed timing results.

## Tesseract worker sweep

A separate Windows CPU run used the tiny detector and German recognizer on
the same four full-page layouts at 200 and 300 DPI. These results compare
settings within the same run. Accuracy matched across the three profiles.
Peak RSS is the server's Windows peak working set over both resolutions.

| Threads per worker | Tesseract workers | 200 DPI median | 300 DPI median | Peak RSS |
| ---: | ---: | ---: | ---: | ---: |
| 2 | 1 | 3.01 s | 3.89 s | 624 MiB |
| 1 | 2 | 2.60 s | 3.46 s | 626 MiB |
| 2 | 2 | 2.50 s | 3.48 s | 626 MiB |

## Bounded page crops

On the generated scans, cropping to the ink box with a 16-point margin
reduced mean decoded raster size from 3.87 to 0.48 MB at 200 DPI and from
8.70 to 1.08 MB at 300 DPI. Mean cropped PNG sizes were 53.2 and 97.8 kB.
The benchmark kept the full text reference for each page. All three CPU
profiles used two threads and one recognizer worker.

| Detector and recognizer | 200 DPI median / CER / WER | 300 DPI median / CER / WER | Peak RSS |
| --- | ---: | ---: | ---: |
| PP-OCRv6 tiny detector + Tesseract German | 2.37 s / 1.77% / 11.90% | 2.50 s / 0.79% / 5.67% | 492 MiB |
| DBNet + Tesseract German | 1.98 s / 3.11% / 17.56% | 2.07 s / 1.06% / 3.97% | 813 MiB |
| PP-OCRv6 small detector and recognizer | 12.19 s / 0.32% / 2.27% | 12.21 s / 0.16% / 1.13% | 1,014 MiB |

The small pair is a candidate for a selective high-accuracy pass on known
page regions. Its two models total 26.2 MB. Qualify the crop margin on scans
with marginalia, diagrams and tables before using automatic cropping.

## Vulkan and bundle size

The pinned Windows x64 CPU, Vulkan and CUDA archives are 12.7 MB, 28.7 MB
and 682.0 MB respectively. See [the CrispEmbed 0.17.20 release](https://github.com/CrispStrobe/CrispEmbed/releases/tag/v0.17.20).

On the Vulkan bundle, the automatic adapter selection used the integrated
GPU for native PP-OCRv6 recognition. The repeat with one visible discrete
GPU confirmed the selected adapter in the server log. In that repeat, the
tiny detector and German recognizer produced the same error rates as the
CPU run and took 3.25 s at 200 DPI and 4.36 s at 300 DPI. The native small
pair took 4.04 s and 4.91 s, with 100% WER at both resolutions. The discrete
adapter's sampled device-wide memory peaked at 255 MiB during these two
profiles. The automatic Vulkan run also had 100% WER for the native tiny and
small pairs. Use CPU for the selective cropped PP-OCRv6 small profile
until Vulkan passes output parity.

## Candidate settings for printed German

A first pass at 200 DPI using
`tesseract-deu` with the PP-OCRv6 tiny detector limits model download and
decoded pixels. The measured low-CPU setting is `-t 1` and
`CRISPEMBED_TESSERACT_WORKERS=2`. For dense copy or poor recognition, the
300 DPI DBNet and Tesseract German pairing gave better word accuracy.
Apply the higher DPI selectively to pages that need it. Treat the native
PP-OCRv6 tiny recognizer as a failed German qualification for this fixture.

The current CrispSorter renderer defaults to 200 DPI when source DPI is
unavailable. With the embedded source-DPI probe, the current range is
150–400 DPI. The fixture supports testing a 200 DPI first pass and a
bounded 300 DPI retry. Apply a production default after real scan
transcriptions and CUDA and Metal tests. See
[the pinned model registry](https://github.com/CrispStrobe/CrispEmbed/blob/d842065436fddae00a89bbc89db56b92293a1b23/examples/cli/model_mgr.cpp).

## Recognition evidence and selective resolution

With `--log-verbosity debug`, whole-page CrispEmbed runs with review diagnostics
and system Tesseract runs record `recognition_quality` in the page operation.
The record carries the PDF page number, engine, scoring unit, count of scored
units, mean and lower-tail score. A missing score is reported as null. The
log carries numeric summaries and engine identifiers. CrispEmbed's review
snapshot marks the source of each score, and system Tesseract contributes
word scores from its TSV output. OCRS and the current Paddle integration
return text without recognition evidence to this extraction path. Some
embedded vision models expose generation-token scores, whose units and
calibration differ from Tesseract word or line scores.

The Tesseract and tiny detector profile's dense page improved from **26.4%**
word error at 200 DPI to **6.9%** at 300 DPI. Its 200 DPI mean recognition score
was **0.885**, higher than the other three pages, which had lower word error.
A page mean alone would miss that retry. Compare the lower tail, detected
region coverage, text yield, layout and language on a labeled sample before
choosing a threshold for a specific engine and profile.

On the same four layouts and the two-worker configuration, 200, 300 and
400 DPI took 2.35, 3.31 and 4.15 s/page median and produced 17.28%, 7.08%
and 5.10% word error. Mean decoded grayscale rasters occupied 3.87, 8.70
and 15.47 MB. Start a retry evaluation at 300 DPI. Use 400 DPI for a second
qualification tier when source detail and page size justify its pixel cost.
Automatic retries require a validated policy.

The pinned CrispEmbed PP-OCRv6 legacy response put detector scores above
0.95 in the recognition field while tiny returned scrambled text. The
typed review snapshot already labels that value as detection. On five
identical generated line crops, the official Paddle tiny ONNX export read
four exactly and missed one character on the fifth. CrispEmbed tiny failed
all five at F32, F16 and head-only Q8. The small ONNX and small GGUF both read
all five exactly. See [the official tiny ONNX export](https://huggingface.co/PaddlePaddle/PP-OCRv6_tiny_rec_onnx/tree/2612ab37152ae0a677521bae4e1e3d4fb4cf7c30)
and [the native review schema](https://github.com/CrispStrobe/CrispEmbed/blob/d842065436fddae00a89bbc89db56b92293a1b23/docs/ocr-review-snapshot.md).
