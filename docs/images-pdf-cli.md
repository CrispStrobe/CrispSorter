# Image-only PDF from the CLI

```sh
crispsorter images pdf *.JPG --out homework.pdf --black-and-white --perspective
```

This command prepares local JPEG, PNG, BMP and TIFF files and writes one image-only
PDF, without opening the desktop, accessing the index, running OCR, or downloading
models. Inputs retain command-line order; all TIFF frames are included. Animated
formats, including APNG, fail rather than silently dropping frames. EXIF orientation
is applied, alpha is composited onto white, and sources are never changed.

`--black-and-white` uses native adaptive Sauvola binarization, with pure white paper
and black marks. `--deskew` enables rotation correction. `--perspective` implies
deskew and requests ruled-photo geometry correction; it requires black and white.
Sparse or unsupported geometry falls back to rotation. The application still pins
CrispEmbed 0.17.19; that runtime falls back to rotation because ruled-photo correction
is pending the next native release. Use the paired worksheet native candidate to
exercise the perspective implementation before release.

`--paper a4` is the default. `letter` also pads white without stretching the image;
orientation follows each prepared image. `source` retains physical source dimensions
when available, otherwise uses `--dpi 300`. Output is staged in its destination
directory and published after successful export. Existing PDFs require `--force`;
the output cannot replace an input image. `--pdfa` requests PDF/A-2b where supported.
Limits are 500 pages, 64 megapixels per prepared page, and 2 GiB of PDF output.

The normal global `--format json` returns the destination, page count, byte count,
and timings in milliseconds. `preparation_ms` includes decoding, cleanup, padding
and temporary PNG writes; `cleanup_ms` is its native-processing subset. `pdf_ms`
measures the renderer, and `total_ms` includes staging and publication. Progress
is written to stderr. `--format text` prints a short completion message.

Local validation on six homework JPEGs (Apple M1, one run): optimized production
module executable with native candidate `0eca8e35d5d675d6fef106a6316db38355c6841a`
completed in 1.239 seconds wall time, 0.744 seconds inside the command. Preparation
took 0.551 seconds, including 0.357 seconds native cleanup; PDF rendering took
0.177 seconds. The resulting six-page PDF was 185,287 bytes. An independent PDF
parser confirmed one-bit black/white image layers and no text on every page.
This measures the focused executable below, not an installed desktop launch;
it uses Sauvola, whereas the earlier Python replay used Otsu, so timings are not
a controlled before/after comparison. Local contracts: 33 Rust tests, independent
PDF/CLI inspection, and the build without native features passed.

## Focused native contract

The contract executable compiles the production module, image sourcing and atomic
publication code with the real CrispEmbed Rust binding. It avoids the unrelated
desktop and indexing dependencies. It is a test executable, not a desktop release.
Check out CrispEmbed beside CrispSorter, at the same revision as the native runtime:

```sh
# On macOS also export DYLD_LIBRARY_PATH=/absolute/path/to/native/runtime;
# on Linux set LD_LIBRARY_PATH to the native library and its ggml directories.
CRISPEMBED_SYS_LIB_DIR=/absolute/path/to/native/runtime \
  cargo test --locked --manifest-path tests/images-pdf-contract/Cargo.toml
CRISPEMBED_SYS_LIB_DIR=/absolute/path/to/native/runtime \
  cargo build --locked --release --manifest-path tests/images-pdf-contract/Cargo.toml
# The focused executable accepts the same images pdf arguments:
tests/images-pdf-contract/target/release/crispsorter-images-pdf images pdf \
  *.JPG --out homework.pdf --black-and-white --perspective
```
