# Mobile native archive and notice identity

The OCR mobile builder compiles the pinned CrispEmbed and ggml checkouts into
one Android shared library or a flattened iOS static archive. The final app
receipt ties the packaged runtime to that build and retains each native license
and notice as an app resource.

## Vendored text layout implementation

The source audit of CrispEmbed
`2111db64d845b4ef6a374da2a2f69c202f1d1ce7` found that CMake appends
`vendor/sheenbidi/Source/SheenBidi.c` to `CRISPEMBED_SOURCES` with
`SB_CONFIG_UNITY` before constructing either library target. The iOS bundler
already includes `libcrispembed-static.a` when flattening the core and ggml
archives. Android compiles the same implementation into `libcrispembed.so`.
Neither target needs an additional SheenBidi library.

The builder now inspects the actual unstripped runtime with the NDK's
`llvm-nm` or Xcode's `nm`. It requires defined implementations of the seven
SheenBidi entry points used by the renderer, records the runtime's SHA-256, and
records the entire vendored Git tree and upstream revision. An undefined
reference, similarly named symbol, or archive member name cannot satisfy this
check. The final app's internal symbol names may be stripped; the proof is
explicitly attached to the original built runtime.

When selecting a source that includes this vendor tree, the feature runtime
lock must include the reviewed component pin:

```json
"native_components": {
  "sheenbidi": {
    "source_tree": "c39bc929d47b3a770995df2d155b942bc4edac4e"
  }
}
```

The existing source and ggml commit pins remain mandatory. The component check
rejects an omitted or changed vendor tree. Older pinned releases without this
dependency continue to package their existing native notices.

## App resources

The previous mobile path bypassed CMake's install target and copied only the
library and build receipt. It consequently omitted CMake's native notices,
including the new SheenBidi license. The builder now copies the committed
CrispEmbed, ggml, and OCR component notices into its `notices` directory. When
SheenBidi is present, it also includes `SheenBidi-LICENSE.txt`,
`NOTICE.sheenbidi`, and `SheenBidi-UPSTREAM.md`.

Every entry records its source repository, relative path, Git blob, byte size,
and SHA-256. Native builds must leave both source checkouts clean. Notice files
must be regular files whose bytes match their committed Git blobs. The mobile
workflow copies these files into the app's existing `bin` resources and uploads
them with the runtime receipt.

Before an app receipt is issued, the verifier rechecks the original native
artifact and notices. It then reads the actual APK or `.app` bundle and requires
exactly one copy of each expected notice with matching bytes. Missing,
duplicated, changed, or symlinked resources fail the gate. Android's independent
mapped-section identity check still binds the packaged runtime after symbol
stripping; the final APK's ELF load segments are also inspected.

## Qualification scope

The source audit and exact input hashes are retained in
`tests/results/ocr-studio/mobile-native-closure-2111.json`. Eight new helper
tests exercise the source pin, notice packaging, original artifact identity,
and symbol rejection paths. Their native symbol output and tiny archive
payloads are explicit structural fixtures; no target code executes in those
tests.

These checks establish archive closure and provenance when a target build
passes. They do not establish OCR quality, physical-device behavior, PDF reader
compatibility, or archival conformance. Source `2111db64` intentionally reports
PDF capability mask 47, withholding the general bidirectional-text bit 16.
Actual target and reader gates remain separate requirements.
