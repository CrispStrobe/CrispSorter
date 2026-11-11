# Bounded 7z reader contracts

```sh
cargo test --locked --manifest-path tests/sevenz-contract/Cargo.toml --lib
```

This compiles the exact vendored `sevenz-rust2` production source with its
application decoder dependency and optional codecs disabled. It runs upstream
stream/CRC/truncation/BCJ2/LZMA integrity regressions and the added metadata
preallocation and aggregate-dictionary limit regressions. Actual on-disk
COPY/LZMA2/solid/compressed-header extraction and member reopening are exercised
by `tests/extraction-contract`.
