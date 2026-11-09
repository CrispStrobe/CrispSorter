# JPEG source integrity at import

Ordinary JPEG imports use the shared Rust image path, including files selected
on desktop and raw camera or provider bytes on mobile. The native iOS image
helper's checks alone do not protect this path.

Before decoding or entering the general OCR extractor fallback, the shared
`extractors::image_input` guard verifies a JPEG's marker structure. It requires
an end-of-image marker after a scan, respects segment lengths so an end-marker
byte sequence inside metadata cannot terminate the file, and handles entropy
stuffing, restart markers, DNL and multiple scans. It streams through a bounded
buffer and does not rewrite the source. Valid metadata and legal bytes after
the primary image's end marker remain intact. Pixel decoding still determines
whether an otherwise complete image is usable.

The workspace validates before publishing page pixels. Original file bytes or
the private raw-import archive remain available if validation fails; opaque
provider source identity is separate from the archive path. The extractor
fallback repeats the same source validation so it cannot bypass a rejected
JPEG by invoking another OCR engine.

## Executed decoder and guard qualification

[Run 42616849573](https://github.com/CrispStrobe/CrispSorter/actions/runs/42616849573)
tested source `8dad704c604708415fdc20fb0eb475c720d34cee` on Ubuntu 24.04.5 with
Rust 1.99.1. Its focused crate includes the exact production guard and uses
the application's locked `image 0.25.10` and `zune-jpeg 0.5.15` dependencies.
All registry versions and checksums were compared with the application lock
before execution. All six tests passed.

| Source derived from the pinned JPEG fixture | Raw decoder accepts | Source guard accepts |
| --- | --- | --- |
| Complete original | Yes | Yes |
| Complete original with legal trailing bytes | Yes | Yes |
| Complete original with an end-marker byte sequence in metadata | Yes | Yes |
| Missing the final end-of-image marker | Yes | No |
| Truncated scan, missing the last 100 bytes | Yes | No |
| Truncated scan with a misleading end-marker byte sequence in metadata | Yes | No |

The raw decoder accepted all three damaged cases. This is observed behavior
of the pinned dependencies, not an assumption about every JPEG decoder. The
guard rejected all three while the complete controls retained their original
bytes. Additional marker-grammar cases covered multiple scans, restart and
stuffing markers, DNL, invalid segment lengths and every truncated prefix of
a small structural control.

The four-entry test artifact `ocr-image-input-contracts`, ID `13676195121`,
was downloaded and its SHA-256 verified as
`6897138d992592c356195c5405cba4c9f2137730a6944db74935f315ab04c4db`.
The [qualification receipt](../tests/results/ocr-image-input/qualification.json)
records source hashes, dependency checksums, fixture identity and the six
decoder observations.

## Application and device scope

The source commit also adds full application regressions for both workspace
file import and raw provider-byte import. These assert rejection before page
assets are committed, preservation of source and archive bytes, orientation
and page-hash equivalence for valid controls, and rejection before the
general extractor's OCR fallback. Those tests require the complete application
test binary and are not covered by the focused six-test result above.

This host result does not qualify physical camera capture, a document picker,
or a phone's decoder. Native provider qualification remains separately
documented in [mobile import](ocr-mobile-import.md).
