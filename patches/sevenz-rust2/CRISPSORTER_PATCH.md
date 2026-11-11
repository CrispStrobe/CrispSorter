# Bounded container reader patch

Upstream: sevenz-rust2 0.25.0, Apache-2.0. The upstream LICENSE and source attribution are retained.
Original crates.io package SHA-256: e5e465701779cbc6d0d10cf907f175035cfdd50b8d3cb628d7f1f435ef02972f.

The application disables default features and uses one decoding thread. This narrow patch adds independent 8 MiB encoded/decoded header bounds; 2,048 file, block, pack-stream and aggregate sub-stream limits before allocation; eight coders and sixteen input streams per block; 64-byte coder properties and 2,048 UTF-16 units per name; and a 32 MiB individual metadata-vector ceiling. These fixed products bound header metadata separately from decoder dictionaries. Dictionary/model estimates for all LZMA/LZMA2 coders in a block are summed before any coder is allocated. The caller supplies the aggregate dictionary limit. These are component budgets, not a claim about total process RSS.

The caller streams every accepted or skipped solid entry under one actual-output budget, reads through CRC/terminal validation, and returns an error immediately on a budget breach. Nested decoding starts only after the outer decoder has been dropped. Encrypted content and optional codecs are not enabled.
